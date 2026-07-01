//! ratatui-kit MemoryPanel component.
//!
//! 搜索型面板：SearchInput（Input 替代） + ScrollView 列表。

use ratatui_kit::{
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Style, Stylize},
        text::Line,
    },
};
use crate::ui::theme;

#[component]
fn MemoryPanel(_hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let search_input = tui_input::Input::default();

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Memory ").fg(theme::THINKING).bold().centered(),
            width: Constraint::Length(40),
            height: Constraint::Length(12),
        ) {
            Input(
                input: search_input,
                cursor_style: Style::new().fg(theme::ACCENT),
                placeholder: "Search memories...".to_string(),
                placeholder_style: Style::new().fg(theme::DIM),
                style: Style::new().fg(theme::TEXT),
                hide_cursor: false,
            )
            ScrollView(
                scroll_bars: ScrollBars::default(),
                width: Constraint::Length(38),
                height: Constraint::Length(8),
            ) {
                Text(text: Line::from("CLAUDE.md: GLOBAL").fg(theme::SAGE))
                Text(text: Line::from("CLAUDE.md: peri-tui").fg(theme::TEXT))
                Text(text: Line::from("skills/ratatui-kit").fg(theme::TEXT))
                Text(text: Line::from("skills/design-an-interface").fg(theme::MUTED))
            }
        }
    )
}
