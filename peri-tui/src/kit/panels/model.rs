//! ratatui-kit ModelPanel component.

use ratatui_kit::{
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction, Flex},
        style::{Style, Stylize},
        text::Line,
    },
};
use crate::ui::theme;

#[component]
fn ModelPanel(_hooks: Hooks) -> impl Into<AnyElement<'static>> {
    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Model ").fg(theme::THINKING).bold().centered(),
            width: Constraint::Length(44),
            height: Constraint::Length(14),
        ) {
            Text(text: Line::from("claude-sonnet-4-20250514").fg(theme::ACCENT).bold())
            Text(text: Line::from("claude-opus-4-20250514").fg(theme::TEXT))
            Text(text: Line::from("GPT-4.1").fg(theme::TEXT))
            Text(text: Line::from("GPT-4.1-mini").fg(theme::MUTED))
        }
    )
}
