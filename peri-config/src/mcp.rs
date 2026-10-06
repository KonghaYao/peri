use std::collections::{hash_map::DefaultHasher, BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};

use peri_acp_types::plugin::{ConfigSource, McpServerConfig};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use thiserror::Error;

#[cfg(test)]
#[path = "mcp_test.rs"]
mod tests;

pub const MCP_CACHE_ENV: &str = "PERI_MCP_CACHE";
pub const MCP_BUILTIN_ENV: &str = "PERI_MCP_BUILTIN";

pub fn builtin_enabled(environment: &BTreeMap<String, String>) -> bool {
    match environment
        .get(MCP_BUILTIN_ENV)
        .map(|value| value.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("off" | "0") => false,
        None => true,
        Some(_) => {
            tracing::warn!(
                env = MCP_BUILTIN_ENV,
                "unknown value; enabling builtin MCP servers"
            );
            true
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpCachePolicy {
    Enabled,
    Disabled,
}

impl McpCachePolicy {
    pub fn from_setting(setting: Option<bool>) -> Self {
        if setting == Some(false) {
            Self::Disabled
        } else {
            Self::Enabled
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct McpConfigFile {
    #[serde(default)]
    pub mcp_servers: HashMap<String, McpServerConfig>,
    #[serde(
        default,
        deserialize_with = "deserialize_cache_setting",
        skip_serializing_if = "Option::is_none"
    )]
    pub mcp_cache: Option<bool>,
}

fn deserialize_cache_setting<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: Deserializer<'de>,
{
    Value::deserialize(deserializer)?
        .as_bool()
        .map(Some)
        .ok_or_else(|| serde::de::Error::custom("mcpCache must be a boolean"))
}

#[derive(Debug, Error)]
pub enum McpConfigError {
    #[error("invalid MCP configuration: {0}")]
    InvalidConfig(#[source] serde_json::Error),
    #[error("MCP server configuration is invalid: {server_name}: {source}")]
    InvalidServer {
        server_name: String,
        #[source]
        source: peri_acp_types::plugin::McpServerConfigValidationError,
    },
    #[error("PERI_MCP_CACHE must be true/false, 1/0, or on/off")]
    InvalidCacheEnvironment,
}

pub fn parse_global(value: &Value) -> Result<McpConfigFile, McpConfigError> {
    let nested = value.get("config");
    let nested_servers = nested
        .and_then(|config| config.get("mcpServers"))
        .map(parse_servers)
        .transpose()?;
    let top_level_servers = value.get("mcpServers").map(parse_servers).transpose()?;
    let nested_cache = parse_cache_setting(nested)?;
    let top_level_cache = parse_cache_setting(Some(value))?;
    Ok(McpConfigFile {
        mcp_servers: nested_servers.or(top_level_servers).unwrap_or_default(),
        mcp_cache: nested_cache.or(top_level_cache),
    })
}

pub fn parse_project(value: &Value) -> Result<McpConfigFile, McpConfigError> {
    serde_json::from_value(value.clone()).map_err(McpConfigError::InvalidConfig)
}

pub fn parse_servers(value: &Value) -> Result<HashMap<String, McpServerConfig>, McpConfigError> {
    serde_json::from_value(value.clone()).map_err(McpConfigError::InvalidConfig)
}

fn parse_cache_setting(container: Option<&Value>) -> Result<Option<bool>, McpConfigError> {
    container
        .and_then(|value| value.get("mcpCache"))
        .map(|value| serde_json::from_value(value.clone()).map_err(McpConfigError::InvalidConfig))
        .transpose()
}

pub fn parse_environment(value: Option<&str>) -> Result<Option<bool>, McpConfigError> {
    match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        None => Ok(None),
        Some("true" | "1" | "on") => Ok(Some(true)),
        Some("false" | "0" | "off") => Ok(Some(false)),
        Some(_) => Err(McpConfigError::InvalidCacheEnvironment),
    }
}

pub fn merge_settings(settings: &[Option<bool>]) -> Option<bool> {
    if settings.contains(&Some(false)) {
        Some(false)
    } else if settings.contains(&Some(true)) {
        Some(true)
    } else {
        None
    }
}

pub fn server_config_hash(config: &McpServerConfig) -> u64 {
    let mut hasher = DefaultHasher::new();
    if let Some(command) = &config.command {
        command.hash(&mut hasher);
    }
    if let Some(args) = &config.args {
        args.hash(&mut hasher);
    }
    if let Some(environment) = &config.env {
        let mut entries: Vec<_> = environment.iter().collect();
        entries.sort_by_key(|(key, _)| *key);
        for (key, value) in entries {
            key.hash(&mut hasher);
            value.hash(&mut hasher);
        }
    }
    if let Some(system_mcp) = &config.system_mcp {
        system_mcp.hash(&mut hasher);
    }
    if let Some(system_mcp_tools) = &config.system_mcp_tools {
        system_mcp_tools.hash(&mut hasher);
    }
    if let Some(system_mcp_timeout) = &config.system_mcp_timeout {
        system_mcp_timeout.hash(&mut hasher);
    }
    hasher.finish()
}

pub fn plugin_server_name(plugin_name: &str, server_name: &str) -> String {
    format!("plugin:{plugin_name}:{server_name}")
}

pub fn resolve(
    raw_global: &Value,
    raw_project: &Value,
    plugins: &HashMap<String, McpServerConfig>,
    environment: &BTreeMap<String, String>,
) -> Result<McpConfigFile, McpConfigError> {
    let global = parse_global(raw_global)?;
    let project = parse_project(raw_project)?;
    resolve_from_files(&global, &project, plugins, environment)
}

pub fn resolve_from_files(
    global: &McpConfigFile,
    project: &McpConfigFile,
    plugins: &HashMap<String, McpServerConfig>,
    environment: &BTreeMap<String, String>,
) -> Result<McpConfigFile, McpConfigError> {
    validate_config(global)?;
    validate_config(project)?;
    let environment_cache = parse_environment(environment.get(MCP_CACHE_ENV).map(String::as_str))?;
    validate_config(global)?;
    validate_config(project)?;

    let manual_hashes: HashSet<u64> = global
        .mcp_servers
        .values()
        .chain(project.mcp_servers.values())
        .map(server_config_hash)
        .collect();
    let mut servers = global.mcp_servers.clone();
    for (name, config) in plugins {
        let mut config = config.clone();
        config.source = Some(ConfigSource::Plugin);
        config
            .validate()
            .map_err(|source| McpConfigError::InvalidServer {
                server_name: name.clone(),
                source,
            })?;
        if config.system_mcp != Some(true) && manual_hashes.contains(&server_config_hash(&config)) {
            tracing::debug!("插件 MCP 服务器与手动配置内容相同（hash 去重），已跳过");
            continue;
        }
        servers.insert(name.clone(), config);
    }
    servers.extend(project.mcp_servers.clone());
    validate_servers(&servers)?;

    let cache_setting = merge_settings(&[global.mcp_cache, project.mcp_cache, environment_cache]);
    Ok(McpConfigFile {
        mcp_servers: servers,
        mcp_cache: cache_setting,
    })
}

pub fn validate_config(config: &McpConfigFile) -> Result<(), McpConfigError> {
    validate_servers(&config.mcp_servers)
}

fn validate_servers(servers: &HashMap<String, McpServerConfig>) -> Result<(), McpConfigError> {
    let mut names: Vec<_> = servers.keys().collect();
    names.sort();
    for name in names {
        servers[name]
            .validate()
            .map_err(|source| McpConfigError::InvalidServer {
                server_name: name.clone(),
                source,
            })?;
    }
    Ok(())
}
