mod history;
mod render;
pub mod text_area_state;
pub use self::text_area_state as state;
pub mod text_area;
pub use self::text_area as widget;
mod word;
pub use history::{History, Snapshot};
pub use render::{
    TextAreaRenderOptions, TextAreaStyles, VisualLine, WrapResult, display_width_before,
    render_multiline_with_cursor, wrap_text,
};
pub use state::{TextAreaState, YankText};
pub use widget::TextArea;
pub use word::{CharCategory, classify_char, next_word_boundary, prev_word_boundary};

#[cfg(test)]
#[path = "render_test.rs"]
mod render_tests;

#[cfg(test)]
#[path = "state_test.rs"]
mod tests;
