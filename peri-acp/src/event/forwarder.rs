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
use peri_agent::agent::events_v2::EventHandles;

use crate::event::mapper_v2::{
    observe_event_to_executor, render_event_to_executor, state_event_to_executor,
};

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
pub fn spawn_eventbus_forwarder<F>(mut handles: EventHandles, on_event: F)
where
    F: Fn(ExecutorEvent) + Send + Sync + 'static,
{
    tokio::spawn(async move {
        loop {
            // biased + render 优先：保证 Render 通道（含 TurnCompleted）先于 State 通道
            // 被消费，否则 partial 污染（详见模块顶部不变量注释）。
            tokio::select! {
                biased;
                Some(ev) = handles.render_rx.recv() => {
                    if let Some(exec_ev) = render_event_to_executor(ev) {
                        on_event(exec_ev);
                    }
                }
                Some(ev) = handles.state_rx.recv() => {
                    if let Some(exec_ev) = state_event_to_executor(ev) {
                        on_event(exec_ev);
                    }
                }
                ev_res = handles.observe_rx.recv() => {
                    match ev_res {
                        Ok(ev) => {
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
