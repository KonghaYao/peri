//! Safe model diagnostics are chosen where the failure is understood, never inferred
//! from arbitrary error text. Recovery may include only static explanations and
//! tool-owned recovery references (task IDs, numeric PIDs, captured log paths, draft IDs).
//! Never construct recovery from a backend error, command or file contents.
//! The private detail preserves native diagnostic context.
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
