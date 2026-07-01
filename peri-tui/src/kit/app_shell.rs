//! ratatui-kit AppShell root component.

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
fn AppShell(_hooks: Hooks) -> impl Into<AnyElement<'static>> {
    element!(
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            Text(text: Line::from("AppShell (Esc to quit)").fg(theme::TEXT).centered())
        }
    )
}
