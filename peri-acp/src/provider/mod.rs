//! LLM Provider and model configuration.
//!
//! Manages provider configuration, model alias resolution, and LLM factory creation.
//! Decoupled from TUI-specific types.

pub mod subscription_auth;
pub use subscription_auth as auth;
pub mod config;
pub mod store;
pub mod transport_route;

mod llm_provider;

pub use config::{AppConfig, PeriConfig, ProfileConfig, Profiles, ProviderConfig, ProviderModels};
pub use llm_provider::LlmProvider;
pub use store::{
    config_path, load, load_from, save_to, set_global_config_path, workspace_config_path,
    ConfigSource,
};
