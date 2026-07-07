mod render;
pub mod state;
pub mod widget;
pub use render::{display_width_before, render_multiline_with_cursor};
pub use state::TextAreaState;
pub use widget::TextArea;
#[cfg(test)]
#[path = "state_test.rs"]
mod tests;
