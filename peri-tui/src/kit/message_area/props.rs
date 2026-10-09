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
/// 最右列是正文的最后一列）。因此本 hook 必须在 `CenterBandHook` **之前**注册——
/// `pre_component_draw` 捕获的是收窄前的整幅区域；垂直范围与带一致（带只改水平轴）。
pub(super) struct ScrollbarHook {
    pub(super) fields: State<ScrollbarFields>,
    /// 收窄前的组件区域；命中测试（`scroll::handle_event`）与渲染共用同一矩形。
    pub(super) outer: Option<Rect>,
}

impl Hook for ScrollbarHook {
    fn pre_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        self.outer = Some(drawer.area);
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
