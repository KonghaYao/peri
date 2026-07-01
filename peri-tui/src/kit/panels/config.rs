//! ratatui-kit ConfigPanel component.
//!
//! 配置面板支持 toggle / cycle / text 三种行类型。

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
fn ConfigPanel(_hooks: Hooks) -> impl Into<AnyElement<'static>> {
    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Config ").fg(theme::THINKING).bold().centered(),
            width: Constraint::Length(44),
            height: Constraint::Length(16),
        ) {
            Text(text: Line::from("YOLO Mode       [OFF]").fg(theme::TEXT))
            Text(text: Line::from("Auto Compact    [ON] ").fg(theme::TEXT))
            Text(text: Line::from("Show Reasoning  [ON] ").fg(theme::TEXT))
            Text(text: Line::from("Model:  claude-sonnet-4-20250514").fg(theme::ACCENT))
            Text(text: Line::from("Max Iterations: 500").fg(theme::TEXT))
            Text(text: Line::from("t) Toggle  c) Cycle  e) Edit").fg(theme::DIM))
        }
    )
}
