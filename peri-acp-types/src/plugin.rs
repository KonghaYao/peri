//! 插件契约（manifest / 加载结果 / 安装范围 / 管理端口）。
//!
//! 自 `peri-middlewares`（`plugin/types.rs` / `plugin/loader.rs` / `mcp/config.rs`）
//! 迁入（3.0 批 2 波 1：协议类型归契约层；middlewares 保留 re-export 保兼容）。
//! 加载/安装/卸载逻辑留在 middlewares；ACP 协议面经 [`PluginManagerPort`]
//! 装配注入访问（波 2）。

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::hooks::{HooksConfig, RegisteredHook};
use crate::lsp::LspServerConfig;
use crate::skills::SkillRoot;

// ─── MCP 服务器配置（mcp/config.rs 迁入）────────────────────

/// MCP 服务器配置来源
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    /// 项目级配置（{cwd}/.mcp.json）
    Project(PathBuf),
    /// 全局配置（~/.peri/settings.json）
    Global(PathBuf),
    /// 插件配置
    Plugin,
}

/// 单个 MCP 服务器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// stdio 传输的可执行命令（如 "npx"）
    pub command: Option<String>,
    /// stdio 传输的命令参数
    #[serde(default)]
    pub args: Option<Vec<String>>,
    /// 传递给子进程的环境变量
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
    /// Streamable HTTP 传输的 URL
    pub url: Option<String>,
    /// HTTP 请求的自定义头
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
    /// OAuth 2.0 配置
    #[serde(default)]
    pub oauth: Option<OAuthConfig>,
    /// 是否禁用（默认 false，不序列化默认值以保持配置简洁）
    #[serde(default, skip_serializing_if = "is_false")]
    pub disabled: Option<bool>,
    /// 配置来源（运行时标记，不序列化）
    #[serde(skip)]
    pub source: Option<ConfigSource>,
}

fn is_false(v: &Option<bool>) -> bool {
    !v.unwrap_or(false)
}

/// MCP 服务器 OAuth 2.0 配置
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OAuthConfig {
    /// 是否启用 OAuth（默认 true）
    #[serde(default)]
    pub enabled: Option<bool>,
    /// OAuth 客户端 ID
    #[serde(default)]
    pub client_id: Option<String>,
    /// OAuth 客户端密钥（支持 ${VAR} 环境变量展开）
    #[serde(default)]
    pub client_secret: Option<String>,
    /// OAuth 权限范围列表
    #[serde(default)]
    pub scopes: Option<Vec<String>>,
}

impl OAuthConfig {
    /// 判断 OAuth 是否启用，默认 true
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
}

// ─── plugin.json 数据结构（plugin/types.rs 迁入）──────────────

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

/// 插件安装/启用范围
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

// ─── 命令条目 / 加载结果（plugin/loader.rs 迁入）──────────────

#[derive(Debug, Clone)]
pub enum CommandSource {
    Builtin,
    Plugin { path: PathBuf },
}

#[derive(Debug, Clone)]
pub struct CommandEntry {
    pub name: String,
    pub description: String,
    pub source: CommandSource,
}

pub trait CommandProvider: Send + Sync {
    fn commands(&self) -> Vec<CommandEntry>;
}

#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    pub name: String,
    pub version: String,
    pub install_path: PathBuf,
    pub manifest: PluginManifest,
    pub commands: Vec<CommandEntry>,
    pub skills_roots: Vec<SkillRoot>,
    pub agents_dirs: Vec<PathBuf>,
    pub mcp_servers: HashMap<String, McpServerConfig>,
    /// 插件数据目录（install_path/.claude-plugin/data），供 ${CLAUDE_PLUGIN_DATA} 展开
    pub data_path: PathBuf,
    /// 插件 hooks 配置（从 hooks/hooks.json 或 plugin.json hooks 字段提取）
    pub hooks_config: Option<HooksConfig>,
    /// 插件来源 marketplace（如 "claude-plugins-official"），用于追踪插件来源
    pub marketplace: String,
}

/// 插件聚合加载结果（`load_enabled_plugins_aggregated` 返回值）。
#[derive(Debug, Clone)]
pub struct PluginLoadResult {
    pub plugins: Vec<LoadedPlugin>,
    pub all_skill_roots: Vec<SkillRoot>,
    pub all_mcp_servers: HashMap<String, McpServerConfig>,
    pub all_agent_dirs: Vec<PathBuf>,
    pub all_commands: Vec<CommandEntry>,
    pub all_hooks: Vec<RegisteredHook>,
    /// 聚合所有插件的 LSP 服务器配置
    pub all_lsp_servers: Vec<LspServerConfig>,
}

// ─── 插件管理端口（波 2 装配注入）────────────────────────────

/// 插件管理端口：ACP 协议面（plugin/install 等命令）经此访问插件管理能力。
///
/// 装配点构造具体实现（`peri-middlewares` 的 `PluginManager`）后注入；
/// 端口错误以 `String` 呈现（错误文本直接回协议错误信息）。
#[async_trait]
pub trait PluginManagerPort: Send + Sync {
    /// 安装插件（marketplace 名 + 插件名），返回安装记录。
    async fn install(
        &self,
        name: &str,
        marketplace: &str,
        scope: InstallScope,
        cache_dir: &Path,
        claude_dir: &Path,
    ) -> Result<InstalledPlugin, String>;

    /// 卸载插件。
    async fn uninstall(&self, plugin_id: &str, claude_dir: &Path) -> Result<(), String>;

    /// 启用/禁用插件（写 enabledPlugins 配置）。
    fn set_enabled(
        &self,
        plugin_id: &str,
        scope: InstallScope,
        claude_dir: &Path,
        enable: bool,
    ) -> Result<(), String>;

    /// marketplace 缓存目录。
    fn cache_dir(&self) -> PathBuf;

    /// 更新已安装插件。
    async fn update(
        &self,
        plugin_id: &str,
        cache_dir: &Path,
        claude_dir: &Path,
    ) -> Result<InstalledPlugin, String>;

    /// 刷新 marketplace（按名称定位 known_marketplaces 条目），返回插件数量。
    async fn refresh_marketplace(&self, name: &str) -> Result<usize, String>;

    /// 清理孤儿插件文件（`plugin/cleanup` 命令面；返回清理数量）。
    async fn cleanup(&self, claude_dir: &Path) -> Result<usize, String>;

    /// 注册 marketplace（解析 source → 加载/去重 known_marketplaces →
    /// clone/fetch），返回 marketplace 显示名。
    async fn marketplace_add(&self, source: &str) -> Result<String, String>;

    /// 移除 marketplace（按名称），并清除其磁盘缓存目录。
    async fn marketplace_remove(&self, name: &str) -> Result<(), String>;

    /// 更新 marketplace（按名称 refresh + 记录 install_location/last_updated），
    /// 返回 marketplace 显示名。
    async fn marketplace_update(&self, name: &str) -> Result<String, String>;

    /// Marketplace 面板数据快照（`marketplace/list` 命令面数据源）：
    /// `{"marketplaces": [...], "discover": [...]}`。派生逻辑（known
    /// marketplaces × 缓存 manifest × installed 记录 → 状态/计数）与迁移前
    /// TUI 面板 `load_marketplace_data` / `load_discover_plugins_from_disk`
    /// 一致（JSON 透传，契约层不引入面板类型）。
    fn marketplace_snapshot(&self) -> serde_json::Value;

    /// 聚合快照：已启用插件 × 已安装记录 → 协议快照条目（plugin-snapshot 事件）。
    fn snapshot(&self, claude_dir: &Path) -> Vec<crate::event_data::PluginSnapshotEntry>;
}
