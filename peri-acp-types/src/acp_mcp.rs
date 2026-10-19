//! MCP over ACP（UNSTABLE）契约类型。
//!
//! client 在会话 setup（`session/new` / `load` / `resume` / `fork`）中以
//! `McpServer::Acp { name, serverId }` 声明由 ACP 通道承载的 MCP server；
//! agent 侧不再建立长连接，而是把每次内层 MCP 调用落成一条**独立的**
//! `mcp/message` 请求（`serverId` + `requestId` + `method` + `params`）。
//! schema 1.10 起 `mcp/connect` / `mcp/disconnect` 已移除，连接生命周期归
//! 声明所在会话所有。
//!
//! 本模块只描述协议载荷与运行时标识，不依赖 rmcp 或具体 transport；连接
//! 建立、消息路由与池提交归 `peri-middlewares`。

use std::collections::HashMap;

use parking_lot::Mutex;
use serde_json::{Map, Value};

/// ACP 连接级声明归属；由连接创建并在该连接的会话服务之间共享。
///
/// 不共享 server：通知没有 sessionId，跨会话重名无法无歧义分流，故以
/// CODE_UNAVAILABLE 拒绝占用中的 serverId。同会话重复声明幂等。
#[derive(Default)]
pub struct AcpMcpRegistry {
    owners: Mutex<HashMap<String, String>>,
}

impl AcpMcpRegistry {
    pub fn claim(&self, server_id: &str, session_id: &str) -> Result<(), AcpMcpError> {
        let mut owners = self.owners.lock();
        if let Some(owner) = owners.get(server_id) {
            if owner != session_id {
                return Err(AcpMcpError::unavailable(format!(
                    "MCP-over-ACP serverId 已被另一会话占用: {server_id}"
                )));
            }
        }
        owners.insert(server_id.to_owned(), session_id.to_owned());
        Ok(())
    }

    pub fn owner(&self, server_id: &str) -> Option<String> {
        self.owners.lock().get(server_id).cloned()
    }

    /// 只释放调用会话自己的声明；重复关闭不影响其他会话。
    pub fn release_session(&self, session_id: &str) {
        self.owners.lock().retain(|_, owner| owner != session_id);
    }
}

/// client 声明的一个 acp 型 MCP server（绑定到具体会话）。
///
/// `server_id` 是 client 生成的不透明标识，`mcp/message` 依它把请求路由回
/// 声明方；同一条 ACP 连接内必须唯一。`name` 是人类可读名，用于工具命名与
/// 展示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpMcpServerSpec {
    pub session_id: String,
    pub name: String,
    pub server_id: String,
}

/// client → agent 的 `mcp/message` 通知载荷。
///
/// schema 1.12 的单向面：client 只能下发通知（provider → consumer 的进度 /
/// 订阅更新），不再有 client → agent 的请求方向。`request_id` 是 agent 发出
/// 对应请求时生成的逻辑标识；桥接以它校验活跃请求并还原 rmcp 关联 ID。
#[derive(Debug, Clone, PartialEq)]
pub struct AcpMcpInbound {
    /// 接收该通知的已声明 ACP MCP server。
    pub server_id: String,
    /// agent 为关联请求生成的逻辑标识。
    pub request_id: String,
    /// 内层 MCP 方法名。
    pub method: String,
    /// 内层 MCP 参数；缺省表示无参数。
    pub params: Option<Map<String, Value>>,
}

/// MCP over ACP 运行面错误（协议码 + 摘要，不含内层消息正文）。
///
/// 由实现方在连接失败、声明未知、内层 MCP 错误等场景返回；host 侧按其
/// `code` 映射为 ACP JSON-RPC 错误码。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpMcpError {
    pub code: i64,
    pub message: String,
}

impl AcpMcpError {
    /// 资源不存在（未知/已关闭的 `serverId`、未声明的 serverId）。
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
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "MCP over ACP error [{}]: {}",
            self.code, self.message
        )
    }
}

impl std::error::Error for AcpMcpError {}
