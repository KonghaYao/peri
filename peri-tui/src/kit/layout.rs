//! ratatui-kit SessionColumn layout component.

// element! 宏展开触发 clippy::needless_update（ratatui-kit 上游问题），模块级抑制。
#![allow(clippy::needless_update)]

use crate::kit::atoms;
use crate::kit::input_area::InputArea;
use crate::kit::message_area::MessageArea;
use crate::kit::panel_overlay::PanelOverlay;
use ratatui_kit::{
    prelude::*,
    ratatui::layout::{Constraint, Direction},
};

#[component]
pub fn SessionColumn(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let acp = hooks.use_atom(&atoms::ACP_STATE);
    let active_panel = hooks.use_atom(&atoms::ACTIVE_PANEL);

    let loading = acp.read().is_loading;
    let panel_open = active_panel.read().is_some();

    // I18-A：响应式终端宽度——替代硬编码 width=100，让 markdown 折行准。
    // 减去 4 列内边距（左右各 2）保留视觉透气感；下限 20 防极窄终端溢出。
    let (term_w, _) = hooks.use_terminal_size();
    let width: usize = (term_w as usize).saturating_sub(4).max(20);
    let mut last_width = hooks.use_state(|| 0u16);
    let next_width = width as u16;
    if *last_width.read() != next_width {
        last_width.set(next_width);
        if let Some(tx) = atoms::RESIZE_TX.get() {
            let _ = tx.send(next_width);
        }
    }

    element!(
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            MessageArea(width: width)

            // 面板位于消息流之上、输入区之上；参与主布局，不再是根级浮动覆盖。
            PanelOverlay()

            // 面板打开时隐藏输入区，避免输入框抢占用户注意力；关闭面板后自动恢复。
            InputArea(loading: loading, hidden: panel_open)
        }
    )
}
