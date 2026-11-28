//! Admission, validation and result projection for the v2 session lifecycle.
//!
//! `session/new` and `session/resume` no longer require the connection to hold no
//! other session: each one binds its own slot. Resume binds the slot *before*
//! forwarding so the host's setup-time metadata updates have somewhere to go
//! (`Attach::Attaching`), and an in-flight setup whose reply fails drops the slot
//! again.

use serde_json::{json, Value};

use crate::transport::types::RequestId;

use super::params as wire;
use super::state::{Attach, Connection, Reply, Routed};
use super::{errors, CLOSE, INPUT};

/// `session/new`: one more session on the same connection.
pub(super) fn new_session(connection: &mut Connection, id: RequestId, params: &Value) -> Routed {
    if let Err(error) = wire::setup_params(params) {
        return Routed::Answered(Err(error));
    }
    connection.replies.insert(
        id.clone(),
        Reply::Setup {
            session: None,
            replay: false,
            owner: 0,
        },
    );
    Routed::Forward {
        id,
        method: "session/new".into(),
        params: params.clone(),
    }
}

/// `session/resume`: attach an existing session, replaying when asked.
pub(super) fn resume_session(connection: &mut Connection, id: RequestId, params: &Value) -> Routed {
    let Some(session_id) = params
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return Routed::Answered(Err(errors::invalid_params("Missing sessionId")));
    };
    if let Err(error) = wire::setup_params(params) {
        return Routed::Answered(Err(error));
    }
    let replay = match params.get("replayFrom").filter(|cursor| !cursor.is_null()) {
        None => false,
        Some(cursor) if cursor.get("type").and_then(Value::as_str) == Some("start") => true,
        _ => return Routed::Answered(Err(errors::invalid_params("Unsupported replay cursor"))),
    };
    // The adapter can hold live updates until the reply and report the current
    // foreground state, but the shared host still refuses `session/load` while
    // this session's turn holds the per-session prompt lock. The v2 client sees
    // a stable busy server error with `reason = "session_busy"` and should retry after
    // `idle`; replay during an active turn is not supported end to end today.
    let mut forwarded = params.clone();
    if let Some(object) = forwarded.as_object_mut() {
        object.remove("replayFrom");
    }
    let owner = match connection.slots.get_mut(&session_id) {
        Some(slot) => {
            slot.setup += 1;
            slot.attach = Attach::Attaching;
            slot.setup
        }
        None => {
            let slot = connection.insert_slot(&session_id, Attach::Attaching);
            slot.setup = 1;
            1
        }
    };
    if let Some(slot) = connection.slots.get_mut(&session_id) {
        // Bind this attempt's replay state before the host sees the request:
        // a cursorless attempt stops the active stream, a replay attempt owns
        // a fresh one, and earlier attempts keep theirs for a handback.
        slot.start_setup_replay(owner, replay);
    }
    connection.replies.insert(
        id.clone(),
        Reply::Setup {
            session: Some(session_id),
            replay,
            owner,
        },
    );
    Routed::Forward {
        id,
        method: if replay {
            "session/load"
        } else {
            "session/resume"
        }
        .into(),
        params: forwarded,
    }
}

/// `session/close`: only this connection's own slot is affected.
///
/// A session this connection never attached is a success (the spec's SHOULD for
/// closing something already gone); the host still runs the shared close.
pub(super) fn close_session(connection: &mut Connection, id: RequestId, params: &Value) -> Routed {
    let Some(session_id) = params
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return Routed::Answered(Err(errors::invalid_params("Missing sessionId")));
    };
    let Some(slot) = connection.slots.get(&session_id) else {
        return Routed::Answered(Ok(json!({})));
    };
    let input_ids = slot.prompts.pending_ids(&session_id);
    connection
        .replies
        .insert(id.clone(), Reply::Close(session_id.clone()));
    Routed::Forward {
        id,
        method: CLOSE.into(),
        params: json!({"sessionId": session_id, "inputIds": input_ids}),
    }
}

/// `session/delete`: forward to the host, settle this connection's prompts and
/// drop its slot.
///
/// A delete is a durable operation on a session that need not be attached to
/// this connection at all (the pinned `session/list` → `session/delete` →
/// `session/list` flow), so every delete reaches the host even without a local
/// slot. Only a successful reply tears a slot down; the host's own error for a
/// session it cannot delete is passed through unchanged.
pub(super) fn delete_session(connection: &mut Connection, id: RequestId, params: &Value) -> Routed {
    let Some(session_id) = params
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return Routed::Answered(Err(errors::invalid_params("Missing sessionId")));
    };
    connection
        .replies
        .insert(id.clone(), Reply::Delete(session_id));
    Routed::Forward {
        id,
        method: "session/delete".into(),
        params: params.clone(),
    }
}

/// `session/prompt`: the slot must be attached; content is translated here.
pub(super) fn prompt(connection: &mut Connection, id: RequestId, params: &Value) -> Routed {
    let Some(session_id) = params
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return Routed::Answered(Err(errors::invalid_params("Missing sessionId")));
    };
    if connection.slot(&session_id).is_none() {
        return Routed::Answered(Err(errors::session_not_found(&session_id)));
    }
    let blocks = match wire::prompt_blocks(params) {
        Ok(blocks) => blocks,
        Err(error) => return Routed::Answered(Err(error)),
    };
    let key = match params.get("idempotencyKey").filter(|key| !key.is_null()) {
        None => None,
        Some(key)
            if key
                .as_str()
                .is_some_and(super::prompts::valid_idempotency_key) =>
        {
            key.as_str()
        }
        _ => return Routed::Answered(Err(errors::invalid_params("Invalid idempotencyKey"))),
    };
    let admission = {
        let Some(slot) = connection.slots.get_mut(&session_id) else {
            return Routed::Answered(Err(errors::session_not_found(&session_id)));
        };
        slot.prompts
            .admit(&session_id, key, blocks.clone(), id.clone())
    };
    match admission {
        Ok(super::prompts::Admission::Submit(input)) => {
            let internal_id = RequestId::String(uuid::Uuid::now_v7().to_string());
            connection
                .replies
                .insert(internal_id.clone(), Reply::Input(input.clone()));
            Routed::Forward {
                id: internal_id,
                method: INPUT.into(),
                params: json!({"sessionId": session_id, "inputId": input, "prompt": blocks}),
            }
        }
        Ok(super::prompts::Admission::Pending) => Routed::Parked,
        Err(error) => Routed::Answered(Err(error)),
    }
}

/// Project a v1 lifecycle result into its v2 shape.
///
/// v2 replaced the mode/model surfaces with `configOptions`, so the legacy
/// `modes`/`models` blocks and the v1 `_meta` never reach the client; the option
/// list itself is kept with the v2 identity field (`session/load` and
/// `session/new` already return it).
pub(super) fn lifecycle_result(method: &str, mut result: Value) -> Value {
    if method == "session/new" || method == "session/resume" {
        if let Some(object) = result.as_object_mut() {
            for key in ["modes", "models"] {
                object.remove(key);
            }
            super::config::project_options(object);
        }
    }
    result
}

#[cfg(test)]
#[path = "lifecycle_test.rs"]
mod tests;
