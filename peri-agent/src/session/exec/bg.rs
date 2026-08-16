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
//!
//! 反馈契约（Phase 5 Step 2）：三处反馈（用法 / 错误 / 启动确认）收敛为
//! [`CommandFeedback`]（UiOnly，不进会话），不再伪装 `ExecutorEvent::TextChunk`
//! 伪消息段（设计文档 §79/§81）；spawner 依赖经 [`CommandContext::dep`] 按
//! `Arc<dyn BgForkSpawner>` 接口获取（Phase 2 拆层注入），缺失优雅报错。

use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::command::{
    BgForkRequest, BgForkSpawner, CommandContext, CommandFeedback, CommandHandler, CommandOutcome,
    CommandResult, FeedbackChannel, FeedbackLevel, PromptStopReason,
};

/// `/bg <prompt>` 命令。
#[derive(Default)]
pub struct BgCommand;

impl BgCommand {
    pub const NAME: &'static str = "bg";
    /// 别名（注册条目挂载，命令声明单一事实源；旧 AgentCommand impl 已删）。
    pub const ALIASES: &'static [&'static str] = &["background"];
    /// 描述（注册条目挂载）。
    pub const DESCRIPTION: &'static str = "Fork 当前会话启动后台子 Agent 执行独立任务";
}

#[async_trait]
impl CommandHandler for BgCommand {
    async fn execute(&self, ctx: CommandContext) -> CommandOutcome {
        // free-form prompt：不声明结构化参数（ArgsSchema::default()），
        // 参数原文经 ctx.args 获取（解析器统一 trim，语义与现状一致）。
        let prompt = ctx.args.trim().to_string();

        // 空参数：返回用法反馈（UiOnly，不进会话）
        if prompt.is_empty() {
            return CommandOutcome::Done(CommandResult {
                messages: ctx.history,
                stop_reason: PromptStopReason::EndTurn,
                feedback: Some(CommandFeedback {
                    level: FeedbackLevel::Info,
                    message: "用法: /bg <任务描述>".into(),
                    channel: FeedbackChannel::UiOnly,
                }),
            });
        }

        // 装配注入的 spawner（executor 内部路径经 Phase 2 拆层 deps 注入；
        // RPC 直调等缺少装配面的入口为 None，优雅降级报错，不 panic）。
        let Some(spawner) = ctx.dep::<Arc<dyn BgForkSpawner>>() else {
            return CommandOutcome::Done(CommandResult {
                messages: ctx.history,
                stop_reason: PromptStopReason::EndTurn,
                feedback: Some(CommandFeedback {
                    level: FeedbackLevel::Error,
                    message: "bg_spawner 未配置（/bg 需经 executor 内部路径执行，RPC 直调缺少装配注入面）"
                        .into(),
                    channel: FeedbackChannel::UiOnly,
                }),
            });
        };

        // bg_event_sender / thread_store 是 spawner 的必需项，缺失时无合理
        // fallback 语义，只能报错（RPC 直调入口可传 None，不能用 expect）。
        let Some(bg_event_sender) = ctx.bg_event_sender else {
            return CommandOutcome::Done(CommandResult {
                messages: ctx.history,
                stop_reason: PromptStopReason::EndTurn,
                feedback: Some(CommandFeedback {
                    level: FeedbackLevel::Error,
                    message: "bg_event_sender 未配置（/bg 需经 executor 内部路径执行，RPC 直调缺少后台事件通道）"
                        .into(),
                    channel: FeedbackChannel::UiOnly,
                }),
            });
        };
        let Some(thread_store) = ctx.thread_store else {
            return CommandOutcome::Done(CommandResult {
                messages: ctx.history,
                stop_reason: PromptStopReason::EndTurn,
                feedback: Some(CommandFeedback {
                    level: FeedbackLevel::Error,
                    message: "thread_store 未配置（/bg 需经 executor 内部路径执行，RPC 直调缺少持久化存储）"
                        .into(),
                    channel: FeedbackChannel::UiOnly,
                }),
            });
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
            return CommandOutcome::Done(CommandResult {
                messages: ctx.history,
                stop_reason: PromptStopReason::EndTurn,
                feedback: Some(CommandFeedback {
                    level: FeedbackLevel::Error,
                    message: e,
                    channel: FeedbackChannel::UiOnly,
                }),
            });
        }

        // 确认消息（CJK-safe truncation: chars().take(80)）
        let truncated: String = prompt.chars().take(80).collect();
        CommandOutcome::Done(CommandResult {
            messages: ctx.history,
            stop_reason: PromptStopReason::EndTurn,
            feedback: Some(CommandFeedback {
                level: FeedbackLevel::Info,
                message: format!("◆ 后台任务已启动: {truncated}"),
                channel: FeedbackChannel::UiOnly,
            }),
        })
    }
}

#[cfg(test)]
#[path = "bg_test.rs"]
mod tests;
