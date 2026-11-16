//! 会话级 MCP over ACP 声明管理。
//!
//! 每个 [`AcpMcpService`] 持有会话自己的 MCP 池；`attach` 使用 ACP 连接级
//! 共享注册表登记 client 声明的 acp 型 server 并后台完成发现；client 经
//! `mcp/message` 反向下发的通知按 `serverId` 路由进对应 rmcp 连接；
//! `close_session` 移除该会话的全部声明与池条目。
//!
//! schema 1.10 起没有 `mcp/connect` / `mcp/disconnect`：每条内层请求都是独立
//! 的 ACP 请求，声明本身只在该会话存续期间有效。建连顺序（与配置型 MCP 的
//! 既有链路同构）：
//! 登记声明 → 建桥接 → 注册入站路由 → rmcp 协商（`server/discover`，失败回退
//! `initialize`）→ 工具发现（`tools/list`）→ `retain_service` →
//! `commit_acp_connection`。
//!
//! 两条不变量：
//! 1. **不阻塞会话建立**：`attach` 只登记与派发任务，协商与发现在后台任务里
//!    有界完成，失败留在池状态面（`ClientStatus::Failed`）而不回抛给调用方；
//! 2. **不跨会话泄漏**：连接在池中登记 `acp_owners` 归属，工具桥接、发现面与
//!    状态面按归属会话过滤（`McpClientPool::is_visible_to_session`）；同一
//!    ACP 连接内 `serverId` 唯一，重复声明按失败处理。

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;

use parking_lot::Mutex;
use peri_acp_types::acp_mcp::{AcpMcpError, AcpMcpInbound, AcpMcpRegistry, AcpMcpServerSpec};
use peri_acp_types::plugin::ConfigSource;
use peri_acp_types::ports::{AcpMcpGatewayPort, AcpMcpServerPort};
use serde_json::Value;

use super::transport;
use super::transport::AcpBridgeHandle;
use crate::mcp::client;
use crate::mcp::client::ClientStatus;
use crate::mcp::client::McpClientHandle;
use crate::mcp::client::McpClientPool;
use crate::mcp::client::OAuthStatus;
use crate::mcp::client::HTTP_CONNECT_TIMEOUT;
use crate::mcp::client::SHUTDOWN_TIMEOUT;
use crate::mcp::task_scope::McpTaskKey;

#[cfg(test)]
#[path = "session_test.rs"]
mod tests;

#[path = "session/acp_mcp_service.rs"]
mod acp_mcp_service;
pub use acp_mcp_service::AcpMcpService;
