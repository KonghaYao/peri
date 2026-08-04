//! Panel overlay——根据 `ACTIVE_PANEL` atom 渲染当前激活面板。
//!
//! 这是 kit 路径"面板栈"的渲染入口。面板参与 `SessionColumn` 的垂直布局，位于
//! 消息流和输入区之间：打开面板时覆盖消息流底部区域，但不遮挡输入区。
//! - `None`：渲染零高度容器，不占用布局空间。
//! - `Some(kind)`：渲染对应 `#[component]` 面板。
//!
//! ## 渲染语义
//!
//! 面板不再是根级居中浮层，也不清屏；它占用消息区底部的一段高度，输入区仍固定在
//! 最底部。这样面板语义更接近“消息流上的抽屉/覆盖层”，而不是 modal。
//!
//! ## Esc 关闭
//!
//! 全局 Esc 由 `event_handlers::register_root_handlers` 处理：
//! 若 `ACTIVE_PANEL` 为 Some，调用 `close_active_panel`；否则交由子组件。

use crate::kit::atoms;
use crate::kit::panel_registry;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    prelude::*,
    ratatui::layout::{Constraint, Direction, Flex},
};

/// 面板覆盖层组件。
///
/// 订阅 `ACTIVE_PANEL` atom，渲染当前激活面板。无面板时返回空 View。
/// 同时挂载面板滚轮仲裁 handler（Global+High，先于 ScrollView 的层内处理）
/// 与渲染帧兜底 flush（消除停手后残留 pending）。
#[component]
pub fn PanelOverlay(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let active_panel = hooks.use_atom(&atoms::ACTIVE_PANEL);
    let active = *active_panel.read();
    let (_term_w, term_h) = hooks.use_terminal_size();
    hooks.use_event_handler(EventScope::Global, EventPriority::High, |event| {
        crate::kit::panel_scroll::handle_panel_scroll(&event)
    });
    crate::kit::panel_scroll::flush_panel_scroll_due();
    match active {
        Some(kind) => match panel_registry::render(kind) {
            Some(panel) => render_panel(kind, panel, term_h),
            None => render_empty(),
        },
        None => render_empty(),
    }
}

/// 预留 MessageArea 的最小行数
const MESSAGE_RESERVE: u16 = 4;

/// 包裹面板——动态高度自适应终端尺寸，水平居中显示面板内容。
fn render_panel(
    kind: crate::app::panel_types::PanelKind,
    panel: AnyElement<'static>,
    term_h: u16,
) -> AnyElement<'static> {
    let _layout = panel_registry::panel_layout(kind);

    let theme_guard = THEME_ATOM.state();
    let theme = theme_guard.read();
    let max_h = theme.component.panel.max_height;
    let min_h = theme.component.panel.min_height;
    drop(theme);
    let _ = theme_guard;

    let available = term_h.saturating_sub(MESSAGE_RESERVE);
    let height = max_h.min(available).max(min_h.min(available));

    element!(
        View(
            flex_direction: Direction::Horizontal,
            justify_content: Flex::Center,
            width: Constraint::Fill(1),
            height: Constraint::Length(height),
        ) {
            View(width: Constraint::Fill(1), height: Constraint::Fill(1)) {
                { panel }
            }
        }
    )
    .into()
}

/// 空面板——无面板激活时返回零尺寸 Positioned，避免默认 View/Fragment 布局参与父级 flex。
fn render_empty() -> AnyElement<'static> {
    element!(Positioned(x: 0u16, y: 0u16, width: 0u16, height: 0u16, clear: false)).into()
}
