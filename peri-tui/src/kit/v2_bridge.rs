//! v2 事件直连桥接——消费 v2 RenderEvent/StateEvent/ObserveEvent，映射为 AcpEventData
//! 后推入现有的 bridge_tx 通道（复用 spawn_acp_bridge 的分发与渲染逻辑）。
//!
//! Phase A：与 ACP 路径双轨运行。Phase B 下线 ACP 路径后本模块成为唯一事件源。

use crate::kit::acp_types::{AcpEventData, AcpEventWithEpoch};
use crate::kit::atoms::{CONTEXT_USAGE, RENDER_HEARTBEAT};
use peri_acp::event::V2Event;
use peri_acp_types::event_data::BudgetWarning;
use peri_agent::agent::events_v2::{ObserveEvent, RenderEvent, StateEvent};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// 映射单个 v2 事件到 AcpEventData。返回 None 表示该事件不需要推送到 bridge
/// （如 HitlPending 走 ACP 独立通道、Langfuse-only 事件如 LlmCallStart）。
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
                let input_summary = peri_acp::event::truncate::summarize_input(&name, &input);
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
            ObserveEvent::SubagentStart {
                agent_name,
                child_agent_id,
                is_background,
                ..
            } => Some(AcpEventData::SubagentStarted {
                agent_id: child_agent_id.to_string(),
                agent_name,
                is_background,
            }),
            ObserveEvent::SubagentStop { child_agent_id, .. } => {
                Some(AcpEventData::SubagentStopped {
                    agent_id: child_agent_id.to_string(),
                })
            }
            ObserveEvent::CompactStarted { .. } => Some(AcpEventData::CompactStarted),
            ObserveEvent::MessagesCompacted {
                summary,
                files,
                skills,
                before_count,
                after_count,
                messages,
                ..
            } => {
                let messages_json = serde_json::to_string(&messages).ok()?;
                Some(AcpEventData::CompactCompleted {
                    summary,
                    files: files
                        .into_iter()
                        .filter_map(|f| serde_json::to_value(f).ok())
                        .collect(),
                    skills,
                    micro_cleared: before_count.saturating_sub(after_count),
                    messages_json,
                })
            }
            ObserveEvent::TurnError { message, .. } => {
                Some(AcpEventData::AgentExecutionFailed { message })
            }
            // Langfuse/Tracer-only events — not rendered in TUI
            ObserveEvent::LlmCallStart { .. }
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
