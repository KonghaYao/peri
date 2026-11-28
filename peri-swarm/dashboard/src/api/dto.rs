//! Wire DTOs for the gateway responses.
//!
//! Field names follow `peri-swarm/src/domain/*.rs`; this crate never imports
//! the gateway. Unknown gateway fields are ignored.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

/// `AgentSession` projection returned by list/retrieve/create/update.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    /// Public logical id.
    pub id: String,
    /// Caller-facing session id.
    pub logical_id: String,
    /// Peri root session id.
    pub root_session_id: String,
    /// Working directory.
    pub cwd: String,
    /// Durable state (`creating`, `idle`, `running`, ...).
    pub state: String,
    /// Official status (`idle`, `in_progress`, `requires_action`, `failed`).
    pub status: String,
    /// Current turn facts.
    pub turn: SessionTurn,
    /// Highest persisted item sequence.
    pub last_item_seq: u64,
    /// Creation time in Unix seconds.
    #[serde(rename = "created_at")]
    pub created_at: i64,
    /// Last activity in Unix seconds.
    #[serde(rename = "last_active_at")]
    pub last_active_at: i64,
    /// Agent configuration.
    pub agent: SessionAgent,
    /// Requested/allotted environment.
    pub environment: SessionEnvironment,
    /// Bound sandbox, when the gateway reports one.
    #[serde(default)]
    pub sandbox: Option<Value>,
    /// Pending approval requests, in the official `required_actions` shape.
    #[serde(rename = "required_actions", default)]
    pub required_actions: Vec<Value>,
    /// Vault ids. The gateway exposes no vaults, so this is normally empty.
    #[serde(rename = "vault_ids", default)]
    pub vault_ids: Vec<String>,
    /// Usage totals, when known.
    #[serde(default)]
    pub usage: Option<Value>,
    /// Customer-safe failure description while failed.
    #[serde(default)]
    pub error: Option<String>,
    /// Caller metadata exactly as persisted.
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
    /// Session-default prompt cache lifetime.
    #[serde(rename = "prompt_cache_options", default)]
    pub prompt_cache_options: Option<PromptCacheOptions>,
}

/// Current turn information embedded in [`Session`].
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTurn {
    /// Request id of the active turn.
    #[serde(default)]
    pub request_id: Option<String>,
    /// Turn start timestamp.
    #[serde(default)]
    pub started_at: Option<String>,
}

/// Agent section of [`Session`].
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionAgent {
    /// Model name.
    #[serde(default)]
    pub model: Option<String>,
    /// Instructions text.
    #[serde(default)]
    pub instructions: Option<String>,
}

/// Environment section of [`Session`].
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironment {
    /// `none` or `openai_hosted`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Logical environment id (`none` omits it).
    #[serde(default)]
    pub id: Option<String>,
}

/// Session-default prompt cache lifetime.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct PromptCacheOptions {
    /// `30m` or `12h`.
    pub ttl: String,
}

/// One `CursorPage<AgentSession>` page.
///
/// The gateway serializes both `has_more` and `hasMore` on the same page, so a
/// plain serde alias would reject the second spelling as a duplicate field.
/// The raw shape accepts both and prefers the camelCase value.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(from = "SessionPageFields")]
pub struct SessionPage {
    /// Object discriminator.
    pub object: Option<String>,
    /// Sessions in gateway order (`logical_id` desc).
    pub data: Vec<Session>,
    /// Whether more sessions exist.
    pub has_more: bool,
    /// Cursor for the next page.
    pub next_cursor: Option<String>,
}

#[derive(Deserialize)]
struct SessionPageFields {
    #[serde(default)]
    object: Option<String>,
    #[serde(default)]
    data: Vec<Session>,
    #[serde(default, rename = "has_more")]
    has_more_snake: Option<bool>,
    #[serde(default, rename = "hasMore")]
    has_more_camel: Option<bool>,
    #[serde(default, rename = "next_cursor")]
    next_cursor_snake: Option<String>,
    #[serde(default, rename = "nextCursor")]
    next_cursor_camel: Option<String>,
}

impl From<SessionPageFields> for SessionPage {
    fn from(fields: SessionPageFields) -> Self {
        Self {
            object: fields.object,
            data: fields.data,
            has_more: fields
                .has_more_camel
                .or(fields.has_more_snake)
                .unwrap_or(false),
            next_cursor: fields.next_cursor_camel.or(fields.next_cursor_snake),
        }
    }
}

/// One canonical turn (`TurnsAPI.Turn`).
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Turn {
    /// Turn id.
    pub id: String,
    /// `queued`, `in_progress`, `waiting`, `completed`, `failed`, `cancelled`.
    pub status: String,
    /// Creation time in Unix seconds.
    #[serde(rename = "created_at")]
    pub created_at: i64,
    /// Start time in Unix seconds.
    #[serde(default, rename = "started_at")]
    pub started_at: Option<i64>,
    /// Completion time in Unix seconds.
    #[serde(default, rename = "completed_at")]
    pub completed_at: Option<i64>,
    /// Customer-safe failure description.
    #[serde(default)]
    pub error: Option<Value>,
    /// Usage totals, when known.
    #[serde(default)]
    pub usage: Option<Value>,
}

/// One `CursorPage<Turn>` page.
///
/// Like [`SessionPage`], the gateway emits both `has_more` and `hasMore`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(from = "TurnsFields")]
pub struct Turns {
    /// Object discriminator.
    pub object: Option<String>,
    /// Turns in gateway order.
    pub data: Vec<Turn>,
    /// Whether more turns exist.
    pub has_more: bool,
    /// Cursor for the next page.
    pub next_cursor: Option<String>,
}

#[derive(Deserialize)]
struct TurnsFields {
    #[serde(default)]
    object: Option<String>,
    #[serde(default)]
    data: Vec<Turn>,
    #[serde(default, rename = "has_more")]
    has_more_snake: Option<bool>,
    #[serde(default, rename = "hasMore")]
    has_more_camel: Option<bool>,
    #[serde(default, rename = "next_cursor")]
    next_cursor_snake: Option<String>,
    #[serde(default, rename = "nextCursor")]
    next_cursor_camel: Option<String>,
}

impl From<TurnsFields> for Turns {
    fn from(fields: TurnsFields) -> Self {
        Self {
            object: fields.object,
            data: fields.data,
            has_more: fields
                .has_more_camel
                .or(fields.has_more_snake)
                .unwrap_or(false),
            next_cursor: fields.next_cursor_camel.or(fields.next_cursor_snake),
        }
    }
}

/// One item page.
///
/// `items` keeps the raw persisted rows and `data` the official projection.
/// Both stay raw values: the dashboard renders the official item shapes
/// without redefining them from a partial guess. Both `has_more` spellings are
/// accepted because the gateway serializes both.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(from = "ItemsPageFields")]
pub struct ItemsPage {
    /// Object discriminator.
    pub object: Option<String>,
    /// Raw persisted item rows.
    pub items: Vec<Value>,
    /// Official item projection.
    pub data: Vec<Value>,
    /// Cursor for the next page. The gateway emits the maximum `seq` as a
    /// number; strings are accepted for the official item-id spelling.
    pub next_cursor: Option<Value>,
    /// Whether more items exist.
    pub has_more: bool,
    /// Page order (`asc` or `desc`).
    pub order: Option<String>,
    /// Highest item sequence in the page.
    pub last_item_seq: Option<u64>,
}

#[derive(Deserialize)]
struct ItemsPageFields {
    #[serde(default)]
    object: Option<String>,
    #[serde(default)]
    items: Vec<Value>,
    #[serde(default)]
    data: Vec<Value>,
    #[serde(default, rename = "next_cursor")]
    next_cursor_snake: Option<Value>,
    #[serde(default, rename = "nextCursor")]
    next_cursor_camel: Option<Value>,
    #[serde(default, rename = "has_more")]
    has_more_snake: Option<bool>,
    #[serde(default, rename = "hasMore")]
    has_more_camel: Option<bool>,
    #[serde(default)]
    order: Option<String>,
    #[serde(default, rename = "last_item_seq")]
    last_item_seq_snake: Option<u64>,
    #[serde(default, rename = "lastItemSeq")]
    last_item_seq_camel: Option<u64>,
}

impl From<ItemsPageFields> for ItemsPage {
    fn from(fields: ItemsPageFields) -> Self {
        Self {
            object: fields.object,
            items: fields.items,
            data: fields.data,
            next_cursor: fields.next_cursor_camel.or(fields.next_cursor_snake),
            has_more: fields
                .has_more_camel
                .or(fields.has_more_snake)
                .unwrap_or(false),
            order: fields.order,
            last_item_seq: fields.last_item_seq_camel.or(fields.last_item_seq_snake),
        }
    }
}

impl ItemsPage {
    /// Render [`Self::next_cursor`] as the query value accepted by `after`.
    #[must_use]
    pub fn next_cursor_text(&self) -> Option<String> {
        match self.next_cursor.as_ref()? {
            Value::String(text) => Some(text.clone()),
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        }
    }
}

/// `POST /v1/agents/sessions` response.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct CreatedSession {
    /// The created or attached session.
    #[serde(flatten)]
    pub session: Session,
    /// Whether this call created a new session (`false` on attach).
    #[serde(default)]
    pub created: bool,
}

/// `GET /healthz` response.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Health {
    /// Gateway liveness marker.
    #[serde(default)]
    pub status: String,
}

/// `POST /events` acceptance receipt.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputReceipt {
    /// Idempotency/input id used for this submission.
    pub input_id: String,
    /// Always `true` for `202 Accepted`.
    pub accepted: bool,
    /// `inserted`, `existing` or `conflict`.
    pub disposition: String,
    /// `turn` or `steer`.
    pub mode: String,
    /// Highest item sequence at acceptance.
    pub last_item_seq: u64,
}

/// `POST /events` approval answer receipt.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalReceipt {
    /// Pending approval that was settled.
    pub request_id: String,
    /// `approve`, `deny` or `cancel`; `null` when nothing was decided.
    #[serde(default)]
    pub decision: Option<String>,
    /// Settlement reason.
    pub reason: String,
    /// Idempotency/input id of the answer, when supplied.
    #[serde(default)]
    pub input_id: Option<String>,
    /// `applied` or `duplicate`.
    pub disposition: String,
}

/// Gateway error body: `{ "error": { "code", "message", "details" } }`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ErrorEnvelope {
    /// The nested error object.
    pub error: ErrorBody,
}

/// Nested gateway error object.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ErrorBody {
    /// Stable machine-readable code.
    pub code: String,
    /// Repository-controlled message.
    pub message: String,
    /// Structured field details.
    #[serde(default)]
    pub details: Value,
}
