//! SubAgent 生命周期统一处理：事件发射、lifecycle hook、deregister RAII guard。
//!
//! P0-3 + P0-4：SubAgent 停止路径后处理步骤收敛为单一 helper，避免各文件自行组装
//! SubagentStopped emit + lifecycle hook + thread_store 的顺序不一致。

use std::sync::Arc;

use peri_agent::{
    agent::{
        events::{AgentEventHandler, ExecutorEvent},
        events_v2::EventBus,
    },
    group::pipeline::AgentId,
    session::turn::TurnId,
};

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

/// SubagentStopped 补发参数（BgCleanupGuard 取消兜底路径使用）
pub(crate) struct BgStopEmit {
    pub(crate) sender: tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
    pub(crate) agent_name: String,
    pub(crate) instance_id: String,
}

/// v2 SubagentStop 补发参数（BgCleanupGuard 取消兜底路径使用）。
///
/// 字段与 `v2_bridge::emit_subagent_stop_v2` 调用参数一一对应（C3 配对契约）：
/// abort 兜底路径下 v2 Start 已 emit 而 v2 Stop 永不 emit → Langfuse AGENT span
/// 悬挂，Drop 时经 child EventBus 补发（`emit_observe` 为同步 unbounded send，
/// 可在同步 Drop 中安全执行）。
pub(crate) struct BgStopEmitV2 {
    pub(crate) event_bus: Arc<EventBus>,
    pub(crate) turn_id: TurnId,
    pub(crate) parent_agent_id: Option<AgentId>,
    pub(crate) child_agent_id: AgentId,
    pub(crate) agent_name: String,
}

/// bg 任务同步收尾 guard（S3.2）：Drop 时（任务被 abort / panic / 正常结束）执行：
/// - `deregister_runtime`（active_agents 清理，防泄漏）
/// - 补发 `SubagentStopped`（若未显式 emit——正常路径 emit 后需 `disarm_stop`，
///   保证与已 emit 的 `SubagentStarted` 配对，subagent_depth 正确递减）
/// - 补发 v2 `SubagentStop`（若未显式 emit——正常路径 emit 后需 `disarm_stop_v2`，
///   保证与已 emit 的 v2 `SubagentStart` 配对，Langfuse AGENT span 闭合）
///
/// async 收尾（`update_thread_status` / `fire_stop_hooks`）无法在同步 Drop 中 await，
/// abort 兜底路径丢失，由 background.rs `cancel()` 的 abort 分支记日志。
pub(crate) struct BgCleanupGuard {
    pub(crate) thread_id: String,
    pub(crate) deregister: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    /// 未显式 emit SubagentStopped 时补发（取消/abort 兜底路径）
    pub(crate) stop: Option<BgStopEmit>,
    /// 未显式 emit v2 SubagentStop 时补发（取消/abort 兜底路径）
    pub(crate) stop_v2: Option<BgStopEmitV2>,
}

impl BgCleanupGuard {
    /// 正常路径已显式 emit SubagentStopped 后调用，防止 drop 时重复发射。
    pub(crate) fn disarm_stop(&mut self) {
        self.stop = None;
    }

    /// 正常路径已显式 emit v2 SubagentStop 后调用，防止 drop 时重复发射。
    pub(crate) fn disarm_stop_v2(&mut self) {
        self.stop_v2 = None;
    }
}

impl Drop for BgCleanupGuard {
    fn drop(&mut self) {
        if let Some(ref deregister) = self.deregister {
            deregister(&self.thread_id);
        }
        if let Some(stop) = &self.stop {
            emit_subagent_stop_bg(
                &stop.sender,
                &stop.agent_name,
                "Background sub-agent was cancelled".to_string(),
                true,
                &stop.instance_id,
            );
        }
        if let Some(stop_v2) = &self.stop_v2 {
            crate::subagent::v2_bridge::emit_subagent_stop_v2(
                &stop_v2.event_bus,
                stop_v2.turn_id,
                stop_v2.parent_agent_id,
                stop_v2.child_agent_id,
                &stop_v2.agent_name,
                "Background sub-agent was cancelled",
                true,
            );
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
