//! ratatui-kit LoginPanel component.
//!
//! 登录面板支持 4 种模式：Browse / Edit / New / ConfirmDelete。

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
fn LoginPanel(_hooks: Hooks) -> impl Into<AnyElement<'static>> {
    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Login ").fg(theme::THINKING).bold().centered(),
            width: Constraint::Length(44),
            height: Constraint::Length(14),
        ) {
            Text(text: Line::from("Mode: Browse").fg(theme::SAGE).bold())
            Text(text: Line::from("[1] api.example.com").fg(theme::TEXT))
            Text(text: Line::from("[2] api.anthropic.com").fg(theme::TEXT))
            Text(text: Line::from("[3] localhost:8080").fg(theme::MUTED))
            Text(text: Line::from("e) Edit  n) New  d) Delete").fg(theme::DIM))
        }
    )
}
