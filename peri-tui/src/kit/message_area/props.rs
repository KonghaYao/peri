//! Props + 位置 Hook + 滚动条 Hook。

use super::grid::GridSpec;
use ratatui_kit::prelude::*; // Hook, ComponentDrawer, State, Props derive
use ratatui_kit::ratatui::layout::Rect;

// ── 鼠标辅助 ─────────────────────────────────────────────────────────────

pub(super) fn mouse_in_area(mouse_row: u16, mouse_col: u16, area: Rect) -> bool {
    let area_bottom = area.y.saturating_add(area.height);
    let area_right = area.x.saturating_add(area.width);
    mouse_row >= area.y && mouse_row < area_bottom && mouse_col >= area.x && mouse_col < area_right
}

// ── 消息区位置追踪 Hook ─────────────────────────────────────────────────

pub(super) struct MsgAreaTracker {
    pub(super) rect: Option<Rect>,
    geometry_changed: bool,
}

impl MsgAreaTracker {
    pub(super) fn new() -> Self {
        Self {
            rect: None,
            geometry_changed: false,
        }
    }
}

impl Hook for MsgAreaTracker {
    fn poll_change(&mut self, _cx: &mut std::task::Context) -> std::task::Poll<()> {
        if std::mem::take(&mut self.geometry_changed) {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    }

    fn pre_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        if self.rect != Some(drawer.area) {
            self.rect = Some(drawer.area);
            self.geometry_changed = true;
        }
    }
}

// ── 滚动条 Hook ─────────────────────────────────────────────────────────

/// 视口右侧滚动条字段——通过 use_state 存储，避免 use_hook 的 borrow 冲突。
#[derive(Default, Clone, Copy)]
pub(super) struct ScrollbarFields {
    pub(super) content_length: usize,
    pub(super) position: usize,
    pub(super) viewport_length: usize,
}

/// 视口右侧滚动条——post_component_draw 时基于 fields 渲染。
///
/// 替代被移除的 ScrollView 内置滚动条。每帧 render body 更新 ScrollbarFields state。
///
/// [§3.1] 滚动条是窗口级 chrome：锚在**窗口最右列**，不随居中带内移（带内的
/// 最右列是正文的最后一列）。MessageArea 由 `CenterBand` 在 draw 期收窄后，
/// 本 hook 的 `pre_component_draw` 看到的是 band；render body 每帧写入终端
/// 宽度与网格，由 [`scrollbar_outer_area`] 显式还原外框。渲染与命中测试
/// （`scroll::handle_event`）共用该矩形。
pub(super) struct ScrollbarHook {
    pub(super) fields: State<ScrollbarFields>,
    /// 外框（终端整幅宽度）；命中测试与渲染共用同一矩形。
    pub(super) outer: Option<Rect>,
    /// 终端总列数——render body 每帧写入；0 表示未知（回退 band 本身）。
    pub(super) term_width: u16,
    /// 当前水平网格——`left_pad` 用于从 band 反推外框左缘。
    pub(super) grid: GridSpec,
}

impl ScrollbarHook {
    /// 每帧由 render body 写入（终端 resize 后宽度与网格随帧变化）。
    pub(super) fn set_layout(&mut self, term_width: u16, grid: GridSpec) {
        self.term_width = term_width;
        self.grid = grid;
    }
}

/// 由居中带区域还原滚动条外框：左缘退回带前留白、宽取终端总宽。
///
/// `term_width == 0`（终端尺寸不可用）时回退 band——找不到窗口边界时宁可
/// 只覆盖带内，不构造跨屏矩形。
pub(super) fn scrollbar_outer_area(band: Rect, grid: GridSpec, term_width: u16) -> Rect {
    if term_width == 0 {
        return band;
    }
    Rect {
        x: band.x.saturating_sub(grid.left_pad),
        y: band.y,
        width: term_width,
        height: band.height,
    }
}

impl Hook for ScrollbarHook {
    fn pre_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        self.outer = Some(scrollbar_outer_area(
            drawer.area,
            self.grid,
            self.term_width,
        ));
    }

    fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        let f = *self.fields.read();
        let area = self.outer.unwrap_or(drawer.area);
        crate::kit::scrollbar_chrome::draw_scrollbar(
            drawer,
            area,
            f.content_length,
            f.position,
            f.viewport_length,
        );
    }
}

// ── Props ──────────────────────────────────────────────────────────────────

#[derive(Default, Props)]
pub struct MessageAreaProps {
    /// Transcript 水平网格（§3.1）——由 SessionColumn 按终端宽度计算。
    pub grid: GridSpec,
}

#[cfg(test)]
#[path = "props_test.rs"]
mod tests;
