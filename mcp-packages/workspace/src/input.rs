use std::sync::Arc;

use peri_acp_types::tasks::{OnBgCompleteFn, TaskManager};

/// Session-scoped inputs delivered to the workspace instance's `BashTool`.
///
/// All values are passed to the existing tool unchanged. Missing input is a supported degraded
/// mode: the workspace server still exposes all seven tools, while Bash lacks the corresponding
/// background-task behavior.
#[derive(Clone, Default)]
pub struct WorkspaceInstanceInput {
    /// Per-session task manager for background execution, timeout promotion, and process ownership.
    pub task_manager: Option<Arc<dyn TaskManager>>,
    /// Synchronous callback run before a background task is marked complete.
    pub on_bg_complete: Option<OnBgCompleteFn>,
    /// Effective `run_in_background` default when the call omits the field.
    ///
    /// Injected once at session assembly from the frozen beta-flag projection
    /// (`full-async-tools`); an explicit `false` still runs in the foreground. `false` is the
    /// built-in default, so unconfigured sessions behave exactly as before.
    pub default_run_in_background: bool,
}
