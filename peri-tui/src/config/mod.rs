// Re-export config types from peri-acp (single source of truth)
// Re-export store functions from peri-acp

pub use peri_acp::provider::{
    AppConfig, PeriConfig, ProfileConfig, Profiles, ProviderConfig, ProviderModels,
};
pub use peri_acp::provider::{
    ConfigSource, config_path, load, load_from, save_to, set_global_config_path,
    workspace_config_path,
};
pub use peri_config::{ConfigurationRevision, ConfigurationSnapshot};

pub mod tui_config;
pub use tui_config::TuiConfig;

mod persistence;
pub use persistence::{
    SaveEffectiveError, configuration_revision, reload_effective, save_effective, save_effective_at,
};

#[cfg(test)]
#[path = "types_test.rs"]
mod tests;
