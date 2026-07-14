use thiserror::Error;

#[derive(Debug, Error)]
pub enum WorkflowError {
    #[error("Failed to spawn workflow runner: {0}")]
    SpawnFailed(String),

    #[error("RPC error: {0}")]
    Rpc(String),

    #[error("Script parse error: {0}")]
    ScriptParse(String),

    #[error("Maximum {0} concurrent workflows reached")]
    ConcurrentLimit(usize),

    #[error("Workflow {0} not found")]
    NotFound(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}
