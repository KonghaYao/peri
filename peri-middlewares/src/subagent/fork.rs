//! Fork semantics: tool filtering, fork directive construction, agent override extraction.
//!
//! Pure computation functions for sub-agent inheritance from parent agent.
//! No async, no external state mutation — safe for unit testing without mocks.

use std::sync::Arc;

use peri_agent::tools::BaseTool;

use crate::tool_search::core_tools::{TOOL_AGENT, TOOL_ASK_USER, TOOL_WORKFLOW};
use crate::tools::ArcToolWrapper;
use peri_acp_types::agents::AgentOverrides;
use peri_mcp_core::agent_definition::ToolsValue;

/// 子链不持有的 builtin 实例（按 `BaseTool::builtin_mcp_instance()` 的**可信
/// 实例身份**判定，不按工具名猜）：v4 起 cron 是 builtin 实例，子链没有其持有者，
/// 因此不进入子执行。
const CHILD_ABSENT_BUILTIN_INSTANCES: [&str; 1] = ["cron"];

/// 子 Agent 继承策略（**单一权威**，与 `subagent/tool/descriptions/agent.md`
/// 的继承声明同一契约）：父工具集中这些扩展面不进入子执行——
/// `Agent`（防递归）、`AskUserQuestion`（子链无提问持有者，与子能力投影
/// `subagent_chain_capabilities` 的 ask_user=false 一致）、`Workflow`、
/// builtin `cron` 实例工具。
///
/// 定义型（[`canonical_tool_filter`] 组合本函数）与前台/后台 fork 共用本函数；
/// deferred 搜索与执行消费同一 `tool_filter`（`SessionToolCatalog::with_filter`
/// 的 published 视图派生 ToolSearch 索引），因此 direct 与 deferred 同步闭合。
///
/// Plugin 扩展面无需在此列：插件命令不是模型工具，插件声明的 MCP server 不进入
/// `build_parent_tools`（其只桥接 builtin 实例），不会经继承进入子执行——不新增
/// 按名字猜插件的第二份名单。名字匹配对用户 MCP 同名工具是 fail-closed 的过度
/// 过滤，不是放行。
pub fn child_inheritance_filter() -> peri_agent::session::tool_catalog::ToolFilter {
    std::sync::Arc::new(|tool: &dyn BaseTool| {
        let excluded = tool.name() == TOOL_AGENT
            || tool.name() == TOOL_ASK_USER
            || tool.name() == TOOL_WORKFLOW
            || tool
                .builtin_mcp_instance()
                .is_some_and(|instance| CHILD_ABSENT_BUILTIN_INSTANCES.contains(&instance));
        !excluded
    })
}

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
    // 定义型与 fork 共用同一继承策略（含 Agent/AskUser/Workflow/cron），
    // 再叠加定义自身的 allow/disallow 策略。
    let inheritance = child_inheritance_filter();
    Arc::new(move |tool| inheritance(tool) && policy(tool))
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
