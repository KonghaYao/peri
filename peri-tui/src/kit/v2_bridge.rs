//! v2 事件直连桥接——消费 v2 RenderEvent/StateEvent/ObserveEvent，映射为 AcpEventData
//! 后推入现有的 bridge_tx 通道（复用 spawn_acp_bridge 的分发与渲染逻辑）。
//!
//! Phase A：与 ACP 路径双轨运行。Phase B 下线 ACP 路径后本模块成为唯一事件源。
//!
//! ## 有意排除的 ObserveEvent 变体
//!
//! SubAgent 生命周期事件（SubagentStart、SubagentStop）不在 v2_bridge 中映射。
//! 其活跃路径为：
//!   handler.on_event(ExecutorEvent) → event_sink → peri/agent_event → acp_notifier → bridge_tx
//!
//! ObserveEvent::SubagentStart / SubagentStop 在生产代码中从不被 emit
//!（参见 2026-07-16 eventbus 统一发射架构——EventBus 在 SubagentStarted 之后创建，存在时序死结）。
//! v2_event_to_acp_event_data 中这些变体返回 None，作为防御性兜底——防止未来
//! forwarder.rs 的 try_send_v2_event 与 handler.on_event 两条路径同时触发造成双重发送。

use crate::kit::acp_types::{AcpEventData, AcpEventWithEpoch};
use crate::kit::atoms::{CONTEXT_USAGE, RENDER_HEARTBEAT};
use peri_acp::event::V2Event;
use peri_acp_types::event_data::BudgetWarning;
use peri_agent::agent::events_v2::{ObserveEvent, RenderEvent, StateEvent};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// 映射单个 v2 事件到 AcpEventData。返回 None 表示该事件不需要推送到 bridge
/// （如 HitlPending 走 ACP 独立通道、SubAgent 事件走 event_sink→acp_notifier 路径、
/// Langfuse-only 事件如 LlmCallStart）。
fn v2_event_to_acp_event_data(event: V2Event) -> Option<AcpEventData> {
    match event {
        V2Event::Render(ev) => match ev {
            RenderEvent::TextChunk { chunk, .. } => Some(AcpEventData::TextChunk(
                crate::kit::stream_data::TuiTextChunk {
                    text: chunk,
                    message_id: None,
                    agent_id: None,
                },
            )),
            RenderEvent::ThinkingChunk { chunk, .. } => Some(AcpEventData::ReasoningChunk(
                crate::kit::stream_data::TuiReasoningChunk {
                    text: chunk,
                    message_id: None,
                    agent_id: None,
                },
            )),
            RenderEvent::ToolStarted {
                tool_call_id,
                name,
                input,
                ..
            } => {
                let input_summary = crate::truncate::summarize_input(&name, &input);
                Some(AcpEventData::ToolStarted(
                    crate::kit::stream_data::TuiToolStarted {
                        tool_id: tool_call_id,
                        tool_name: name,
                        input_summary,
                        agent_id: None,
                    },
                ))
            }
            RenderEvent::ToolEnded {
                tool_call_id,
                output,
                is_error,
                ..
            } => Some(AcpEventData::ToolEnded(
                crate::kit::stream_data::TuiToolEnded {
                    tool_id: tool_call_id,
                    output_summary: output,
                    is_error,
                    agent_id: None,
                },
            )),
            RenderEvent::BudgetWarning {
                used_tokens,
                total_tokens,
                percentage,
                ..
            } => Some(AcpEventData::BudgetWarning(BudgetWarning {
                used: used_tokens,
                limit: total_tokens,
                threshold: format!("{:.2}", percentage),
            })),
            // HitlPending 走 ACP RequestPermission 独立通道，不经过 bridge
            RenderEvent::HitlPending { .. } => None,
            RenderEvent::TurnCompleted {
                finalized_messages,
                steps,
                ..
            } => {
                let messages_json = serde_json::to_string(&*finalized_messages).ok()?;
                Some(AcpEventData::TurnCommitted {
                    messages_json,
                    steps,
                })
            }
        },
        V2Event::State(ev) => match ev {
            // StateSnapshot 由 process_v2_event 处理副作用（写 CONTEXT_USAGE），不推 bridge
            StateEvent::StateSnapshot { .. } => None,
            StateEvent::SyntheticUserMessage { text, .. } => {
                Some(AcpEventData::BgCallbackBubble { text })
            }
            StateEvent::TurnSuspended { .. } => Some(AcpEventData::TurnSuspended),
        },
        V2Event::Observe(ev) => match ev {
            ObserveEvent::TurnError { message, .. } => {
                Some(AcpEventData::AgentExecutionFailed { message })
            }
            // 不走 v2_bridge 的事件（由 on_event → event_sink → acp_notifier → bridge_tx 路径覆盖）
            //
            // Compact 生命周期事件：[Fix] forwarder.rs 对 observe 事件做 v2_tx + on_event
            // 双路扇出，v2_bridge 再处理 CompactStarted / MessagesCompacted 会导致同一条紧凑
            // 通知被注入两次。render 事件已有相同修复（forwarder.rs:65-69），本处对齐一致。
            //
            // SubAgent 生命周期事件：ObserveEvent::SubagentStart / SubagentStop 在生产代码中
            // 从不被 emit；此处返回 None 作为防御性兜底，防止 forwarder.rs 未来启用发射后造成双重发送。
            ObserveEvent::CompactStarted { .. }
            | ObserveEvent::MessagesCompacted { .. }
            | ObserveEvent::SubagentStart { .. }
            | ObserveEvent::SubagentStop { .. }
            // Langfuse/Tracer-only events — not rendered in TUI
            | ObserveEvent::LlmCallStart { .. }
            | ObserveEvent::LlmCallEnd { .. }
            | ObserveEvent::AiReasoningChunk { .. }
            | ObserveEvent::StageStarted { .. }
            | ObserveEvent::StageEnded { .. }
            | ObserveEvent::MessageQueueDrained { .. }
            | ObserveEvent::LlmRequestPayload { .. } => None,
        },
    }
}

/// 启动 v2→bridge 转发 task。
///
/// 消费 v2_rx（mpsc::UnboundedReceiver<V2Event>），将每个事件映射为 AcpEventData
/// 后推入 bridge_tx（复用 spawn_acp_bridge 的分发逻辑）。同时对 StateSnapshot 等
/// 事件直接写 CONTEXT_USAGE atom 副作用。
pub fn spawn_v2_bridge(
    mut v2_rx: mpsc::UnboundedReceiver<V2Event>,
    bridge_tx: mpsc::UnboundedSender<AcpEventWithEpoch>,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                event = v2_rx.recv() => {
                    let Some(ev) = event else { break };
                    process_v2_event(ev, &bridge_tx);
                }
            }
        }
    })
}

/// 处理单个 v2 事件：副作用 + 映射 + 推入 bridge。
fn process_v2_event(ev: V2Event, bridge_tx: &mpsc::UnboundedSender<AcpEventWithEpoch>) {
    // StateSnapshot 副作用（不推 bridge 事件，直接写 atom）
    if let V2Event::State(StateEvent::StateSnapshot {
        budget_pct,
        context_total_tokens: Some(total),
        ..
    }) = &ev
    {
        let pct = budget_pct.unwrap_or(0.0);
        *CONTEXT_USAGE.state().write() = Some((pct, *total));
        RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
    }

    // 映射为 AcpEventData
    if let Some(data) = v2_event_to_acp_event_data(ev) {
        let epoch = AcpEventWithEpoch {
            event: data,
            // v2 直连路径使用空 session_id，BRIDGE_RESET_COUNTER 清空机制不依赖 session_id
            active_session_id: String::new(),
        };
        let _ = bridge_tx.send(epoch);
    }
}
