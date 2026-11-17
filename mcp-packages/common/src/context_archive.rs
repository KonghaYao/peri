//! Read-only MCP archive capability. The injected port is already bound to one session.
use peri_acp_types::context_archive::{ArchiveReadRequest, ContextArchiveReadPort};
use rmcp::model::CallToolRequestParams;
use rmcp::model::CallToolResponse;
use rmcp::model::CallToolResult;
use rmcp::model::ContentBlock;
use rmcp::model::ListToolsResult;
use rmcp::model::PaginatedRequestParams;
use rmcp::model::ServerConfig;
use rmcp::model::Tool;
use rmcp::service::RequestContext;
use rmcp::service::RoleServer;
use rmcp::ErrorData as McpError;
use rmcp::ServerHandler;
use serde_json::{json, Value};
use std::fmt;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;

#[path = "context_archive/context_archive_mcp_server.rs"]
mod context_archive_mcp_server;
pub use context_archive_mcp_server::parameters;
pub use context_archive_mcp_server::ContextArchiveMcpServer;
pub use context_archive_mcp_server::DESCRIPTION;
pub use context_archive_mcp_server::TOOL_NAME;
