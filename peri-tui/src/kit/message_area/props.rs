//! Props + 位置 Hook + 滚动条 Hook。

use super::grid::GridSpec;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::prelude::*; // Hook, ComponentDrawer, State, Props derive
use ratatui_kit::ratatui::layout::Rect;
use ratatui_kit::ratatui::style::{Modifier, Style};

// ── 鼠标辅助 ─────────────────────────────────────────────────────────────

pub(super) fn mouse_in_area(mouse_row: u16, mouse_col: u16, area: Rect) -> bool {
    let area_bottom = area.y.saturating_add(area.height);
    let area_right = area.x.saturating_add(area.width);
    mouse_row >= area.y && mouse_row < area_bottom && mouse_col >= area.x && mouse_col < area_right
}

// ── 消息区位置追踪 Hook ─────────────────────────────────────────────────

pub(super) struct MsgAreaTracker {
    pub(super) rect: Option<Rect>,
}

impl MsgAreaTracker {
    pub(super) fn new() -> Self {
        Self { rect: None }
    }
}

impl Hook for MsgAreaTracker {
    fn pre_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        self.rect = Some(drawer.area);
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
pub(super) struct ScrollbarHook {
    pub(super) fields: State<ScrollbarFields>,
}

impl Hook for ScrollbarHook {
    fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        let f = *self.fields.read();
        // 仅当内容超出视口时才渲染滚动条
        if f.content_length <= f.viewport_length {
            return;
        }
        let sem = THEME_ATOM.state().read().semantic;
        let thumb_bg = sem.text.dim;
        let scrollbar =
            ratatui::widgets::Scrollbar::new(ratatui::widgets::ScrollbarOrientation::VerticalRight)
                .thumb_symbol(" ")
                .thumb_style(Style::default().fg(thumb_bg).bg(thumb_bg))
                .track_symbol(None)
                .begin_symbol(Some("▲"))
                .begin_style(
                    Style::default()
                        .fg(sem.text.muted)
                        .add_modifier(Modifier::BOLD),
                )
                .end_symbol(Some("▼"))
                .end_style(
                    Style::default()
                        .fg(sem.text.muted)
                        .add_modifier(Modifier::BOLD),
                );
        let mut state = ratatui::widgets::ScrollbarState::new(f.content_length)
            .position(f.position)
            .viewport_content_length(f.viewport_length);
        drawer.render_stateful_widget(scrollbar, drawer.area, &mut state);
    }
}

// ── Props ──────────────────────────────────────────────────────────────────

#[derive(Default, Props)]
pub struct MessageAreaProps {
    /// Transcript 水平网格（§3.1）——由 SessionColumn 按终端宽度计算。
    pub grid: GridSpec,
}
