//! 客户端句柄、状态与 scoped connection identity；由 client 保持原导出路径。

use crate::mcp::config::ConfigSource;
use peri_acp_types::dynamic_mcp::DynamicMcpInstanceKey;
use rmcp::{
    model::{Resource, Tool},
    service::{Peer, RoleClient},
};
use thiserror::Error;

/// MCP 客户端连接状态
#[derive(Debug, Clone, PartialEq)]
pub enum ClientStatus {
    Connected,
    Failed(String),
    Disconnected,
    Disabled,
    /// 配置存在但从未尝试连接（不在 clients 表中，仅在 configs 表中）
    Uninitialized,
}

/// MCP 连接池初始化状态
#[derive(Debug, Clone, PartialEq)]
pub enum McpInitStatus {
    Pending,
    Initializing { connected: usize, total: usize },
    Ready { total: usize },
    Failed(String),
}

/// MCP 服务器 OAuth 授权状态
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum OAuthStatus {
    /// 不使用 OAuth（stdio 传输或未配置 OAuth）
    #[default]
    None,
    /// 已授权（token 有效）
    Authorized,
    /// 需要授权（HTTP 传输且配置了 OAuth，但 token 缺失或过期）
    NeedsAuthorization,
}

/// 单个 MCP 服务器的详细信息（用于 TUI 面板展示）
#[derive(Debug, Clone)]
pub struct ServerInfo {
    pub name: String,
    pub version: Option<String>,
    pub cache_version: Option<String>,
    pub transport_type: String,
    pub status: ClientStatus,
    /// 供 UI 显示的稳定状态标签，不暴露 `ClientStatus::Failed` 的完整错误链。
    pub status_label: String,
    /// 供 UI 显示的一行安全错误摘要；完整诊断仅写入 tracing 日志。
    pub error_summary: Option<String>,
    /// 供 UI 显示最近一次持久化 cache 结果；None 表示尚无缓存请求。
    pub cache_status: Option<String>,
    pub tool_count: usize,
    pub resource_count: usize,
    /// OAuth 授权状态
    pub oauth_status: OAuthStatus,
    /// 配置来源
    pub source: Option<ConfigSource>,
    /// 服务器 URL（HTTP 传输）
    pub url: Option<String>,
    /// 插件来源标识（`"name@marketplace"`），非插件 server 为 None
    pub plugin_source: Option<String>,
}

/// 连接池级别错误
#[derive(Debug, Error)]
pub enum McpPoolError {
    #[error("MCP 服务器 \"{server}\" 连接失败: {reason}")]
    ConnectionFailed { server: String, reason: String },
    #[error("MCP 服务器 \"{server}\" 工具发现失败: {reason}")]
    ToolDiscoveryFailed { server: String, reason: String },
    #[error("MCP 服务器 \"{server}\" 未连接 (状态: {status:?})")]
    NotConnected {
        server: String,
        status: ClientStatus,
    },
}

/// 单个 MCP 服务器的客户端句柄
#[derive(Clone)]
pub struct McpClientHandle {
    pub name: String,
    pub version: Option<String>,
    pub cache_version: Option<String>,
    pub peer: Option<Peer<RoleClient>>,
    pub tools: Vec<Tool>,
    pub resources: Vec<Resource>,
    pub status: ClientStatus,
    pub oauth_status: OAuthStatus,
    /// 配置来源
    pub source: Option<ConfigSource>,
    /// 服务器 URL（HTTP 传输）
    pub url: Option<String>,
    /// Whether the MCP server declared experimental.claude/channel capability
    pub channel_capable: bool,
    /// Whether the MCP server declared the `io.modelcontextprotocol/skills`
    /// extension (SEP-2640)：true 时 skill 发现走 `skills/list` + digest 校验，
    /// false 时回退 legacy `skill://` resources 扫描兜底。
    pub skills_capable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum McpConnectionKey {
    Static { server_name: String },
    Dynamic { instance: DynamicMcpInstanceKey },
}

impl McpConnectionKey {
    pub(crate) fn static_server(server_name: impl Into<String>) -> Self {
        Self::Static {
            server_name: server_name.into(),
        }
    }

    pub(crate) fn dynamic(instance: DynamicMcpInstanceKey) -> Self {
        Self::Dynamic { instance }
    }

    pub(crate) fn server_name(&self) -> &str {
        match self {
            Self::Static { server_name } => server_name,
            Self::Dynamic { instance } => &instance.logical.server_name,
        }
    }

    pub(crate) fn is_dynamic(&self) -> bool {
        matches!(self, Self::Dynamic { .. })
    }
}
