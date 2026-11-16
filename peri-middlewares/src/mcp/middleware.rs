use peri_agent::middleware::capabilities as hook_state;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt;
use std::fmt::{Debug, Formatter};
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::client::McpClientHandle;
use async_trait::async_trait;
use peri_acp_types::command_registry::CommandRegistry;
use peri_acp_types::mcp_skills::{HandleToken, McpSkillRegistry};
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
};
use peri_agent::agent::stages::SharedToolMap;
use peri_agent::agent::AgentCancellationToken;
use peri_agent::middleware::r#trait::Middleware;
use peri_agent::session::MessageKind;
use peri_agent::session::MessageSource as QueueMessageSource;
use peri_agent::session::QueuedMessage;
use peri_agent::tools::BaseTool;
use serde_json::json;

use super::admission::McpAdmissionPreparation;
use super::admission::MCP_MIDDLEWARE_NAME;
use super::client;
use super::client::ClientStatus;
use super::client::McpClientPool;
use super::client::SystemMcpManifest;
use super::client::SystemReadinessError;
use super::discover_tool::DiscoverMCPTool;
use super::resource_tool::McpResourceTool;
use super::system_tools;
use super::tool_bridge::McpToolBridge;

#[cfg(test)]
#[path = "middleware_test.rs"]
mod tests;

#[path = "middleware/mcp_middleware.rs"]
mod mcp_middleware;
pub use mcp_middleware::attach_connection_notifier;
pub use mcp_middleware::prewarm_discovery;
#[cfg(test)]
pub(crate) use mcp_middleware::run_ensure_discovery;
pub use mcp_middleware::McpMiddleware;
#[cfg(test)]
use mcp_middleware::MCP_TOOL_USAGE_HINT;
