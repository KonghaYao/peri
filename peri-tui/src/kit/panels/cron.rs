//! ratatui-kit CronPanel component.

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
fn CronPanel(_hooks: Hooks) -> impl Into<AnyElement<'static>> {
    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Cron ").fg(theme::THINKING).bold().centered(),
            width: Constraint::Length(40),
            height: Constraint::Length(12),
        ) {
            ScrollView(
                scroll_bars: ScrollBars::default(),
                width: Constraint::Length(38),
                height: Constraint::Length(10),
            ) {
                Text(text: Line::from("0 */6 * * *  summary").fg(theme::SAGE))
                Text(text: Line::from("0 9 * * 1  weekly report").fg(theme::TEXT))
                Text(text: Line::from("---").fg(theme::DIM))
                Text(text: Line::from("a) Add  e) Edit  d) Delete").fg(theme::DIM))
            }
        }
    )
}
