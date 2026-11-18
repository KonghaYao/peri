use std::fmt;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;
use peri_acp_types::identity::AgentId;
use peri_agent::agent::events::AgentEventHandler;
use peri_agent::messages::BaseMessage;
use peri_agent::session::subagent::SubagentHost;
use peri_agent::tools::BaseTool;
use tokio_util::sync::CancellationToken as AgentCancellationToken;

use super::invocation::InvocationArgs;
use crate::hooks::types::RegisteredHook;
use crate::mcp::McpAgentRegistry;
use crate::tool_search::core_tools::TOOL_AGENT;

#[path = "define/sub_agent_tool.rs"]
mod sub_agent_tool;
pub use sub_agent_tool::SubAgentTool;
