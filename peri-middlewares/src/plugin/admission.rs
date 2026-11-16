//! 插件来源准入（M6）：关闭位在读取任何插件来源之前判定，是唯一入口。
//!
//! 由 `loader.rs` 迁出（STD-SIZE-001）；`plugin/mod.rs` 保留 re-export。

use std::collections::HashMap;
use std::path::Path;

use super::loader;
use super::loader::{LoadedPlugin, LoaderError, PluginLoadResult};

#[path = "admission/plugin_source_admission.rs"]
mod plugin_source_admission;
pub use plugin_source_admission::PluginSourceAdmission;
