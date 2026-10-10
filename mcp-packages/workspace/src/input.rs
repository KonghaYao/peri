use std::sync::Arc;

use peri_acp_types::tasks::{OnBgCompleteFn, TaskManager};

/// Session-scoped inputs delivered to the workspace instance's `BashTool`.
///
/// Both values are passed to the existing tool unchanged. Missing input is a supported degraded
/// mode: the workspace server still exposes all seven tools, while Bash lacks the corresponding
/// background-task behavior.
///
/// `run_in_background` 的有效缺省不是本类型的成员：它只经生产构造器
/// [`crate::WorkspaceMcpServer::standalone`] 的显式参数注入（会话装配从冻结的 beta
/// flag 投影派生），本类型保持既有两名成员不变。
#[derive(Clone, Default)]
pub struct WorkspaceInstanceInput {
    /// Per-session task manager for background execution, timeout promotion, and process ownership.
    pub task_manager: Option<Arc<dyn TaskManager>>,
    /// Synchronous callback run before a background task is marked complete.
    pub on_bg_complete: Option<OnBgCompleteFn>,
}
