//! User-facing terminal UI modules.
//!
//! All English user-facing strings live in [`text`] so a future locale layer
//! can replace them in one place.

pub mod dialogs;
pub mod empty;
pub mod help;
pub mod hit;
pub mod mouse;
pub mod onboarding;
pub mod pages;
pub mod screen;
pub mod session_detail;
pub mod session_form;
pub mod session_log;
pub mod settings;
pub mod shell;
pub mod text;
pub mod widgets;

pub use screen::render;
