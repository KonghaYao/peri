//! `/bg` 命令 — 后台 Fork Agent 启动。
//!
//! 用户通过 `/bg <任务描述>` 主动发起后台子 Agent，
//! fork 当前会话上下文，使用定制 bg-fork directive 隔离执行。
//! 结果按现有 bg agent 机制自动注入主 Agent 下一轮对话。

mod events;

use std::sync::Arc;

use async_trait::async_trait;
use peri_middlewares::prelude::*;
use peri_middlewares::tools::BoxToolWrapper;

use super::{AgentCommand, CommandContext, CommandKind, CommandResult};
use crate::provider::LlmProvider;
use crate::session::executor::PromptStopReason;

/// `/bg <prompt>` 命令。
pub struct BgCommand;

impl BgCommand {
    pub const NAME: &'static str = "bg";
}

#[async_trait]
impl AgentCommand for BgCommand {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn aliases(&self) -> Vec<&str> {
        vec!["background"]
    }

    fn description(&self) -> &str {
        "Fork 当前会话启动后台子 Agent 执行独立任务"
    }

    fn kind(&self) -> CommandKind {
        CommandKind::Immediate
    }

    async fn execute(&self, ctx: CommandContext) -> CommandResult {
        let prompt = ctx.args.trim().to_string();

        // 空参数：返回用法提示
        if prompt.is_empty() {
            events::emit_bg_usage_hint(&ctx.event_sink, &ctx.session_id).await;
            return CommandResult {
                messages: ctx.history,
                stop_reason: PromptStopReason::EndTurn,
            };
        }

        // 构造 LLM 实例（从 peri_config 构建）
        let llm: Box<dyn peri_agent::agent::react::ReactLLM + Send + Sync> =
            match LlmProvider::from_config(&ctx.peri_config) {
                Some(provider) => Box::new(peri_agent::agent::model_bridge::AgentModelBridge::new(
                    Arc::from(provider.into_model()),
                )),
                None => {
                    events::emit_bg_llm_error(&ctx.event_sink, &ctx.session_id).await;
                    return CommandResult {
                        messages: ctx.history,
                        stop_reason: PromptStopReason::EndTurn,
                    };
                }
            };

        // 构造父工具集（文件系统 + 终端 + Web = Read/Write/Edit/Bash/Grep/Glob/WebFetch/WebSearch）
        // NOTE: MCP tools are intentionally excluded because:
        // 1. Background workers should not depend on external MCP servers that may be unavailable
        // 2. MCP tools may require interactive approval, which doesn't work for background agents
        // 3. Core filesystem + terminal + web tools cover the majority of background task use cases
        let parent_tools: Arc<Vec<Arc<dyn peri_agent::tools::BaseTool>>> = {
            let mut tools: Vec<Box<dyn peri_agent::tools::BaseTool>> =
                FilesystemMiddleware::build_tools(&ctx.cwd);
            tools.extend(TerminalMiddleware::build_tools(&ctx.cwd));
            tools.extend(WebMiddleware::build_tools());
            Arc::new(
                tools
                    .into_iter()
                    .map(|t| Arc::new(BoxToolWrapper(t)) as Arc<dyn peri_agent::tools::BaseTool>)
                    .collect(),
            )
        };

        // 调用共享 spawner 启动后台 fork agent。
        // 两个字段是公开 RPC（session/execute-command / session/rewind）可传 None 的
        // 入口（dispatch/execute_command.rs 与 dispatch/rewind.rs 均 Option 直传），
        // 不能用 expect——外部调用方传 None 会 panic 并可能崩掉整个 server task。
        // 优雅降级：emit 错误提示后返回（/bg 命令报错返回，不 panic）。
        // bg_event_sender / task_manager 是 spawn_background_fork 的必需项，缺失时
        // 无合理 fallback 语义，只能报错。
        let Some(bg_event_sender) = ctx.bg_event_sender else {
            events::emit_bg_spawn_error(
                &ctx.event_sink,
                &ctx.session_id,
                "bg_event_sender 未配置（/bg 需经 executor 内部路径执行，RPC 直调缺少后台事件通道）",
            )
            .await;
            return CommandResult {
                messages: ctx.history,
                stop_reason: PromptStopReason::EndTurn,
            };
        };
        let Some(task_manager) = ctx.task_manager else {
            events::emit_bg_spawn_error(
                &ctx.event_sink,
                &ctx.session_id,
                "task_manager 未配置（/bg 需经 executor 内部路径执行，RPC 直调缺少后台任务管理器）",
            )
            .await;
            return CommandResult {
                messages: ctx.history,
                stop_reason: PromptStopReason::EndTurn,
            };
        };

        // 并发限制（迁移前由 spawn_background_fork 内部预检，错误文案保持）
        if task_manager.active_count() >= 3 {
            events::emit_bg_spawn_error(
                &ctx.event_sink,
                &ctx.session_id,
                "已有 3 个后台任务在运行",
            )
            .await;
            return CommandResult {
                messages: ctx.history,
                stop_reason: PromptStopReason::EndTurn,
            };
        }

        // L3：/bg 经 Agent 层统一入口 spawn_subagent（parent 缺失：无主 session 对象，
        // 父侧数据经 config 显式携带；frozen 数据来自 executor 注入的冻结值，不重读磁盘）。
        let host = peri_agent::session::subagent::SubagentHost {
            thread_store: ctx.thread_store.clone(),
            task_manager: Some(Arc::clone(&task_manager)),
            bg_event_sender: Some(bg_event_sender),
            on_bg_complete: None, // /bg 命令的主 agent 不在 loop，注入无效
            register_runtime: None,
            deregister_runtime: None,
            langfuse_bridge: None, // /bg 命令无 Langfuse tracer
            frozen_claude_local_md: ctx.frozen_claude_local_md.clone(),
            frozen_system_prompt: ctx.frozen_system_prompt.clone(),
            parent_thread_id: ctx.thread_id.clone(),
            frozen_claude_md: ctx.frozen_claude_md.clone(),
            frozen_skill_summary: ctx.frozen_skill_summary.clone(),
        };
        let _spawned = match peri_agent::session::subagent::SessionFactory::spawn_subagent(
            None,
            peri_agent::session::subagent::SubagentSpawnConfig {
                agent_name: "fork".to_string(),
                prompt: prompt.clone(),
                parent_messages: ctx.history.clone(),
                cancel_policy: peri_agent::session::subagent::SubagentCancelPolicy::Independent,
                max_iterations: 200,
                fork_directive_kind: Some(peri_agent::session::subagent::ForkDirectiveKind::Bg),
                run_mode: peri_agent::session::subagent::SubagentRunMode::Background,
                skill_names: Vec::new(),
                llm,
                chain_assembler: Arc::new(peri_middlewares::subagent::SubagentChainAssemblerImpl),
                tools: parent_tools
                    .iter()
                    .cloned()
                    .collect::<Vec<Arc<dyn peri_agent::tools::BaseTool>>>(),
                system_prompt: ctx.frozen_system_prompt.as_ref().map(|s| s.to_string()),
                error_suggest_registry: None,
                tool_registry_snapshot: None,
                tool_invocation_resolver: Some(Arc::new(
                    peri_middlewares::tool_search::ExecuteExtraToolResolver::default(),
                )),
                compact_config: None,
                context_budget: None,
                compact_llm: None,
                thread_store: ctx.thread_store.clone(),
                event_handler: None,
                bg_event_sender: Some(host.bg_event_sender.clone().unwrap()),
                task_manager: Some(Arc::clone(&task_manager)),
                on_bg_complete: None,
                langfuse_bridge: None,
                on_subagent_start: None,
                on_subagent_stop: None,
                register_runtime: None,
                deregister_runtime: None,
                parent_agent_id: None, // /bg 命令无父 agent 身份（不 emit v2 Start/Stop）
                cancel_token: None,    // /bg 独立任务，Independent 策略内部新建
                cwd: Some(ctx.cwd.clone()),
                parent_thread_id: ctx.thread_id.clone(),
                frozen_claude_md: ctx.frozen_claude_md.as_deref().map(|s| s.to_string()),
                frozen_claude_local_md: ctx
                    .frozen_claude_local_md
                    .as_deref()
                    .map(|s| s.to_string()),
                frozen_skill_summary: ctx.frozen_skill_summary.as_deref().map(|s| s.to_string()),
                frozen_date: None,
            },
        )
        .await
        {
            Ok(s) => s,
            Err(e) => {
                events::emit_bg_spawn_error(&ctx.event_sink, &ctx.session_id, &e).await;
                return CommandResult {
                    messages: ctx.history,
                    stop_reason: PromptStopReason::EndTurn,
                };
            }
        };

        // P2：v1 SubagentStarted 已移入 spawner 任务内（gate 放行后）经
        // bg_event_sender 发送（bg pump → event_sink），此处不再同步推送——
        // 消除"任务快速完成/被 cancel 时 Stop 先于 Start 到达"的窗口。

        // 确认消息（CJK-safe truncation: chars().take(80)）
        events::emit_bg_confirmation(&ctx.event_sink, &ctx.session_id, &prompt).await;

        CommandResult {
            messages: ctx.history,
            stop_reason: PromptStopReason::EndTurn,
        }
    }
}

#[cfg(test)]
#[path = "bg_test.rs"]
mod tests;
