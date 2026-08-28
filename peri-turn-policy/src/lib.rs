#![no_std]

mod compact;
mod content;

pub use compact::{select_compact_action, CompactAction};
pub use content::{is_message_content_empty, MessageContentShape};

#[cfg(test)]
#[path = "lib_test.rs"]
mod tests;
