#[cfg(test)]
pub use peri_config::mcp::merge_settings;

use super::McpConfigError;

#[cfg(test)]
pub(super) fn parse_environment(value: Option<&str>) -> Result<Option<bool>, McpConfigError> {
    peri_config::mcp::parse_environment(value).map_err(|error| match error {
        peri_config::mcp::McpConfigError::InvalidCacheEnvironment => {
            McpConfigError::InvalidCacheEnvironment
        }
        _ => unreachable!(),
    })
}
