#[derive(Debug, thiserror::Error)]
#[error("{detail}")]
pub struct ToolFailure {
    pub recovery: String,
    pub(super) detail: String,
}

impl ToolFailure {
    #[must_use]
    pub fn new(recovery: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            recovery: recovery.into(),
            detail: detail.into(),
        }
    }
}
