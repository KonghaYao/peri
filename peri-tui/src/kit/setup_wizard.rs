//! ratatui-kit SetupWizard component.

use crate::kit::theme;
use ratatui_kit::{
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Style, Stylize},
        text::Line,
        widgets::Paragraph,
    },
};

#[component]
pub fn SetupWizard(_hooks: Hooks) -> impl Into<AnyElement<'static>> {
    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Setup Wizard ").fg(theme::THINKING).bold().centered(),
            width: Constraint::Length(60),
            height: Constraint::Length(15),
        ) {
            Text(text: Paragraph::new(Line::from("Step 1/5: TODO").fg(theme::TEXT).centered()))
        }
    )
}
