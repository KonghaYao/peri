//! ratatui-kit StatusPanel component.

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
fn StatusPanel(_hooks: Hooks) -> impl Into<AnyElement<'static>> {
    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Status ").fg(theme::THINKING).bold().centered(),
            width: Constraint::Length(40),
            height: Constraint::Length(12),
        ) {
            Text(text: Line::from("Git: main @ a1b2c3d").fg(theme::TEXT))
            Text(text: Line::from("YOLO: OFF").fg(theme::TEXT))
            Text(text: Line::from("Compact: 0.32 / 0.70").fg(theme::TEXT))
            Text(text: Line::from("---").fg(theme::DIM))
            Text(text: Line::from("Token: 34.2k / 200k").fg(theme::MUTED))
            Text(text: Line::from("Cache: 12.1k Read / 0 Write").fg(theme::DIM))
        }
    )
}
