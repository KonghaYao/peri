//! Fork semantics: tool filtering, fork directive construction, agent override extraction.
//!
//! Pure computation functions for sub-agent inheritance from parent agent.
//! No async, no external state mutation — safe for unit testing without mocks.

use std::sync::Arc;

use peri_agent::tools::BaseTool;

use crate::tool_search::core_tools::TOOL_AGENT;
use crate::{agent_define::AgentOverrides, claude_agent_parser::ToolsValue, tools::ArcToolWrapper};

/// Filter tools from parent set based on agent definition's tools/disallowedTools fields.
///
/// Rules:
/// - `tools` is omitted -> inherit all parent tools (but always exclude `Agent` itself to prevent recursion)
/// - `tools: []` -> inherit no parent tools
/// - `tools` has value -> only keep tools in the list (also exclude `Agent`)
/// - then remove tools listed in `disallowed_tools` from the result
///
/// Matching is case-insensitive (users often write PascalCase in agent.md).
pub fn filter_tools(
    parent_tools: &[Arc<dyn BaseTool>],
    allowed: &ToolsValue,
    disallowed: &ToolsValue,
) -> Vec<Box<dyn BaseTool>> {
    let disallowed_list = disallowed.to_vec();

    parent_tools
        .iter()
        .filter(|tool| {
            let name = tool.name();
            let name_lower = name.to_lowercase();
            if name == TOOL_AGENT {
                return false;
            }
            let is_allowed = match allowed {
                ToolsValue::Empty => true,
                ToolsValue::NoTools => false,
                ToolsValue::List(allowed_list) => {
                    allowed_list.len() == 1 && allowed_list[0] == "*"
                        || allowed_list.iter().any(|n| n.to_lowercase() == name_lower)
                }
            };
            if !is_allowed {
                return false;
            }
            if disallowed_list
                .iter()
                .any(|n| n.to_lowercase() == name_lower)
            {
                return false;
            }
            true
        })
        .map(|tool| Box::new(ArcToolWrapper(Arc::clone(tool))) as Box<dyn BaseTool>)
        .collect()
}

/// Whether an agent declaration permits tools injected outside parent-tool inheritance.
/// Explicit `tools: []` is a strict zero-tool boundary.
pub(crate) fn allows_injected_tools(allowed: &ToolsValue) -> bool {
    !matches!(allowed, ToolsValue::NoTools)
}

/// Extract [`AgentOverrides`] from already-parsed agent definition fields.
///
/// Returns `None` when all fields are empty (no overrides needed).
///
/// `mode: "full"` 在下游 `PromptTemplate::with_overrides` 中只替换
/// PersonaDomain 层；不可替换层（安全/工程/能力/运行时边界）始终渲染。
pub fn overrides_from_agent_def(
    system_prompt: &str,
    tone: &Option<String>,
    proactiveness: &Option<String>,
    mode: &Option<String>,
) -> Option<AgentOverrides> {
    let persona = if system_prompt.is_empty() {
        None
    } else {
        Some(system_prompt.to_string())
    };
    let overrides = AgentOverrides {
        persona,
        tone: tone.clone(),
        proactiveness: proactiveness.clone(),
        mode: mode.clone(),
    };
    if overrides.is_empty() {
        None
    } else {
        Some(overrides)
    }
}

// ─── fork / bg-fork / prediction 指令模板（L3 迁至 peri-agent，此处 re-export
// 保持调用方兼容；mod.rs 统一对外 re-export） ───
pub use peri_agent::session::subagent::{
    build_bg_fork_directive, build_fork_directive, build_prediction_directive,
};

#[cfg(test)]
#[path = "fork_test.rs"]
mod tests;
