//! `/bg` 命令 — 后台 Fork Agent 启动。
//!
//! 用户通过 `/bg <任务描述>` 主动发起后台子 Agent，
//! fork 当前会话上下文，使用定制 bg-fork directive 隔离执行。
//! 结果按现有 bg agent 机制自动注入主 Agent 下一轮对话。

mod events;

use std::path::PathBuf;
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
        // bg_event_sender / bg_registry 是 spawn_background_fork 的必需项，缺失时
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
        let Some(bg_registry) = ctx.bg_registry else {
            events::emit_bg_spawn_error(
                &ctx.event_sink,
                &ctx.session_id,
                "bg_registry 未配置（/bg 需经 executor 内部路径执行，RPC 直调缺少后台任务注册中心）",
            )
            .await;
            return CommandResult {
                messages: ctx.history,
                stop_reason: PromptStopReason::EndTurn,
            };
        };

        let _spawned = match peri_middlewares::subagent::spawner::spawn_background_fork(
            peri_middlewares::subagent::spawner::BgForkConfig {
                prompt: prompt.clone(),
                parent_messages: ctx.history.clone(),
                cwd: PathBuf::from(&ctx.cwd),
                llm,
                max_iterations: 200,
                parent_tools,
                registered_hooks: Arc::new(Vec::new()),
                thread_store: ctx.thread_store.clone(),
                parent_thread_id: ctx.thread_id.clone(),
                register_runtime: None,
                deregister_runtime: None,
                bg_event_sender,
                bg_registry,
                fork_directive_kind: peri_middlewares::subagent::spawner::BgForkDirectiveKind::Bg,
                on_bg_complete: None, // /bg 命令的主 agent 不在 loop，注入无效
                frozen_claude_md: ctx.frozen_claude_md.clone(),
                frozen_claude_local_md: ctx.frozen_claude_local_md.clone(),
                frozen_skill_summary: ctx.frozen_skill_summary.clone(),
                frozen_system_prompt: ctx.frozen_system_prompt.clone(),
                langfuse_bridge: None, // /bg 命令无 Langfuse tracer
                parent_agent_id: None, // /bg 命令无父 agent 身份（不 emit v2 Start/Stop）
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
