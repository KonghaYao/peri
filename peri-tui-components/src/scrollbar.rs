//! Scrollbar appearance is supplied by the caller; this module never reads theme atoms.
use ratatui::style::Style;
use ratatui::widgets::{Scrollbar, ScrollbarOrientation};

pub fn vertical_scrollbar(thumb: Style, arrow: Style) -> Scrollbar<'static> {
    Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .thumb_symbol(" ")
        .thumb_style(thumb)
        .track_symbol(None)
        .begin_symbol(Some("▲"))
        .begin_style(arrow)
        .end_symbol(Some("▼"))
        .end_style(arrow)
}
