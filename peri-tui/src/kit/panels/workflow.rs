//! ratatui-kit WorkflowPanel component.

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
fn WorkflowPanel(_hooks: Hooks) -> impl Into<AnyElement<'static>> {
    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Workflow ").fg(theme::THINKING).bold().centered(),
            width: Constraint::Length(40),
            height: Constraint::Length(12),
        ) {
            ScrollView(
                scroll_bars: ScrollBars::default(),
                width: Constraint::Length(38),
                height: Constraint::Length(10),
            ) {
                Text(text: Line::from("[1] PR Review       → 2 agents").fg(theme::SAGE))
                Text(text: Line::from("[2] Code Generation  → 3 agents").fg(theme::TEXT))
                Text(text: Line::from("[3] Test Suite       → 4 agents").fg(theme::TEXT))
                Text(text: Line::from("[4] Doc Translate    → 2 agents").fg(theme::TEXT))
                Text(text: Line::from("---").fg(theme::DIM))
                Text(text: Line::from("r) Run  e) Edit  d) Delete").fg(theme::DIM))
            }
        }
    )
}
