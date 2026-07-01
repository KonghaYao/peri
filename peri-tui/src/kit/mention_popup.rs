//! @mention 文件提醒弹出组件。
//!
//! 当用户在输入框中输入 "@" 时激活，按路径前缀过滤文件列表。
//! Phase 4 编译桩——Phase 5 实现完整交互。

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Style, Stylize},
        text::Line,
    },
};

use crate::ui::theme;

#[component]
pub fn MentionPopup(
    mut hooks: Hooks,
    prefix: String,
    items: Vec<String>,
    _on_select: Handler<'static, String>,
    _on_cancel: Handler<'static, ()>,
) -> impl Into<AnyElement<'static>> {
    // Phase 5: use_input_layer 替代
    hooks.use_local_events(move |event| {
        let Event::Key(key) = event else { return };
        if key.kind != KeyEventKind::Press {
            return;
        }
        match key.code {
            KeyCode::Esc => {
                // Phase 5: 触发 on_cancel
            }
            KeyCode::Enter => {
                // Phase 5: 注入选中项到 textarea
            }
            _ => {}
        }
    });

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::THINKING),
            top_title: Line::from(format!(" @{} ", prefix)).fg(theme::THINKING).bold(),
            width: Constraint::Length(50),
            height: Constraint::Length((items.len() + 2).min(10) as u16),
        ) {
            for (_, item) in items.iter().enumerate() {
                Text(text: Line::from(item.clone()).fg(theme::TEXT))
            }
        }
    )
}
