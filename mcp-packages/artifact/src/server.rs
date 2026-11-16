//! Builtin artifact MCP server that exposes the artifact upload tool over rmcp.
//!
//! Tool declaration and IF-D14 result mapping are shared through `peri_mcp_common`;
//! host-side instance dispatch and lifecycle remain in `peri-middlewares`.
//!
//! 边界（sub-plan F §6.2）：
//! - `cwd` 来自实例构造点，只用于相对路径解析，**不是**安全沙箱；`ToolContext` 与
//!   工具自身的 `cwd` 保持一致。
//! - 凭据由实例环境传入，在 Artifact MCP 内解释；url / token 不出现在 `ServerConfig`、
//!   `Tool::description` 或任何错误文本里（本模块从不打印它们）。
//! - 不覆写 `discover`（§10 R2）。

use std::fmt;
use std::fmt::{Debug, Formatter};
use std::path::Path;
use std::sync::Arc;

use peri_agent::tools::BaseTool;
use rmcp::model::CallToolRequestParams;
use rmcp::model::CallToolResponse;
use rmcp::model::ListToolsResult;
use rmcp::model::PaginatedRequestParams;
use rmcp::model::ServerConfig;
use rmcp::service::RequestContext;
use rmcp::service::RoleServer;
use rmcp::ErrorData as McpError;
use rmcp::ServerHandler;

use crate::ArtifactTool;

#[cfg(test)]
#[path = "server_test.rs"]
mod tests;

#[path = "server/artifact_mcp_server.rs"]
mod artifact_mcp_server;
pub use artifact_mcp_server::ArtifactMcpServer;
