use std::sync::Arc;

use peri_acp_types::identity::AgentId;

use super::types::SubagentLifecycleStop;
use super::v2_bridge::build_subagent_stop_v2;
use crate::agent::events::ExecutorEvent;
use crate::agent::events_v2::{observe_event_to_executor, EventBus};
use crate::session::factory::DeregisterRuntimeFn;
use crate::session::turn::TurnId;
use crate::thread::ThreadStore;

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
/// abort 兜底路径下 v2 Start 已 emit 而 v2 Stop 永不 emit → Langfuse AGENT span
/// 悬挂，Drop 时经 child EventBus 补发；同时 v1 协议化直发（`sender` 存在时）
/// 补发 SubagentStopped——两者共用同一 v2 事件构造（发射语义单一事实源）。
pub(crate) struct BgStopEmitV2 {
    pub(crate) event_bus: Arc<EventBus>,
    pub(crate) turn_id: TurnId,
    pub(crate) parent_agent_id: Option<AgentId>,
    pub(crate) child_agent_id: AgentId,
    pub(crate) agent_name: String,
    /// v1 协议化直发目标（bg 泵；None = 无 bg 通道，仅 v2 补发）
    pub(crate) sender: Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>,
}

/// bg 任务同步收尾 guard（S3.2）：Drop 时（任务被 abort / panic / 正常结束）执行：
/// - `deregister_runtime`（active_agents 清理，防泄漏）
/// - 补发 v2 `SubagentStop`（若未显式 emit——正常路径 emit 后需 `disarm_stop`）
///   + v1 协议化直发 `SubagentStopped`（sender 存在时，同一事件构造）
pub(crate) struct BgCleanupGuard {
    pub(crate) thread_id: String,
    pub(crate) deregister: Option<DeregisterRuntimeFn>,
    /// 未显式 emit v2 SubagentStop 时补发（取消/abort 兜底路径）
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
                stop.event_bus.emit_observe(ev.clone());
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
