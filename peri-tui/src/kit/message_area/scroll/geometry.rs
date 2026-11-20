//! 滚动条拖拽状态——几何、绘制与命中由组件的 `VerticalBar` 统一持有。

use peri_tui_components::scrollbar::Throttle;
use ratatui_kit::ratatui::layout::Rect;

/// 滚动条 thumb 拖拽状态。
/// `thumb_offset` 是按下时鼠标 row 相对 thumb 顶部的偏移，拖动期间锁定——
/// 让「点击 thumb 中央 → 拖拽」时 thumb 不跳变。
#[derive(Debug, Clone, Copy, Default)]
pub(in crate::kit::message_area) struct ScrollbarDragState {
    pub(in crate::kit::message_area) active: bool,
    pub(in crate::kit::message_area) thumb_offset: u16,
    pub(in crate::kit::message_area) throttle: Throttle,
}

/// 判断鼠标列是否落在滚动条列（drawer.area 最右 1 列）。
#[must_use]
pub(in crate::kit::message_area) fn is_scrollbar_column(mouse_col: u16, area: Rect) -> bool {
    mouse_col == area.x.saturating_add(area.width).saturating_sub(1)
}
