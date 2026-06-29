//! MCP DTOs -- 取代 peri_middlewares::mcp::{ClientStatus, ConfigSource, ...}
//!
//! 注意：OAuthFlowEvent 包含 oneshot::Sender，无法序列化为 DTO。
//! 本模块仅提供渲染用 DTO，OAuthFlowEvent 回调仍在 acp_server/ 边界处理。

use serde::{Deserialize, Serialize};

/// MCP 服务器连接状态（对齐 peri_middlewares::mcp::ClientStatus）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ClientStatusDto {
    Connected,
    Failed {
        reason: String,
    },
    Disconnected,
    Disabled,
    /// 配置存在但从未尝试连接
    Uninitialized,
}

/// MCP 配置来源（对齐 peri_middlewares::mcp::ConfigSource）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ConfigSourceDto {
    Project { path: String },
    Global { path: String },
    Plugin,
}

/// MCP OAuth 授权状态（对齐 peri_middlewares::mcp::OAuthStatus）
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum OAuthStatusDto {
    #[default]
    None,
    Authorized,
    NeedsAuthorization,
}

/// MCP 服务器详细信息（对齐 peri_middlewares::mcp::ServerInfo）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerInfoDto {
    pub name: String,
    pub transport_type: String,
    pub status: ClientStatusDto,
    pub tool_count: usize,
    pub resource_count: usize,
    pub oauth_status: OAuthStatusDto,
    pub source: Option<ConfigSourceDto>,
    pub server_url: Option<String>,
}

/// MCP 初始化进度（对齐 peri_middlewares::mcp::McpInitStatus）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum McpInitStatusDto {
    Pending,
    Initializing { connected: usize, total: usize },
    Ready { total: usize },
    Failed(String),
}

/// OAuth 回调结果（对齐 peri_middlewares::mcp::OAuthCallbackResult）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OAuthCallbackResultDto {
    pub code: String,
    pub state: String,
}

/// OAuth 流程事件 DTO（仅用于 TUI 展示，不含 oneshot channel）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum OAuthFlowEventDto {
    AuthorizationNeeded {
        server_name: String,
        authorization_url: String,
    },
    AuthorizationCompleted {
        server_name: String,
    },
    AuthorizationFailed {
        server_name: String,
        error: String,
    },
}

/// MCP 工具 DTO（对齐 summary.rs 中的 McpToolDto）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpToolDto {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}
