use serde::{Deserialize, Serialize};

use crate::domain::identity::LogicalId;

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
    pub id: Option<String>,
}

impl EnvironmentRef {
    #[must_use]
    pub fn none(logical_id: &LogicalId) -> Self {
        Self {
            kind: EnvironmentKind::None,
            id: Some(logical_id.as_str().to_owned()),
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
    pub created_at: String,
    pub last_activity_at: String,
    pub agent: AgentView,
    pub environment: EnvironmentRef,
    pub sandbox: Option<SandboxView>,
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
