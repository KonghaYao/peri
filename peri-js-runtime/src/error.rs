use std::time::Duration;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, JsRuntimeError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceKind {
    SourceBytes,
    InputBytes,
    FrameBytes,
    LogBytes,
    ResultBytes,
    InternalCalls,
    ConcurrentExecutions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsExecutionFailure {
    ToolFailed,
    ResourceLimit,
    Timeout,
    Cancelled,
}

impl JsExecutionFailure {
    pub fn code(self) -> &'static str {
        match self {
            Self::ToolFailed => "TOOL_FAILED",
            Self::ResourceLimit => "RESOURCE_LIMIT",
            Self::Timeout => "TIMEOUT",
            Self::Cancelled => "CANCELLED",
        }
    }

    pub fn public_message(self) -> &'static str {
        match self {
            Self::ToolFailed => "JavaScript execution failed",
            Self::ResourceLimit => "JavaScript resource limit exceeded",
            Self::Timeout => "JavaScript execution timed out",
            Self::Cancelled => "JavaScript execution cancelled",
        }
    }
}

#[derive(Debug, Error)]
pub enum JsRuntimeError {
    #[error("JavaScript runtime artifact failed integrity validation")]
    ArtifactTampered,

    #[error("JavaScript runtime artifact is unavailable")]
    ArtifactUnavailable,

    #[error("Failed to spawn JavaScript runtime: {0}")]
    SpawnFailed(String),

    #[error("JavaScript RPC protocol error")]
    Rpc(String),

    #[error("JavaScript RPC request failed")]
    RpcResponse(crate::JsonRpcError),

    #[error("{}", .0.public_message())]
    ExecutionFailed(JsExecutionFailure),

    #[error("JavaScript execution cancelled")]
    Cancelled,

    #[error("JavaScript execution timed out after {limit:?}")]
    Timeout { limit: Duration },

    #[error(
        "JavaScript resource limit exceeded: {resource:?}, limit={limit}, observed={observed}"
    )]
    ResourceLimit {
        resource: ResourceKind,
        limit: usize,
        observed: usize,
    },

    #[error(
        "JavaScript runtime exited unexpectedly: success={success}, code={code:?}, stderr_bytes={stderr_bytes}"
    )]
    RuntimeExited {
        success: bool,
        code: Option<i32>,
        stderr_bytes: usize,
    },

    #[error("JavaScript process cleanup failed: {0}")]
    CleanupFailed(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

impl JsRuntimeError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ExecutionFailed(failure) => failure.code(),
            Self::Cancelled => "CANCELLED",
            Self::Timeout { .. } => "TIMEOUT",
            Self::ResourceLimit { .. } => "RESOURCE_LIMIT",
            Self::ArtifactTampered
            | Self::ArtifactUnavailable
            | Self::SpawnFailed(_)
            | Self::RuntimeExited { .. }
            | Self::CleanupFailed(_)
            | Self::Io(_) => "RUNTIME_FAILED",
            Self::Rpc(_) | Self::RpcResponse(_) | Self::Json(_) => "PROTOCOL_ERROR",
        }
    }

    pub fn public_message(&self) -> &'static str {
        match self {
            Self::ExecutionFailed(failure) => failure.public_message(),
            Self::Cancelled => "JavaScript execution cancelled",
            Self::Timeout { .. } => "JavaScript execution timed out",
            Self::ResourceLimit { .. } => "JavaScript resource limit exceeded",
            Self::ArtifactTampered
            | Self::ArtifactUnavailable
            | Self::SpawnFailed(_)
            | Self::RuntimeExited { .. }
            | Self::CleanupFailed(_)
            | Self::Io(_) => "JavaScript runtime failed",
            Self::Rpc(_) | Self::RpcResponse(_) | Self::Json(_) => "JavaScript RPC protocol error",
        }
    }
}
