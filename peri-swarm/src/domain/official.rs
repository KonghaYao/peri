//! Official `openai@7.60.0` Agents API event frames (`design §4.2`).
//!
//! The gateway keeps emitting its own `item`/`turn` frames; these types are the
//! additive compatibility layer an official SDK consumes. Identity rules:
//!
//! - `turn_id` is derived from the persisted user item that opened the turn
//!   ([`turn_id_for_item`]); it survives a reconnect because replay derives it
//!   from the same row.
//! - `event_id` is derived from that `turn_id`, or from the persisted item
//!   `seq` for [`OfficialEvent::TurnItemAdded`]. No clock or random source is
//!   involved, so live delivery and backlog replay produce the same ids.
//!
//! Field names follow the SDK's snake_case JSON; timestamps are Unix seconds.

use std::collections::BTreeMap;

use chrono::DateTime;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::session::SessionStatus;

/// Derive the internal turn id from the user item that opened the turn.
///
/// The item id is stable across replay (`itim_input_<inputId>` for submitted
/// inputs, a generated `itim_<uuid>` for initial create input), so the derived
/// turn id is stable too. It is deliberately **not** the assistant
/// `messageId`: one message can span several persisted frames and does not
/// identify the turn.
#[must_use]
pub fn turn_id_for_item(item_id: &str) -> String {
    format!("turn_{}", item_id.strip_prefix("itim_").unwrap_or(item_id))
}

#[must_use]
pub fn turn_created_event_id(turn_id: &str) -> String {
    format!("evt_{turn_id}_created")
}

#[must_use]
pub fn turn_item_added_event_id(seq: u64) -> String {
    format!("evt_item_{seq}_added")
}

#[must_use]
pub fn turn_completed_event_id(turn_id: &str) -> String {
    format!("evt_{turn_id}_completed")
}

#[must_use]
pub fn turn_failed_event_id(turn_id: &str) -> String {
    format!("evt_{turn_id}_failed")
}

#[must_use]
pub fn session_idle_event_id(turn_id: &str) -> String {
    format!("evt_{turn_id}_idle")
}

#[must_use]
pub fn session_failed_event_id(turn_id: &str) -> String {
    format!("evt_{turn_id}_session_failed")
}

/// Unix seconds, used for the `created_at`/`started_at` fields an official
/// `Turn`/session carries.
#[must_use]
pub fn unix_now() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0),
    )
    .unwrap_or(i64::MAX)
}

/// Convert the persisted RFC3339 item timestamp to Unix seconds.
///
/// A malformed timestamp yields `0` rather than a fabricated "now": replay must
/// be deterministic, and `0` is visibly not a real observation.
#[must_use]
pub fn unix_from_rfc3339(value: &str) -> i64 {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.timestamp())
        .unwrap_or(0)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TurnStatus {
    Queued,
    InProgress,
    Waiting,
    Completed,
    Failed,
    Cancelled,
}

impl TurnStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::InProgress => "in_progress",
            Self::Waiting => "waiting",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageRole {
    User,
    Assistant,
}

impl MessageRole {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

/// The canonical public turn snapshot (`TurnsAPI.Turn`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OfficialTurn {
    pub id: String,
    pub object: String,
    pub agent_id: String,
    pub session_id: String,
    pub status: String,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub subagent_id: Option<String>,
    pub error: Option<Value>,
    pub usage: Option<Value>,
}

impl OfficialTurn {
    #[must_use]
    pub fn new(
        session_id: &str,
        turn_id: &str,
        status: TurnStatus,
        created_at: i64,
        completed_at: Option<i64>,
    ) -> Self {
        Self {
            id: turn_id.to_owned(),
            object: "agent.session.turn".to_owned(),
            // Swarm turns are always the root agent's; subagent turns are not
            // modelled (`design §4.2`).
            agent_id: "root".to_owned(),
            session_id: session_id.to_owned(),
            status: status.as_str().to_owned(),
            created_at,
            started_at: Some(created_at),
            completed_at,
            subagent_id: None,
            error: None,
            usage: None,
        }
    }

    /// Attach the customer-safe failure description (`SessionTurnError`).
    ///
    /// Only the live failure path has a message; the persisted log does not
    /// carry one, so replay keeps `error: null` instead of inventing a reason.
    #[must_use]
    pub fn with_error(mut self, message: Option<String>) -> Self {
        self.error = message
            .filter(|message| !message.trim().is_empty())
            .map(|message| {
                serde_json::json!({
                    "code": "internal_error",
                    "message": message,
                })
            });
        self
    }
}

/// One content block of an official `message` item.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum OfficialContent {
    #[serde(rename = "input_text")]
    InputText { text: String },
    #[serde(rename = "output_text")]
    OutputText { text: String },
}

/// The `message` member of `AgentSessionItem`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OfficialMessageItem {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub role: String,
    pub status: String,
    pub phase: Option<String>,
    pub content: Vec<OfficialContent>,
    pub turn_id: String,
}

impl OfficialMessageItem {
    #[must_use]
    pub fn user(item_id: &str, turn_id: &str, text: &str) -> Self {
        Self {
            id: item_id.to_owned(),
            kind: "message".to_owned(),
            role: MessageRole::User.as_str().to_owned(),
            status: "completed".to_owned(),
            phase: None,
            content: vec![OfficialContent::InputText {
                text: text.to_owned(),
            }],
            turn_id: turn_id.to_owned(),
        }
    }

    #[must_use]
    pub fn assistant(item_id: &str, turn_id: &str, text: &str) -> Self {
        Self {
            id: item_id.to_owned(),
            kind: "message".to_owned(),
            role: MessageRole::Assistant.as_str().to_owned(),
            status: "completed".to_owned(),
            phase: Some("final_answer".to_owned()),
            content: vec![OfficialContent::OutputText {
                text: text.to_owned(),
            }],
            turn_id: turn_id.to_owned(),
        }
    }
}

/// Minimal `AgentSession.Agent` projection: only what the gateway persists.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OfficialSessionAgent {
    pub id: String,
    pub model: Option<String>,
    pub instructions: Option<String>,
}

/// Minimal `AgentSession` carried by `agent.session.idle`/`agent.session.failed`.
///
/// Required collections are present but empty, and `model`/`instructions` stay
/// `null` when the deployment did not record them. The gateway never invents a
/// model name, vault id or metadata entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OfficialSession {
    pub id: String,
    pub object: String,
    pub status: String,
    pub created_at: i64,
    pub last_active_at: i64,
    pub error: Option<String>,
    pub agent: OfficialSessionAgent,
    pub metadata: BTreeMap<String, String>,
    pub required_actions: Vec<Value>,
    pub usage: Option<Value>,
    pub vault_ids: Vec<String>,
}

impl OfficialSession {
    #[must_use]
    pub fn new(
        session_id: &str,
        status: SessionStatus,
        created_at: i64,
        last_active_at: i64,
        error: Option<String>,
        model: Option<String>,
        instructions: Option<String>,
    ) -> Self {
        Self {
            id: session_id.to_owned(),
            object: "agent.session".to_owned(),
            status: status.as_str().to_owned(),
            created_at,
            last_active_at,
            error,
            agent: OfficialSessionAgent {
                id: "root".to_owned(),
                model,
                instructions,
            },
            metadata: BTreeMap::new(),
            required_actions: Vec::new(),
            usage: None,
            vault_ids: Vec::new(),
        }
    }
}

/// The subset of `AgentSessionEvent` the gateway can honor.
///
/// `requires_action`, `turn.cancelled`, budget and subagent frames are not
/// emitted: the local runtime has no corresponding fact.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum OfficialEvent {
    #[serde(rename = "agent.session.turn.created")]
    TurnCreated {
        event_id: String,
        session_id: String,
        turn_id: String,
        turn: OfficialTurn,
    },
    #[serde(rename = "agent.session.turn.item.added")]
    TurnItemAdded {
        event_id: String,
        session_id: String,
        turn_id: Option<String>,
        output_index: Option<u32>,
        item: OfficialMessageItem,
    },
    #[serde(rename = "agent.session.turn.completed")]
    TurnCompleted {
        event_id: String,
        session_id: String,
        turn_id: String,
        turn: OfficialTurn,
        usage: Option<Value>,
    },
    #[serde(rename = "agent.session.turn.failed")]
    TurnFailed {
        event_id: String,
        session_id: String,
        turn_id: String,
        turn: OfficialTurn,
        usage: Option<Value>,
    },
    #[serde(rename = "agent.session.idle")]
    SessionIdle {
        event_id: String,
        session: OfficialSession,
    },
    #[serde(rename = "agent.session.failed")]
    SessionFailed {
        event_id: String,
        session: OfficialSession,
    },
}

impl OfficialEvent {
    #[must_use]
    pub fn turn_created(session_id: &str, turn_id: &str, turn: OfficialTurn) -> Self {
        Self::TurnCreated {
            event_id: turn_created_event_id(turn_id),
            session_id: session_id.to_owned(),
            turn_id: turn_id.to_owned(),
            turn,
        }
    }

    #[must_use]
    pub fn turn_item_added(
        seq: u64,
        session_id: &str,
        turn_id: &str,
        item: OfficialMessageItem,
    ) -> Self {
        Self::TurnItemAdded {
            event_id: turn_item_added_event_id(seq),
            session_id: session_id.to_owned(),
            turn_id: Some(turn_id.to_owned()),
            output_index: None,
            item,
        }
    }

    #[must_use]
    pub fn turn_completed(session_id: &str, turn_id: &str, turn: OfficialTurn) -> Self {
        Self::TurnCompleted {
            event_id: turn_completed_event_id(turn_id),
            session_id: session_id.to_owned(),
            turn_id: turn_id.to_owned(),
            turn,
            usage: None,
        }
    }

    #[must_use]
    pub fn turn_failed(session_id: &str, turn_id: &str, turn: OfficialTurn) -> Self {
        Self::TurnFailed {
            event_id: turn_failed_event_id(turn_id),
            session_id: session_id.to_owned(),
            turn_id: turn_id.to_owned(),
            turn,
            usage: None,
        }
    }

    #[must_use]
    pub fn session_idle(turn_id: &str, session: OfficialSession) -> Self {
        Self::SessionIdle {
            event_id: session_idle_event_id(turn_id),
            session,
        }
    }

    #[must_use]
    pub fn session_failed(turn_id: &str, session: OfficialSession) -> Self {
        Self::SessionFailed {
            event_id: session_failed_event_id(turn_id),
            session,
        }
    }

    /// SSE `event:` name and JSON `type` value.
    #[must_use]
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::TurnCreated { .. } => "agent.session.turn.created",
            Self::TurnItemAdded { .. } => "agent.session.turn.item.added",
            Self::TurnCompleted { .. } => "agent.session.turn.completed",
            Self::TurnFailed { .. } => "agent.session.turn.failed",
            Self::SessionIdle { .. } => "agent.session.idle",
            Self::SessionFailed { .. } => "agent.session.failed",
        }
    }

    /// Stable id used by `TurnState` to dedupe live and replayed deliveries.
    #[must_use]
    pub fn event_id(&self) -> &str {
        match self {
            Self::TurnCreated { event_id, .. }
            | Self::TurnItemAdded { event_id, .. }
            | Self::TurnCompleted { event_id, .. }
            | Self::TurnFailed { event_id, .. }
            | Self::SessionIdle { event_id, .. }
            | Self::SessionFailed { event_id, .. } => event_id,
        }
    }
}

#[cfg(test)]
#[path = "official_test.rs"]
mod tests;
