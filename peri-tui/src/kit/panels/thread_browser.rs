//! ratatui-kit ThreadBrowserPanel component.
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
fn ThreadBrowserPanel(_hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let search_input = tui_input::Input::default();

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Thread Browser ").fg(theme::THINKING).bold().centered(),
            width: Constraint::Length(40),
            height: Constraint::Length(14),
        ) {
            Input(
                input: search_input,
                cursor_style: Style::new().fg(theme::ACCENT),
                placeholder: "Filter sessions...".to_string(),
                placeholder_style: Style::new().fg(theme::DIM),
                style: Style::new().fg(theme::TEXT),
                hide_cursor: false,
            )
            ScrollView(
                scroll_bars: ScrollBars::default(),
                width: Constraint::Length(38),
                height: Constraint::Length(10),
            ) {
                Text(text: Line::from("▶ session-2026-07-01-a3f2").fg(theme::SAGE))
                Text(text: Line::from("  session-2026-06-30-b1c4").fg(theme::TEXT))
                Text(text: Line::from("  session-2026-06-29-d5e6").fg(theme::TEXT))
                Text(text: Line::from("  session-2026-06-28-f7a8").fg(theme::TEXT))
                Text(text: Line::from("  session-2026-06-27-c9b0").fg(theme::MUTED))
                Text(text: Line::from("---").fg(theme::DIM))
                Text(text: Line::from("Enter) Open  /) Filter  q) Back").fg(theme::DIM))
            }
        }
    )
}
