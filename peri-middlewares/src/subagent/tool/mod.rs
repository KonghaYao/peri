use std::collections::HashMap;

use peri_agent::{messages::BaseMessage, middleware::r#trait::Middleware, session::Session};
use tokio::sync::mpsc;

use crate::{
    agents_md::AgentsMdMiddleware,
    hooks::types::{HookEvent, RegisteredHook},
    middleware::todo::TodoMiddleware,
    skills::SkillsMiddleware,
    subagent::{skill_preload::SkillPreloadMiddleware, SubAgentMiddlewareConfig},
};

/// 构造 SubAgent 标准中间件链
///
/// ## 与主 Agent 中间件链的差异（P1-13）
///
/// 以下中间件**有意不在 SubAgent 链中注册**：
///
/// | 中间件 | 省略原因 |
/// |--------|----------|
/// | `GitAttributionMiddleware` | SubAgent 工具调用无需 git 贡献追踪 |
/// | `AtMentionMiddleware` | @path 解析仅在主 Agent 用户交互中生效 |
/// | `AgentDefineMiddleware` | SubAgent 定义由调用方单独注入 system_prompt |
/// | `PluginMiddleware` | 插件仅在主 Agent 中加载 |
/// | `CronMiddleware` | SubAgent 独立生命周期，不参与调度 |
/// | `HITLMiddleware` | SubAgent 工具执行沿用父 Agent 的审批模式 |
///
/// 以下中间件通过**参数注入**方式支持 SubAgent：
///
/// | 中间件 | 注入方式 |
/// |--------|----------|
/// | `ErrorSuggest` | 通过 `build_v2_subagent_context(error_suggest_registry)` 注入 |
/// | `Hook` (生命周期) | 通过 `fire_subagent_lifecycle_hooks_static()` 独立触发 |
pub fn build_subagent_middlewares(config: SubAgentMiddlewareConfig) -> Vec<Box<dyn Middleware>> {
    let mut middlewares: Vec<Box<dyn Middleware>> = Vec::new();

    // [TRAP] SubAgent 复用 main agent 在 session/new 时捕获的 frozen CLAUDE.md，
    // 避免文件中途变更导致 system prompt 漂移（第一优先级不变量）。
    let mut agents_md = AgentsMdMiddleware::new();
    if let Some(main) = config.frozen_claude_md {
        agents_md = agents_md.with_frozen_content(main, config.frozen_claude_local_md);
    }
    middlewares.push(Box::new(agents_md));

    // [TRAP] 同上：SubAgent 复用 frozen skill summary。
    let mut skills = SkillsMiddleware::new().with_global_config();
    if let Some(summary) = config.frozen_skill_summary {
        skills = skills.with_frozen_summary(summary);
    }
    middlewares.push(Box::new(skills));

    if !config.skill_names.is_empty() {
        middlewares.push(Box::new(SkillPreloadMiddleware::new(
            config.skill_names,
            &config.cwd,
        )));
    }
    middlewares.push(Box::new(TodoMiddleware::new({
        let (tx, _rx) = mpsc::channel(8);
        tx
    })));
    middlewares
}

/// 独立（非方法）版本的 SubagentStart/SubagentStop hook 触发逻辑
pub(crate) async fn fire_subagent_lifecycle_hooks_static(
    registered_hooks: &[RegisteredHook],
    event: HookEvent,
    cwd: &str,
    subagent_name: &str,
    result: Option<&str>,
) {
    let matching: Vec<&RegisteredHook> = registered_hooks
        .iter()
        .filter(|h| h.event == event)
        .collect();
    if matching.is_empty() {
        return;
    }

    let input = match &event {
        HookEvent::SubagentStart => {
            crate::hooks::types::HookInput::subagent_start("", "", cwd, subagent_name)
        }
        HookEvent::SubagentStop => crate::hooks::types::HookInput::subagent_stop(
            "",
            "",
            cwd,
            subagent_name,
            result.unwrap_or(""),
        ),
        _ => return,
    };

    for registered in &matching {
        let _action = match &registered.hook {
            crate::hooks::types::HookType::Command { .. } => {
                crate::hooks::executor::execute_command_hook(&registered.hook, &input, registered)
                    .await
            }
            crate::hooks::types::HookType::Http { .. } => {
                crate::hooks::executor::execute_http_hook(&registered.hook, &input).await
            }
            _ => crate::hooks::types::HookAction::Allow,
        };
    }
}

/// Format sub-agent execution result as a summary string returned to the parent agent.
fn format_subagent_result(output: &peri_agent::agent::react::AgentOutput) -> String {
    if output.tool_calls.is_empty() {
        return output.text.clone();
    }

    let mut tool_counts: HashMap<&str, usize> = HashMap::new();
    for (call, _) in &output.tool_calls {
        *tool_counts.entry(call.name.as_str()).or_insert(0) += 1;
    }

    let mut tools: Vec<_> = tool_counts.into_iter().collect();
    tools.sort_by_key(|b| std::cmp::Reverse(b.1));

    let tool_summary = tools
        .into_iter()
        .map(|(name, count)| format!("{} {} times", name, count))
        .collect::<Vec<_>>()
        .join(", ");

    format!(
        "[Sub-agent executed {} tool calls: {}]\n\n{}",
        output.tool_calls.len(),
        tool_summary,
        output.text
    )
}

/// 从 session transcript 提取最后一条非空 AI 消息文本（P1-11: define/execute_fork/execute_bg/spawner 共用）。
pub(crate) fn extract_last_ai_text(session: &std::sync::Arc<Session>) -> String {
    let transcript = session.transcript();
    let tx = transcript.read();
    tx.visible_messages()
        .iter()
        .rev()
        .find_map(|m| {
            if matches!(m, BaseMessage::Ai { .. }) {
                let t = m.content();
                let trimmed = t.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            } else {
                None
            }
        })
        .unwrap_or_default()
}

mod build_agent;
mod define;
mod execute_bg;
mod execute_fork;
pub(crate) mod lifecycle;
pub use define::SubAgentTool;

#[cfg(test)]
#[path = "tool_test.rs"]
mod tests;
