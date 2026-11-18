//! DiscoverMCP —— 只读 MCP 域查询工具（deferred、namespace=meta）。
//!
//! JSON-RPC 风格入参（`{method, params}`），错误契约 `{error:{code,message}}`
//! （无 id）：未知 method `-32601`、参数错误 `-32602`、server 不存在/未连接
//! `-32000`。**只读硬约束**：本文件不调用任何 MCP tools/call、read_resource、
//! ToolInvoke、build_tool_bridges——数据全部来自会话工具目录、pool
//! 只读快照与 registry。

use std::fmt;
use std::fmt::{Debug, Formatter};
use std::sync::{Arc, Weak};

use async_trait::async_trait;
use parking_lot::RwLock;
use peri_acp_types::mcp_skills::McpSkillRegistry;
use peri_acp_types::skills::SkillMetadata;
use peri_acp_types::skills::SkillOrigin;
use peri_agent::agent::stages::SharedToolMap;
use peri_agent::tools::BaseTool;
use peri_agent::tools::ToolContext;
use rmcp::model::Tool;
use serde_json::{json, Value};

use super::agent_registry::McpAgentRegistry;
use super::client::ClientStatus;
use super::client::McpClientPool;
use super::client::OAuthStatus;
use super::config::ConfigSource;

#[cfg(test)]
#[path = "discover_tool_test.rs"]
mod tests;

#[path = "discover_tool/discover_mcp_tool.rs"]
mod discover_mcp_tool;
#[cfg(test)]
use discover_mcp_tool::sort_key;
pub use discover_mcp_tool::DiscoverMCPTool;
