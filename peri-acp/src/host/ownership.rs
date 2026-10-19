//! HandedOff 所有权的 drop-safe 恢复。
//!
//! `run_prompt_attempt` 在短锁内把正文移交给执行器后，future 可能在任意 await 点被
//! 丢弃（客户端断开/任务 abort）。本模块用一个原子活跃标记 + Drop guard：
//! - Drop 同步标记，并用 try_lock 尽力恢复；锁竞争时不阻塞、不派生后台任务；
//! - 下一次 prompt/restore 还会通过 `mark_stale_if_dead` 检测「HandedOff 但无活跃
//!   attempt」，即使 Drop 未取得锁也能冷恢复，不会永久 -32010。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use peri_acp_types::store::PersistedPayload;

use super::{ContextOwnership, SessionState, SharedSessions};

/// 一次 Handoff 的活跃标记；Drop 时翻转，只尝试短锁，不阻塞、不 await。
pub(crate) struct HandoffGuard {
    sessions: SharedSessions,
    session_id: String,
    alive: Arc<AtomicBool>,
}

impl Drop for HandoffGuard {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
        // 锁竞争时由下一次 prompt/restore 检查失活标记，不依赖 runtime 或任务调度。
        if let Ok(mut states) = self.sessions.try_lock() {
            if let Some(state) = states.get_mut(&self.session_id) {
                let same_attempt = state
                    .history_window
                    .handed_off_alive
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &self.alive));
                if same_attempt {
                    mark_stale_if_dead(state);
                }
            }
        }
    }
}

/// 在短锁内移出正文并把会话置为 HandedOff；guard 必须在 attempt 生命周期内存活。
pub(crate) fn begin_handoff(
    state: &mut SessionState,
    sessions: &SharedSessions,
    session_id: &str,
) -> (Vec<PersistedPayload>, HandoffGuard) {
    let payloads = std::mem::take(&mut state.history_payloads);
    state.context_ownership = ContextOwnership::HandedOff;
    let alive = Arc::new(AtomicBool::new(true));
    state.history_window.handed_off_alive = Some(Arc::clone(&alive));
    (
        payloads,
        HandoffGuard {
            sessions: Arc::clone(sessions),
            session_id: session_id.to_owned(),
            alive,
        },
    )
}

/// 若当前是 HandedOff 且活跃标记已失效，则把热态降级为 ReloadRequired。
///
/// 返回 `true` 表示本次完成了 stale 恢复；调用方必须已持有 sessions 锁。
pub(crate) fn mark_stale_if_dead(state: &mut SessionState) -> bool {
    if state.context_ownership != ContextOwnership::HandedOff {
        return false;
    }
    // 没有本模块登记的标记时保守保持忙；不把其他所有者误判为失活 attempt。
    let Some(alive) = state.history_window.handed_off_alive.as_ref() else {
        return false;
    };
    if alive.load(Ordering::Acquire) {
        return false;
    }
    tracing::warn!(session_id = %state.session_id, "prompt 已失活，丢弃热态并等待存储恢复");
    state.history_payloads.clear();
    state.context_ownership = ContextOwnership::ReloadRequired;
    state.history_window.handed_off_alive = None;
    state.cancel_token = None;
    super::history_window::invalidate(state);
    true
}
