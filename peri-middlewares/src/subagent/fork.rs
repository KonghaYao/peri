//! Fork semantics: tool filtering, fork directive construction, agent override extraction.
//!
//! Pure computation functions for sub-agent inheritance from parent agent.
//! No async, no external state mutation — safe for unit testing without mocks.

use std::sync::Arc;

use peri_agent::tools::BaseTool;

use crate::tool_search::core_tools::TOOL_AGENT;
use crate::tools::ArcToolWrapper;
use peri_acp_types::agents::AgentOverrides;
use peri_mcp_common::agent_definition::ToolsValue;

pub fn canonical_tool_filter(
    allowed: &ToolsValue,
    disallowed: &ToolsValue,
) -> peri_agent::session::tool_catalog::ToolFilter {
    let allowed = match allowed {
        ToolsValue::Empty => None,
        ToolsValue::NoTools => Some(Vec::new()),
        // A lone `*` inherits all. In a mixed list it was never a wildcard:
        // preserve the explicit names without widening the child tool set.
        ToolsValue::List(names) if names.len() == 1 && names[0] == "*" => Some(names.clone()),
        ToolsValue::List(names) => {
            Some(names.iter().filter(|name| *name != "*").cloned().collect())
        }
    };
    let policy = peri_agent::session::tool_catalog::ToolFilterPolicy::canonical(
        allowed,
        disallowed.to_vec(),
    );
    Arc::new(move |tool| tool.name() != TOOL_AGENT && policy(tool))
}

/// Filter tools from parent set based on agent definition's tools/disallowedTools fields.
///
/// Rules:
/// - `tools` is omitted -> inherit all parent tools (but always exclude `Agent` itself to prevent recursion)
/// - `tools: []` -> inherit no parent tools
/// - `tools` has value -> only keep tools in the list (also exclude `Agent`)
/// - `tools: ["*"]` alone inherits all; `*` in a mixed list adds no permissions
/// - then remove tools listed in `disallowed_tools` from the result
///
/// Matching is case-insensitive. Explicit builtin allowlists also check the bound
/// tool source, so an external MCP tool named `Read` cannot enter a read-only agent.
pub fn filter_tools(
    parent_tools: &[Arc<dyn BaseTool>],
    allowed: &ToolsValue,
    disallowed: &ToolsValue,
) -> Vec<Box<dyn BaseTool>> {
    let filter = canonical_tool_filter(allowed, disallowed);

    parent_tools
        .iter()
        .filter(|tool| filter(tool.as_ref()))
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
