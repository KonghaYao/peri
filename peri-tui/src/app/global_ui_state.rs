//! App 级 UI 状态：跨 session 共享的全局 UI 临时状态

use std::{cell::Cell, time::Instant};

use super::{oauth_prompt::OAuthPrompt, setup_wizard::SetupWizardPanel};

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
        }
    }
}
