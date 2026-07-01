//! App 级 UI 状态：跨 session 共享的全局 UI 临时状态。
//!
//! (I16-B) 大幅瘦身：mode_highlight_until / model_highlight_until /
//! provider_highlight_until / mcp_ready_shown_until / mcp_failed_shown_until /
//! quit_pending_since / rewind_pending_since / rewind_busy_hint_until /
//! quit_requested / mouse_available / workflow_tracker /
//! pending_view_rewind_to 全部退役——kit 单路径下，瞬时高亮、rewind 触发、
//! workflow 累积、退出确认等均由 atoms（MODEL_HIGHLIGHT_UNTIL /
//! LAST_ESC_TIME / REWIND_PREVIEW 等）独立维护。

use super::setup_wizard::SetupWizardPanel;

/// App 级 UI 临时状态：跨 session 共享的 UI 层状态。
pub struct GlobalUiState {
    pub setup_wizard: Option<SetupWizardPanel>,
}

impl Default for GlobalUiState {
    fn default() -> Self {
        Self::new()
    }
}

impl GlobalUiState {
    pub fn new() -> Self {
        Self { setup_wizard: None }
    }
}
