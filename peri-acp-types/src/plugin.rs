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

/// 显式 MCP 协议版本。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum McpProtocolVersion {
    /// 使用 `server/discover` lifecycle 的 MCP 2026-07-28。
    #[serde(rename = "2026-07-28")]
    V2026_07_28,
}

/// 单个 MCP 服务器配置
///
/// `Deserialize` 由手写实现提供（见 [`McpServerConfigWire`]）：System 三个字段
/// 拒绝显式 `null`，非法组合在解析期即按 [`McpServerConfigValidationError`] 拒绝。
#[derive(Debug, Clone, Serialize)]
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
    /// 显式 MCP 协议版本。仅 `2026-07-28` 使用 `server/discover` lifecycle；
    /// 未配置使用官方 Auto 自动协商，未知版本会使配置解析失败。
    #[serde(
        default,
        rename = "protocolVersion",
        skip_serializing_if = "Option::is_none"
    )]
    pub protocol_version: Option<McpProtocolVersion>,
    /// subscriptions/listen 订阅配置（2026-07-28 协议；仅负责连接后建立订阅）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscriptions: Option<McpSubscriptionsConfig>,
    /// 启动依赖标识（System MCP）：react loop 启动前必须完成 transport / initialize /
    /// 能力协商。缺省 `None`，消费判定固定为 `== Some(true)`；`false`/`None` 是普通 MCP。
    /// 写回可省略 `false`/`None`。
    #[serde(
        default,
        rename = "system_mcp",
        alias = "systemMcp",
        skip_serializing_if = "is_false"
    )]
    pub system_mcp: Option<bool>,
    /// System MCP 必须提供的工具名数组（在所属 server 的原始工具名上精确匹配）：
    /// 只与 `system_mcp = true` 配合使用；显式 `[]` 表示只要求 ready、不注入额外工具。
    /// `None` 与 `Some([])` 必须保持可区分并可无损写回。
    #[serde(
        default,
        rename = "system_mcp_tools",
        alias = "systemMcpTools",
        skip_serializing_if = "Option::is_none"
    )]
    pub system_mcp_tools: Option<Vec<String>>,
    /// System MCP 启动等待超时（毫秒）；缺省
    /// [`McpServerConfig::DEFAULT_SYSTEM_MCP_TIMEOUT_MS`]，合法区间
    /// [`McpServerConfig::MIN_SYSTEM_MCP_TIMEOUT_MS`]`..=`[`McpServerConfig::MAX_SYSTEM_MCP_TIMEOUT_MS`]。
    /// 只与 `system_mcp = true` 配合使用。
    #[serde(
        default,
        rename = "system_mcp_timeout",
        alias = "systemMcpTimeout",
        skip_serializing_if = "Option::is_none"
    )]
    pub system_mcp_timeout: Option<u64>,
    /// 配置来源（运行时标记，不序列化）
    #[serde(skip)]
    pub source: Option<ConfigSource>,
}

/// `McpServerConfig` 的私有 wire helper。
///
/// 字段与 serde 属性同 [`McpServerConfig`]（`source` 仍不参与 wire），差别只在
/// System 三个字段使用 `deserialize_with`：字段缺失才走 `default`（`None`），
/// 显式 `null` / 类型不符一律解析失败，不得降级为“未配置”。
///
/// 保留 derive 的 duplicate key 检测：不得先转 `serde_json::Value`
/// （对象去重会丢失重复 key，两种拼法同时出现就无法报错）。
#[derive(Deserialize)]
struct McpServerConfigWire {
    command: Option<String>,
    #[serde(default)]
    args: Option<Vec<String>>,
    #[serde(default)]
    env: Option<HashMap<String, String>>,
    url: Option<String>,
    #[serde(default)]
    headers: Option<HashMap<String, String>>,
    #[serde(default)]
    oauth: Option<OAuthConfig>,
    #[serde(default)]
    disabled: Option<bool>,
    #[serde(default, rename = "protocolVersion")]
    protocol_version: Option<McpProtocolVersion>,
    #[serde(default)]
    subscriptions: Option<McpSubscriptionsConfig>,
    #[serde(
        default,
        rename = "system_mcp",
        alias = "systemMcp",
        deserialize_with = "deserialize_present_system_mcp"
    )]
    system_mcp: Option<bool>,
    #[serde(
        default,
        rename = "system_mcp_tools",
        alias = "systemMcpTools",
        deserialize_with = "deserialize_present_system_mcp_tools"
    )]
    system_mcp_tools: Option<Vec<String>>,
    #[serde(
        default,
        rename = "system_mcp_timeout",
        alias = "systemMcpTimeout",
        deserialize_with = "deserialize_present_system_mcp_timeout"
    )]
    system_mcp_timeout: Option<u64>,
}

/// `system_mcp` 的 wire 反序列化：显式 `null` 拒绝，缺失由 `default` 落到 `None`。
fn deserialize_present_system_mcp<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<bool>, D::Error> {
    bool::deserialize(deserializer).map(Some)
}

/// `system_mcp_tools` 的 wire 反序列化：显式 `null`、非数组、非字符串元素均拒绝。
fn deserialize_present_system_mcp_tools<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<String>>, D::Error> {
    Vec::<String>::deserialize(deserializer).map(Some)
}

/// `system_mcp_timeout` 的 wire 反序列化：显式 `null`、非整数均拒绝。
fn deserialize_present_system_mcp_timeout<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    u64::deserialize(deserializer).map(Some)
}

impl<'de> Deserialize<'de> for McpServerConfig {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = McpServerConfigWire::deserialize(deserializer)?;
        let config = McpServerConfig {
            command: wire.command,
            args: wire.args,
            env: wire.env,
            url: wire.url,
            headers: wire.headers,
            oauth: wire.oauth,
            disabled: wire.disabled,
            protocol_version: wire.protocol_version,
            subscriptions: wire.subscriptions,
            system_mcp: wire.system_mcp,
            system_mcp_tools: wire.system_mcp_tools,
            system_mcp_timeout: wire.system_mcp_timeout,
            source: None,
        };
        // 非法组合在解析期拒绝，错误正文为固定契约文本。
        config.validate().map_err(serde::de::Error::custom)?;
        Ok(config)
    }
}

impl McpServerConfig {
    /// `system_mcp_timeout` 缺省值（毫秒）：未配置时的有效启动等待超时。
    pub const DEFAULT_SYSTEM_MCP_TIMEOUT_MS: u64 = 30_000;
    /// `system_mcp_timeout` 合法区间下界（毫秒）。
    pub const MIN_SYSTEM_MCP_TIMEOUT_MS: u64 = 1;
    /// `system_mcp_timeout` 合法区间上界（毫秒，10 分钟）。
    pub const MAX_SYSTEM_MCP_TIMEOUT_MS: u64 = 600_000;

    /// 纯数据不变量校验：System key 与 `system_mcp = true` 的组合、timeout 区间。
    ///
    /// 无副作用、无 namespace / transport / I/O 依赖；`disabled = true` 也照常校验。
    /// 确定性优先级：先组合错误（`system_mcp_tools` 先于 `system_mcp_timeout`），
    /// 再 timeout 区间。
    pub fn validate(&self) -> Result<(), McpServerConfigValidationError> {
        if self.system_mcp != Some(true) {
            if self.system_mcp_tools.is_some() {
                return Err(McpServerConfigValidationError::SystemMcpToolsRequiresSystemMcp);
            }
            if self.system_mcp_timeout.is_some() {
                return Err(McpServerConfigValidationError::SystemMcpTimeoutRequiresSystemMcp);
            }
        }
        if let Some(timeout) = self.system_mcp_timeout {
            if !(Self::MIN_SYSTEM_MCP_TIMEOUT_MS..=Self::MAX_SYSTEM_MCP_TIMEOUT_MS)
                .contains(&timeout)
            {
                return Err(McpServerConfigValidationError::SystemMcpTimeoutOutOfRange);
            }
        }
        Ok(())
    }
}

/// MCP 服务器配置的纯校验错误（固定规则文本，不携带配置内容）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum McpServerConfigValidationError {
    /// 声明了 `system_mcp_tools` 却没有 `system_mcp = true`（含显式 `[]`）。
    #[error("system_mcp_tools requires system_mcp = true")]
    SystemMcpToolsRequiresSystemMcp,
    /// 声明了 `system_mcp_timeout` 却没有 `system_mcp = true`。
    #[error("system_mcp_timeout requires system_mcp = true")]
    SystemMcpTimeoutRequiresSystemMcp,
    /// `system_mcp_timeout` 超出合法区间。
    #[error("system_mcp_timeout must be within 1..=600000 milliseconds")]
    SystemMcpTimeoutOutOfRange,
}

/// `subscriptions/listen` 订阅配置（2026-07-28 协议）
///
/// 任一字段非空即启用订阅：连接后建立对应过滤器的
/// `subscriptions/listen` 长流；收到通知时
/// 唤醒 agent 会话（注入 `<system-reminder>` Defer 消息）。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct McpSubscriptionsConfig {
    /// 订阅的资源 URI 列表（内容变化时收到 `notifications/resources/updated`）
    #[serde(default)]
    pub resources: Vec<String>,
    /// 订阅工具列表变化（`notifications/tools/list_changed`）
    #[serde(default)]
    pub tools_list_changed: bool,
    /// 订阅 prompts 列表变化（`notifications/prompts/list_changed`）
    #[serde(default)]
    pub prompts_list_changed: bool,
    /// 订阅资源列表变化（`notifications/resources/list_changed`）
    #[serde(default)]
    pub resources_list_changed: bool,
}

impl McpSubscriptionsConfig {
    /// 是否为空配置（空配置视为未启用订阅）
    pub fn is_empty(&self) -> bool {
        self.resources.is_empty()
            && !self.tools_list_changed
            && !self.prompts_list_changed
            && !self.resources_list_changed
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 仅含缺省字段的 typed 配置（`McpServerConfig` 没有 `Default`）。
    fn empty_config() -> McpServerConfig {
        McpServerConfig {
            command: None,
            args: None,
            env: None,
            url: None,
            headers: None,
            oauth: None,
            disabled: None,
            protocol_version: None,
            subscriptions: None,
            system_mcp: None,
            system_mcp_tools: None,
            system_mcp_timeout: None,
            source: None,
        }
    }

    fn parse(json: &str) -> Result<McpServerConfig, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// 旧 JSON 兼容：新增 key 全部缺省为 None，输出不出现新 key，既有语义不变。
    #[test]
    fn test_system_mcp_legacy_defaults() {
        let cfg = parse(r#"{"command":"npx","protocolVersion":"2026-07-28"}"#)
            .expect("旧 JSON 必须仍可解析");
        assert!(cfg.system_mcp.is_none(), "旧 JSON 不得推断出启动依赖");
        assert!(cfg.system_mcp_tools.is_none());
        assert!(cfg.system_mcp_timeout.is_none());
        assert!(cfg.validate().is_ok(), "缺省 System 字段必须合法");
        assert_eq!(cfg.protocol_version, Some(McpProtocolVersion::V2026_07_28));
        assert!(cfg.source.is_none(), "source 是运行时标记，不从 wire 读取");

        let json = serde_json::to_value(&cfg).unwrap();
        for key in ["system_mcp", "system_mcp_tools", "system_mcp_timeout"] {
            assert!(json.get(key).is_none(), "缺省不得序列化 {key}: {json}");
        }
        assert_eq!(json["protocolVersion"], serde_json::json!("2026-07-28"));
        assert!(json.get("source").is_none(), "source 不进入 wire: {json}");

        let legacy = parse(r#"{"command":"npx","disabled":true,"args":["-y"]}"#).unwrap();
        assert_eq!(legacy.disabled, Some(true));
        assert_eq!(legacy.args, Some(vec!["-y".to_string()]));
    }

    /// 契约 1：`system_mcp_tools` 没有 `system_mcp = true` 一律非法，含显式 `[]`。
    #[test]
    fn test_system_mcp_tools_requires_true() {
        for json in [
            r#"{"command":"npx","system_mcp_tools":[]}"#,
            r#"{"command":"npx","system_mcp_tools":["Read"]}"#,
            r#"{"command":"npx","system_mcp":false,"system_mcp_tools":[]}"#,
            r#"{"command":"npx","system_mcp":false,"system_mcp_tools":["Read"]}"#,
            r#"{"command":"npx","systemMcpTools":["Read"]}"#,
        ] {
            let err = parse(json).expect_err("非法组合必须解析失败");
            assert!(
                err.to_string()
                    .contains("system_mcp_tools requires system_mcp = true"),
                "固定规则正文必须保留: {json} -> {err}"
            );
        }

        let typed = McpServerConfig {
            system_mcp: Some(false),
            system_mcp_tools: Some(vec![]),
            ..empty_config()
        };
        assert_eq!(
            typed.validate().unwrap_err(),
            McpServerConfigValidationError::SystemMcpToolsRequiresSystemMcp
        );
        let missing = McpServerConfig {
            system_mcp_tools: Some(vec!["Read".to_string()]),
            ..empty_config()
        };
        assert_eq!(
            missing.validate().unwrap_err(),
            McpServerConfigValidationError::SystemMcpToolsRequiresSystemMcp
        );

        for tools in ["[]", r#"["Read","Write"]"#] {
            let json =
                format!(r#"{{"command":"npx","system_mcp":true,"system_mcp_tools":{tools}}}"#);
            let cfg = parse(&json).expect("true + 工具数组必须合法");
            assert_eq!(cfg.system_mcp, Some(true));
            assert!(cfg.validate().is_ok());
        }
    }

    /// 契约 4 配置语义：`true + []` 与 `true + 缺省 tools` 必须保持可区分并无损写回。
    #[test]
    fn test_system_mcp_empty_tools_roundtrip() {
        let cfg = parse(r#"{"command":"npx","system_mcp":true,"system_mcp_tools":[]}"#).unwrap();
        assert_eq!(cfg.system_mcp, Some(true));
        assert_eq!(cfg.system_mcp_tools, Some(vec![]));

        let json = serde_json::to_value(&cfg).unwrap();
        assert_eq!(json["system_mcp"], serde_json::json!(true));
        assert_eq!(
            json["system_mcp_tools"],
            serde_json::json!([]),
            "Some([]) 不得被省略: {json}"
        );
        let back: McpServerConfig = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(back.system_mcp_tools, Some(vec![]));
        assert_eq!(serde_json::to_value(&back).unwrap(), json, "往返必须无损");

        let no_tools = parse(r#"{"command":"npx","system_mcp":true}"#).unwrap();
        assert!(
            no_tools.system_mcp_tools.is_none(),
            "未声明 tools 保持 None，不自动补空数组"
        );
        let no_tools_json = serde_json::to_value(&no_tools).unwrap();
        assert!(
            no_tools_json.get("system_mcp_tools").is_none(),
            "None 不得序列化: {no_tools_json}"
        );
    }

    /// snake_case 为 canonical；camelCase 别名输入等价；两种拼法同时出现报 duplicate field。
    #[test]
    fn test_system_mcp_key_aliases() {
        let snake = parse(
            r#"{"command":"npx","system_mcp":true,"system_mcp_tools":["Read"],"system_mcp_timeout":1500}"#,
        )
        .unwrap();
        let camel = parse(
            r#"{"command":"npx","systemMcp":true,"systemMcpTools":["Read"],"systemMcpTimeout":1500}"#,
        )
        .unwrap();
        assert_eq!(camel.system_mcp, snake.system_mcp);
        assert_eq!(camel.system_mcp_tools, snake.system_mcp_tools);
        assert_eq!(camel.system_mcp_timeout, snake.system_mcp_timeout);

        let json = serde_json::to_value(&camel).unwrap();
        assert_eq!(json["system_mcp"], serde_json::json!(true));
        assert_eq!(json["system_mcp_tools"], serde_json::json!(["Read"]));
        assert_eq!(json["system_mcp_timeout"], serde_json::json!(1500));
        let text = serde_json::to_string(&camel).unwrap();
        assert!(
            !text.contains("systemMcp"),
            "canonical 输出只允许 snake_case: {text}"
        );

        for json in [
            r#"{"system_mcp":true,"systemMcp":true}"#,
            r#"{"system_mcp":true,"system_mcp_tools":[],"systemMcpTools":[]}"#,
            r#"{"system_mcp":true,"system_mcp_timeout":1000,"systemMcpTimeout":1000}"#,
        ] {
            let err = parse(json).expect_err("两种拼法同时出现必须失败");
            assert!(
                err.to_string().contains("duplicate field"),
                "必须报 duplicate field: {json} -> {err}"
            );
        }
    }

    /// 新增 key 的显式 null 与错误类型一律拒绝，不得降级为 None / 空数组。
    #[test]
    fn test_system_mcp_rejects_null_and_wrong_types() {
        for json in [
            r#"{"command":"npx","system_mcp":null}"#,
            r#"{"command":"npx","system_mcp":"true"}"#,
            r#"{"command":"npx","system_mcp":1}"#,
            r#"{"command":"npx","system_mcp_tools":null}"#,
            r#"{"command":"npx","system_mcp":true,"system_mcp_tools":"Read"}"#,
            r#"{"command":"npx","system_mcp":true,"system_mcp_tools":[1]}"#,
            r#"{"command":"npx","system_mcp":true,"system_mcp_tools":[null]}"#,
            r#"{"command":"npx","system_mcp_timeout":null}"#,
            r#"{"command":"npx","system_mcp":true,"system_mcp_timeout":"30000"}"#,
            r#"{"command":"npx","system_mcp":true,"system_mcp_timeout":-1}"#,
        ] {
            assert!(parse(json).is_err(), "必须拒绝: {json}");
        }

        let err = parse(r#"{"system_mcp":true,"system_mcp_tools":null}"#).unwrap_err();
        assert!(
            err.to_string().contains("invalid type: null"),
            "显式 null 不得被当成未配置: {err}"
        );

        let ok = parse(
            r#"{"command":"npx","system_mcp":true,"system_mcp_tools":[],"system_mcp_timeout":30000}"#,
        )
        .expect("合法组合不得被误拒");
        assert!(ok.validate().is_ok());
    }

    /// 工具名数组逐项保真：不 trim / 不排序 / 不去重 / 不展开 `${...}` / 不加前缀。
    #[test]
    fn test_system_mcp_tools_preserve_exact_values() {
        let json = r#"{"command":"npx","system_mcp":true,"system_mcp_tools":["Read","read","READ","Read","","${VAR}","  spaced  ","mcp__other__Tool"]}"#;
        let cfg = parse(json).unwrap();
        let expected = serde_json::json!([
            "Read",
            "read",
            "READ",
            "Read",
            "",
            "${VAR}",
            "  spaced  ",
            "mcp__other__Tool"
        ]);
        assert_eq!(
            cfg.system_mcp_tools,
            Some(
                expected
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap().to_string())
                    .collect::<Vec<_>>()
            )
        );
        let back = serde_json::to_value(&cfg).unwrap();
        assert_eq!(back["system_mcp_tools"], expected, "值与顺序必须原样写回");
    }

    /// `system_mcp_timeout` 只与 `system_mcp = true` 配合，缺省为常量 30_000ms。
    #[test]
    fn test_system_mcp_timeout_requires_true() {
        for json in [
            r#"{"command":"npx","system_mcp_timeout":1000}"#,
            r#"{"command":"npx","system_mcp":false,"system_mcp_timeout":1000}"#,
            r#"{"command":"npx","systemMcpTimeout":1000}"#,
        ] {
            let err = parse(json).expect_err("无 system_mcp = true 时 timeout 必须非法");
            assert!(
                err.to_string()
                    .contains("system_mcp_timeout requires system_mcp = true"),
                "固定规则正文必须保留: {json} -> {err}"
            );
        }

        let typed = McpServerConfig {
            system_mcp: Some(false),
            system_mcp_timeout: Some(1000),
            ..empty_config()
        };
        assert_eq!(
            typed.validate().unwrap_err(),
            McpServerConfigValidationError::SystemMcpTimeoutRequiresSystemMcp
        );
        assert_eq!(McpServerConfig::DEFAULT_SYSTEM_MCP_TIMEOUT_MS, 30_000);
        assert_eq!(McpServerConfig::MIN_SYSTEM_MCP_TIMEOUT_MS, 1);
        assert_eq!(McpServerConfig::MAX_SYSTEM_MCP_TIMEOUT_MS, 600_000);
    }

    /// `system_mcp_timeout` 区间 1..=600_000 毫秒；区间内往返保真，缺省不写回。
    #[test]
    fn test_system_mcp_timeout_range_and_roundtrip() {
        for ms in [1u64, 30_000, 600_000] {
            let json = format!(r#"{{"system_mcp":true,"system_mcp_timeout":{ms}}}"#);
            let cfg = parse(&json).unwrap();
            assert_eq!(cfg.system_mcp_timeout, Some(ms));
            assert_eq!(
                serde_json::to_value(&cfg).unwrap()["system_mcp_timeout"],
                serde_json::json!(ms)
            );
        }

        for ms in [0u64, 600_001] {
            let json = format!(r#"{{"system_mcp":true,"system_mcp_timeout":{ms}}}"#);
            let err = parse(&json).expect_err("越界 timeout 必须解析失败");
            assert!(
                err.to_string()
                    .contains("system_mcp_timeout must be within 1..=600000 milliseconds"),
                "固定规则正文必须保留: {err}"
            );
            let typed = McpServerConfig {
                system_mcp: Some(true),
                system_mcp_timeout: Some(ms),
                ..empty_config()
            };
            assert_eq!(
                typed.validate().unwrap_err(),
                McpServerConfigValidationError::SystemMcpTimeoutOutOfRange
            );
        }

        let none = parse(r#"{"system_mcp":true}"#).unwrap();
        assert!(none.system_mcp_timeout.is_none());
        assert_eq!(
            none.system_mcp_timeout
                .unwrap_or(McpServerConfig::DEFAULT_SYSTEM_MCP_TIMEOUT_MS),
            30_000,
            "缺省有效值为 30_000ms"
        );
        assert!(serde_json::to_value(&none)
            .unwrap()
            .get("system_mcp_timeout")
            .is_none());
    }
}
