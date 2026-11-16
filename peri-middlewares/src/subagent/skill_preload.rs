use async_trait::async_trait;
use peri_acp_types::mcp_skills::{McpSkillRegistry, SkillLookup};
use peri_acp_types::skills::SkillOrigin;
use peri_agent::error::AgentResult;
use peri_agent::messages::BaseMessage;
use peri_agent::messages::ContentBlock;
use peri_agent::middleware::capabilities as hook_state;
use peri_agent::middleware::r#trait::Middleware;
use std::fmt;
use std::fmt::{Debug, Formatter};

#[cfg(test)]
#[path = "skill_preload_test.rs"]
mod tests;

#[path = "skill_preload/skill_preload_middleware.rs"]
mod skill_preload_middleware;
#[cfg(test)]
pub use skill_preload_middleware::extract_skill_names_from_text;
pub use skill_preload_middleware::SkillPreloadMiddleware;
#[cfg(test)]
use skill_preload_middleware::MAX_PRELOAD_ITEM_BYTES;
