use serde::{Deserialize, Deserializer};

use super::McpConfigError;

pub const MCP_CACHE_ENV: &str = "PERI_MCP_CACHE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpCachePolicy {
    Enabled,
    Disabled,
}

impl McpCachePolicy {
    pub(crate) fn from_setting(setting: Option<bool>) -> Self {
        if setting == Some(false) {
            Self::Disabled
        } else {
            Self::Enabled
        }
    }
}

pub(super) fn deserialize_cache_setting<'de, Input>(
    deserializer: Input,
) -> Result<Option<bool>, Input::Error>
where
    Input: Deserializer<'de>,
{
    bool::deserialize(deserializer).map(Some)
}

pub(super) fn parse_environment(value: Option<&str>) -> Result<Option<bool>, McpConfigError> {
    match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        None => Ok(None),
        Some("true" | "1" | "on") => Ok(Some(true)),
        Some("false" | "0" | "off") => Ok(Some(false)),
        Some(_) => Err(McpConfigError::InvalidCacheEnvironment),
    }
}

pub(super) fn load_environment() -> Result<Option<bool>, McpConfigError> {
    let value = peri_mcp_config::read_environment(MCP_CACHE_ENV)
        .map_err(|source| McpConfigError::CacheEnvironmentRead { source })?;
    parse_environment(value.as_deref())
}

pub(super) fn merge_settings(settings: &[Option<bool>]) -> Option<bool> {
    if settings.contains(&Some(false)) {
        Some(false)
    } else if settings.contains(&Some(true)) {
        Some(true)
    } else {
        None
    }
}
