#[cfg(any(test, feature = "headless"))]
pub mod headless;
pub mod main_ui;
pub mod markdown;
pub mod message_render;
pub mod message_view;
// pub mod render_thread; // P5: deleted, sync rendering from state machine
pub mod theme;
pub mod tips;
pub mod welcome;
