//! Source-bound excerpts retained by Full Compact, never an authorization grant.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

use crate::messages::MessageId;

pub const HANDOFF_VERSION: u32 = 1;
pub const HANDOFF_MAX_ENTRIES: usize = 64;
pub const HANDOFF_MAX_BYTES: usize = 16 * 1024;
pub const HANDOFF_MAX_QUOTE_BYTES: usize = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffKind {
    Goal,
    Constraint,
    Acceptance,
    Authorization,
    Entity,
    Recovery,
    Finding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffOrigin {
    UserMessage,
    SystemMessage,
    ToolOutput,
    AssistantClaim,
    Notification,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffEvidence {
    pub message_id: MessageId,
    pub origin: HandoffOrigin,
    pub quote: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_failed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<crate::tools::ToolExecutionEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffEntry {
    pub id: String,
    pub kind: HandoffKind,
    pub evidence: HandoffEvidence,
}

impl HandoffEntry {
    /// Stable identity covers the kind and all engine-derived evidence fields.
    pub fn new(kind: HandoffKind, evidence: HandoffEvidence) -> Result<Self, serde_json::Error> {
        let id = Sha256::digest(serde_json::to_vec(&(kind, &evidence))?)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(Self { id, kind, evidence })
    }
}

/// Stored on the summary's existing projection JSON, alongside its rendered text.
/// Only the compact producer creates this state; matching text is not provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactHandoff {
    pub version: u32,
    pub entries: Vec<HandoffEntry>,
}

impl Default for CompactHandoff {
    fn default() -> Self {
        Self {
            version: HANDOFF_VERSION,
            entries: Vec::new(),
        }
    }
}

impl CompactHandoff {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != HANDOFF_VERSION {
            return Err(format!(
                "unsupported compact handoff version {}",
                self.version
            ));
        }
        if self.entries.len() > HANDOFF_MAX_ENTRIES {
            return Err("too many compact handoff entries".into());
        }
        let mut ids = HashSet::new();
        for entry in &self.entries {
            let expected = HandoffEntry::new(entry.kind, entry.evidence.clone())
                .map_err(|error| error.to_string())?;
            if entry.id != expected.id || !ids.insert(&entry.id) {
                return Err("invalid or duplicate compact handoff identity".into());
            }
            let evidence = &entry.evidence;
            if evidence.quote.trim().is_empty() || evidence.quote.len() > HANDOFF_MAX_QUOTE_BYTES {
                return Err("invalid compact handoff quote size".into());
            }
            if entry.kind == HandoffKind::Authorization
                && evidence.origin != HandoffOrigin::UserMessage
            {
                return Err("authorization excerpt must originate in a user message".into());
            }
            match evidence.origin {
                HandoffOrigin::UserMessage | HandoffOrigin::SystemMessage => {}
                HandoffOrigin::ToolOutput | HandoffOrigin::Notification => {
                    if matches!(
                        entry.kind,
                        HandoffKind::Goal | HandoffKind::Constraint | HandoffKind::Acceptance
                    ) {
                        return Err(
                            "tool or notification sources cannot establish task contracts".into(),
                        );
                    }
                }
                HandoffOrigin::AssistantClaim => {
                    if entry.kind != HandoffKind::Finding {
                        return Err("assistant claims cannot establish task contracts".into());
                    }
                }
            }
            if evidence.origin != HandoffOrigin::ToolOutput
                && (evidence.tool_call_id.is_some()
                    || evidence.tool_failed.is_some()
                    || evidence.execution.is_some())
            {
                return Err("tool evidence attached to a non-tool source".into());
            }
            if evidence.origin == HandoffOrigin::ToolOutput
                && (evidence
                    .tool_call_id
                    .as_ref()
                    .is_none_or(|id| id.is_empty())
                    || evidence.tool_failed.is_none())
            {
                return Err("tool source lacks execution identity".into());
            }
        }
        let bytes = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        if bytes.len() > HANDOFF_MAX_BYTES {
            return Err(
                "compact handoff exceeds its byte budget; retire superseded entries with evidence"
                    .into(),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "compact_handoff_test.rs"]
mod tests;
