//! Official `openai@7.60.0` Agents API event frames (`design §4.2`).
//!
//! The gateway keeps emitting its own `item`/`turn` frames; these types are the
//! additive compatibility layer an official SDK consumes. Identity rules:
//!
//! - `turn_id` is derived from the persisted user item that opened the turn
//!   ([`turn_id_for_item`]); it survives a reconnect because replay derives it
//!   from the same row.
//! - `event_id` is derived from that `turn_id`, from the persisted item `seq`,
//!   or — for an assistant item's added/content-part frames — from the stable
//!   `item_id`. No clock or random source is involved, so live delivery and
//!   backlog replay produce the same ids for those frames.
//! - The one deliberate exception is [`OfficialEvent::TurnOutputTextDelta`]:
//!   live chunks have no persisted `seq`, so their ids use an in-process chunk
//!   counter while replay uses the completion row's `seq`. The two deliveries
//!   therefore do not share a delta `event_id` (`design §4.2.1`).
//!
//! Field names follow the SDK's snake_case JSON; timestamps are Unix seconds.

use std::collections::BTreeMap;

use chrono::DateTime;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::item::ItemOrder;
use crate::domain::session::{EnvironmentRef, PromptCacheOptions, SessionStatus, SessionView};

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
pub fn turn_item_done_event_id(seq: u64) -> String {
    format!("evt_item_{seq}_done")
}

/// Assistant `item.added` is keyed by the stable item id, so the first live
/// chunk and replay name the frame the same way. The user item keeps the
/// `seq`-derived id that predates live deltas.
#[must_use]
pub fn turn_assistant_item_added_event_id(item_id: &str) -> String {
    format!("evt_item_{item_id}_added")
}

#[must_use]
pub fn turn_item_part_added_event_id(item_id: &str) -> String {
    format!("evt_item_{item_id}_part_added")
}

/// Live text delta: deterministic within one process, keyed by the stream's
/// stable item id and its chunk ordinal. It cannot be derived from a persisted
/// `seq` because chunks are never stored.
#[must_use]
pub fn turn_output_text_delta_event_id(item_id: &str, chunk: u64) -> String {
    format!("evt_item_{item_id}_delta_{chunk}")
}

/// Replay text delta: one full-text delta per completion row, keyed by that
/// row's `seq`. It intentionally differs from the live chunk id.
#[must_use]
pub fn turn_output_text_delta_replay_event_id(seq: u64) -> String {
    format!("evt_item_{seq}_text_delta")
}

#[must_use]
pub fn turn_item_part_done_event_id(seq: u64) -> String {
    format!("evt_item_{seq}_part_done")
}

#[must_use]
pub fn turn_item_text_done_event_id(seq: u64) -> String {
    format!("evt_item_{seq}_text_done")
}

#[must_use]
pub fn turn_in_progress_event_id(turn_id: &str) -> String {
    format!("evt_{turn_id}_in_progress")
}

#[must_use]
pub fn session_in_progress_event_id(turn_id: &str) -> String {
    format!("evt_{turn_id}_session_in_progress")
}

/// `requires_action` is a snapshot of the **current** pending approvals, keyed
/// by the persisted approval item `seq` (no clock, no random source).
#[must_use]
pub fn requires_action_event_id(seq: u64) -> String {
    format!("evt_item_{seq}_requires_action")
}

#[must_use]
pub fn session_created_event_id(session_id: &str) -> String {
    format!("evt_{session_id}_session_created")
}

#[must_use]
pub fn turn_error_event_id(turn_id: &str) -> String {
    format!("evt_{turn_id}_error")
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

    /// Parse a persisted status; unknown text is rejected instead of guessed.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(Self::Queued),
            "in_progress" => Some(Self::InProgress),
            "waiting" => Some(Self::Waiting),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    /// Whether a turn in this status has not reached a terminal state.
    #[must_use]
    pub fn is_open(self) -> bool {
        matches!(self, Self::Queued | Self::InProgress | Self::Waiting)
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

    /// Attach the turn's accumulated usage (`Turn.usage`).
    ///
    /// The live terminal frames read the same value, so a streamed
    /// `turn.completed`/`turn.failed` and a later `turns.retrieve` agree.
    #[must_use]
    pub fn with_usage(mut self, usage: Option<Value>) -> Self {
        self.usage = usage;
        self
    }
}

/// Query window for `GET /v1/agents/sessions/{id}/turns` (`TurnListParams`).
///
/// `after` is a turn id and is exclusive in the selected order. Turns are
/// ordered by `(created_at, turn_id)`, the stable total order used by the
/// store; the id is the tie-break for two turns created in the same second.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TurnsWindow {
    pub order: ItemOrder,
    pub after: Option<String>,
    pub limit: u32,
}

impl TurnsWindow {
    pub const DEFAULT_LIMIT: u32 = 20;
    pub const MAX_LIMIT: u32 = 500;
}

/// One `CursorPage<Turn>` page.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct TurnPage {
    pub object: String,
    pub data: Vec<OfficialTurn>,
    pub has_more: bool,
    /// Id of the last turn in page order; `None` on an empty page. Accepts
    /// the camelCase name this gateway serializes.
    #[serde(rename = "nextCursor", default)]
    pub next_cursor: Option<String>,
}

impl TurnPage {
    #[must_use]
    pub fn new(data: Vec<OfficialTurn>, has_more: bool, next_cursor: Option<String>) -> Self {
        Self {
            object: "list".to_owned(),
            data,
            has_more,
            next_cursor,
        }
    }
}

impl Serialize for TurnPage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut page = serializer.serialize_struct("TurnPage", 5)?;
        page.serialize_field("object", &self.object)?;
        page.serialize_field("data", &self.data)?;
        page.serialize_field("has_more", &self.has_more)?;
        page.serialize_field("hasMore", &self.has_more)?;
        page.serialize_field("nextCursor", &self.next_cursor)?;
        page.end()
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

    /// Replace the assistant message phase.
    ///
    /// Live `finish_turn` knows which flushed message is the turn's last one and
    /// persists that decision; replay reads it back instead of deriving the
    /// phase a second way.
    #[must_use]
    pub fn with_phase(mut self, phase: Option<&str>) -> Self {
        self.phase = phase.map(str::to_owned);
        self
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
    /// Official `AgentSession.environment`: the session's declared/bound
    /// environment. Unknown stays `none` with no id -- never invented.
    pub environment: EnvironmentRef,
    pub metadata: BTreeMap<String, String>,
    /// Optional in `AgentSession`; omitted entirely when the session has no
    /// configured value instead of sending `null`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_options: Option<PromptCacheOptions>,
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
            environment: EnvironmentRef::none(),
            metadata: BTreeMap::new(),
            prompt_cache_options: None,
            required_actions: Vec::new(),
            usage: None,
            vault_ids: Vec::new(),
        }
    }

    /// Fill the current `required_actions` snapshot.
    #[must_use]
    pub fn with_required_actions(mut self, required_actions: Vec<Value>) -> Self {
        self.required_actions = required_actions;
        self
    }

    /// Fill the persisted environment projection (`agent.session.*` snapshots).
    #[must_use]
    pub fn with_environment(mut self, environment: EnvironmentRef) -> Self {
        self.environment = environment;
        self
    }

    /// Fill the caller's persisted metadata (`agent.session.idle/failed`).
    #[must_use]
    pub fn with_metadata(mut self, metadata: BTreeMap<String, String>) -> Self {
        self.metadata = metadata;
        self
    }

    /// Fill the persisted session-default prompt cache lifetime.
    #[must_use]
    pub fn with_prompt_cache_options(
        mut self,
        prompt_cache_options: Option<PromptCacheOptions>,
    ) -> Self {
        self.prompt_cache_options = prompt_cache_options;
        self
    }

    /// Project the public `SessionView` into the official session snapshot.
    ///
    /// `POST /sessions?stream=true` has no store read at this layer; the create
    /// result already carries the persisted projection. Only fields the gateway
    /// actually recorded are copied; an unknown environment stays `none`.
    #[must_use]
    pub fn from_session_view(view: &SessionView) -> Self {
        Self {
            id: view.id.clone(),
            object: "agent.session".to_owned(),
            status: view.status.as_str().to_owned(),
            created_at: view.created_at,
            last_active_at: view.last_active_at,
            error: view.error.clone(),
            agent: OfficialSessionAgent {
                id: "root".to_owned(),
                model: view.agent.model.clone(),
                instructions: view.agent.instructions.clone(),
            },
            environment: view.environment.clone(),
            metadata: view.metadata.clone(),
            prompt_cache_options: view.prompt_cache_options,
            required_actions: view.required_actions.clone(),
            usage: view.usage.clone(),
            vault_ids: view.vault_ids.clone(),
        }
    }
}

/// Customer-safe failure carried by the official `error` event (`SessionError`).
///
/// The gateway has one failure category: the persisted turn row records
/// `{"code":"internal_error"}`. No finer classification is derived here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OfficialSessionError {
    pub code: String,
    pub message: String,
    pub param: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
}

impl OfficialSessionError {
    #[must_use]
    pub fn internal(message: &str) -> Self {
        Self {
            code: "internal_error".to_owned(),
            message: message.to_owned(),
            param: None,
            kind: "internal_error".to_owned(),
        }
    }
}

/// The subset of `AgentSessionEvent` the gateway can honor.
///
/// `requires_action` is emitted only for a declared MCP approval that is
/// currently pending. `turn.cancelled`, budget and subagent frames are not
/// emitted: the local runtime has no corresponding fact. Assistant
/// `output_text.delta` is derived from live chunks and from completion rows on
/// replay; reasoning and environment frames are absent (`design §4.2.1`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum OfficialEvent {
    #[serde(rename = "agent.session.created")]
    SessionCreated {
        event_id: String,
        session: OfficialSession,
    },
    #[serde(rename = "agent.session.in_progress")]
    SessionInProgress {
        event_id: String,
        session: OfficialSession,
    },
    #[serde(rename = "agent.session.turn.created")]
    TurnCreated {
        event_id: String,
        session_id: String,
        turn_id: String,
        turn: OfficialTurn,
    },
    #[serde(rename = "agent.session.turn.in_progress")]
    TurnInProgress {
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
    #[serde(rename = "agent.session.turn.item.done")]
    TurnItemDone {
        event_id: String,
        session_id: String,
        turn_id: Option<String>,
        output_index: u32,
        item: OfficialMessageItem,
    },
    #[serde(rename = "agent.session.turn.content_part.added")]
    TurnContentPartAdded {
        event_id: String,
        session_id: String,
        turn_id: Option<String>,
        item_id: String,
        content_index: u32,
        output_index: u32,
        part: OfficialContent,
    },
    #[serde(rename = "agent.session.turn.content_part.done")]
    TurnContentPartDone {
        event_id: String,
        session_id: String,
        turn_id: Option<String>,
        item_id: String,
        content_index: u32,
        output_index: u32,
        part: OfficialContent,
    },
    #[serde(rename = "agent.session.turn.output_text.delta")]
    TurnOutputTextDelta {
        event_id: String,
        session_id: String,
        turn_id: Option<String>,
        item_id: String,
        content_index: u32,
        output_index: u32,
        delta: String,
    },
    #[serde(rename = "agent.session.turn.output_text.done")]
    TurnOutputTextDone {
        event_id: String,
        session_id: String,
        turn_id: Option<String>,
        item_id: String,
        content_index: u32,
        output_index: u32,
        text: String,
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
    /// A current `required_actions` snapshot.
    ///
    /// Reused for settlement: after an answer, the same event carries the
    /// remaining set (possibly empty), so the official collector stops treating
    /// a resolved action as pending without inventing a terminal.
    #[serde(rename = "agent.session.requires_action")]
    RequiresAction {
        event_id: String,
        session: OfficialSession,
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
    #[serde(rename = "error")]
    Error {
        event_id: String,
        session_id: String,
        error: OfficialSessionError,
    },
}

impl OfficialEvent {
    #[must_use]
    pub fn session_created(session: OfficialSession) -> Self {
        Self::SessionCreated {
            event_id: session_created_event_id(&session.id),
            session,
        }
    }

    #[must_use]
    pub fn session_in_progress(turn_id: &str, session: OfficialSession) -> Self {
        Self::SessionInProgress {
            event_id: session_in_progress_event_id(turn_id),
            session,
        }
    }

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
    pub fn turn_in_progress(session_id: &str, turn_id: &str, turn: OfficialTurn) -> Self {
        Self::TurnInProgress {
            event_id: turn_in_progress_event_id(turn_id),
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

    /// Assistant item introduction keyed by the stable item id.
    ///
    /// The first live chunk sends this before the first delta; replay sends it
    /// from the completion row. Both deliveries therefore use the same id.
    #[must_use]
    pub fn turn_assistant_item_added(
        item_id: &str,
        session_id: &str,
        turn_id: &str,
        item: OfficialMessageItem,
    ) -> Self {
        Self::TurnItemAdded {
            event_id: turn_assistant_item_added_event_id(item_id),
            session_id: session_id.to_owned(),
            turn_id: Some(turn_id.to_owned()),
            output_index: None,
            item,
        }
    }

    #[must_use]
    pub fn turn_item_done(
        seq: u64,
        session_id: &str,
        turn_id: &str,
        output_index: u32,
        item: OfficialMessageItem,
    ) -> Self {
        Self::TurnItemDone {
            event_id: turn_item_done_event_id(seq),
            session_id: session_id.to_owned(),
            turn_id: Some(turn_id.to_owned()),
            output_index,
            item,
        }
    }

    #[must_use]
    pub fn turn_content_part_added(
        item_id: &str,
        session_id: &str,
        turn_id: &str,
        output_index: u32,
        text: &str,
    ) -> Self {
        Self::TurnContentPartAdded {
            event_id: turn_item_part_added_event_id(item_id),
            session_id: session_id.to_owned(),
            turn_id: Some(turn_id.to_owned()),
            item_id: item_id.to_owned(),
            content_index: 0,
            output_index,
            part: OfficialContent::OutputText {
                text: text.to_owned(),
            },
        }
    }

    #[must_use]
    pub fn turn_content_part_done(
        seq: u64,
        session_id: &str,
        turn_id: &str,
        item_id: &str,
        output_index: u32,
        text: &str,
    ) -> Self {
        Self::TurnContentPartDone {
            event_id: turn_item_part_done_event_id(seq),
            session_id: session_id.to_owned(),
            turn_id: Some(turn_id.to_owned()),
            item_id: item_id.to_owned(),
            content_index: 0,
            output_index,
            part: OfficialContent::OutputText {
                text: text.to_owned(),
            },
        }
    }

    #[must_use]
    pub fn turn_output_text_done(
        seq: u64,
        session_id: &str,
        turn_id: &str,
        item_id: &str,
        output_index: u32,
        text: &str,
    ) -> Self {
        Self::TurnOutputTextDone {
            event_id: turn_item_text_done_event_id(seq),
            session_id: session_id.to_owned(),
            turn_id: Some(turn_id.to_owned()),
            item_id: item_id.to_owned(),
            content_index: 0,
            output_index,
            text: text.to_owned(),
        }
    }

    /// One live chunk's text delta.
    #[must_use]
    pub fn turn_output_text_delta_live(
        item_id: &str,
        chunk: u64,
        session_id: &str,
        turn_id: &str,
        output_index: u32,
        delta: &str,
    ) -> Self {
        Self::TurnOutputTextDelta {
            event_id: turn_output_text_delta_event_id(item_id, chunk),
            session_id: session_id.to_owned(),
            turn_id: Some(turn_id.to_owned()),
            item_id: item_id.to_owned(),
            content_index: 0,
            output_index,
            delta: delta.to_owned(),
        }
    }

    /// One full-text replay delta for a completion row.
    #[must_use]
    pub fn turn_output_text_delta_replay(
        seq: u64,
        session_id: &str,
        turn_id: &str,
        item_id: &str,
        output_index: u32,
        text: &str,
    ) -> Self {
        Self::TurnOutputTextDelta {
            event_id: turn_output_text_delta_replay_event_id(seq),
            session_id: session_id.to_owned(),
            turn_id: Some(turn_id.to_owned()),
            item_id: item_id.to_owned(),
            content_index: 0,
            output_index,
            delta: text.to_owned(),
        }
    }

    #[must_use]
    pub fn turn_completed(session_id: &str, turn_id: &str, turn: OfficialTurn) -> Self {
        Self::TurnCompleted {
            event_id: turn_completed_event_id(turn_id),
            session_id: session_id.to_owned(),
            turn_id: turn_id.to_owned(),
            // One source of truth: the event-level usage is the turn's own
            // value, so the live frame and `turns.retrieve` cannot disagree.
            usage: turn.usage.clone(),
            turn,
        }
    }

    #[must_use]
    pub fn turn_failed(session_id: &str, turn_id: &str, turn: OfficialTurn) -> Self {
        Self::TurnFailed {
            event_id: turn_failed_event_id(turn_id),
            session_id: session_id.to_owned(),
            turn_id: turn_id.to_owned(),
            usage: turn.usage.clone(),
            turn,
        }
    }

    #[must_use]
    pub fn requires_action(seq: u64, session: OfficialSession) -> Self {
        Self::RequiresAction {
            event_id: requires_action_event_id(seq),
            session,
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

    /// Failed turn's `error` frame; the code mirrors the persisted turn row.
    #[must_use]
    pub fn error(turn_id: &str, session_id: &str, message: &str) -> Self {
        Self::Error {
            event_id: turn_error_event_id(turn_id),
            session_id: session_id.to_owned(),
            error: OfficialSessionError::internal(message),
        }
    }

    /// SSE `event:` name and JSON `type` value.
    #[must_use]
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::SessionCreated { .. } => "agent.session.created",
            Self::SessionInProgress { .. } => "agent.session.in_progress",
            Self::TurnCreated { .. } => "agent.session.turn.created",
            Self::TurnInProgress { .. } => "agent.session.turn.in_progress",
            Self::TurnItemAdded { .. } => "agent.session.turn.item.added",
            Self::TurnItemDone { .. } => "agent.session.turn.item.done",
            Self::TurnContentPartAdded { .. } => "agent.session.turn.content_part.added",
            Self::TurnContentPartDone { .. } => "agent.session.turn.content_part.done",
            Self::TurnOutputTextDelta { .. } => "agent.session.turn.output_text.delta",
            Self::TurnOutputTextDone { .. } => "agent.session.turn.output_text.done",
            Self::TurnCompleted { .. } => "agent.session.turn.completed",
            Self::TurnFailed { .. } => "agent.session.turn.failed",
            Self::RequiresAction { .. } => "agent.session.requires_action",
            Self::SessionIdle { .. } => "agent.session.idle",
            Self::SessionFailed { .. } => "agent.session.failed",
            Self::Error { .. } => "error",
        }
    }

    /// Stable id used by `TurnState` to dedupe live and replayed deliveries.
    #[must_use]
    pub fn event_id(&self) -> &str {
        match self {
            Self::SessionCreated { event_id, .. }
            | Self::SessionInProgress { event_id, .. }
            | Self::TurnCreated { event_id, .. }
            | Self::TurnInProgress { event_id, .. }
            | Self::TurnItemAdded { event_id, .. }
            | Self::TurnItemDone { event_id, .. }
            | Self::TurnContentPartAdded { event_id, .. }
            | Self::TurnContentPartDone { event_id, .. }
            | Self::TurnOutputTextDelta { event_id, .. }
            | Self::TurnOutputTextDone { event_id, .. }
            | Self::TurnCompleted { event_id, .. }
            | Self::TurnFailed { event_id, .. }
            | Self::RequiresAction { event_id, .. }
            | Self::SessionIdle { event_id, .. }
            | Self::SessionFailed { event_id, .. }
            | Self::Error { event_id, .. } => event_id,
        }
    }
}

#[cfg(test)]
#[path = "official_test.rs"]
mod tests;
