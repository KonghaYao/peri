//! Tool failures retain diagnostic detail alongside actionable recovery guidance.
#[derive(Debug, thiserror::Error)]
#[error("{detail}")]
pub struct ToolFailure {
    pub recovery: String,
    detail: String,
}
impl ToolFailure {
    pub fn new(recovery: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            recovery: recovery.into(),
            detail: detail.into(),
        }
    }
}
