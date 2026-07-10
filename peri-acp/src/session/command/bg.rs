//! `/bg` 命令 — 后台 Fork Agent 启动。
//!
//! 用户通过 `/bg <任务描述>` 主动发起后台子 Agent，
//! fork 当前会话上下文，使用定制 bg-fork directive 隔离执行。
//! 结果按现有 bg agent 机制自动注入主 Agent 下一轮对话。

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use peri_agent::agent::events::ExecutorEvent;
use peri_agent::messages::MessageId;
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
            ctx.event_sink
                .push_event(
                    &ctx.session_id,
                    &ExecutorEvent::TextChunk {
                        message_id: MessageId::new(),
                        chunk: "用法: /bg <任务描述>\n".into(),
                        source_agent_id: None,
                    },
                    0,
                )
                .await;
            return CommandResult {
                messages: ctx.history,
                stop_reason: PromptStopReason::EndTurn,
            };
        }

        // 构造 LLM 实例（从 peri_config 构建）
        let llm: Box<dyn peri_agent::agent::react::ReactLLM + Send + Sync> =
            match LlmProvider::from_config(&ctx.peri_config) {
                Some(provider) => Box::new(peri_agent::llm::BaseModelReactLLM::new(
                    provider.into_model(),
                )),
                None => {
                    ctx.event_sink
                        .push_event(
                            &ctx.session_id,
                            &ExecutorEvent::TextChunk {
                                message_id: MessageId::new(),
                                chunk: "✗ 后台任务启动失败: 无法构造 LLM 实例（请检查 peri-config.toml 的 Provider 配置）\n".into(),
                                source_agent_id: None,
                            },
                            0,
                        )
                        .await;
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

        // 调用共享 spawner 启动后台 fork agent
        let bg_event_sender = ctx
            .bg_event_sender
            .expect("bg_event_sender 总是 Some（executor 前置创建）");
        let bg_registry = ctx
            .bg_registry
            .expect("bg_registry 总是 Some（executor 前置创建）");

        let spawned = match peri_middlewares::subagent::spawner::spawn_background_fork(
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
                on_bg_complete: None,  // /bg 命令的主 agent 不在 loop，注入无效
                frozen_claude_md: ctx.frozen_claude_md.clone(),
                frozen_claude_local_md: ctx.frozen_claude_local_md.clone(),
                frozen_skill_summary: ctx.frozen_skill_summary.clone(),
            },
        )
        .await
        {
            Ok(s) => s,
            Err(e) => {
                ctx.event_sink
                    .push_event(
                        &ctx.session_id,
                        &ExecutorEvent::TextChunk {
                            message_id: MessageId::new(),
                            chunk: format!("✗ 后台任务启动失败: {e}\n"),
                            source_agent_id: None,
                        },
                        0,
                    )
                    .await;
                return CommandResult {
                    messages: ctx.history,
                    stop_reason: PromptStopReason::EndTurn,
                };
            }
        };

        // 同步推送 SubagentStarted 到 ACP transport——必须在 push_done 之前
        // 完成（本命令是 CommandKind::Immediate，execute 返回后立即 push_done）。
        // 走 event_sink 直接入 TransportEventSink channel（in-memory mpsc），
        // 保证 TUI 端按 FIFO 顺序处理：SubagentStarted 必先先于 Done 到达，
        // handle_subagent_start 先注册 background_agents，handle_done 才能正确
        // 设置 agent_done_pending。
        //
        // 不走 bg_event_sender（pump task 异步推送）——pump 与 push_done 并发，
        // 事件到达 TUI 的顺序无保证（race condition 已通过此同步路径根治）。
        ctx.event_sink
            .push_event(&ctx.session_id, &spawned.started_event, 0)
            .await;

        // 确认消息（CJK-safe truncation: chars().take(80)）
        let truncated: String = prompt.chars().take(80).collect();
        ctx.event_sink
            .push_event(
                &ctx.session_id,
                &ExecutorEvent::TextChunk {
                    message_id: MessageId::new(),
                    chunk: format!("◆ 后台任务已启动: {truncated}\n"),
                    source_agent_id: None,
                },
                0,
            )
            .await;

        CommandResult {
            messages: ctx.history,
            stop_reason: PromptStopReason::EndTurn,
        }
    }
}

#[cfg(test)]
#[path = "bg_test.rs"]
mod tests;
