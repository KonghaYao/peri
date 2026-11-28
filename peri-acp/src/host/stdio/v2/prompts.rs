//! Connection-local JSON-RPC receipts; the Agent mailbox owns actual inputs.

use std::collections::HashMap;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::transport::types::{AcpError, RequestId};

const MAX_PENDING_PER_SESSION: usize = 32;
const MAX_WAITERS: usize = 16;
const MAX_RECEIPTS: usize = 4096;

#[derive(Default)]
pub(super) struct Prompts {
    pending: HashMap<String, Pending>,
    receipts: HashMap<(String, String), Receipt>,
}

struct Pending {
    session_id: String,
    key: Option<String>,
    signature: [u8; 32],
    waiters: Vec<RequestId>,
    blocks: Value,
}

struct Receipt {
    message_id: String,
    signature: [u8; 32],
}

pub(super) enum Admission {
    Submit(String),
    Pending,
    Accepted(String),
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
        let compound = key.map(|key| (session_id.to_owned(), key.to_owned()));
        if let Some(receipt) = compound.as_ref().and_then(|key| self.receipts.get(key)) {
            if receipt.signature != signature {
                return Err(conflict());
            }
            return Ok(Admission::Accepted(receipt.message_id.clone()));
        }
        if let Some(key) = key {
            if let Some(pending) = self
                .pending
                .values_mut()
                .find(|p| p.session_id == session_id && p.key.as_deref() == Some(key))
            {
                if pending.signature != signature {
                    return Err(conflict());
                }
                if pending.waiters.len() >= MAX_WAITERS {
                    return Err(busy());
                }
                pending.waiters.push(id);
                return Ok(Admission::Pending);
            }
        }
        if (key.is_some() && self.receipts.len() + self.pending.len() >= MAX_RECEIPTS)
            || self
                .pending
                .values()
                .filter(|p| p.session_id == session_id)
                .count()
                >= MAX_PENDING_PER_SESSION
        {
            return Err(busy());
        }
        let message_id = uuid::Uuid::now_v7().to_string();
        self.pending.insert(
            message_id.clone(),
            Pending {
                session_id: session_id.to_owned(),
                key: key.map(str::to_owned),
                signature,
                waiters: vec![id],
                blocks,
            },
        );
        Ok(Admission::Submit(message_id))
    }

    pub(super) fn accepted(
        &mut self,
        sid: &str,
        message_id: &str,
    ) -> Option<(Value, Vec<RequestId>)> {
        if self.pending.get(message_id)?.session_id != sid {
            return None;
        }
        let Pending {
            session_id,
            key,
            signature,
            waiters,
            blocks,
        } = self.pending.remove(message_id)?;
        if let Some(key) = key {
            self.receipts.insert(
                (session_id, key),
                Receipt {
                    message_id: message_id.into(),
                    signature,
                },
            );
        }
        Some((
            json!({"sessionUpdate":"user_message","messageId":message_id,"content":blocks}),
            waiters,
        ))
    }

    pub(super) fn reject(&mut self, message_id: &str) -> Vec<RequestId> {
        self.pending
            .remove(message_id)
            .map_or_else(Vec::new, |p| p.waiters)
    }

    pub(super) fn pending_ids(&self, sid: &str) -> Vec<String> {
        self.pending
            .iter()
            .filter(|(_, p)| p.session_id == sid)
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub(super) fn reject_session(&mut self, sid: &str) -> Vec<RequestId> {
        let ids = self
            .pending
            .iter()
            .filter(|(_, p)| p.session_id == sid)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
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
        self.pending.iter().find_map(|(input, p)| {
            p.waiters
                .contains(id)
                .then(|| (p.session_id.clone(), input.clone()))
        })
    }
}

fn conflict() -> AcpError {
    AcpError::new(
        -32602,
        "Prompt idempotency key reused for different content",
    )
}

/// ACP v2 keys are opaque strings of 1 to 255 **characters**, not bytes.
pub(super) fn valid_idempotency_key(key: &str) -> bool {
    !key.is_empty() && key.chars().count() <= 255
}

fn busy() -> AcpError {
    AcpError::new(-32000, "ACP prompt receipt capacity reached")
}

#[cfg(test)]
#[path = "prompts_test.rs"]
mod tests;
