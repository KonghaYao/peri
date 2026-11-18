//! Deterministic cell-level fixture: run twice to compare component extraction.
use peri_tui_components::textarea::{TextArea, TextAreaState};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    widgets::StatefulWidgetRef,
};

fn main() {
    for (width, height) in [(80, 24), (120, 40), (20, 6), (1, 1)] {
        for text in [
            "",
            "hello 世界\nsecond line",
            "0123456789".repeat(25).as_str(),
        ] {
            for focused in [false, true] {
                let mut state = TextAreaState::default();
                state.insert_str(text);
                state.set_placeholder("Type a message");
                let area = Rect::new(0, 0, width, height);
                let mut buffer = Buffer::empty(area);
                TextArea::new(
                    Style::default().bg(Color::Blue),
                    Style::default().bg(Color::DarkGray),
                )
                .show_cursor(focused)
                .render_ref(area, &mut buffer, &mut state);
                println!("{width}x{height} focus={focused} input={text:?}\n{buffer:?}");
            }
        }
    }
}
