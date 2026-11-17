//! Tests for mid_mcp

use super::{
    Arc, BaseTool, McpClientPool, McpMiddleware, McpResourceTool, Middleware, ReminderAudience,
    ReminderCategory, ReminderDelivery, ReminderSeverity,
};
use crate::mcp::client;
use crate::mcp::client::McpClientHandle;
use crate::mcp::client::OAuthStatus;
use crate::mcp::ClientStatus;
use peri_agent::session::{MessageKind, MessageQueue};

// ─── before_agent：MCP skill 发现投映（验收 7/13/14）────────────────────────

use peri_acp_types::command::command_route::{
    CommandEntryKind, CommandLifecycle, CommandProvenance, CommandSource,
};
use peri_acp_types::mcp_skills::{HandleToken, McpSkillRegistry, ServerDiscoveryState};
use peri_acp_types::skills::SkillMetadata;
use peri_agent::agent::state::AgentState;
use peri_agent::agent::AgentCancellationToken;
use rmcp::model::Resource;

mod command_tests {
    include!("middleware_command_test.rs");
}

mod system_tests {
    include!("middleware_system_test.rs");
}

mod discovery_state_tests {
    include!("middleware_discovery_state_test.rs");
}

#[path = "middleware_test/test_cases.rs"]
mod test_cases;
use test_cases::insert_skill_handle;
use test_cases::owned_discovery_pool;
use test_cases::wait_discovered;
