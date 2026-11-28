//! In-flight prompt admission; durable de-duplication is the message identity.
//!
//! An ACP v2 `idempotencyKey` is honoured by construction: a keyed submission's
//! message id is derived from `(sessionId, key)`, so the durable store's existing
//! "this id is already in the transcript" fact answers a retry across
//! connections and process restarts without a receipt table. This module only
//! tracks submissions that are still waiting for their first committed delivery:
//! a second request for the same key while the first is in flight joins it, and
//! different content under one key is a conflict.

use std::collections::HashMap;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::{uuid, Uuid};

use crate::transport::types::{AcpError, RequestId};

use super::errors;

const MAX_PENDING_PER_SESSION: usize = 32;
const MAX_WAITERS: usize = 16;

/// `UUIDv5(NAMESPACE_DNS, "peri-acp.prompt-idempotency-key.v1")`.
///
/// Persistence contract: this value decides every keyed message id. Changing it
/// makes each already-accepted key map to a new message, so a retry after an
/// upgrade would insert a second message. Treat it as frozen.
const PROMPT_NAMESPACE: Uuid = uuid!("8a408d08-2da5-51d9-9677-584d3f005800");

/// Separator between the two hash inputs. `sessionId` is a session this
/// connection attached, so it is always one of Peri's UUIDs and can never
/// contain the separator; the encoding is therefore injective.
const SCOPE_SEPARATOR: u8 = 0x1f;

#[derive(Default)]
pub(super) struct Prompts {
    pending: HashMap<String, Pending>,
}

struct Pending {
    session_id: String,
    signature: [u8; 32],
    waiters: Vec<RequestId>,
    blocks: Value,
}

pub(super) enum Admission {
    /// Send this message id to the host for insertion.
    Submit(String),
    /// The same submission is already in flight; the caller waits.
    Pending,
}

impl Prompts {
    pub(super) fn admit(
        &mut self,
        session_id: &str,
        key: Option<&str>,
        blocks: Value,
        id: RequestId,
    ) -> Result<Admission, AcpError> {
        let signature = Sha256::digest(blocks.to_string().as_bytes()).into();
        let message_id = match key {
            Some(key) => derive_message_id(session_id, key),
            None => Uuid::now_v7().to_string(),
        };
        if let Some(pending) = self.pending.get_mut(&message_id) {
            if pending.signature != signature {
                return Err(conflict());
            }
            if pending.waiters.len() >= MAX_WAITERS {
                return Err(errors::prompt_capacity(session_id));
            }
            pending.waiters.push(id);
            return Ok(Admission::Pending);
        }
        let pending = self
            .pending
            .values()
            .filter(|pending| pending.session_id == session_id)
            .count();
        if pending >= MAX_PENDING_PER_SESSION {
            return Err(errors::prompt_capacity(session_id));
        }
        self.pending.insert(
            message_id.clone(),
            Pending {
                session_id: session_id.to_owned(),
                signature,
                waiters: vec![id],
                blocks,
            },
        );
        Ok(Admission::Submit(message_id))
    }

    /// The submission reached the transcript: acknowledge every waiter and
    /// return the live `user_message` update for it.
    pub(super) fn accepted(
        &mut self,
        sid: &str,
        message_id: &str,
    ) -> Option<(Value, Vec<RequestId>)> {
        if self.pending.get(message_id)?.session_id != sid {
            return None;
        }
        let Pending {
            waiters, blocks, ..
        } = self.pending.remove(message_id)?;
        Some((
            json!({"sessionUpdate":"user_message","messageId":message_id,"content":blocks}),
            waiters,
        ))
    }

    /// Acknowledge a submission the host reports as already handed off.
    ///
    /// The message is in the transcript, so the retry gets the original id and
    /// no second insertion. The pending entry is dropped rather than kept: a
    /// delivery event for an inserted message never arrives, and a kept entry
    /// would make the next retry of that key park on it forever.
    pub(super) fn acknowledge(&mut self, message_id: &str) -> Vec<RequestId> {
        self.pending
            .remove(message_id)
            .map_or_else(Vec::new, |pending| pending.waiters)
    }

    pub(super) fn reject(&mut self, message_id: &str) -> Vec<RequestId> {
        self.pending
            .remove(message_id)
            .map_or_else(Vec::new, |pending| pending.waiters)
    }

    pub(super) fn pending_ids(&self, sid: &str) -> Vec<String> {
        self.pending
            .iter()
            .filter(|(_, pending)| pending.session_id == sid)
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub(super) fn reject_session(&mut self, sid: &str) -> Vec<RequestId> {
        let ids = self.pending_ids(sid);
        ids.into_iter().flat_map(|id| self.reject(&id)).collect()
    }

    /// Reject a specific input only if it still belongs to `sid`.
    pub(super) fn reject_in_session(&mut self, sid: &str, message_id: &str) -> Vec<RequestId> {
        match self.pending.get(message_id) {
            Some(pending) if pending.session_id == sid => self.reject(message_id),
            _ => Vec::new(),
        }
    }

    pub(super) fn target(&self, id: &RequestId) -> Option<(String, String)> {
        self.pending.iter().find_map(|(input, pending)| {
            pending
                .waiters
                .contains(id)
                .then(|| (pending.session_id.clone(), input.clone()))
        })
    }
}

/// The message id a keyed submission has once it reaches the transcript.
///
/// `UUIDv5(namespace, "sessionId\u{1f}key")`: the key is scoped to one session,
/// and the session id is part of the hash input, so the same key in another
/// session derives a different id.
pub(super) fn derive_message_id(session_id: &str, key: &str) -> String {
    let mut name = Vec::with_capacity(session_id.len() + 1 + key.len());
    name.extend_from_slice(session_id.as_bytes());
    name.push(SCOPE_SEPARATOR);
    name.extend_from_slice(key.as_bytes());
    Uuid::new_v5(&PROMPT_NAMESPACE, &name).to_string()
}

fn conflict() -> AcpError {
    errors::invalid_params("Prompt idempotency key reused for different content")
}

/// ACP v2 keys are opaque strings of 1 to 255 **characters**, not bytes.
pub(super) fn valid_idempotency_key(key: &str) -> bool {
    !key.is_empty() && key.chars().count() <= 255
}

#[cfg(test)]
#[path = "prompts_test.rs"]
mod tests;
