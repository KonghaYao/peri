// Re-export config types from peri-acp (single source of truth)
// Re-export store functions from peri-acp
pub use peri_acp::provider::{
    AppConfig, PeriConfig, ProfileConfig, Profiles, ProviderConfig, ProviderModels,
};
pub use peri_acp::provider::{
    config_path, load, load_from, save, save_to, set_global_config_path, workspace_config_path,
};

pub mod tui_config;
pub use tui_config::TuiConfig;

#[cfg(test)]
#[path = "types_test.rs"]
mod tests;
