use serde::{Deserialize, Serialize};

use crate::domain::identity::LogicalId;

/// Durable session state exposed by `GET /v1/agents/sessions/{logicalId}`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
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
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Recovering => "recovering",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
        }
    }
}

/// Environment requested/allotted for a session.
///
/// `none` is the only kind implemented in S3. `openai_hosted` exists so request
/// parsing can return a diagnosable `mcp_mode_unsupported` instead of silently
/// degrading to a local workspace (`design §5.5`).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentKind {
    None,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxView {
    pub logical_env_id: String,
    pub sandbox_id: Option<String>,
    pub run_id: Option<String>,
    pub state: SandboxState,
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
