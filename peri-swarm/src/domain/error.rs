use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Stable machine-readable error codes exposed by the Swarm HTTP contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Unauthorized,
    UnsupportedBeta,
    UnsupportedContract,
    UnknownField,
    InvalidRequest,
    InvalidQuery,
    InvalidContentType,
    RequestTooLarge,
    InvalidLogicalId,
    CwdNotFound,
    CwdNotNormalizable,
    ModelNotSupported,
    IdentityConflict,
    InvalidInput,
    EmptyInput,
    UnknownAgent,
    TurnInProgress,
    InputConflict,
    QueueUnavailable,
    SessionCreationPending,
    UpstreamUnavailable,
    SessionConflict,
    CapabilityUnavailable,
    AgentError,
    CursorTooOld,
    ConflictingCursor,
    McpModeUnsupported,
    UnsupportedEnvironment,
    UnsupportedTool,
    SeqConflict,
}

impl ErrorCode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::UnsupportedBeta => "unsupported_beta",
            Self::UnsupportedContract => "unsupported_contract",
            Self::UnknownField => "unknown_field",
            Self::InvalidRequest => "invalid_request",
            Self::InvalidQuery => "invalid_query",
            Self::InvalidContentType => "invalid_content_type",
            Self::RequestTooLarge => "request_too_large",
            Self::InvalidLogicalId => "invalid_logical_id",
            Self::CwdNotFound => "cwd_not_found",
            Self::CwdNotNormalizable => "cwd_not_normalizable",
            Self::ModelNotSupported => "model_not_supported",
            Self::IdentityConflict => "identity_conflict",
            Self::InvalidInput => "invalid_input",
            Self::EmptyInput => "empty_input",
            Self::UnknownAgent => "unknown_agent",
            Self::TurnInProgress => "turn_in_progress",
            Self::InputConflict => "input_conflict",
            Self::QueueUnavailable => "queue_unavailable",
            Self::SessionCreationPending => "session_creation_pending",
            Self::UpstreamUnavailable => "upstream_unavailable",
            Self::SessionConflict => "session_conflict",
            Self::CapabilityUnavailable => "capability_unavailable",
            Self::AgentError => "agent_error",
            Self::CursorTooOld => "cursor_too_old",
            Self::ConflictingCursor => "conflicting_cursor",
            Self::McpModeUnsupported => "mcp_mode_unsupported",
            Self::UnsupportedEnvironment => "unsupported_environment",
            Self::UnsupportedTool => "unsupported_tool",
            Self::SeqConflict => "seq_conflict",
        }
    }
}

/// Transport-neutral status class; [`crate::http`] maps it to an HTTP status.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    BadRequest,
    Unauthorized,
    NotFound,
    Conflict,
    PayloadTooLarge,
    UnsupportedMediaType,
    Unprocessable,
    Unavailable,
    BadGateway,
}

/// One Swarm failure. Messages are repository-controlled and never echo credentials.
#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub struct SwarmError {
    kind: ErrorKind,
    code: ErrorCode,
    message: String,
    details: Value,
}

impl SwarmError {
    #[must_use]
    pub fn new(kind: ErrorKind, code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            kind,
            code,
            message: message.into(),
            details: Value::Null,
        }
    }

    #[must_use]
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }

    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    #[must_use]
    pub fn code(&self) -> ErrorCode {
        self.code
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub fn details(&self) -> &Value {
        &self.details
    }

    #[must_use]
    pub fn unauthorized() -> Self {
        Self::new(
            ErrorKind::Unauthorized,
            ErrorCode::Unauthorized,
            "missing or invalid deployment API key",
        )
    }

    #[must_use]
    pub fn unsupported_beta(value: &str) -> Self {
        Self::new(
            ErrorKind::BadRequest,
            ErrorCode::UnsupportedBeta,
            "OpenAI-Beta must be agents=v1",
        )
        .with_details(serde_json::json!({ "received": value }))
    }

    #[must_use]
    pub fn unsupported_contract(value: &str) -> Self {
        Self::new(
            ErrorKind::Conflict,
            ErrorCode::UnsupportedContract,
            "x-peri-swarm-expect must be `1`",
        )
        .with_details(serde_json::json!({ "received": value, "supported": "1" }))
    }

    #[must_use]
    pub fn invalid_logical_id(reason: &str) -> Self {
        Self::new(
            ErrorKind::BadRequest,
            ErrorCode::InvalidLogicalId,
            "logicalId must match ^[A-Za-z0-9_-]{1,128}$",
        )
        .with_details(serde_json::json!({ "field": "logicalId", "reason": reason }))
    }

    #[must_use]
    pub fn cwd_not_found() -> Self {
        Self::new(
            ErrorKind::BadRequest,
            ErrorCode::CwdNotFound,
            "cwd does not exist",
        )
        .with_details(serde_json::json!({ "field": "cwd" }))
    }

    #[must_use]
    pub fn cwd_not_normalizable(reason: &str) -> Self {
        Self::new(
            ErrorKind::BadRequest,
            ErrorCode::CwdNotNormalizable,
            "cwd cannot be normalized to an absolute directory",
        )
        .with_details(serde_json::json!({ "field": "cwd", "reason": reason }))
    }

    #[must_use]
    pub fn invalid_query(reason: &str) -> Self {
        Self::new(ErrorKind::BadRequest, ErrorCode::InvalidQuery, reason)
    }

    #[must_use]
    pub fn invalid_input(message: &str) -> Self {
        Self::new(ErrorKind::BadRequest, ErrorCode::InvalidInput, message)
    }

    #[must_use]
    pub fn empty_input() -> Self {
        Self::new(
            ErrorKind::BadRequest,
            ErrorCode::EmptyInput,
            "text and attachments must not both be empty",
        )
    }

    #[must_use]
    pub fn unknown_agent(logical_id: &str) -> Self {
        Self::new(
            ErrorKind::NotFound,
            ErrorCode::UnknownAgent,
            "unknown agent session",
        )
        .with_details(serde_json::json!({ "logicalId": logical_id }))
    }

    #[must_use]
    pub fn turn_in_progress() -> Self {
        Self::new(
            ErrorKind::Conflict,
            ErrorCode::TurnInProgress,
            "a turn is already running; retry with mode=steer",
        )
        .with_details(serde_json::json!({ "suggest": "steer" }))
    }

    #[must_use]
    pub fn input_conflict(input_id: &str) -> Self {
        Self::new(
            ErrorKind::Conflict,
            ErrorCode::InputConflict,
            "inputId was already accepted with a different payload",
        )
        .with_details(serde_json::json!({ "inputId": input_id }))
    }

    #[must_use]
    pub fn conflicting_cursor() -> Self {
        Self::new(
            ErrorKind::BadRequest,
            ErrorCode::ConflictingCursor,
            "afterSeq, after and Last-Event-ID disagree",
        )
    }

    #[must_use]
    pub fn unsupported_environment(kind: &str) -> Self {
        Self::new(
            ErrorKind::Unprocessable,
            ErrorCode::UnsupportedEnvironment,
            "environment type is outside this deployment's supported range",
        )
        .with_details(serde_json::json!({ "received": kind, "supported": ["none"] }))
    }

    #[must_use]
    pub fn mcp_mode_unsupported() -> Self {
        Self::new(
            ErrorKind::BadRequest,
            ErrorCode::McpModeUnsupported,
            "openai_hosted environments are not implemented by this deployment",
        )
    }

    /// A tool declaration outside the implemented range. S8a records MCP
    /// declarations only; other official tool types are a stated gap rather
    /// than a policy refusal (`design §5.1`).
    #[must_use]
    pub fn unsupported_tool(kind: &str, field: &str) -> Self {
        Self::new(
            ErrorKind::Unprocessable,
            ErrorCode::UnsupportedTool,
            "tool type is not implemented by this deployment; only `mcp` declarations are accepted",
        )
        .with_details(serde_json::json!({
            "field": field,
            "received": kind,
            "supported": ["mcp"],
        }))
    }

    #[must_use]
    pub fn queue_unavailable(operation: &str) -> Self {
        Self::new(
            ErrorKind::Unavailable,
            ErrorCode::QueueUnavailable,
            "input queue is unavailable; the submission was not accepted",
        )
        .with_details(serde_json::json!({ "operation": operation }))
    }

    #[must_use]
    pub fn upstream_unavailable(operation: &str) -> Self {
        Self::new(
            ErrorKind::BadGateway,
            ErrorCode::UpstreamUnavailable,
            "swarm upstream is unavailable",
        )
        .with_details(serde_json::json!({ "operation": operation }))
    }

    /// Map an ACP JSON-RPC error code to the HTTP contract (`design §2.7`).
    ///
    /// The upstream message is deliberately dropped: it may embed credentials or
    /// provider payloads. Turn-level SSE errors carry the ACP message separately.
    #[must_use]
    pub fn from_acp_error(code: i64) -> Self {
        let details = serde_json::json!({ "acpCode": code });
        match code {
            -32010 => Self::new(
                ErrorKind::Conflict,
                ErrorCode::SessionConflict,
                "agent session is busy or already exists",
            )
            .with_details(details),
            -32601 => Self::new(
                ErrorKind::Unavailable,
                ErrorCode::CapabilityUnavailable,
                "agent capability is not negotiated",
            )
            .with_details(details),
            -32602 => Self::new(
                ErrorKind::BadRequest,
                ErrorCode::InvalidInput,
                "agent rejected the request parameters",
            )
            .with_details(details),
            _ => Self::new(
                ErrorKind::BadGateway,
                ErrorCode::AgentError,
                "agent returned an error",
            )
            .with_details(details),
        }
    }
}

#[cfg(test)]
#[path = "error_test.rs"]
mod tests;
