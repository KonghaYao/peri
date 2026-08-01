//! SubAgent 同步 Fork 路径：v2 stages 实现
//!
//! Fork 语义：子 agent 继承父会话消息历史（parent_messages），用 prompt 继续执行。
//! Cancel policy = Cascade（父 cancel 传播到子）。无 agent_def（fork 是直接调用）。

use std::sync::Arc;

use peri_agent::{
    agent::{
        events::ExecutorEvent,
        stages::{run_react_loop, LoopResult},
    },
    messages::BaseMessage,
    middleware::chain::MiddlewareChain,
    thread::ThreadMeta,
};
use tokio_util::sync::CancellationToken;

use super::{
    format_subagent_result,
    lifecycle::{on_subagent_stop_handler, DeregisterGuard},
};
use crate::subagent::{v2_bridge::build_v2_subagent_context, SubAgentMiddlewareConfig};

impl super::SubAgentTool {
    pub(crate) async fn invoke_fork(
        &self,
        prompt: &str,
        cwd: &str,
        parent_messages: Vec<BaseMessage>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let parent_msgs = parent_messages;

        // 1. 创建子线程（cancel_policy=cascade）
        let child_thread_id = uuid::Uuid::now_v7().to_string();
        if let Some(ref store) = self.thread_store {
            let snapshot_id = parent_msgs.last().map(|m| m.id().as_uuid().to_string());
            let mut child_meta = ThreadMeta::new(cwd);
            child_meta.id = child_thread_id.clone();
            child_meta.parent_thread_id = self.parent_thread_id.clone();
            child_meta.snapshot_at_message_id = snapshot_id;
            child_meta.hidden = true;
            child_meta.cancel_policy = "cascade".parse().expect("合法 cancel_policy 字符串");
            child_meta.title = Some("fork".to_string());
            store
                .create_thread(child_meta)
                .await
                .map_err(|e| format!("Failed to create child thread: {}", e))?;
        }

        // 2. Cascade cancel token：父 cancel 传播到子
        let cancel_token: CancellationToken = self
            .cancel
            .as_ref()
            .map(|t| t.child_token())
            .unwrap_or_default();

        // 3. 中间件链（复用 build_subagent_middlewares）
        let mw_config = SubAgentMiddlewareConfig::for_agent_def(Vec::new(), cwd).with_frozen(
            self.frozen_claude_md
                .as_deref()
                .map(|s| s.as_str().to_string()),
            self.frozen_claude_local_md
                .as_deref()
                .map(|s| s.as_str().to_string()),
            self.frozen_skill_summary
                .as_deref()
                .map(|s| s.as_str().to_string()),
        );
        let middlewares = super::build_subagent_middlewares(mw_config);
        let mut chain = MiddlewareChain::new();
        for mw in middlewares {
            chain.add(mw);
        }

        // 4. system prompt（frozen 优先 + system_builder 回退）
        let system_prompt = self
            .frozen_system_prompt
            .clone()
            .map(|sp| sp.as_ref().to_string())
            .or_else(|| self.system_builder.as_ref().map(|b| b(None, cwd)));

        // 5. 工具集：父工具 clone 为 Vec<Arc<dyn BaseTool>>
        let tools: Vec<Arc<dyn peri_agent::tools::BaseTool>> =
            self.parent_tools.iter().cloned().collect();

        // 6. LLM（fork 用默认 provider，无 model alias）
        let mut llm = (self.llm_factory)(None);
        // 注入 event_handler，使 SubAgent 内 LLM 的重试事件能被父级 Langfuse 追踪
        llm.inject_event_handler(self.event_handler.clone());

        // 7. 注册到 active_agents（register_runtime）
        if let Some(register) = &self.register_runtime {
            register(
                child_thread_id.clone(),
                cancel_token.clone(),
                "cascade".into(),
            );
        }

        // RAII guard：scope 退出时自动 deregister
        let _deregister_guard = DeregisterGuard {
            thread_id: child_thread_id.clone(),
            deregister: self.deregister_runtime.clone(),
        };

        // 8. SubagentStarted 事件 + lifecycle hook
        let instance_id = child_thread_id.clone();
        if let Some(ref handler) = self.event_handler {
            handler.on_event(ExecutorEvent::SubagentStarted {
                agent_name: "fork".to_string(),
                instance_id: instance_id.clone(),
                is_background: false,
            });
        }
        self.fire_subagent_lifecycle_hook(
            crate::hooks::types::HookEvent::SubagentStart,
            cwd,
            "fork",
            None,
        )
        .await;

        // 9. 构造 v2 StageContext（fork 注入 parent_msgs 到 transcript）
        let v2_ctx = build_v2_subagent_context(
            llm,
            chain,
            tools,
            cwd,
            cancel_token,
            parent_msgs,
            system_prompt,
            None, // shared_tools
            None, // compact_config
            None, // context_budget
            None, // compact_llm
            None, // error_suggest_registry
            None, // tool_registry_snapshot
        );

        // 9.5. 启动 v2 事件转发器：消费 SubAgent EventBus 的事件，注入 source_agent_id
        // 后转发到父 Agent 的事件处理器。让 TUI 能看到 SubAgent 内的工具调用 / AI 文本 /
        // 推理内容（否则 SubAgent 拥有独立 EventBus，事件全部丢弃）。
        // 必须在 run_react_loop 之前取出 event_handles（之后 v2_ctx.context 被 move）。
        let _forwarder_handle =
            peri_agent::agent::subagent_event_forwarder::spawn_subagent_event_forwarder(
                v2_ctx.event_handles,
                self.event_handler.clone(),
                self.langfuse_bridge.clone(),
                child_thread_id.clone(),
            );

        // 10. push prompt 到 queue（Receive 阶段消费）
        // 套用 fork directive 模板（与 spawner.rs:147-150 的 /bg 路径对齐）：
        // 注入"禁止生成子 agent / 禁止提问 / 输出格式约束"等规则。
        let fork_directive = crate::subagent::fork::build_fork_directive(prompt);
        v2_ctx
            .context
            .session
            .queue
            .push(peri_agent::session::queue::QueuedMessage::new(
                peri_agent::session::queue::MessageKind::Prompt,
                peri_agent::session::queue::MessageSource::UserInput,
                BaseMessage::human(fork_directive),
            ));

        // 11. 运行 v2 ReAct 循环
        let max_iterations = 200;
        let loop_result = run_react_loop(v2_ctx.context, max_iterations).await;

        // 13. 从 transcript 提取最终 AI 文本
        let (final_text, interrupted) = match loop_result {
            LoopResult::Completed => {
                let text = extract_last_ai_text(&v2_ctx.session);
                (text, false)
            }
            LoopResult::Interrupted => (String::new(), true),
            LoopResult::Error(e) => {
                let error_summary = format!("Fork sub-agent execution failed: {}", e);
                let error_result: String = error_summary.chars().take(500).collect();
                on_subagent_stop_handler(
                    &self.event_handler,
                    &self.registered_hooks,
                    &self.thread_store,
                    "fork",
                    &child_thread_id,
                    &error_result,
                    true,
                    cwd,
                )
                .await;
                return Err(error_summary.into());
            }
        };

        // 14. SubagentStopped 事件 + lifecycle hook + thread_store
        let output_summary: String = if interrupted {
            "interrupted".to_string()
        } else {
            final_text.chars().take(500).collect()
        };
        on_subagent_stop_handler(
            &self.event_handler,
            &self.registered_hooks,
            &self.thread_store,
            "fork",
            &child_thread_id,
            &output_summary,
            interrupted,
            cwd,
        )
        .await;

        if interrupted {
            return Ok("Fork sub-agent execution was interrupted".to_string());
        }

        // 复用 format_subagent_result 的格式（构造 AgentOutput）
        let output = peri_agent::agent::react::AgentOutput {
            text: final_text,
            steps: 0,
            tool_calls: Vec::new(),
            stop_reason: None,
            block_continue: None,
        };
        let result_text = format_subagent_result(&output);
        if self.thread_store.is_some() {
            Ok(format!(
                "child_thread_id: {}\n{}",
                child_thread_id, result_text
            ))
        } else {
            Ok(result_text)
        }
    }
}

/// 从 session transcript 提取最后一条非空 AI 消息文本
fn extract_last_ai_text(session: &std::sync::Arc<peri_agent::session::Session>) -> String {
    // P1-11: 委托给 super::extract_last_ai_text（tool/mod.rs 共用实现）
    super::extract_last_ai_text(session)
}
