use std::sync::Arc;

use peri_acp_types::tasks::{OnBgCompleteFn, TaskManager};

/// Session-scoped inputs delivered to the workspace instance's `BashTool`.
///
/// Both values are passed to the existing tool unchanged. Missing input is a supported degraded
/// mode: the workspace server still exposes all seven tools, while Bash lacks the corresponding
/// background-task behavior.
#[derive(Clone)]
pub struct WorkspaceInstanceInput {
    /// Per-session task manager for background execution, timeout promotion, and process ownership.
    pub task_manager: Option<Arc<dyn TaskManager>>,
    /// Synchronous callback run before a background task is marked complete.
    pub on_bg_complete: Option<OnBgCompleteFn>,
}
