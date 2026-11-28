//! Single-line text field.
//!
//! The state, masking window and caret mapping live in the shared
//! `peri_tui_components::input` module; this module only keeps the dashboard's
//! import path stable. Rendering stays in [`crate::ui::widgets`].

pub use peri_tui_components::input::{FieldDisplay, TextField, MASK};

#[cfg(test)]
#[path = "text_field_test.rs"]
mod tests;
