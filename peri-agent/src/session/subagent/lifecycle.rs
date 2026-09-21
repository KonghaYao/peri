use std::sync::{Arc, Weak};

use peri_acp_types::identity::AgentId;

use super::types::SubagentLifecycleStop;
use super::v2_bridge::build_subagent_stop_v2;
use crate::agent::events::ExecutorEvent;
use crate::agent::events_v2::{observe_event_to_executor, EventBus};
use crate::session::factory::DeregisterRuntimeFn;
use crate::session::turn::TurnId;
use crate::thread::ThreadStore;

// The loop has released its producers before this seam. A cleanup guard must
// only retain a Weak<EventBus>, otherwise closing the stream would deadlock.
// Join failure is execution failure, even when the model already finished.
pub(super) async fn drain_subagent_events(
    event_bus: Arc<EventBus>,
    forwarder: tokio_util::task::AbortOnDropHandle<Option<crate::agent::events_v2::ObserveEvent>>,
    bridge: Option<Arc<dyn crate::agent::LangfuseBridgeLike>>,
) -> crate::error::AgentResult<()> {
    drop(event_bus);
    let stop = forwarder.await.map_err(|error| {
        tracing::error!(
            is_panic = error.is_panic(),
            is_cancelled = error.is_cancelled(),
            "Subagent event forwarding failed"
        );
        crate::error::AgentError::Other(anyhow::anyhow!("Subagent event forwarding failed"))
    })?;
    // No await separates successful drain from terminal publication. Forced
    // abort while waiting leaves telemetry incomplete rather than successful.
    if let (Some(bridge), Some(stop)) = (bridge, stop) {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            bridge.process_observe_event(&stop);
        }))
        .map_err(|_| {
            crate::error::AgentError::Other(anyhow::anyhow!(
                "Subagent terminal event forwarding failed"
            ))
        })?;
    }
    Ok(())
}

// ─── 生命周期工具（自 tool/lifecycle.rs 迁移；hook 触发闭包化） ────────────

/// RAII guard that calls deregister on drop (panic-safe cleanup).
pub(crate) struct DeregisterGuard {
    pub(crate) thread_id: String,
    pub(crate) deregister: Option<DeregisterRuntimeFn>,
}

impl Drop for DeregisterGuard {
    fn drop(&mut self) {
        if let Some(ref deregister) = self.deregister {
            deregister(&self.thread_id);
        }
    }
}

/// v2 SubagentStop 补发参数（BgCleanupGuard 取消兜底路径使用）。
///
/// 字段与 [`build_subagent_stop_v2`] 参数一一对应（C3 配对契约）：
/// Drop 时同步补发可见的 SubagentStopped（`sender` 存在时）；若 producer
/// 仍存活，也向 child EventBus 发射相同的 v2 事件。强制 abort 不等待异步
/// 排空，遥测可以保持 incomplete，不能宣称取消后仍能保证 v2 Stop 交付。
pub(crate) struct BgStopEmitV2 {
    pub(crate) event_bus: Weak<EventBus>,
    pub(crate) turn_id: TurnId,
    pub(crate) parent_agent_id: Option<AgentId>,
    pub(crate) child_agent_id: AgentId,
    pub(crate) agent_name: String,
    /// v1 协议化直发目标（bg 泵；None = 无 bg 通道，仅 v2 补发）
    pub(crate) sender: Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>,
}

/// bg 任务同步收尾 guard（S3.2）：Drop 时（任务被 abort / panic / 正常结束）执行：
/// - `deregister_runtime`（active_agents 清理，防泄漏）
/// - 补发可见 `SubagentStopped`，producer 存活时也发射 v2 `SubagentStop`
///   （正常路径排空并完成可见 Stop 之后调用 `disarm_stop`）
pub(crate) struct BgCleanupGuard {
    pub(crate) thread_id: String,
    pub(crate) deregister: Option<DeregisterRuntimeFn>,
    /// 尚未完成可见 Stop 时补发（包括等待 forwarder 期间取消/abort）。
    pub(crate) stop: Option<BgStopEmitV2>,
}

impl BgCleanupGuard {
    /// 正常路径已显式 emit v2 SubagentStop + v1 协议化直发后调用，
    /// 防止 drop 时重复发射。
    pub(crate) fn disarm_stop(&mut self) {
        self.stop = None;
    }
}

impl Drop for BgCleanupGuard {
    fn drop(&mut self) {
        if let Some(ref deregister) = self.deregister {
            deregister(&self.thread_id);
        }
        if let Some(stop) = &self.stop {
            // 单一 v2 事件构造：v2 发射（parent 身份存在时）+ v1 协议化直发
            // （sender 存在时）。ObserveEvent 身份透传：child_agent_id → instance_id。
            let ev = build_subagent_stop_v2(
                stop.turn_id,
                stop.parent_agent_id,
                stop.child_agent_id,
                &stop.agent_name,
                "Background sub-agent was cancelled",
                true,
            );
            if stop.parent_agent_id.is_some() {
                if let Some(event_bus) = stop.event_bus.upgrade() {
                    event_bus.emit_observe(ev.clone());
                }
            }
            if let Some(sender) = &stop.sender {
                if let Some(exec_ev) = observe_event_to_executor(ev) {
                    let _ = sender.send(exec_ev);
                }
            }
        }
    }
}

/// 同步 SubAgent 停止统一后处理（fork + agent 定义路径）。
///
/// 按顺序执行：
/// 1. lifecycle hook (SubagentStop，经闭包)
/// 2. thread_store 状态更新（仅 sync 路径有此步骤）
///
/// v1 SubagentStopped 协议化直发不在本函数内——由调用方在
/// `emit_subagent_stop_v2` 之后经 `forward_subagent_stop_v1` 同步映射发出
/// （发射语义单一事实源 = v2 事件构造，v1 仅 ACP 协议化载体）。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn on_subagent_stop_handler(
    on_subagent_stop: &Option<SubagentLifecycleStop>,
    thread_store: &Option<Arc<dyn ThreadStore>>,
    agent_id: &str,
    child_thread_id: &str,
    output_summary: &str,
    is_error: bool,
    cwd: &str,
) {
    // 1. lifecycle hook（闭包由 middlewares 构造，内部触发 RegisteredHook）
    if let Some(ref on_stop) = on_subagent_stop {
        on_stop(agent_id, cwd, output_summary, is_error);
    }
    // 3. thread_store（仅 sync 路径有此步骤）
    if let Some(ref store) = thread_store {
        let status = if is_error { "error" } else { "done" };
        let _ = store
            .update_thread_status(&child_thread_id.to_string(), status)
            .await;
    }
}

#[cfg(test)]
#[path = "lifecycle_test.rs"]
mod tests;
