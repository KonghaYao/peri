//! ratatui-kit BetasPanel component.

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
fn BetasPanel(_hooks: Hooks) -> impl Into<AnyElement<'static>> {
    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Betas ").fg(theme::THINKING).bold().centered(),
            width: Constraint::Length(40),
            height: Constraint::Length(12),
        ) {
            Text(text: Line::from("[ON]  subagent_v2").fg(theme::SAGE))
            Text(text: Line::from("[OFF] experimental_compact").fg(theme::TEXT))
            Text(text: Line::from("[OFF] mcp_logging").fg(theme::TEXT))
            Text(text: Line::from("[ON]  ui_v2").fg(theme::SAGE))
            Text(text: Line::from("---").fg(theme::DIM))
            Text(text: Line::from("t) Toggle  Enter) Confirm").fg(theme::DIM))
        }
    )
}
