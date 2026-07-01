//! ratatui-kit SessionColumn layout component.

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
fn SessionColumn(_hooks: Hooks) -> impl Into<AnyElement<'static>> {
    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Session ").fg(theme::THINKING).bold().centered(),
            width: Constraint::Length(80),
            height: Constraint::Length(20),
        ) {
            View(
                flex_direction: Direction::Vertical,
                width: Constraint::Fill(1),
                height: Constraint::Fill(1),
            ) {
                Text(text: Line::from("TODO: scrollback").fg(theme::TEXT))
                Text(text: Line::from("TODO: input area").fg(theme::MUTED))
            }
        }
    )
}
