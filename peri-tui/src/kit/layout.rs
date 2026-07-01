//! ratatui-kit SessionColumn layout component.

use ratatui_kit::{
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::Stylize,
        text::Line,
        widgets::Paragraph,
    },
};
use crate::kit::atoms;
use crate::kit::input_area::InputArea;
use crate::ui::theme;

#[component]
pub fn SessionColumn(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    // 订阅数据
    let vms = hooks.use_store(*atoms::VIEW_MODELS.get().unwrap());
    let scroll = hooks.use_store(*atoms::SCROLL_OFFSET.get().unwrap());
    let acp = hooks.use_store(*atoms::ACP_STATE.get().unwrap());

    let view_models = vms.read(); // ViewModelsSnapshot: 非 Copy，用 .read()
    let _scroll_offset = scroll.get(); // u16: Copy
    let _is_loading = acp.read().is_loading; // AcpStateSnapshot: 非 Copy

    let loading = acp.read().is_loading;
    let _ = acp; // 仅用于读取，StoreState 是 Copy 类型

    element!(
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            // 消息区（吃剩余高度）
            Text(text: Paragraph::new(
                Line::from(format!("Messages ({} committed, {} current)", view_models.committed.len(), view_models.current_turn.len()))
                    .fg(theme::TEXT)
            ))

            // 输入区（底部固定高度）
            InputArea(loading: loading)
        }
    )
}
