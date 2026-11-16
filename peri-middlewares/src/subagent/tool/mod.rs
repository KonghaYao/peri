use peri_acp_types::mcp_skills::McpSkillRegistry;
use peri_agent::middleware::chain::MiddlewareChain;
use peri_agent::middleware::r#trait::Middleware;
use peri_agent::session::subagent::SubagentChainAssembler;
use peri_agent::session::subagent::SubagentChainContext;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::agents_md::AgentsMdMiddleware;
use crate::middleware::todo::TodoMiddleware;
use crate::skills::SkillsMiddleware;
use crate::subagent::skill_preload::SkillPreloadMiddleware;
use crate::subagent::SubAgentMiddlewareConfig;

mod build_agent;
mod configuration;
mod define;
mod definitions;
mod execute_bg;
mod execute_fork;
mod execute_resume;
mod invocation;
mod mcp_activation;
mod spawn_context;

#[cfg(test)]
#[path = "subagent_lifecycle_test.rs"]
mod subagent_lifecycle_tests;

pub use define::SubAgentTool;

mod session_binding;

#[cfg(test)]
#[path = "tool_test.rs"]
mod tests;

#[cfg(test)]
#[path = "model_failure_test.rs"]
mod model_failure_tests;

mod subagent_chain_assembler_impl;
#[cfg(test)]
pub use subagent_chain_assembler_impl::build_subagent_middlewares;
pub use subagent_chain_assembler_impl::SubagentChainAssemblerImpl;
