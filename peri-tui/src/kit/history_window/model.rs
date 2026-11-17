//! History window value types and model implementation.

mod history_window;
mod types;

pub use history_window::HistoryWindow;
pub use types::*;

pub(crate) use super::source::{HistorySource, PersistentEntryKey, PersistentSubItemKey};
