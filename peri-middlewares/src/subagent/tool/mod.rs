use peri_agent::{
    middleware::chain::MiddlewareChain,
    middleware::r#trait::Middleware,
    session::subagent::{SubagentChainAssembler, SubagentChainContext},
};
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

mod build_agent;
mod define;
mod execute_bg;
mod execute_fork;
mod execute_resume;
pub use define::SubAgentTool;

/// 子 agent 链装配器实现（L3）：经 [`SubagentChainAssembler`] trait 依赖反转，
/// 由 middlewares 提供实现——Agent 层 [`spawn_subagent`] 从父 session copy frozen
/// 数据后调用本实现构建子链，链序保持 [`build_subagent_middlewares`] 不变
/// （AgentsMd→Skills→[SkillPreload]→Todo，ARC-MIDDLEWARE-001）。
pub struct SubagentChainAssemblerImpl;

impl SubagentChainAssembler for SubagentChainAssemblerImpl {
    fn assemble(&self, ctx: &SubagentChainContext) -> MiddlewareChain {
        let config =
            super::SubAgentMiddlewareConfig::for_agent_def(ctx.skill_names.clone(), &ctx.cwd)
                .with_frozen(
                    ctx.frozen_claude_md.clone(),
                    ctx.frozen_claude_local_md.clone(),
                    ctx.frozen_skill_summary.clone(),
                );
        let mut chain = MiddlewareChain::new();
        for mw in build_subagent_middlewares(config) {
            chain.add(mw);
        }
        chain
    }
}

#[cfg(test)]
#[path = "tool_test.rs"]
mod tests;
