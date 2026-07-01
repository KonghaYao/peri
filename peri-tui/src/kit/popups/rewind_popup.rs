//! ratatui-kit RewindPopup component.

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
fn RewindPopup(_hooks: Hooks) -> impl Into<AnyElement<'static>> {
    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Rewind ").fg(theme::THINKING).bold().centered(),
            width: Constraint::Length(50),
            height: Constraint::Length(10),
        ) {
            Text(text: Line::from("TODO").fg(theme::TEXT).centered())
        }
    )
}
