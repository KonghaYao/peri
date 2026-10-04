use thiserror::Error;

pub type Result<T> = std::result::Result<T, JsRuntimeError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceKind {
    FrameBytes,
}

#[derive(Debug, Error)]
pub enum JsRuntimeError {
    #[error("Failed to spawn JavaScript runtime: {0}")]
    SpawnFailed(String),

    #[error("JavaScript RPC protocol error")]
    Rpc(String),

    #[error("JavaScript RPC request failed")]
    RpcResponse(crate::JsonRpcError),

    #[error(
        "JavaScript resource limit exceeded: {resource:?}, limit={limit}, observed={observed}"
    )]
    ResourceLimit {
        resource: ResourceKind,
        limit: usize,
        observed: usize,
    },

    #[error("JavaScript process cleanup failed: {0}")]
    CleanupFailed(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}
