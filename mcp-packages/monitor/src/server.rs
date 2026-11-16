//! `monitor` builtin MCP handler.
//!
//! v1 exposes command-sourced monitors only. `monitor_start` returns a standard
//! MCP Tasks handle; the host observes terminal state through the existing
//! `tasks/get` projection and cancels through `tasks/cancel`. No custom task
//! lifecycle methods and no ACP methods are added.
//!
//! The handler never interprets captured output. Event text is returned as data
//! with `untrusted: true`; nothing in it is executed.

use std::fmt;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;

use peri_agent::tools::BaseTool;
use peri_mcp_common::result_mapping;
use peri_mcp_common::task_owner;
use rmcp::model::CallToolRequestParams;
use rmcp::model::CallToolResponse;
use rmcp::model::CallToolResult;
use rmcp::model::CancelTaskParams;
use rmcp::model::ContentBlock;
use rmcp::model::CreateTaskResult;
use rmcp::model::DetailedTask;
use rmcp::model::GetTaskParams;
use rmcp::model::GetTaskResult;
use rmcp::model::Implementation;
use rmcp::model::JsonObject;
use rmcp::model::ListToolsResult;
use rmcp::model::MetaObject;
use rmcp::model::PaginatedRequestParams;
use rmcp::model::ServerCapabilities;
use rmcp::model::ServerConfig;
use rmcp::model::Task;
use rmcp::model::TaskPayload;
use rmcp::model::TaskStatus;
use rmcp::model::TASKS_EXTENSION_ID;
use rmcp::service::RequestContext;
use rmcp::service::RoleServer;
use rmcp::ErrorData as McpError;
use rmcp::ServerHandler;

use crate::state::{MonitorState, MonitorStatus, TaskSnapshot};
use crate::tools;
use peri_acp_types::monitor::{MonitorAlertMeta, MONITOR_META_KEY};

#[cfg(test)]
#[path = "server_test.rs"]
mod tests;

#[cfg(test)]
#[path = "server_wire_test.rs"]
mod wire_tests;

#[path = "server/monitor_mcp_server.rs"]
mod monitor_mcp_server;
#[cfg(test)]
use monitor_mcp_server::detailed_task;
#[cfg(test)]
use monitor_mcp_server::seed_task;
#[cfg(test)]
use monitor_mcp_server::unknown_task;
pub use monitor_mcp_server::MonitorMcpServer;
