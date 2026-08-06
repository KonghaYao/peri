//! `/bg` 命令 — 后台 Fork Agent 启动（L5：自 peri-acp/src/host/exec/bg.rs 迁入）。
//!
//! 用户通过 `/bg <任务描述>` 主动发起后台子 Agent，
//! fork 当前会话上下文，使用定制 bg-fork directive 隔离执行。
//! 结果按现有 bg agent 机制自动注入主 Agent 下一轮对话。
//!
//! 本模块只做命令定义（参数解析 / 用法提示 / 错误提示 / 确认消息），
//! fork agent 的实际发起（LLM 构造 / 工具集 / `SessionFactory::spawn_subagent`）
//! 经装配注入的 [`BgForkSpawner`] 调用（实现见 ACP executor 装配面），
//! 命令层不引用业务面实现。

use async_trait::async_trait;
use peri_acp_types::command::{
    AgentCommand, BgForkRequest, CommandContext, CommandKind, CommandResult, PromptStopReason,
};

use super::events::{emit_bg_confirmation, emit_bg_spawn_error, emit_bg_usage_hint};

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
            emit_bg_usage_hint(&ctx.event_sink, &ctx.session_id).await;
            return CommandResult {
                messages: ctx.history,
                stop_reason: PromptStopReason::EndTurn,
            };
        }

        // 装配注入的 spawner（executor 内部路径提供；RPC 直调等缺少装配面的
        // 入口为 None，优雅降级报错，不 panic）。
        let Some(spawner) = ctx.bg_spawner else {
            emit_bg_spawn_error(
                &ctx.event_sink,
                &ctx.session_id,
                "bg_spawner 未配置（/bg 需经 executor 内部路径执行，RPC 直调缺少装配注入面）",
            )
            .await;
            return CommandResult {
                messages: ctx.history,
                stop_reason: PromptStopReason::EndTurn,
            };
        };

        // bg_event_sender / thread_store 是 spawner 的必需项，缺失时无合理
        // fallback 语义，只能报错（RPC 直调入口可传 None，不能用 expect）。
        let Some(bg_event_sender) = ctx.bg_event_sender else {
            emit_bg_spawn_error(
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
        let Some(thread_store) = ctx.thread_store else {
            emit_bg_spawn_error(
                &ctx.event_sink,
                &ctx.session_id,
                "thread_store 未配置（/bg 需经 executor 内部路径执行，RPC 直调缺少持久化存储）",
            )
            .await;
            return CommandResult {
                messages: ctx.history,
                stop_reason: PromptStopReason::EndTurn,
            };
        };

        // 构造纯数据请求（深绑 Agent 层的实现细节在 spawner 内；
        // peri_config 由 spawner 自持，不进入请求契约——L5 依赖反转）。
        let req = BgForkRequest {
            prompt: prompt.clone(),
            parent_messages: ctx.history.clone(),
            parent_thread_id: ctx.thread_id.clone(),
            cwd: ctx.cwd.clone(),
            frozen_claude_md: ctx.frozen_claude_md.as_deref().map(|s| s.to_string()),
            frozen_claude_local_md: ctx.frozen_claude_local_md.as_deref().map(|s| s.to_string()),
            frozen_skill_summary: ctx.frozen_skill_summary.as_deref().map(|s| s.to_string()),
            frozen_system_prompt: ctx.frozen_system_prompt.as_deref().map(|s| s.to_string()),
            bg_event_sender,
            thread_store,
        };

        if let Err(e) = spawner.spawn_fork(req).await {
            emit_bg_spawn_error(&ctx.event_sink, &ctx.session_id, &e).await;
            return CommandResult {
                messages: ctx.history,
                stop_reason: PromptStopReason::EndTurn,
            };
        }

        // 确认消息（CJK-safe truncation: chars().take(80)）
        emit_bg_confirmation(&ctx.event_sink, &ctx.session_id, &prompt).await;

        CommandResult {
            messages: ctx.history,
            stop_reason: PromptStopReason::EndTurn,
        }
    }
}

#[cfg(test)]
#[path = "bg_test.rs"]
mod tests;
