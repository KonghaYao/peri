//! MCP over ACP（UNSTABLE）契约类型。
//!
//! client 在会话 setup（`session/new` / `load` / `resume` / `fork`）中以
//! `McpServer::Acp { name, serverId }` 声明由 ACP 通道承载的 MCP server；
//! agent 侧经 `mcp/connect` 建立连接，用 `mcp/message` 双向转发内层 MCP
//! JSON-RPC 消息，`mcp/disconnect` 收尾。
//!
//! 本模块只描述协议载荷与运行时标识，不依赖 rmcp 或具体 transport；连接
//! 建立、消息路由与池提交归 `peri-middlewares`。

use serde_json::{Map, Value};

/// client 声明的一个 acp 型 MCP server（绑定到具体会话）。
///
/// `server_id` 是 client 生成的不透明标识，`mcp/connect` 依它把连接路由回
/// 声明方；`name` 是人类可读名，用于工具命名与展示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpMcpServerSpec {
    pub session_id: String,
    pub name: String,
    pub server_id: String,
}

/// client → agent 的 `mcp/message` 载荷。
///
/// `params` 为内层 MCP 消息参数；内层请求的配对由 ACP 请求 id 承载，载荷
/// 本身不含 id（通知载荷无 ACP id，以 `mcp/message` 通知投递）。
#[derive(Debug, Clone, PartialEq)]
pub struct AcpMcpInbound {
    /// 该消息所属的 MCP-over-ACP 连接（`mcp/connect` 返回的
    /// `connectionId`）。
    pub connection_id: String,
    /// 内层 MCP 方法名。
    pub method: String,
    /// 内层 MCP 参数；缺省表示无参数。
    pub params: Option<Map<String, Value>>,
}

/// MCP over ACP 运行面错误（协议码 + 摘要，不含内层消息正文）。
///
/// 由实现方在连接失败、连接未知、内层 MCP 错误等场景返回；host 侧按其
/// `code` 映射为 ACP JSON-RPC 错误码。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpMcpError {
    pub code: i64,
    pub message: String,
}

impl AcpMcpError {
    /// 资源不存在（未知/已关闭的 connectionId、未声明的 serverId）。
    pub const CODE_NOT_FOUND: i64 = -32001;
    /// 服务不可用（会话已关闭、连接不可建立、池已关闭）。
    pub const CODE_UNAVAILABLE: i64 = -32002;

    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            code: Self::CODE_NOT_FOUND,
            message: message.into(),
        }
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            code: Self::CODE_UNAVAILABLE,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for AcpMcpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MCP over ACP error [{}]: {}", self.code, self.message)
    }
}

impl std::error::Error for AcpMcpError {}
