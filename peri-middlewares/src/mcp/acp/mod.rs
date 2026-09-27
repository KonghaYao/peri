//! MCP over ACP（UNSTABLE）接入。
//!
//! client 在会话 setup 中以 `McpServer::Acp { name, serverId }` 声明由 ACP
//! 通道承载的 MCP server；本模块负责 agent 侧的全部运行时：
//!
//! - `session`：会话级连接管理（`mcp/connect` → rmcp 握手 → 工具发现 →
//!   提交 MCP 池 → 会话结束时 `mcp/disconnect`），实现
//!   [`peri_acp_types::ports::AcpMcpServerPort`]；
//! - `transport`：把 rmcp 的 JSON-RPC 消息经 ACP `mcp/message` 双向转发的
//!   [`rmcp::transport::Transport`] 实现。
//!
//! 连接就绪是异步的：会话 setup 只登记声明并后台建连，工具经既有的 deferred
//! 发现面（`tool_search` 索引 / `SearchExtraTools`）进入模型可见集，因此建连
//! 失败不阻塞会话建立，失败事实留在池状态面。

mod session;
mod transport;

pub use session::AcpMcpService;
