//! 斜杠命令补全组件。
//!
//! 当用户输入 "/" 时激活，过滤可用命令列表。
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
pub fn SlashCompletion(
    mut hooks: Hooks,
    prefix: String,
    commands: Vec<String>,
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
                // Phase 5: 注入选中命令
            }
            _ => {}
        }
    });

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::THINKING),
            top_title: Line::from(format!(" /{} ", prefix)).fg(theme::THINKING).bold(),
            width: Constraint::Length(40),
            height: Constraint::Length((commands.len() + 2).min(10) as u16),
        ) {
            for (_, cmd) in commands.iter().enumerate() {
                Text(text: Line::from(cmd.clone()).fg(theme::TEXT))
            }
        }
    )
}
