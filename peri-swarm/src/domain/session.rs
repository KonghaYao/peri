use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::item::ItemOrder;

/// Official `metadata` limits (`SessionCreateParams.metadata`).
pub const MAX_METADATA_PAIRS: usize = 16;
pub const MAX_METADATA_KEY_CHARS: usize = 64;
pub const MAX_METADATA_VALUE_CHARS: usize = 512;

/// Default prompt cache lifetime (`SessionCreateParams.PromptCacheOptions.ttl`).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PromptCacheTtl {
    #[serde(rename = "30m")]
    ThirtyMinutes,
    #[serde(rename = "12h")]
    TwelveHours,
}

impl PromptCacheTtl {
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "30m" => Some(Self::ThirtyMinutes),
            "12h" => Some(Self::TwelveHours),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ThirtyMinutes => "30m",
            Self::TwelveHours => "12h",
        }
    }
}

/// Persisted `prompt_cache_options`: `{"ttl": "30m" | "12h"}`.
///
/// A session with no configured value stores `NULL`; on the wire `null` clears
/// it. An input object without `ttl` is the official default `30m`, not
/// "unset".
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PromptCacheOptions {
    pub ttl: PromptCacheTtl,
}

impl PromptCacheOptions {
    #[must_use]
    pub fn new(ttl: PromptCacheTtl) -> Self {
        Self { ttl }
    }
}

/// Query window for `GET /v1/agents/sessions` (`SessionListParams`).
///
/// `after` is the official id cursor and is exclusive in the selected order.
/// The order is over `logical_id` (the public session id), which is unique and
/// therefore a stable total order; `created_at` is not used because two
/// sessions can share a timestamp.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionsWindow {
    pub order: ItemOrder,
    pub after: Option<String>,
    pub limit: u32,
    pub agent_id: Option<String>,
    pub metadata: BTreeMap<String, String>,
}

impl SessionsWindow {
    /// Gateway default for a session page. The official SDK always sends an
    /// explicit `limit` when it auto-paginates, so this only affects direct
    /// callers.
    pub const DEFAULT_LIMIT: u32 = 20;
    pub const MAX_LIMIT: u32 = 500;
}

/// One `CursorPage<AgentSession>` page.
///
/// Serialized by hand so one `has_more` value is emitted under both the
/// official snake_case name and the camelCase name the rest of the gateway
/// uses, matching [`crate::domain::ItemPage`].
#[derive(Clone, Debug)]
pub struct SessionPage {
    pub object: String,
    pub data: Vec<SessionView>,
    pub has_more: bool,
    /// Id of the last session in page order; `None` on an empty page.
    pub next_cursor: Option<String>,
}

impl SessionPage {
    #[must_use]
    pub fn new(data: Vec<SessionView>, has_more: bool, next_cursor: Option<String>) -> Self {
        Self {
            object: "list".to_owned(),
            data,
            has_more,
            next_cursor,
        }
    }
}

impl Serialize for SessionPage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut page = serializer.serialize_struct("SessionPage", 5)?;
        page.serialize_field("object", &self.object)?;
        page.serialize_field("data", &self.data)?;
        page.serialize_field("has_more", &self.has_more)?;
        page.serialize_field("hasMore", &self.has_more)?;
        page.serialize_field("nextCursor", &self.next_cursor)?;
        page.end()
    }
}

/// `sessions.delete` response (`AgentSessionDeleted`).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionDeleted {
    pub id: String,
    pub object: String,
    pub deleted: bool,
}

impl SessionDeleted {
    #[must_use]
    pub fn new(id: &str) -> Self {
        Self {
            id: id.to_owned(),
            object: "agent.session.deleted".to_owned(),
            deleted: true,
        }
    }
}

/// Durable session state exposed by `GET /v1/agents/sessions/{logicalId}`.
///
/// `Creating` is the durable "identity reserved, host not live yet" state: the
/// row is written before the Peri session exists, so an interrupted create can
/// reconcile against the persisted `root_session_id` instead of minting a new
/// one (`design §3`).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Creating,
    Idle,
    Running,
    Recovering,
    Stopped,
    Failed,
}

impl SessionState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Creating => "creating",
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Recovering => "recovering",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
        }
    }
}

/// Official `sessions.retrieve` status (`openai@7.60.0` `agents.ts`).
///
/// `requires_action` is deliberately absent: the local runtime never waits for
/// an approval round-trip, so the gateway would have nothing honest to report.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Idle,
    InProgress,
    Failed,
}

impl SessionStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::InProgress => "in_progress",
            Self::Failed => "failed",
        }
    }

    /// Project the durable session state plus the in-process turn slot.
    ///
    /// An open turn is `in_progress` even while the row still says `running`;
    /// a failed row without an open turn is `failed` until a later turn
    /// recovers it. `creating`/`recovering`/`stopped` are not official states
    /// and project as `idle`.
    #[must_use]
    pub fn project(state: SessionState, turn_active: bool) -> Self {
        if turn_active || state == SessionState::Running {
            Self::InProgress
        } else if state == SessionState::Failed {
            Self::Failed
        } else {
            Self::Idle
        }
    }
}

/// Environment requested/allotted for a session.
///
/// `none` runs without a session workspace. `openai_hosted` reserves a sandbox
/// in the local Bun bridge and injects its `workspace` MCP entry (S8a3a); the
/// sandbox is started lazily by the first real MCP request, not by the gateway.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentKind {
    None,
    #[serde(rename = "openai_hosted")]
    OpenAiHosted,
}

impl EnvironmentKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::OpenAiHosted => "openai_hosted",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentRef {
    #[serde(rename = "type")]
    pub kind: EnvironmentKind,
    /// Official `EnvironmentResourceNone` has only `type`, so `none` omits the
    /// id. Hosted sessions still carry their logical environment id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

impl EnvironmentRef {
    #[must_use]
    pub fn none() -> Self {
        Self {
            kind: EnvironmentKind::None,
            id: None,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnView {
    pub request_id: Option<String>,
    pub started_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentView {
    pub model: Option<String>,
    pub instructions: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxState {
    Cold,
    Starting,
    Ready,
    Stopped,
    Failed,
}

impl SandboxState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cold => "cold",
            Self::Starting => "starting",
            Self::Ready => "ready",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
        }
    }
}

/// Bound Bun environment projected from `swarm_bindings`.
///
/// The gateway does not observe Bun's live SRT lifecycle, so `state` stays
/// `null`; the binding identity and `workspacePath` are what the session can
/// report. Use `GET <bun>/v1/sandboxes/{logicalEnvId}` for the live state.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxView {
    pub logical_env_id: String,
    pub sandbox_id: Option<String>,
    pub run_id: Option<String>,
    pub state: Option<SandboxState>,
    pub workspace_path: Option<String>,
}

/// Response body for `GET /sessions/{logicalId}` and the `session` SSE event.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    pub id: String,
    pub object: String,
    pub logical_id: String,
    pub root_session_id: String,
    pub cwd: String,
    pub state: SessionState,
    /// Official SDK status; derived from `state` and the in-process turn slot.
    pub status: SessionStatus,
    pub turn: TurnView,
    pub last_item_seq: u64,
    /// Official `AgentSession.created_at`: Unix seconds, not the persisted
    /// RFC3339 column. The explicit name overrides `rename_all = "camelCase"`;
    /// the official SDK reads only the snake_case field.
    #[serde(rename = "created_at")]
    pub created_at: i64,
    /// Official `AgentSession.last_active_at`: Unix seconds, projected from the
    /// persisted RFC3339 `last_activity_at` column.
    #[serde(rename = "last_active_at")]
    pub last_active_at: i64,
    pub agent: AgentView,
    pub environment: EnvironmentRef,
    pub sandbox: Option<SandboxView>,
    /// Official `AgentSession.required_actions`; always empty because the local
    /// runtime never waits for an approval round-trip.
    #[serde(rename = "required_actions", default)]
    pub required_actions: Vec<Value>,
    /// Official `AgentSession.vault_ids`; the gateway exposes no vaults.
    #[serde(rename = "vault_ids", default)]
    pub vault_ids: Vec<String>,
    /// Official `AgentSession.usage`; `null` when unknown, which is always.
    #[serde(default)]
    pub usage: Option<Value>,
    /// Official `AgentSession.error`; `null` while the session is not failed.
    #[serde(default)]
    pub error: Option<String>,
    /// Caller metadata, exactly as persisted. Never synthesized.
    pub metadata: BTreeMap<String, String>,
    /// Session-default prompt cache lifetime; `null` when the session has no
    /// configured value.
    ///
    /// The official `AgentSession.prompt_cache_options` spelling is snake_case,
    /// unlike the rest of this Swarm-defined projection; the official SDK reads
    /// only that name, so it overrides `rename_all = "camelCase"`.
    #[serde(rename = "prompt_cache_options")]
    pub prompt_cache_options: Option<PromptCacheOptions>,
}

/// Response body for `POST /sessions`: [`SessionView`] plus the `created` flag.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSessionResponse {
    #[serde(flatten)]
    pub session: SessionView,
    pub created: bool,
}

impl CreateSessionResponse {
    #[must_use]
    pub fn new(session: SessionView, created: bool) -> Self {
        Self { session, created }
    }
}
