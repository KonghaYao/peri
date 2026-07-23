use std::{collections::HashMap, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{hooks::types::HooksConfig, mcp::McpServerConfig};

/// plugin.json 中 mcpServers 字段的值：内联配置对象或文件路径引用
#[derive(Debug, Clone)]
pub enum McpServerEntry {
    /// 内联 MCP 服务器配置
    Config(Box<McpServerConfig>),
    /// .mcp.json 文件路径（相对于插件根目录）
    FilePath(String),
}

impl Serialize for McpServerEntry {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            McpServerEntry::Config(cfg) => cfg.serialize(serializer),
            McpServerEntry::FilePath(path) => serializer.serialize_str(path),
        }
    }
}

impl McpServerEntry {
    /// 如果是内联配置，返回内部 McpServerConfig 的引用
    pub fn as_config(&self) -> Option<&McpServerConfig> {
        match self {
            McpServerEntry::Config(cfg) => Some(cfg),
            McpServerEntry::FilePath(_) => None,
        }
    }
}

impl<'de> Deserialize<'de> for McpServerEntry {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        if let Some(s) = value.as_str() {
            return Ok(McpServerEntry::FilePath(s.to_string()));
        }
        let config: McpServerConfig =
            serde_json::from_value(value).map_err(serde::de::Error::custom)?;
        Ok(McpServerEntry::Config(Box::new(config)))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginAuthor {
    pub name: String,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginCommand {
    pub path: String,
    pub name: Option<String>,
    pub description: Option<String>,
}

/// plugin.json 中 commands 字段的元素：字符串路径或完整 PluginCommand 对象
#[derive(Debug, Clone)]
pub enum PluginCommandEntry {
    /// 字符串路径（目录或文件路径）
    Path(String),
    /// 完整 PluginCommand 对象
    Full(PluginCommand),
}

impl Serialize for PluginCommandEntry {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            PluginCommandEntry::Path(path) => serializer.serialize_str(path),
            PluginCommandEntry::Full(cmd) => cmd.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for PluginCommandEntry {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        if let Some(s) = value.as_str() {
            return Ok(PluginCommandEntry::Path(s.to_string()));
        }
        let cmd: PluginCommand = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
        Ok(PluginCommandEntry::Full(cmd))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginAgent {
    pub path: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginLspServer {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// 文件扩展名到语言 ID 的映射（如 {".rs": "rust"}）
    #[serde(default, rename = "extensionToLanguage")]
    pub extension_to_language: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginChannel {
    pub name: String,
    #[serde(rename = "mcpServer")]
    pub mcp_server: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginOption {
    pub name: String,
    pub description: String,
    #[serde(rename = "type")]
    pub option_type: String,
    pub default: Option<serde_json::Value>,
}

fn deserialize_string_or_vec<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(s) => Ok(Some(vec![s])),
        serde_json::Value::Array(arr) => {
            let strings: Result<Vec<String>, _> = arr
                .into_iter()
                .map(|v| match v {
                    serde_json::Value::String(s) => Ok(s),
                    _ => Err(serde::de::Error::custom("skills element must be string")),
                })
                .collect();
            Ok(Some(strings?))
        }
        _ => Err(serde::de::Error::custom(
            "skills field must be string or array",
        )),
    }
}

/// 兼容 Claude Code 的插件清单
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    pub author: Option<PluginAuthor>,
    pub commands: Option<Vec<PluginCommandEntry>>,
    pub agents: Option<Vec<PluginAgent>>,
    #[serde(default, deserialize_with = "deserialize_string_or_vec")]
    pub skills: Option<Vec<String>>,
    /// 插件 hooks 配置
    pub hooks: Option<HooksConfig>,
    #[serde(rename = "mcpServers")]
    pub mcp_servers: Option<HashMap<String, McpServerEntry>>,
    #[serde(rename = "lspServers")]
    pub lsp_servers: Option<Vec<PluginLspServer>>,
    #[serde(rename = "outputStyles")]
    pub output_styles: Option<Vec<String>>,
    pub channels: Option<Vec<PluginChannel>>,
    pub options: Option<Vec<PluginOption>>,
    pub settings: Option<serde_json::Value>,
    /// 保留 plugin.json 中未声明的字段，确保前向兼容（read→write roundtrip 不丢字段）。
    /// 参考：MarketplacePlugin.extra（同一文件 line 192-193）。
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum InstallScope {
    #[default]
    User,
    Project,
    Local,
}

/// 插件来源路径/机制
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum PluginOrigin {
    /// Peri 从 marketplace 安装的（默认）
    #[default]
    PeriInstalled,
    /// Claude Code CLI 原生安装的（通过 migration backfill 发现或直接读取）
    #[serde(rename = "claude-installed")]
    ClaudeCodeInstalled,
    /// 用户级 ~/.claude/plugins/（CLI 安装）
    #[serde(rename = "claude-user")]
    UserClaude,
    /// 项目级 <project>/.claude/plugins/（CLI 安装）
    #[serde(rename = "claude-project")]
    ProjectClaude,
}

impl PluginOrigin {
    /// 是否由外部工具（Claude Code）安装，非 Peri 管理
    pub fn is_external(&self) -> bool {
        matches!(
            self,
            Self::ClaudeCodeInstalled | Self::UserClaude | Self::ProjectClaude
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledPlugin {
    pub id: String,
    pub name: String,
    pub version: String,
    pub marketplace: String,
    pub install_path: PathBuf,
    #[serde(default)]
    pub scope: InstallScope,
    /// 项目路径 (仅用于 project/local scope)
    #[serde(default, rename = "projectPath")]
    pub project_path: Option<String>,
    /// 插件来源（Peri 安装 vs Claude Code CLI 安装）
    #[serde(default)]
    pub origin: PluginOrigin,
}

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
