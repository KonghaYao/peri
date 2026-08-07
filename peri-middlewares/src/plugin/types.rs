use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// 3.0 批 2 波 1：协议类型归契约层（定义见 `peri_acp_types::plugin`）。
// `McpServerEntry` / `PluginManifest` / `InstallScope` / `PluginOrigin` /
// `InstalledPlugin` 等自本文件迁出；本模块保留 re-export 保兼容。
pub use peri_acp_types::plugin::{
    InstallScope, InstalledPlugin, McpServerConfig, McpServerEntry, PluginAgent, PluginAuthor,
    PluginChannel, PluginCommand, PluginCommandEntry, PluginLspServer, PluginManifest,
    PluginOption, PluginOrigin,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplacePlugin {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// 插件来源：可以是字符串路径（"./plugins/foo"）或对象（{"source":"url","url":"..."}）
    pub source: serde_json::Value,
    #[serde(default)]
    pub version: String,
    pub sha: Option<String>,
    pub author: Option<PluginAuthor>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// 保留 marketplace.json 中未声明的字段（lspServers、mcpServers、strict 等）
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceManifest {
    pub name: String,
    pub plugins: Vec<MarketplacePlugin>,
    #[serde(rename = "allowCrossMarketplaceDependenciesOn")]
    pub allow_cross_marketplace: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source")]
pub enum MarketplaceSource {
    #[serde(rename = "github")]
    GitHub { repo: String },
    #[serde(rename = "git")]
    Git { url: String },
    #[serde(rename = "url")]
    Url { url: String },
    #[serde(rename = "file")]
    File { path: String },
    #[serde(rename = "directory")]
    Directory { path: String },
    #[serde(rename = "npm")]
    Npm { package: String },
}

/// 插件来源路径/机制
// （`PluginOrigin` 定义见 `peri_acp_types::plugin`，含 `is_external` impl）

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledPlugins {
    pub version: u32,
    #[serde(default, deserialize_with = "deserialize_installed_plugins")]
    pub plugins: Vec<InstalledPlugin>,
}

/// Claude Code 的 installed_plugins.json 中每个版本记录的格式
#[derive(Debug, Clone, Deserialize)]
struct ClaudeCodeVersionRecord {
    #[serde(default)]
    scope: String,
    #[serde(rename = "installPath")]
    install_path: String,
    version: String,
    #[serde(default, rename = "projectPath")]
    project_path: Option<String>,
}

/// 兼容 Claude Code 两种 installed_plugins 格式：
/// - Claude Code 对象格式: `{"plugin-id@marketplace": [{version record}]}`
/// - 内部数组格式: `[InstalledPlugin, ...]`
fn deserialize_installed_plugins<'de, D>(deserializer: D) -> Result<Vec<InstalledPlugin>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Object(map) => {
            let mut plugins = Vec::new();
            for (id, versions) in map {
                let version_arr = match versions {
                    serde_json::Value::Array(arr) => arr,
                    _ => continue,
                };
                let latest = match version_arr.first() {
                    Some(v) => v,
                    None => continue,
                };
                let record: ClaudeCodeVersionRecord = match serde_json::from_value(latest.clone()) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                let (name, marketplace) = match id.split_once('@') {
                    Some((n, m)) => (n.to_string(), m.to_string()),
                    None => (id.clone(), String::new()),
                };
                let scope = match record.scope.as_str() {
                    "project" => InstallScope::Project,
                    "local" => InstallScope::Local,
                    _ => InstallScope::User,
                };
                plugins.push(InstalledPlugin {
                    id,
                    name,
                    version: record.version,
                    marketplace,
                    install_path: PathBuf::from(&record.install_path),
                    scope,
                    project_path: record.project_path,
                    origin: PluginOrigin::ClaudeCodeInstalled,
                });
            }
            Ok(plugins)
        }
        serde_json::Value::Array(arr) => {
            serde_json::from_value(serde_json::Value::Array(arr)).map_err(serde::de::Error::custom)
        }
        _ => Ok(Vec::new()),
    }
}

impl Default for InstalledPlugins {
    fn default() -> Self {
        Self {
            version: 2,
            plugins: Vec::new(),
        }
    }
}

/// 已注册的 marketplace 配置条目
///
/// 与 Claude Code 的 KnownMarketplaceSchema 兼容：
/// - source: required - marketplace 来源
/// - installLocation: required - 本地缓存路径
/// - lastUpdated: required - ISO 8601 时间戳
/// - autoUpdate: optional - 是否自动更新
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownMarketplace {
    pub source: MarketplaceSource,
    #[serde(rename = "installLocation")]
    pub install_location: String,
    #[serde(rename = "autoUpdate", default)]
    pub auto_update: bool,
    #[serde(rename = "lastUpdated")]
    pub last_updated: String,
}

/// 声明格式的 marketplace（用于 settings.json 的 extraKnownMarketplaces）
///
/// 这是意图层（intent layer）的声明，只需要 source 字段。
/// 当 marketplace 实际安装后，会转换为 KnownMarketplace 并添加 installLocation 和 lastUpdated。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeclaredMarketplace {
    pub source: MarketplaceSource,
    #[serde(rename = "installLocation", default)]
    pub install_location: Option<String>,
    #[serde(rename = "autoUpdate", default)]
    pub auto_update: bool,
    #[serde(rename = "lastUpdated", default)]
    pub last_updated: Option<String>,
}

impl From<DeclaredMarketplace> for KnownMarketplace {
    fn from(declared: DeclaredMarketplace) -> Self {
        KnownMarketplace {
            source: declared.source,
            install_location: declared.install_location.unwrap_or_default(),
            auto_update: declared.auto_update,
            last_updated: declared.last_updated.unwrap_or_default(),
        }
    }
}

impl From<KnownMarketplace> for DeclaredMarketplace {
    fn from(known: KnownMarketplace) -> Self {
        DeclaredMarketplace {
            source: known.source,
            install_location: if known.install_location.is_empty() {
                None
            } else {
                Some(known.install_location)
            },
            auto_update: known.auto_update,
            last_updated: if known.last_updated.is_empty() {
                None
            } else {
                Some(known.last_updated)
            },
        }
    }
}

#[cfg(test)]
#[path = "types_test.rs"]
mod tests;
