pub mod loader;
pub mod tools;

use peri_agent::middleware::capabilities as hook_state;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
pub use loader::{resolve_skill_roots, SkillMetadata, SkillRoot, SkillSource};
use peri_acp_types::mcp_skills::McpSkillRegistry;
use peri_acp_types::skills::SkillOrigin;
use peri_agent::error::AgentResult;
use peri_agent::middleware::prompt_sections::PromptSection;
use peri_agent::middleware::r#trait::Middleware;
use peri_agent::tools::BaseTool;

// 配置读取（F12 宿主适配器：disableBundledSkills）归
// `crate::settings`——本模块（`peri-middlewares/src/skills/`）W4b 后**不含任何
// 文件系统调用**（静态断言：本目录 grep 无 fs 读取路径），J5：技能来源只有
// MCP 侧（builtin `workspace` 实例的资源面）。
pub use crate::settings::{
    global_config_path, load_disable_bundled_skills, load_disable_bundled_skills_from_path,
};

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;

mod skills_middleware;
pub use skills_middleware::annotate_mcp_content;
pub use skills_middleware::format_discovery_protocol;
pub(crate) use skills_middleware::skill_activation_failed_message;
pub(crate) use skills_middleware::skill_ambiguous_message;
pub(crate) use skills_middleware::skill_catalog_failed_message;
pub(crate) use skills_middleware::skill_catalog_initializing_message;
pub(crate) use skills_middleware::skill_catalog_unavailable_message;
pub(crate) use skills_middleware::skill_not_found_message;
pub(crate) use skills_middleware::skill_preload_budget_message;
pub(crate) use skills_middleware::skill_registry_unwired_message;
pub use skills_middleware::SkillsMiddleware;
