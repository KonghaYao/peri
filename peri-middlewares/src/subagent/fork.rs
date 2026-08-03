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

/// Build fork directive message for fork mode.
///
/// The directive instructs the forked agent to continue from the parent conversation
/// with specific rules to prevent recursion and maintain scope.
pub fn build_fork_directive(prompt: &str) -> String {
    format!(
        "<fork_directive>\n\
         You are a forked agent continuing from the parent conversation.\n\
         You have full access to the conversation history above.\n\
         \n\
         RULES:\n\
         1. Do NOT spawn sub-agents — execute directly using your tools\n\
         2. Do NOT ask questions — act on the directive below\n\
         3. Stay strictly within your assigned scope\n\
         4. Report structured facts, then stop\n\
         5. Keep your response under 500 words unless specified otherwise\n\
         \n\
         Output format:\n\
           Scope: <your assigned scope in one sentence>\n\
           Result: <the answer or key findings>\n\
           Key files: <relevant file paths>\n\
           Files changed: <list if you modified files>\n\
         </fork_directive>\n\n\
         {prompt}"
    )
}

/// Build bg-fork directive message for /bg command path.
///
/// Similar to `build_fork_directive` but in Chinese, with bg-specific identity
/// and output format tailored for background task results.
pub fn build_bg_fork_directive(prompt: &str) -> String {
    // 防御性 XML 注入防护
    let sanitized = prompt.replace("</bg_fork_directive>", "<\u{200b}/bg_fork_directive>");
    format!(
        "<bg_fork_directive>\n\
         你是后台异步 Agent，从父会话 fork 而来。\n\
         你拥有完整的对话历史上下文。\n\
         \n\
         规则：\n\
         1. 禁止生成子 Agent — 直接使用工具执行\n\
         2. 禁止提问 — 按指令行动\n\
         3. 严格限定在分配范围内\n\
         4. 先给出结论，再补充说明\n\
         5. 除非特别说明，回复控制在 500 字以内\n\
         \n\
         输出格式：\n\
           结论: <核心结论或发现>\n\
           详细说明: <补充细节>\n\
           关键文件: <相关文件路径>\n\
           建议: <后续行动建议>\n\
         </bg_fork_directive>\n\n\
         {sanitized}"
    )
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

/// 构建 Prediction 指令模板（中文）。
/// 用于 agent 完成后预测用户下一步输入。
///
/// `current_title` 为会话当前标题（`None` 表示尚无标题）。注入后模型才能判断
/// 现有标题是否需要更新——不传则模型无从得知标题现状，会默认不输出 title 标记。
///
/// 输出格式：纯文本（placeholder）+ 可选 `<peri:xxx>` 标记。
/// 标记仅在该信息确有价值时输出；无法判断时只输出纯文本。
pub fn build_prediction_directive(current_title: Option<&str>) -> String {
    // 防御性 XML 注入防护（标题可能含闭合标签文本）
    let title_ctx = match current_title {
        Some(t) => {
            let sanitized = t.replace("</prediction_directive>", "<\u{200b}/prediction_directive>");
            format!("当前会话标题：\"{sanitized}\"")
        }
        None => "当前会话标题：（无）".to_string(),
    };
    format!(
        "<prediction_directive>\n\
         你是预测输入助手。根据对话上下文，预测用户下一步最可能在输入框中输入什么，\n\
         并同步维护会话元数据。\n\
         \n\
         {title_ctx}\n\
         \n\
         规则：\n\
         1. 默认输出一句预测文本（占位符），不要解释\n\
         2. 预测应该是自然的用户语言，像用户自己会打的那样\n\
         3. 不要加引号、前缀或格式\n\
         4. 长度控制在 5-30 个字\n\
         5. 如果无法判断，输出空字符串\n\
         \n\
         结构化标记（仅在对应信息有价值时输出，可同时输出多个）：\n\
         - <peri:title>新标题</peri:title>：当标题缺失、过时或与当前任务不符时，主动更新为精炼的当前任务标题；话题转变时应立即更新\n\
         - <peri:tag>标签</peri:tag>：检测到明确主题时打一个标签（如 bugfix、refactor）\n\
         - <peri:summary>一句话摘要</peri:summary>：给整个对话写一句简短摘要\n\
         示例：继续排查内存泄漏 <peri:title>排查内存泄漏</peri:title><peri:tag>bugfix</peri:tag>\n\
         示例（话题转变，标题应立即更新）：<peri:title>性能优化</peri:title>\n\
         </prediction_directive>"
    )
}

#[cfg(test)]
#[path = "fork_test.rs"]
mod tests;
