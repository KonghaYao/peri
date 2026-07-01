//! ratatui-kit AgentPanel component.

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
fn AgentPanel(_hooks: Hooks) -> impl Into<AnyElement<'static>> {
    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Agent ").fg(theme::THINKING).bold().centered(),
            width: Constraint::Length(44),
            height: Constraint::Length(14),
        ) {
            Text(text: Line::from("Session: abc123").fg(theme::ACCENT))
            Text(text: Line::from("Provider: Anthropic").fg(theme::TEXT))
            Text(text: Line::from("Model: claude-sonnet-4-20250514").fg(theme::TEXT))
            Text(text: Line::from("Turns: 12 | Tokens: 34.2k").fg(theme::MUTED))
            Text(text: Line::from("Status: Idle").fg(theme::SAGE))
        }
    )
}
