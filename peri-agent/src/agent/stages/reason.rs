//! Reason 阶段 — LLM 推理
//!
//! 流程：snapshot visible_messages → emit LlmCallStart → before_model →
//!       LLM.generate_reasoning（与 cancel 竞争）→ after_model → emit LlmCallEnd

use super::middleware_runner::{run_after_model, run_before_model, run_on_error};
use super::{ReasonInput, ReasonOutput};
use crate::agent::events_v2::ObserveEvent;
use crate::agent::react::Reasoning;
use crate::error::{AgentError, AgentResult};
use crate::llm::types::StreamingContext;
use crate::messages::MessageId;

/// SSE 流式事件 → EventBus 桥接器。
///
/// LLM 适配器在 SSE 解析过程中通过 AgentEventHandler 发射 ExecutorEvent，
/// 此桥接器将其映射为 RenderEvent/ObserveEvent 并通过 EventBus 推送到 TUI。
struct StreamingEventBridge {
    event_bus: std::sync::Arc<crate::agent::events_v2::EventBus>,
    turn_id: crate::session::turn::TurnId,
    agent_id: crate::group::pipeline::AgentId,
}

impl crate::agent::events::AgentEventHandler for StreamingEventBridge {
    fn on_event(&self, event: crate::agent::events::ExecutorEvent) {
        match event {
            crate::agent::events::ExecutorEvent::TextChunk { chunk, .. } => {
                self.event_bus
                    .emit_render(crate::agent::events_v2::RenderEvent::TextChunk {
                        turn_id: self.turn_id,
                        agent_id: self.agent_id,
                        chunk,
                    });
            }
            crate::agent::events::ExecutorEvent::AiReasoning {
                text,
                source_agent_id,
            } => {
                self.event_bus
                    .emit_render(crate::agent::events_v2::RenderEvent::ThinkingChunk {
                        turn_id: self.turn_id,
                        agent_id: self.agent_id,
                        chunk: text.clone(),
                    });
                self.event_bus.emit_observe(
                    crate::agent::events_v2::ObserveEvent::AiReasoningChunk {
                        turn_id: self.turn_id,
                        agent_id: self.agent_id,
                        text,
                        source_agent_id,
                    },
                );
            }
            _ => {}
        }
    }
}

pub async fn run_reason(input: ReasonInput) -> AgentResult<ReasonOutput> {
    let ctx = &input.context;
    let step = ctx.session.turn.current_step();
    let turn_id = ctx.turn_id();
    let agent_id = ctx.session.agent_id;

    tracing::trace!(step, has_tool_calls = input.has_tool_calls, "Reason 阶段");

    // before_model middleware（goal_middleware / compact_middleware 等在此注入）
    run_before_model(ctx).await?;

    // 取出 messages 快照（避免跨 await 持有 RwLockReadGuard）
    let mut messages_snapshot: Vec<crate::messages::BaseMessage> = ctx.visible_messages();
    // Micro Compact 标记为 truncated 的消息需截断输出内容，而非完整发送给 LLM。
    // 截断策略：只保留前 100 字符 + "[truncated]" 标记。
    for msg in &mut messages_snapshot {
        let is_truncated = {
            let guard = ctx.session.transcript.read();
            guard
                .get_flags(msg.id())
                .map(|f| f.truncated)
                .unwrap_or(false)
        };
        if is_truncated {
            if let Some(truncated_text) = msg.truncated_content(100) {
                *msg = truncated_text;
            }
        }
    }

    // 取出 tools 的 Arc clone（避免跨 await 持有 RwLockReadGuard）
    let tools_owned: Vec<std::sync::Arc<dyn crate::tools::BaseTool>> = {
        let guard = ctx.runtime.tools.read();
        guard.values().cloned().collect()
    };
    let tool_refs: Vec<&dyn crate::tools::BaseTool> =
        tools_owned.iter().map(|t| t.as_ref()).collect();
    // 调试日志：确认工具数量与名称（排查 v2 工具丢失问题）
    tracing::info!(
        step,
        tool_count = tool_refs.len(),
        tool_names = ?tool_refs.iter().map(|t| t.name()).collect::<Vec<_>>(),
        msg_count = messages_snapshot.len(),
        "Reason 阶段：准备调用 LLM"
    );

    // emit LlmCallStart（携带 messages + tools 快照，对齐 v1 Langfuse Generation input）
    let start_messages: std::sync::Arc<Vec<crate::messages::BaseMessage>> =
        std::sync::Arc::new(messages_snapshot.clone());
    let start_tools: Vec<crate::tools::ToolDefinition> =
        tool_refs.iter().map(|t| t.definition()).collect();
    ctx.runtime
        .event_bus
        .emit_observe(ObserveEvent::LlmCallStart {
            turn_id,
            agent_id,
            step,
            messages: start_messages,
            tools: start_tools,
        });

    // emit LlmRequestPayload（Provider 实际请求体，紧随 LlmCallStart 之后）
    //
    // Langfuse Generation input 用：raw_body 携带 Provider-native 完整请求体
    // （含正确工具格式与 system 位置），让 Langfuse UI 显示与 Provider 实际收到的一致。
    // 时序约束：LlmCallStart 必须先到（on_llm_start 建 generation_data 缓存），
    // LlmRequestPayload 紧随其后写 raw_body 字段。
    if let Some(body) = ctx
        .runtime
        .llm
        .build_provider_request_body(&messages_snapshot, &tool_refs)
    {
        ctx.runtime
            .event_bus
            .emit_observe(ObserveEvent::LlmRequestPayload {
                turn_id,
                agent_id,
                step,
                body: std::sync::Arc::new(body),
            });
    }

    // 构造 StreamingContext（桥接 v1 ExecutorEvent → v2 RenderEvent）
    // LLM 适配器在 SSE 解析过程中通过 event_handler 发射 ExecutorEvent，
    // 此 handler 将其映射为 RenderEvent 并通过 EventBus::emit_render 推送到 TUI。
    let message_id = MessageId::new();
    let turn_id = ctx.turn_id();
    let agent_id = ctx.session.agent_id;
    let bridge = std::sync::Arc::new(StreamingEventBridge {
        event_bus: std::sync::Arc::clone(&ctx.runtime.event_bus),
        turn_id,
        agent_id,
    });
    let streaming = Some(StreamingContext {
        event_handler: bridge,
        message_id,
        cancel: tokio_util::sync::CancellationToken::clone(&ctx.session.turn.cancel_token),
    });

    // LLM 调用（与 cancel 竞争）
    let reasoning: Reasoning = tokio::select! {
        biased;
        _ = ctx.session.turn.cancel_token.cancelled() => {
            return Err(AgentError::Interrupted);
        }
        result = ctx.runtime.llm.generate_reasoning(&messages_snapshot, &tool_refs, streaming) => {
            match result {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(
                        step,
                        model = %ctx.runtime.llm.model_name(),
                        error = %e,
                        "LLM generate_reasoning 失败"
                    );
                    // LLM 报错时 emit LlmCallEnd，让消费者可见
                    ctx.runtime.event_bus.emit_observe(ObserveEvent::LlmCallEnd {
                        turn_id,
                        agent_id,
                        step,
                        model: ctx.runtime.llm.model_name(),
                        output: format!("ERROR: {}", e),
                        input_tokens: 0,
                        output_tokens: 0,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                        request_id: None,
                    });
                    // 通过 middleware chain 触发 on_error
                    let _ = run_on_error(ctx, &e).await;
                    return Err(e);
                }
            }
        }
    };

    // emit LlmCallEnd（带 usage 完整字段：input/output + cache_creation/cache_read + request_id）
    // [TRAP] cache_read_input_tokens 必须透传，否则 TUI 命中率始终 0%（v2 重做回归）
    let (in_tok, out_tok, cache_create, cache_read, req_id) = reasoning
        .usage
        .as_ref()
        .map(|u| {
            (
                u.input_tokens as u64,
                u.output_tokens as u64,
                u.cache_creation_input_tokens.unwrap_or(0) as u64,
                u.cache_read_input_tokens.unwrap_or(0) as u64,
                u.request_id.clone(),
            )
        })
        .unwrap_or((0, 0, 0, 0, None));
    // output 与 v1 llm_step.rs:92-93 对齐：优先 final_answer，否则回退到 thought
    let llm_output = reasoning
        .final_answer
        .clone()
        .unwrap_or_else(|| reasoning.thought.clone());
    ctx.runtime
        .event_bus
        .emit_observe(ObserveEvent::LlmCallEnd {
            turn_id,
            agent_id,
            step,
            model: reasoning.model.clone(),
            output: llm_output,
            input_tokens: in_tok,
            output_tokens: out_tok,
            cache_creation_input_tokens: cache_create,
            cache_read_input_tokens: cache_read,
            request_id: req_id,
        });

    // 累积 token_tracker（P0 #2 修复：v2 路径下 token tracker 从未累积）
    if let Some(ref usage) = reasoning.usage {
        ctx.compact.token_tracker.write().accumulate(usage);
    }

    // after_model middleware（hook_middleware / git_attribution 等在此）
    run_after_model(ctx, &reasoning).await?;

    Ok(ReasonOutput {
        reasoning,
        messages_snapshot,
    })
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "reason_test.rs"]
mod tests;
