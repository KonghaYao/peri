//! App 级 UI 临时状态：跨 session 共享的 UI 层状态容器。
//!
//! (I16-B/C) 大幅瘦身：所有字段均已退役——kit 单路径下，瞬时高亮、rewind
//! 触发、workflow 累积、退出确认、setup_wizard 实例化等均由 atoms
//! （MODEL_HIGHLIGHT_UNTIL / LAST_ESC_TIME / REWIND_PREVIEW / POPUP_KIND 等）
//! 或 kit/setup_wizard.rs 独立维护。本结构体保留为未来扩展容器。

pub struct GlobalUiState;

impl Default for GlobalUiState {
    fn default() -> Self {
        Self::new()
    }
}

impl GlobalUiState {
    pub fn new() -> Self {
        Self
    }
}
