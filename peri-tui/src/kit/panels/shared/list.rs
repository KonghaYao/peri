//! 面板列表视口——固定行高列表的可见范围、滚动跟随与鼠标命中的唯一推导处。
//!
//! [Why] 各面板此前各自持有 `VISIBLE_ITEMS` 常量（Hooks 3、MCP 6、Cron 4…），
//! 按"面板高度 18"的手算值写死；而面板实际高度由
//! `panel_overlay::panel_height`（主题 min/max 与终端高度）决定，终端变高后
//! 绘制只显示常量个项，鼠标命中却按实测区域反推行号——两者会给出不同的项。
//!
//! 本模块把"内容区行数 → 可见项数 → 滚动起点 → 命中布局"收敛为一次计算：
//! 渲染用 `window()` 取行，事件用 `hit_layout()` 命中，二者来自同一
//! `ListViewport`，不再可能出现绘制与命中不一致。

use ratatui_kit::ratatui::layout::Rect;

use crate::kit::list_nav::{clamp_selection, scroll_start_for_selected};
use crate::kit::panel_mouse::ListLayout;
use crate::kit::panel_overlay::panel_content_rows;

/// 列表内容的固定行度量。
///
/// `header_rows` / `footer_rows` 行不参与命中（`panel_mouse::hit_row` 同语义）；
/// `item_rows` 是每一项占据的行数，项内任意一行都命中该项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ListMetrics {
    pub header_rows: u16,
    pub item_rows: u16,
    pub footer_rows: u16,
}

impl ListMetrics {
    pub(crate) const fn new(header_rows: u16, item_rows: u16, footer_rows: u16) -> Self {
        Self {
            header_rows,
            item_rows,
            footer_rows,
        }
    }
}

/// 一帧的列表视口。绘制（`window`）与鼠标命中（`hit_layout`）必须来自同一实例。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ListViewport {
    metrics: ListMetrics,
    item_count: usize,
    /// 当前可见项数（由内容区行数与项行高推导，可能为 0）
    pub visible_items: usize,
    /// 视口裁剪掉的项数（视口跟随选中项）
    pub scroll_start: usize,
}

impl ListViewport {
    /// 由面板内容区行数（不含上下边框）推导视口。
    ///
    /// `selected` 超出当前列表长度时按 `item_count` 收敛，刷新缩短列表不会留下
    /// 越界选择。
    pub(crate) fn for_content_rows(
        content_rows: u16,
        metrics: ListMetrics,
        item_count: usize,
        selected: usize,
    ) -> Self {
        let item_rows = metrics.item_rows.max(1);
        let usable = content_rows.saturating_sub(metrics.header_rows + metrics.footer_rows);
        let visible_items = (usable / item_rows) as usize;
        let selected = clamp_selection(selected, item_count);
        let scroll_start = scroll_start_for_selected(selected, item_count, visible_items);
        Self {
            metrics,
            item_count,
            visible_items,
            scroll_start,
        }
    }

    /// 渲染窗口：`(全量索引, 项)` 迭代器。
    ///
    /// 索引是全量列表中的下标（不是窗口内序号），面板据此保持选中态判断与
    /// 鼠标命中使用同一坐标。
    pub(crate) fn window<'a, T>(
        &self,
        items: &'a [T],
    ) -> impl Iterator<Item = (usize, &'a T)> + 'a {
        let skip = self.scroll_start;
        let take = self.visible_items;
        items.iter().enumerate().skip(skip).take(take)
    }

    /// 鼠标命中布局——与 `window()` 同源，供 `panel_mouse::hit_item` 使用。
    pub(crate) fn hit_layout(&self) -> ListLayout {
        ListLayout {
            header_rows: self.metrics.header_rows,
            item_rows: self.metrics.item_rows,
            footer_rows: self.metrics.footer_rows,
            visible_items: self.visible_items as u16,
            scroll_start: self.scroll_start,
            item_count: self.item_count,
        }
    }
}

/// 面板内容区行数：已测量时用实测值，首帧未测量时按终端高度预测。
///
/// `measured` 是面板自身的上一帧区域（`AreaTracker` / `use_previous_size`）；
/// 首帧为 `Rect::default()`，此时不能把面板渲染成空列表——用
/// `panel_content_rows` 的预测高度，与 `PanelOverlay` 的实际高度同源。
pub(crate) fn content_rows(measured: Option<Rect>, term_h: u16) -> u16 {
    match measured {
        // 上下边框各占 1 行（`panel_shell!` 的 TOP|BOTTOM），高度不足 3 行按未测量处理
        Some(rect) if rect.height >= 3 => rect.height - 2,
        _ => panel_content_rows(term_h),
    }
}

#[cfg(test)]
#[path = "list_test.rs"]
mod tests;
