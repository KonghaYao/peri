//! App 级 UI 状态：跨 session 共享的全局 UI 临时状态

use std::{cell::Cell, time::Instant};

use super::{
    oauth_prompt::OAuthPrompt, setup_wizard::SetupWizardPanel,
    workflow_tracker::WorkflowProgressTracker,
};

/// App 级 UI 状态：跨 session 共享的全局 UI 临时状态。
///
/// 与 `ServiceRegistry` 中的"服务"字段（config、MCP pool、cron 等）不同，
/// 这里的字段纯粹是 UI 层面的临时状态（高亮计时、弹窗、鼠标探测等）。
pub struct GlobalUiState {
    pub setup_wizard: Option<SetupWizardPanel>,
    pub oauth_prompt: Option<OAuthPrompt>,
    pub mode_highlight_until: Option<Instant>,
    pub model_highlight_until: Option<Instant>,
    pub provider_highlight_until: Option<Instant>,
    pub mcp_ready_shown_until: Cell<Option<Instant>>,
    /// MCP 失败提示自动消失计时器（首次显示后 10 秒消失）
    pub mcp_failed_shown_until: Cell<Option<Instant>>,
    pub quit_pending_since: Option<Instant>,
    /// 双击 ESC 检测时间戳（rewind 弹窗触发）
    pub rewind_pending_since: Option<Instant>,
    /// 运行中按 ESC 的 rewind 提示截止时间
    pub rewind_busy_hint_until: Option<Instant>,
    pub quit_requested: bool,
    pub mouse_available: Option<bool>,
    /// Workflow 进度追踪器（累积 WorkflowProgressPayload 事件）。
    pub workflow_tracker: WorkflowProgressTracker,
    /// Cron #23 P1 fix — App 侧向 SM 请求截断 state.view 到指定索引。
    ///
    /// 由 `handle_interrupted` 分支 2（无工具调用，回滚路径）设置：v1
    /// `view_messages` 已通过 `apply_rebuild_all(user_msg_idx, [])` 截断，
    /// 但 v2 `state.view` 由状态机拥有，App 无法直接修改。main_loop 在
    /// `handle_acp_event` 返回后会检查此 flag，对 `State::Idle.view` /
    /// `State::Streaming.view` 执行 `truncate(idx)`，保持 v1/v2 一致。
    ///
    /// 不修改 streaming.rs 第 7c 步代码（TurnInterrupted 持久化逻辑），
    /// 避免破坏 cron #22 / 7c 步的脆弱修复。
    pub pending_view_rewind_to: Option<usize>,
}

impl Default for GlobalUiState {
    fn default() -> Self {
        Self::new()
    }
}
impl GlobalUiState {
    pub fn new() -> Self {
        Self {
            setup_wizard: None,
            oauth_prompt: None,
            mode_highlight_until: None,
            model_highlight_until: None,
            provider_highlight_until: None,
            mcp_ready_shown_until: Cell::new(None),
            mcp_failed_shown_until: Cell::new(None),
            quit_pending_since: None,
            rewind_pending_since: None,
            rewind_busy_hint_until: None,
            quit_requested: false,
            mouse_available: None,
            workflow_tracker: WorkflowProgressTracker::new(),
            pending_view_rewind_to: None,
        }
    }
}
