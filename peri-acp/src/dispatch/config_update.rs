//! Shared ConfigOptionUpdate construction for TUI/Stdio paths.
//!
//! Both the TUI notify layer and the Stdio handler layer need to build
//! `ConfigOptionUpdate` values from the same trio of `(PeriConfig, LlmProvider, PermissionMode)`.
//! This module centralises that construction to avoid duplication.

use agent_client_protocol::schema::v1::{ConfigOptionUpdate, SessionConfigOption};
use peri_acp_types::permission::PermissionMode;

use crate::provider::{LlmProvider, PeriConfig};
use crate::session::state_builders::build_config_options;

/// Build config options list from current config state.
pub fn make_config_options(
    peri_config: &PeriConfig,
    provider: &LlmProvider,
    permission_mode: PermissionMode,
) -> Vec<SessionConfigOption> {
    build_config_options(peri_config, provider, permission_mode)
}

/// Build a [`ConfigOptionUpdate`] from current config state.
pub fn make_config_option_update(
    peri_config: &PeriConfig,
    provider: &LlmProvider,
    permission_mode: PermissionMode,
) -> ConfigOptionUpdate {
    let config_options = make_config_options(peri_config, provider, permission_mode);
    ConfigOptionUpdate::new(config_options)
}
