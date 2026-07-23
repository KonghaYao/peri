//! SubAgent v2 事件转发器
//!
//! SubAgent（fork / background / define 路径）拥有独立 EventBus，但默认不消费
//! `EventHandles`，导致 `run_react_loop` 内 emit 的 Render/State/Observe 事件
//! 全部丢弃在 SubAgent 自己的 EventBus 内部，TUI 看不到 SubAgent 的工具调用、
//! AI 文本、推理内容。
//!
//! 本模块封装转发器 spawn 函数：从 SubAgent 的 EventBus 消费 v2 事件，经
//! `events_v2_mapper` 映射为 `ExecutorEvent`，注入 `source_agent_id = child_thread_id`
//! 后转发到父 Agent 的 `AgentEventHandler`。
//!
//! ## 关键不变量
//!
//! - **`child_thread_id` 必须与 `SubagentStarted { instance_id }` 一致**：TUI 的
//!   `find_running_subagent_mut(aid)` 按 instance_id 精确匹配，不匹配则事件被忽略。
//!   注意：v2 内部 `AgentId::new()` 与 child_thread_id 是不同值，不能用错。
//! - **转发器 task 在通道关闭时自动退出**：`select! { else => break }` 处理所有
//!   通道关闭场景，避免 task 泄漏。
//! - **ObserveEvent 的 Lagged 不 panic**：只记日志，继续处理后续事件。

use std::sync::Arc;

use tokio::task::JoinHandle;

use super::langfuse_bridge::LangfuseBridgeLike;
use crate::agent::events::AgentEventHandler;
use crate::agent::events::ExecutorEvent;
use crate::agent::events_v2::EventHandles;
use crate::agent::events_v2_mapper::{
    observe_event_to_executor, render_event_to_executor, state_event_to_executor,
};

/// 启动 SubAgent 事件转发器。
///
/// 消费 `handles` 内三层 v2 事件，映射为 ExecutorEvent，注入 source_agent_id 后
/// 通过 `event_handler` 转发到父 Agent。通道全部关闭时自动退出。
///
/// # 参数
///
/// - `handles`：SubAgent v2 `EventHandles`（从 `V2SubagentContext.event_handles` 取出）
/// - `event_handler`：父 Agent 的事件处理器（`SubAgentTool.event_handler` clone）
/// - `bridge`：Langfuse 桥接器（None 表示遥测禁用）
/// - `child_thread_id`：与 `SubagentStarted { instance_id }` 一致的 UUID 字符串
///
/// # 返回
///
/// 转发器 task 的 `JoinHandle`。调用方持有以控制生命周期（通常 `_forwarder_handle`
/// 解绑即表示 fire-and-forget，task 在通道关闭时自动退出）。
pub fn spawn_subagent_event_forwarder(
    mut handles: EventHandles,
    event_handler: Option<Arc<dyn AgentEventHandler>>,
    bridge: Option<Arc<dyn LangfuseBridgeLike>>,
    child_thread_id: String,
) -> JoinHandle<()> {
    let has_handler = event_handler.is_some();
    tokio::spawn(async move {
        tracing::info!(
            target: "agent.subagent_forwarder",
            child_thread_id = %child_thread_id,
            has_event_handler = has_handler,
            "forwarder spawned"
        );
        loop {
            // biased + observe 优先：确保 StageStarted 先于 ToolStarted 到达 tracer，
            // 避免 active_stage=None 时工具 parent 错误回落到主 agent。
            // render 优先于 state：保证同一 ReAct 迭代的 Render 事件在 State 事件
            // 之前被消费，避免 commit_iteration 与残留 Render 事件乱序导致的 partial 污染。
            tokio::select! {
                biased;
                ev_res = handles.observe_rx.recv() => {
                    match ev_res {
                        Ok(ev) => {
                            // Langfuse bridge 调用必须在 ev 被 observe_event_to_executor move 之前
                            if let Some(ref b) = bridge {
                                b.process_observe_event(&ev);
                            }
                            if let Some(mut exec_ev) = observe_event_to_executor(ev) {
                                set_source_agent_id(&mut exec_ev, &child_thread_id);
                                if let Some(h) = &event_handler {
                                    h.on_event(exec_ev);
                                }
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(
                                n,
                                "[subagent-forwarder] observe_rx lagged, events dropped"
                            );
                        }
                    }
                }
                Some(ev) = handles.render_rx.recv() => {
                    // 过滤：子 Agent 的 TurnCommitted / HitlPending 不应转发到父 Agent
                    // （TurnCompleted 已迁移到 RenderEvent；HitlPending 由父 Agent
                    //  HITL 中间件独立处理，转发会导致双重审批弹窗）
                    let should_forward = !matches!(
                        &ev,
                        crate::agent::events_v2::RenderEvent::TurnCompleted { .. }
                        | crate::agent::events_v2::RenderEvent::HitlPending { .. }
                    );
                    if should_forward {
                        // Langfuse bridge 调用必须在 ev 被 render_event_to_executor move 之前
                        if let Some(ref b) = bridge {
                            b.process_render_event(&ev);
                        }
                        if let Some(mut exec_ev) = render_event_to_executor(ev) {
                            tracing::trace!(
                                target: "agent.subagent_forwarder",
                                child_thread_id = %child_thread_id,
                                ev = ?exec_ev,
                                "forwarder: received render event, forwarding to parent"
                            );
                            set_source_agent_id(&mut exec_ev, &child_thread_id);
                            tracing::debug!(
                                target: "agent.subagent_forwarder",
                                child_thread_id = %child_thread_id,
                                "forwarder: tool event sourced from SubAgent"
                            );
                            if let Some(h) = &event_handler {
                                h.on_event(exec_ev);
                            }
                        }
                    }
                }
                Some(ev) = handles.state_rx.recv() => {
                    // 过滤：子 Agent 的 StateSnapshot 不应污染父 Agent transcript
                    // （TurnCompleted 已迁移到 RenderEvent，在 render 分支同样过滤）
                    let should_forward = !matches!(
                        &ev,
                        crate::agent::events_v2::StateEvent::StateSnapshot { .. }
                    );
                    if should_forward {
                        if let Some(exec_ev) = state_event_to_executor(ev) {
                            if let Some(h) = &event_handler {
                                h.on_event(exec_ev);
                            }
                        }
                    }
                }
                else => break,
            }
        }
    })
}

/// 对 ToolStart / ToolEnd / TextChunk / AiReasoning 设置 source_agent_id。
/// 其他变体为 no-op。
fn set_source_agent_id(event: &mut ExecutorEvent, agent_id: &str) {
    match event {
        ExecutorEvent::ToolStart {
            source_agent_id, ..
        }
        | ExecutorEvent::ToolEnd {
            source_agent_id, ..
        }
        | ExecutorEvent::TextChunk {
            source_agent_id, ..
        }
        | ExecutorEvent::AiReasoning {
            source_agent_id, ..
        } => {
            *source_agent_id = Some(agent_id.to_string());
        }
        _ => {}
    }
}

#[cfg(test)]
#[path = "subagent_event_forwarder_test.rs"]
mod tests;
