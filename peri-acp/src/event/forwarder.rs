//! EventBus 转发器（v2 → ExecutorEvent）公共抽取。
//!
//! 主 executor（`peri-agent::session::exec`）与 workflow agent（`peri-agent::
//! agent::workflow`，p1-wa 归位）原先各自维护一份完全相同的 `tokio::spawn` +
//! biased `select!` 循环，仅 "消费 ExecutorEvent 后做什么" 不同。本模块封装
//! 循环骨架，调用方通过 `on_event: F` 闭包注入目标行为：
//!
//! - 主 executor 端：`|ev| { event_tx.send(ev) }`（投递到 mpsc，由 `spawn_event_pump` 消费）
//! - workflow_agent 端：`|ev| { event_handler.on_event(ev) }`（直接调用 handler；
//!   ACP 宿主经 `ForwarderLauncherFn` 注入，workflow 专用 launcher 见
//!   `host/workflow_agent.rs`）
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

use peri_acp_types::event::ExecutorEvent;
use peri_acp_types::event_v2::{
    observe_event_to_executor, render_event_to_executor, state_event_to_executor, EventHandles,
};
use peri_acp_types::identity::EventDeliveryClass;
use peri_acp_types::runtime::UnstampedEvent;

use peri_controller::langfuse::bridge::{LangfuseBridge, UnifiedLangfuseEvent};
use peri_controller::langfuse::tracer::stages::StageHandle;

/// 从 v1 payload 提取 message_id（v2 事件无 message 级身份；映射后的事件
/// 携带 `MessageId`，作为 envelope 身份的一部分）。
fn extract_message_id(event: &ExecutorEvent) -> Option<String> {
    match event {
        ExecutorEvent::TextChunk { message_id, .. }
        | ExecutorEvent::AiReasoning { message_id, .. }
        | ExecutorEvent::ToolStart { message_id, .. }
        | ExecutorEvent::ToolEnd { message_id, .. } => Some(message_id.as_uuid().to_string()),
        _ => None,
    }
}

/// 启动 EventBus forwarder task。
///
/// 消费 `handles` 内三层 v2 事件（render / state / observe），经协议序列化面映射
/// （`events_v2::*_event_to_executor`，v1 ExecutorEvent 中间态已退役、仅保留为
/// ACP 协议化载体——批 2「v1-retire」）转为 [`ExecutorEvent`]，然后调用 `on_event`
/// 闭包投递到调用方指定的目标。
///
/// **事件三层化（3.0 M-event-chain）**：闭包签名携带 `UnstampedEvent`（事件源
/// 身份：turn_id / agent_id / message_id / delivery_class）与 v1 payload；
/// 调用方（ACP 发射点）应把两者交给 Controller（`Controller::publish_event`）
/// 统一发射——Controller 经 Runtime 补打 session_id / session_seq 后扇出，
/// ACP 协议化消费侧从 `Controller::subscribe` / `pop_events` 订阅（不再直连
/// Agent EventBus）。
///
/// v2_tx 双轨（v2 事件直连 TUI）已随
/// `2026-08-05-3.0-m-event-chain-canonical.md` 下线：TUI 事件仅经本 forwarder
/// 的 ACP 协议化路径，不再有第二套扇出。
///
/// # 参数
///
/// - `handles`：v2 [`EventHandles`]（调用方取出所有权后传入，本函数内部 `mut` 消费）
/// - `on_event`：每条映射后的 `ExecutorEvent` + 事件源身份的消费闭包。
///   签名 `Fn(UnstampedEvent, ExecutorEvent) + Send + Sync + 'static`
/// - `bridge`：统一 Langfuse 桥接器。`None` 表示遥测禁用。
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
    bridge: Option<LangfuseBridge>,
) where
    F: Fn(UnstampedEvent, ExecutorEvent) + Send + Sync + 'static,
{
    tokio::spawn(async move {
        // key = 事件 agent_id（主 agent 路径下只有主 agent 的事件，单 entry）
        let mut active_stage: std::collections::HashMap<String, StageHandle> =
            std::collections::HashMap::new();
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
                    // Langfuse: render 层追踪（TextChunk, BudgetWarning）
                    if let Some(ref bridge) = bridge {
                        if let Some(u) = UnifiedLangfuseEvent::from_render_event(ev.clone()) {
                            bridge.process_event(&u, &mut active_stage);
                        }
                    }
                    if let Some(exec_ev) = render_event_to_executor(ev.clone()) {
                        let source = UnstampedEvent::new(
                            ev.turn_id().to_string(),
                            ev.agent_id().to_string(),
                            extract_message_id(&exec_ev),
                            EventDeliveryClass::Critical,
                        );
                        on_event(source, exec_ev);
                    }
                }
                Some(ev) = handles.state_rx.recv() => {
                    // Langfuse: state 层追踪（当前无映射）
                    if let Some(exec_ev) = state_event_to_executor(ev.clone()) {
                        let source = UnstampedEvent::new(
                            ev.turn_id().to_string(),
                            ev.agent_id().to_string(),
                            extract_message_id(&exec_ev),
                            EventDeliveryClass::Critical,
                        );
                        on_event(source, exec_ev);
                    }
                }
                ev_res = handles.observe_rx.recv() => {
                    match ev_res {
                        Ok(ev) => {
                            // v2 SubagentStart/Stop 只在 child EventBus emit（经
                            // subagent_event_forwarder 消费并过滤 v1 mapper 转发，防与工具侧
                            // v1 直发双发，见 subagent_event_forwarder.rs），主 EventBus 上
                            // 不会出现这两个变体，故此处无需过滤。Langfuse 消费在 child
                            // forwarder 侧直达 bridge（C4）。
                            // Langfuse: observe 层追踪（LLM/Tool/Stage/Compact）
                            if let Some(ref bridge) = bridge {
                                if let Some(u) = UnifiedLangfuseEvent::from_observe_event(ev.clone()) {
                                    bridge.process_event(&u, &mut active_stage);
                                }
                            }
                            if let Some(exec_ev) = observe_event_to_executor(ev.clone()) {
                                let source = UnstampedEvent::new(
                                    ev.turn_id().to_string(),
                                    ev.agent_id().to_string(),
                                    extract_message_id(&exec_ev),
                                    EventDeliveryClass::Broadcast,
                                );
                                on_event(source, exec_ev);
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
