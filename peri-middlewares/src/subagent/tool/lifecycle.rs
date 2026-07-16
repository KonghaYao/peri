//! SubAgent 生命周期统一处理：事件发射、lifecycle hook、deregister RAII guard。
//!
//! P0-3 + P0-4：SubAgent 停止路径后处理步骤收敛为单一 helper，避免各文件自行组装
//! SubagentStopped emit + lifecycle hook + thread_store 的顺序不一致。

use std::sync::Arc;

use peri_agent::agent::events::{AgentEventHandler, ExecutorEvent};

use super::fire_subagent_lifecycle_hooks_static;
use crate::hooks::types::{HookEvent, RegisteredHook};

/// RAII guard that calls deregister on drop (panic-safe cleanup).
pub(crate) struct DeregisterGuard {
    pub(crate) thread_id: String,
    pub(crate) deregister: Option<Arc<dyn Fn(&str) + Send + Sync>>,
}

impl Drop for DeregisterGuard {
    fn drop(&mut self) {
        if let Some(ref deregister) = self.deregister {
            deregister(&self.thread_id);
        }
    }
}

/// 同步 SubAgent 停止统一后处理（define + fork 路径）。
///
/// 按顺序执行：
/// 1. emit SubagentStopped
/// 2. lifecycle hook (SubagentStop)
/// 3. thread_store 状态更新（仅 sync 路径有此步骤）
#[allow(clippy::too_many_arguments)]
pub(crate) async fn on_subagent_stop_handler(
    event_handler: &Option<Arc<dyn AgentEventHandler>>,
    registered_hooks: &[RegisteredHook],
    thread_store: &Option<Arc<dyn peri_agent::thread::ThreadStore>>,
    agent_id: &str,
    child_thread_id: &str,
    output_summary: &str,
    is_error: bool,
    cwd: &str,
) {
    // 1. emit SubagentStopped
    if let Some(ref handler) = event_handler {
        handler.on_event(ExecutorEvent::SubagentStopped {
            agent_name: agent_id.to_string(),
            result: output_summary.to_string(),
            is_error,
            instance_id: child_thread_id.to_string(),
        });
    }
    // 2. lifecycle hook
    fire_subagent_lifecycle_hooks_static(
        registered_hooks,
        HookEvent::SubagentStop,
        cwd,
        agent_id,
        Some(output_summary),
    )
    .await;
    // 3. thread_store（仅 sync 路径有此步骤）
    if let Some(ref store) = thread_store {
        let status = if is_error { "error" } else { "done" };
        let _ = store
            .update_thread_status(&child_thread_id.to_string(), status)
            .await;
    }
}

/// BG SubAgent 停止事件发射（execute_bg + spawner 路径）。
///
/// 通过 `bg_event_sender` 发送 `SubagentStopped` 事件。
/// 注意：BG 路径不更新 thread_store（bg 用 registry），不需要 deregister
/// （由显式路径或 tokio::spawn 内部的 RAII 处理）。
pub(crate) fn emit_subagent_stop_bg(
    bg_event_sender: &tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
    agent_name: &str,
    output_summary: String,
    is_error: bool,
    instance_id: &str,
) {
    let _ = bg_event_sender.send(ExecutorEvent::SubagentStopped {
        agent_name: agent_name.to_string(),
        result: output_summary,
        is_error,
        instance_id: instance_id.to_string(),
    });
}
