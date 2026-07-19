//! EventBus 转发器（v2 → ExecutorEvent）公共抽取。
//!
//! 主 executor（`session/executor.rs`）与 workflow agent（`agent/workflow_agent.rs`）
//! 原先各自维护一份完全相同的 `tokio::spawn` + biased `select!` 循环，仅 "消费 ExecutorEvent
//! 后做什么" 不同。本模块封装循环骨架，调用方通过 `on_event: F` 闭包注入目标行为：
//!
//! - 主 executor 端：`|ev| { event_tx.send(ev) }`（投递到 mpsc，由 `spawn_event_pump` 消费）
//! - workflow_agent 端：`|ev| { event_handler.on_event(ev) }`（直接调用 handler）
//!
//! ## 关键不变量（修改者必读）
//!
//! - **render 通道（含 TurnCompleted）必须先于 State 通道被消费**：跨通道 biased `select!`
//!   只保证单次迭代内的优先级，不保证跨迭代——若 iter2 的 TextChunk 先于 iter1 的
//!   TurnCompleted 被消费，会污染 partial，渲染出 "新文本在旧工具之前" 的错乱。因此
//!   `biased` 指令不可移除，`render_rx` 分支必须排在 `state_rx` 之前。
//! - **observe_rx 使用 broadcast channel**：需同时处理 `Lagged`（仅 warn，不 panic）与
//!   `Closed`（break 退出）。
//! - **三通道全部关闭时 task 自动退出**：`else => break` 防止 task 泄漏。

use peri_agent::agent::events::ExecutorEvent;
use peri_agent::agent::events_v2::{EventHandles, ObserveEvent, RenderEvent, StateEvent};
use peri_agent::agent::events_v2_mapper::V2Event;
use peri_agent::llm::types::TokenUsage;

use crate::langfuse::tracer::stages::StageHandle;
use crate::langfuse::tracer::LangfuseTracer;

use super::{observe_event_to_executor, render_event_to_executor, state_event_to_executor};

// ── Langfuse v2 辅助转发函数 ──────────────────────────────────────────────

/// 从 RenderEvent 转发 Langfuse 追踪事件。
///
/// 处理 TextChunk（累积 final_answer）与 BudgetWarning（近似映射为 budget threshold hit）。
/// `tracer` 为 `None`（遥测禁用）时立即返回。
fn forward_langfuse_render(
    tracer: &Option<std::sync::Arc<parking_lot::Mutex<LangfuseTracer>>>,
    ev: &RenderEvent,
) {
    let Some(ref tracer) = tracer else {
        return;
    };
    match ev {
        RenderEvent::TextChunk { chunk, .. } => {
            tracer.lock().on_text_chunk(chunk);
        }
        RenderEvent::BudgetWarning {
            percentage,
            used_tokens,
            total_tokens,
            ..
        } => {
            tracer.lock().on_budget_threshold_hit(
                "context_window",
                *percentage,
                *used_tokens,
                *total_tokens,
            );
        }
        _ => {}
    }
}

/// 从 StateEvent 转发 Langfuse 追踪事件。
///
/// 当前为 no-op：StateEvent 无对应的 Langfuse tracer 方法。
fn forward_langfuse_state(
    _tracer: &Option<std::sync::Arc<parking_lot::Mutex<LangfuseTracer>>>,
    _ev: &StateEvent,
) {
    // no-op: StateEvent 无 Langfuse 映射
}

/// 从 ObserveEvent 转发 Langfuse 追踪事件。
///
/// 处理 LlmCallStart/End/Payload、CompactStarted/MessagesCompacted、
/// StageStarted/Ended、MessageQueueDrained、AiReasoningChunk。
///
/// `active_stage` 用于在 StageStarted/StageEnded 间传递 `StageHandle`，
/// 实现 stage span 的条件上报语义。
fn forward_langfuse_observe(
    tracer: &Option<std::sync::Arc<parking_lot::Mutex<LangfuseTracer>>>,
    ev: &ObserveEvent,
    provider_display_name: &str,
    active_stage: &mut Option<StageHandle>,
) {
    let Some(ref tracer) = tracer else {
        return;
    };
    match ev {
        ObserveEvent::LlmCallStart {
            step,
            messages,
            tools,
            ..
        } => {
            tracer.lock().on_llm_start(*step, messages, tools);
        }
        ObserveEvent::LlmCallEnd {
            step,
            model,
            output,
            input_tokens,
            output_tokens,
            cache_creation_input_tokens,
            cache_read_input_tokens,
            request_id,
            ..
        } => {
            let usage = TokenUsage {
                input_tokens: *input_tokens as u32,
                output_tokens: *output_tokens as u32,
                cache_creation_input_tokens: if *cache_creation_input_tokens > 0 {
                    Some(*cache_creation_input_tokens as u32)
                } else {
                    None
                },
                cache_read_input_tokens: if *cache_read_input_tokens > 0 {
                    Some(*cache_read_input_tokens as u32)
                } else {
                    None
                },
                request_id: request_id.clone(),
            };
            tracer
                .lock()
                .on_llm_end(*step, model, provider_display_name, output, Some(&usage));
        }
        ObserveEvent::LlmRequestPayload { step, body, .. } => {
            tracer
                .lock()
                .on_llm_request_payload(*step, std::sync::Arc::clone(body));
        }
        ObserveEvent::CompactStarted { .. } => {
            tracer.lock().on_compact_start();
        }
        ObserveEvent::MessagesCompacted {
            summary,
            files,
            skills,
            ..
        } => {
            tracer.lock().on_compact_end(
                summary,
                files.len(),
                skills.len(),
                0,     // micro_cleared: v2 无此字段，用 0
                false, // is_error
                "",
            );
        }
        ObserveEvent::StageStarted { stage, turn_id, .. } => {
            let (trace_id, agent_observation_id) = {
                let t = tracer.lock();
                (t.trace_id.clone(), t.agent_observation_id.clone())
            };
            let mut t = tracer.lock();
            let handle = t.stages.on_stage_start(
                *stage,
                &trace_id,
                &turn_id.to_string(),
                &agent_observation_id,
            );
            *active_stage = Some(handle);
        }
        ObserveEvent::StageEnded { status, .. } => {
            if let Some(handle) = active_stage.take() {
                tracer.lock().on_stage_end(&handle, *status);
            }
        }
        ObserveEvent::MessageQueueDrained {
            prompt,
            defer,
            info,
            ..
        } => {
            tracer.lock().on_mq_drained(*prompt, *defer, *info);
        }
        ObserveEvent::AiReasoningChunk { text, .. } => {
            tracer.lock().on_ai_reasoning_chunk(text);
        }
        // TurnError / SubagentStart / SubagentStop → 无 v1 Langfuse 映射，
        // 通过 v2→v1 mapper 产生 ExecutorEvent 到达 pump tail。
        _ => {}
    }
}

/// 启动 EventBus forwarder task。
///
/// 消费 `handles` 内三层 v2 事件（render / state / observe），经 `*_event_to_executor`
/// 映射为 [`ExecutorEvent`]，然后调用 `on_event` 闭包投递到调用方指定的目标。
///
/// # 参数
///
/// - `handles`：v2 [`EventHandles`]（调用方取出所有权后传入，本函数内部 `mut` 消费）
/// - `on_event`：每条映射后的 `ExecutorEvent` 的消费闭包。签名 `Fn(ExecutorEvent) + Send + Sync + 'static`
///
/// # 返回
///
/// forwarder task 的 [`tokio::task::JoinHandle`]。调用方可持有以控制生命周期，也可
/// fire-and-forget（task 在三通道全部关闭时自动退出）。
///
/// # 不变量
///
/// 见模块顶部文档——biased select 顺序、render 先于 state、observe Lagged 容错。
pub fn spawn_eventbus_forwarder<F>(
    mut handles: EventHandles,
    on_event: F,
    langfuse_tracer: Option<std::sync::Arc<parking_lot::Mutex<LangfuseTracer>>>,
    provider_display_name: String,
) where
    F: Fn(ExecutorEvent) + Send + Sync + 'static,
{
    tokio::spawn(async move {
        let mut active_stage: Option<StageHandle> = None;
        loop {
            // biased + render 优先：保证 Render 通道（含 TurnCompleted）先于 State 通道
            // 被消费，否则 partial 污染（详见模块顶部不变量注释）。
            tokio::select! {
                biased;
                Some(ev) = handles.render_rx.recv() => {
                    // [Fix] Phase A 双轨迁移期：render 事件（TextChunk/ToolStarted 等）
                    // 不通过 v2_channel 扇出——ACP 路径（render_event_to_executor → event_tx
                    // → session/update → acp_notifier → bridge_tx）已完整覆盖所有 render 事件。
                    // 双轨扇出导致同一事件被 bridge_tx 接收两次，TextChunk 的 append_text 无
                    // 去重保护，产生流式期间 md 重复渲染（文本以字节偏移交错重复）。
                    // Langfuse v2: render 层追踪（TextChunk, BudgetWarning）
                    forward_langfuse_render(&langfuse_tracer, &ev);
                    if let Some(exec_ev) = render_event_to_executor(ev) {
                        on_event(exec_ev);
                    }
                }
                Some(ev) = handles.state_rx.recv() => {
                    crate::event::v2_channel::try_send_v2_event(V2Event::from_state(ev.clone()));
                    // Langfuse v2: state 层追踪（当前 no-op）
                    forward_langfuse_state(&langfuse_tracer, &ev);
                    if let Some(exec_ev) = state_event_to_executor(ev) {
                        on_event(exec_ev);
                    }
                }
                ev_res = handles.observe_rx.recv() => {
                    match ev_res {
                        Ok(ev) => {
                            // SubAgent 事件（SubagentStart/SubagentStop）不在 v2_bridge 映射。
                            // 规范路径：on_event → event_sink → peri/agent_event → acp_notifier → bridge_tx。
                            // 此处 try_send_v2_event 若与 on_event 同时发送同一 SubAgent 事件会造成双重发送陷阱。
                            // v2_bridge.rs 有意对这些变体返回 None 作为防御性兜底。
                            crate::event::v2_channel::try_send_v2_event(V2Event::from_observe(ev.clone()));
                            // Langfuse v2: observe 层追踪（LLM/Tool/Stage/Compact）
                            forward_langfuse_observe(&langfuse_tracer, &ev, &provider_display_name, &mut active_stage);
                            if let Some(exec_ev) = observe_event_to_executor(ev) {
                                on_event(exec_ev);
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(
                                n,
                                "[eventbus-forwarder] observe_rx lagged, events dropped"
                            );
                        }
                    }
                }
                else => break,
            }
        }
    });
}
