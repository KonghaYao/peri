use peri_agent::middleware::capabilities as hook_state;
use std::sync::Arc;

use async_trait::async_trait;
use peri_agent::agent::react::ToolCall;
use peri_agent::error::AgentError;
use peri_agent::error::AgentResult;
use peri_agent::interaction::ApprovalDecision;
use peri_agent::interaction::ApprovalItem;
use peri_agent::interaction::InteractionContext;
use peri_agent::interaction::InteractionResponse;
use peri_agent::interaction::TrustedInteractionOwner;
use peri_agent::interaction::UserInteractionBroker;
use peri_agent::middleware::prompt_sections::PromptSection;
use peri_agent::middleware::prompt_sections::PromptSectionZone;
use peri_agent::middleware::r#trait::Middleware;

use peri_acp_types::builtin_mcp::original_tool_name_of_effective;
use peri_acp_types::error::ToolRejectionCause;
use peri_acp_types::interaction::UnansweredCause;

use crate::tool_search::core_tools::{
    TOOL_AGENT, TOOL_BASH, TOOL_EDIT, TOOL_FOLDER_OPS, TOOL_WEBFETCH, TOOL_WEBSEARCH, TOOL_WRITE,
};

pub mod auto_classifier;
pub mod shared_mode;

pub use auto_classifier::{AutoClassifier, Classification, LlmAutoClassifier};
pub use peri_agent::hitl::{BatchItem, HitlDecision};
pub use shared_mode::{PermissionMode, SharedPermissionMode};

// ─── ExecuteExtraTool 权限透传 ─────────────────────────────────────────────

/// 获取有效的工具名称
///
/// 当 tool_name 为 [`crate::tool_search::core_tools::EXECUTE_EXTRA_TOOL_NAME`] 时，
/// 从 `input[EXTRA_TOOL_NAME_FIELD]` 提取目标工具名，用于 HITL 权限判断。
/// 否则直接返回原始工具名。
pub use crate::tool_search::core_tools::resolve_effective_tool_name as effective_tool_name;

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;

#[cfg(test)]
#[path = "source_identity_test.rs"]
mod source_identity_tests;

#[cfg(test)]
#[path = "untrusted_owner_test.rs"]
mod untrusted_owner_tests;

#[cfg(test)]
#[path = "unanswered_test.rs"]
mod unanswered_tests;

mod permission_middleware;
pub(crate) use permission_middleware::BROKER_TIMEOUT;

pub use permission_middleware::default_requires_approval;

pub use permission_middleware::is_edit_tool;

pub(crate) use permission_middleware::is_external_mcp;

pub use permission_middleware::SensitiveToolEntry;

pub(crate) use permission_middleware::apply_decision;
pub use permission_middleware::format_sensitive_tools;
pub use permission_middleware::format_sensitive_tools_for_disabled;
pub use permission_middleware::sensitive_tool_entries;
pub use permission_middleware::PermissionMiddleware;

#[cfg(test)]
use permission_middleware::classifier_context;
