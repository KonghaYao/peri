use std::path::{Path, PathBuf};

use thiserror::Error;

#[cfg(test)]
use crate::plugin::config::{load_installed_plugins, save_installed_plugins};
use crate::plugin::marketplace::read_manifest_from_path;
use crate::plugin::types::InstallScope;
#[cfg(test)]
use crate::plugin::types::{InstalledPlugin, InstalledPlugins};
use crate::plugin::PluginConfigError;

mod install;
mod uninstall;

pub use install::{install_plugin, update_plugin};
pub use uninstall::{check_updates, cleanup_orphaned_plugins, uninstall_plugin};

#[cfg(test)]
#[path = "scope_test.rs"]
mod scope_tests;

#[cfg(test)]
#[path = "identity_test.rs"]
mod identity_tests;

#[cfg(test)]
#[path = "installer_test.rs"]
mod tests;

#[cfg(test)]
#[path = "install_path_test.rs"]
mod install_path_tests;

#[expect(
    clippy::module_inception,
    reason = "The style guide keeps the facade declarative while the implementation leaf groups related plugin installation operations."
)]
mod installer;
pub(crate) use installer::atomic_write_settings;
pub(crate) use installer::copy_dir_recursive;
use installer::enabled_plugins_settings_path;
pub use installer::find_plugin_in_marketplaces;
pub(crate) use installer::generate_synthetic_manifest;
pub(crate) use installer::get_marketplace_manifest;
use installer::installation_for_mutation;
pub(crate) use installer::match_project_path;
pub use installer::remove_from_enabled_plugins;
pub(crate) use installer::sanitize_plugin_id;
pub use installer::update_enabled_plugins;
pub use installer::InstallerError;
pub use installer::PluginUpdateInfo;
