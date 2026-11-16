use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[cfg(test)]
#[path = "ui_test.rs"]
mod tests;

#[path = "ui/tui_config.rs"]
mod tui_config;
pub use tui_config::TuiConfig;
