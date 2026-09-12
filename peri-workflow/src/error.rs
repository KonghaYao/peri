use thiserror::Error;

#[derive(Debug, Error)]
pub enum WorkflowError {
    #[error("Workflow cleanup could not be confirmed: {0}")]
    CleanupFailed(String),

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

    #[error(transparent)]
    JsRuntime(peri_js_runtime::JsRuntimeError),
}

impl From<peri_js_runtime::JsRuntimeError> for WorkflowError {
    fn from(error: peri_js_runtime::JsRuntimeError) -> Self {
        match error {
            peri_js_runtime::JsRuntimeError::CleanupFailed(reason) => Self::CleanupFailed(reason),
            error => Self::JsRuntime(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_cleanup_uncertainty_remains_visible_to_the_session_owner() {
        let error = WorkflowError::from(peri_js_runtime::JsRuntimeError::CleanupFailed(
            "process assignment failed".into(),
        ));
        assert!(matches!(error, WorkflowError::CleanupFailed(_)));
    }
}
