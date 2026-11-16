use peri_agent::middleware::capabilities as hook_state;
use std::sync::Arc;

use async_trait::async_trait;
use peri_agent::agent::events::AgentEventHandler;
use peri_agent::agent::AgentCancellationToken;
use peri_agent::error::AgentResult;
use peri_agent::messages::BaseMessage;
use peri_agent::middleware::prompt_sections::PromptSection;
use peri_agent::middleware::prompt_sections::PromptSectionZone;
use peri_agent::middleware::r#trait::Middleware;
use peri_agent::session::Session;
use peri_agent::tools::BaseTool;

use peri_acp_types::builtin_mcp::original_tool_name_of_effective;

use crate::tools::BoxToolWrapper;
use peri_acp_types::agents::AgentOverrides;
use peri_mcp_core::agent_definition::{ClaudeAgentFrontmatter, ToolsValue};

mod agent_result;
mod fork;
mod skill_preload;
mod tool;

#[cfg(test)]
pub(crate) mod test_support;
pub use agent_result::AgentResultTool;
pub use fork::{build_bg_fork_directive, build_fork_directive, build_prediction_directive};
use parking_lot::RwLock;
pub use skill_preload::SkillPreloadMiddleware;
pub use tool::SubAgentTool;
pub use tool::SubagentChainAssemblerImpl;

/// Agent 运行时能力画像，用于主 Agent 调度决策。
///
/// 主 Agent 在 Prompt 中看到此信息后可以判断能否并行调度（readonly/writes）与
/// 期望档位；3.0 批 2 波 1 起协议类型归契约层（定义见
/// `peri_acp_types::agents::AgentCapability`）。
pub use peri_acp_types::agents::{
    AgentCapability, AgentModelSelection, InvalidModelTier, ModelTier,
};

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;

#[cfg(test)]
#[path = "reason_lifecycle_test.rs"]
mod reason_lifecycle_tests;

mod sub_agent_middleware;
pub use sub_agent_middleware::SubAgentMiddleware;
pub use sub_agent_middleware::SubAgentMiddlewareConfig;
pub(crate) use sub_agent_middleware::DEFAULT_SUBAGENT_MAX_ITERATIONS;

pub use sub_agent_middleware::infer_agent_capability;
#[cfg(test)]
use sub_agent_middleware::is_mutation_tool;
