use std::fmt;
use std::fmt::{Debug, Formatter};
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;
use thiserror::Error;

use crate::app::PeriConfig;
use crate::source::{ConfigurationSource, McpConfigurationSource};
use crate::{ConfigurationScope, ConfigurationSnapshot, ConfigurationSystem};

#[cfg(test)]
#[path = "settings_test.rs"]
mod tests;

#[cfg(test)]
#[path = "settings_revision_test.rs"]
mod revision_tests;

#[path = "settings/config_source.rs"]
mod config_source;
pub use config_source::config_path;
pub use config_source::load;
pub use config_source::load_from;
pub use config_source::save_to;
pub use config_source::set_global_config_path;
pub use config_source::workspace_config_path;
pub use config_source::ConfigSource;
pub use config_source::Result;
pub use config_source::SettingsError;
