//! The builtin web MCP server, exposing the web fetch and search tools over rmcp.
//!
//! Shared tool declaration and IF-D14 result mapping live in `peri_mcp_common`;
//! host-side instance dispatch and lifecycle remain in `peri-middlewares`.
//!
//! 资源面（`resources/list|read`）发布 `explorer` 定义：
//! `agent://builtin/explorer/agent.md`。宿主 Agent registry 只信任宿主绑定
//! 的 builtin 实例来源，因此定义随实例可用性一起开闭。

use std::fmt;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;

use crate::agents;
use crate::agents::AgentResourceError;
use crate::web_fetch::WebFetchTool;
use crate::web_search::WebSearchTool;
use peri_agent::tools::BaseTool;
use rmcp::model::CallToolRequestParams;
use rmcp::model::CallToolResponse;
use rmcp::model::Implementation;
use rmcp::model::ListResourcesResult;
use rmcp::model::ListToolsResult;
use rmcp::model::PaginatedRequestParams;
use rmcp::model::ReadResourceRequestParams;
use rmcp::model::ReadResourceResponse;
use rmcp::model::ServerCapabilities;
use rmcp::model::ServerConfig;
use rmcp::service::RequestContext;
use rmcp::service::RoleServer;
use rmcp::ErrorData as McpError;
use rmcp::ServerHandler;

#[cfg(test)]
#[path = "server_test.rs"]
mod tests;

#[path = "server/web_mcp_server.rs"]
mod web_mcp_server;
pub use web_mcp_server::WebMcpServer;
