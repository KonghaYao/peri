//! `session/update` projection and foreground state publication.

use serde_json::{json, Value};

use super::state::Foreground;

/// Metadata updates that describe the session rather than live turn activity.
///
/// While a setup request is in flight (`Attach::Attaching`) the host may already
/// push these for the session being attached; holding them until the response is
/// sent keeps the client from losing the command table or config projection it
/// would only learn about post-attach.
pub(super) fn is_attach_held(update: &Value) -> bool {
    matches!(
        update.get("sessionUpdate").and_then(Value::as_str),
        Some(
            "available_commands_update"
                | "config_option_update"
                | "current_mode_update"
                | "session_info_update"
        )
    )
}

// The content conversion upstream already uses canonical message IDs. Never
// guess an ID for a real message or expose private routing metadata.
pub(super) fn project_update(
    mut update: Value,
    resetting_replay_chunk: bool,
) -> Option<Vec<Value>> {
    let kind = update.get("sessionUpdate")?.as_str()?.to_owned();
    let object = update.as_object_mut()?;
    object.remove("_meta");
    match kind.as_str() {
        "agent_message_chunk" | "agent_thought_chunk" | "user_message_chunk" => {
            let id = object.get("messageId")?.as_str()?.to_owned();
            if resetting_replay_chunk {
                let whole = kind.strip_suffix("_chunk")?;
                return Some(vec![
                    json!({"sessionUpdate": whole, "messageId": id, "content": []}),
                    update,
                ]);
            }
        }
        "tool_call" => {
            object.insert("sessionUpdate".into(), json!("tool_call_update"));
        }
        "tool_call_update" | "session_info_update" => {}
        // Legacy plans have no stable plan ID, and several legacy update
        // shapes are incompatible with v2. Do not emit a misleading upsert.
        _ => return None,
    }
    Some(vec![update])
}

/// Foreground state snapshot published as a `state_update`.
pub(super) fn state(foreground: Foreground, stop_reason: Option<&str>) -> Value {
    let mut update = json!({
        "sessionUpdate": "state_update",
        "state": match foreground {
            Foreground::Idle => "idle",
            Foreground::Running => "running",
            Foreground::RequiresAction => "requires_action",
        }
    });
    match stop_reason {
        Some("error") => {
            update["stopReason"] = json!("error");
            update["error"] = json!({"code": -32603, "message": "Peri execution failed"});
        }
        Some(stop_reason) => update["stopReason"] = json!(stop_reason),
        None => {}
    }
    update
}

#[cfg(test)]
#[path = "updates_test.rs"]
mod tests;
