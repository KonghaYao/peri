use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// One ACP-shaped content block accepted by `POST /events`.
///
/// S3 only supports `text`; any other tag is rejected as `invalid_input` by the
/// request parser instead of being silently dropped (`design §2.4`).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContentBlock {
    Text { text: String },
}

impl ContentBlock {
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    #[must_use]
    pub fn as_text(&self) -> &str {
        match self {
            Self::Text { text } => text,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputKind {
    User,
    Control,
}

impl InputKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Control => "control",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputMode {
    Turn,
    Steer,
}

impl InputMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Turn => "turn",
            Self::Steer => "steer",
        }
    }
}

/// Receipt state persisted in `swarm_inputs.state`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputState {
    Accepted,
    HandedOff,
    Failed,
}

impl InputState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::HandedOff => "handed_off",
            Self::Failed => "failed",
        }
    }
}

/// Result of one `POST /events` acceptance.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    /// Newly accepted input.
    Inserted,
    /// Same `inputId` and fingerprint replay.
    Existing,
    /// Same `inputId`, different fingerprint. Never enqueued.
    Conflict,
}

/// Validated submission passed from the HTTP layer to [`crate::service`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputSubmission {
    pub input_id: String,
    pub kind: InputKind,
    pub mode: InputMode,
    pub text: Option<String>,
    pub attachments: Vec<ContentBlock>,
    pub plugin_selections: Vec<String>,
    pub original_draft: Option<String>,
}

impl InputSubmission {
    /// `inputId` is the idempotency key; the fingerprint is the SHA-256 of a
    /// canonical JSON object with a fixed field order (`design §2.8`).
    /// Only the hash is persisted.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        let canonical = serde_json::json!({
            "kind": self.kind,
            "mode": self.mode,
            "text": self.text,
            "attachments": self.attachments,
            "pluginSelections": self.plugin_selections,
            "originalDraft": self.original_draft,
        });
        let encoded = serde_json::to_vec(&canonical).unwrap_or_default();
        let digest = Sha256::digest(&encoded);
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// Effective text: explicit `text` plus joined text blocks. Used for the
    /// empty-input check and for the S3 fake turn.
    #[must_use]
    pub fn effective_text(&self) -> String {
        let mut parts = Vec::new();
        if let Some(text) = self.text.as_deref() {
            parts.push(text);
        }
        for block in &self.attachments {
            parts.push(block.as_text());
        }
        parts.join("\n")
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        let text_empty = self
            .text
            .as_deref()
            .is_none_or(|text| text.trim().is_empty());
        let attachments_empty = self
            .attachments
            .iter()
            .all(|block| block.as_text().trim().is_empty());
        text_empty && attachments_empty
    }
}

/// Response body for an accepted `POST /events`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputOutcome {
    pub input_id: String,
    pub accepted: bool,
    pub disposition: Disposition,
    pub mode: InputMode,
    pub last_item_seq: u64,
}

impl InputOutcome {
    #[must_use]
    pub fn new(
        input_id: impl Into<String>,
        disposition: Disposition,
        mode: InputMode,
        last_item_seq: u64,
    ) -> Self {
        Self {
            input_id: input_id.into(),
            accepted: true,
            disposition,
            mode,
            last_item_seq,
        }
    }
}

#[cfg(test)]
#[path = "input_test.rs"]
mod tests;
