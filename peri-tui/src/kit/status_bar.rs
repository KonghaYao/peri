//! ratatui-kit StatusBar component.

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
fn StatusBar(_hooks: Hooks) -> impl Into<AnyElement<'static>> {
    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Status ").fg(theme::THINKING).bold().centered(),
            width: Constraint::Fill(1),
            height: Constraint::Length(2),
        ) {
            Text(text: Line::from("Line 1: TODO").fg(theme::TEXT))
            Text(text: Line::from("Line 2: TODO").fg(theme::MUTED))
        }
    )
}
