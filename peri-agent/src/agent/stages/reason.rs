//! Reason 阶段 — LLM 推理
//!
//! 流程：snapshot visible_messages → emit LlmCallStart → before_model →
//!       LLM.generate_reasoning（与 cancel 竞争）→ after_model → emit LlmCallEnd

use super::middleware_runner::{run_after_model, run_before_model, run_on_error};
use super::{ReasonInput, ReasonOutput};
use crate::agent::events_v2::{ObserveEvent, TurnErrorReason};
use crate::agent::react::{Reasoning, StreamingContext};
use crate::error::{AgentError, AgentResult};
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

    // 取出 messages 快照（避免跨 await 持有 RwLockReadGuard）。
    // 直接构建为 Arc<Vec>：LlmCallStart 与 LLM 调用共享同一份，避免二次深拷贝。
    let messages_snapshot: std::sync::Arc<Vec<crate::messages::BaseMessage>> = std::sync::Arc::new(
        {
            let guard = ctx.session.transcript.read();
            let visible: Vec<crate::messages::BaseMessage> =
                guard.visible_messages().into_iter().cloned().collect();

            // 如果有 compact config，生成 plan 并渲染投影视图
            if let Some(ref config) = ctx.compact.compact_config {
                let caps = ctx.runtime.llm.provider_capabilities();
                // 优先使用持久化 directive，避免每 turn 重新规划
                match crate::agent::compact_v2::projection::plan_from_persisted_directives(
                    &guard,
                    crate::agent::compact_v2::PROJECTION_POLICY_VERSION,
                ) {
                    Ok(plan) => {
                        // 持久化 directive 有效 → 直接渲染
                        match crate::agent::compact_v2::projection::render_llm_view(
                            &guard, &plan, &caps,
                        ) {
                            Ok(view) => {
                                tracing::debug!(
                                    action_count = plan.actions.len(),
                                    messages_before = visible.len(),
                                    messages_after = view.len(),
                                    "render_llm_view (persisted directives): 投影后消息数"
                                );
                                view
                            }
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    "render_llm_view (persisted) 失败，fallback 到原始可见消息"
                                );
                                visible
                            }
                        }
                    }
                    Err(e) => {
                        let err_msg = e.to_string();
                        if err_msg.contains(
                            crate::agent::compact_v2::projection::DIRECTIVE_VERSION_MISMATCH,
                        ) {
                            // 版本不匹配 → 重新规划为 v2 directive（skip=false 纳入旧 truncated 消息）
                            // 新 plan 在下次 Compact 阶段持久化后覆盖旧 directive
                            tracing::info!(
                                error = %e,
                                "持久化 directive 版本不匹配，重新规划为 v2"
                            );
                            let plan = crate::agent::compact_v2::planner::plan_micro(
                                &guard, config, false,
                            );
                            if plan.has_changes() {
                                match crate::agent::compact_v2::projection::render_llm_view(
                                    &guard, &plan, &caps,
                                ) {
                                    Ok(view) => view,
                                    Err(render_err) => {
                                        tracing::warn!(
                                            error = %render_err,
                                            "render_llm_view (v2 re-plan) 失败，fallback 原始消息"
                                        );
                                        visible
                                    }
                                }
                            } else {
                                visible
                            }
                        } else {
                            // 无持久化 directive → fallback 到 planner
                            tracing::debug!("无持久化 directive，fallback 到 plan_micro");
                            let plan = crate::agent::compact_v2::planner::plan_micro(
                                &guard, config, false,
                            );
                            if plan.has_changes() {
                                match crate::agent::compact_v2::projection::render_llm_view(
                                    &guard, &plan, &caps,
                                ) {
                                    Ok(view) => {
                                        tracing::debug!(
                                            action_count = plan.actions.len(),
                                            messages_before = visible.len(),
                                            messages_after = view.len(),
                                            "render_llm_view (planner): 投影后消息数"
                                        );
                                        view
                                    }
                                    Err(render_err) => {
                                        tracing::warn!(
                                            error = %render_err,
                                            "render_llm_view (planner) 失败，fallback 到原始可见消息"
                                        );
                                        visible
                                    }
                                }
                            } else {
                                visible
                            }
                        }
                    }
                }
            } else {
                visible
            }
        },
    );

    // 取出 tools 的 Arc clone（避免跨 await 持有 RwLockReadGuard）
    let tools_owned: Vec<std::sync::Arc<dyn crate::tools::BaseTool>> = {
        let guard = ctx.runtime.tools.read();
        guard.values().cloned().collect()
    };
    let tool_refs: Vec<&dyn crate::tools::BaseTool> = tools_owned
        .iter()
        .filter(|t| t.is_direct())
        .map(|t| t.as_ref())
        .collect();
    // 工具数量与名称追踪（调试用；默认 filter 下不写盘）
    tracing::debug!(
        step,
        tool_count = tool_refs.len(),
        tool_names = ?tool_refs.iter().map(|t| t.name()).collect::<Vec<_>>(),
        msg_count = messages_snapshot.len(),
        "Reason 阶段：准备调用 LLM"
    );

    // emit LlmCallStart（携带 messages + tools 快照，对齐 v1 Langfuse Generation input）
    // messages 为 Arc 浅拷贝，与下方 LLM 调用共享同一份快照
    let start_tools: Vec<crate::tools::ToolDefinition> =
        tool_refs.iter().map(|t| t.definition()).collect();
    ctx.runtime
        .event_bus
        .emit_observe(ObserveEvent::LlmCallStart {
            turn_id,
            agent_id,
            step,
            messages: messages_snapshot.clone(),
            tools: start_tools,
        });

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

    // LLM 调用（与 cancel 竞争）。
    // 使用 generate_reasoning_with_observed_body：观测体复用本次调用已构建的
    // request（消除每轮 request 双构建），LlmRequestPayload 在成功后、LlmCallEnd
    // 之前 emit——Langfuse 按 step 缓存 raw_body，时序兼容（on_llm_end 前到达即可）。
    let (reasoning, observed_body): (Reasoning, Option<serde_json::Value>) = tokio::select! {
        biased;
        _ = ctx.session.turn.cancel_token.cancelled() => {
            return Err(AgentError::Interrupted);
        }
        result = ctx.runtime.llm.generate_reasoning_with_observed_body(
            &messages_snapshot,
            &tool_refs,
            streaming,
        ) => {
            match result {
                Ok((r, body)) => (r, body),
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
                    // TurnError：通知 TUI 显示错误 SystemNote（v2_bridge → AgentExecutionFailed → 红色消息）
                    let reason = match &e {
                        AgentError::LlmHttpError { .. } | AgentError::LlmError(..) => {
                            TurnErrorReason::LlmFailure
                        }
                        _ => TurnErrorReason::LlmFailure,
                    };
                    ctx.runtime.event_bus.emit_observe(ObserveEvent::TurnError {
                        turn_id,
                        agent_id,
                        reason,
                        message: e.to_string(),
                    });
                    // 通过 middleware chain 触发 on_error
                    let _ = run_on_error(ctx, &e).await;
                    return Err(e);
                }
            }
        }
    };

    // emit LlmRequestPayload（仅发送 Model 的安全 observation body；复用本次
    // LLM 调用已构建的 request，见 generate_reasoning_with_observed_body）
    if let Some(body) = observed_body {
        ctx.runtime
            .event_bus
            .emit_observe(ObserveEvent::LlmRequestPayload {
                turn_id,
                agent_id,
                step,
                body: std::sync::Arc::new(body),
            });
    }

    // emit LlmCallEnd（带 usage 完整字段：input/output + cache_creation/cache_read + request_id）
    // [TRAP] cache_read_input_tokens 必须透传，否则 TUI 命中率始终 0%（v2 重做回归）
    let (in_tok, out_tok, cache_create, cache_read) = reasoning
        .usage
        .as_ref()
        .map(|u| {
            (
                u.input_tokens as u64,
                u.output_tokens as u64,
                u.cache_creation_input_tokens.unwrap_or(0) as u64,
                u.cache_read_input_tokens.unwrap_or(0) as u64,
            )
        })
        .unwrap_or((0, 0, 0, 0));
    // request_id 与 usage 来源独立（provider 可能不返回 usage 但返回 request_id），
    // 不得随 usage 的 unwrap_or 默认值一起丢弃
    let req_id = reasoning.request_id.clone();
    // output 改为结构化 JSON：包含 text、thinking、tool_calls、stop_reason
    // 与 v1 llm_step.rs:92-93 对齐：优先 final_answer，否则回退到 thought 作为 text
    let llm_output = {
        let text = reasoning
            .final_answer
            .clone()
            .unwrap_or_else(|| reasoning.thought.clone());
        // thinking 从 source_message 的 Reasoning block 提取，fallback 到 reasoning.thought
        let thinking = reasoning
            .source_message
            .as_ref()
            .and_then(|msg| {
                msg.content_blocks()
                    .iter()
                    .find_map(|b| b.as_reasoning().map(|s| s.to_string()))
            })
            .unwrap_or_else(|| reasoning.thought.clone());
        let output_value = serde_json::json!({
            "text": text,
            "thinking": thinking,
            "tool_calls": reasoning.tool_calls.iter().map(|tc| serde_json::json!({
                "id": tc.id,
                "name": tc.name,
                "input": tc.input,
            })).collect::<Vec<_>>(),
            "stop_reason": crate::agent::model_bridge::stop_reason_display(&reasoning.stop_reason),
        });
        serde_json::to_string(&output_value).unwrap_or_else(|_| {
            // fallback: 保留纯文本行为
            reasoning
                .final_answer
                .clone()
                .unwrap_or_else(|| reasoning.thought.clone())
        })
    };
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
