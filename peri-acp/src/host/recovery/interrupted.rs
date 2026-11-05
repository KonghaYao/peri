//! 进入根时收敛遗留 `Active` 的 child（设计 §4）。
//!
//! 只处理「非根、存储仍为 `active`、工作意图已落到 `idle`/`stopped`、本进程没有
//! 运行实例、close 未结清」的 child：把运行状态收尾为 `Error`/`Cancelled`，工作
//! 意图保持原值。收敛不续跑、不注入提醒、不产生结果通知；它只是让下一次显式
//! resume 不再被遗留 `Active` 拒绝。

use std::sync::Arc;

use peri_acp_types::session_resources::{CloseSettlement, SessionResources};
use peri_acp_types::thread::{AgentStatus, ThreadId, WorkState};

use super::super::SharedSessions;
use crate::session::SessionManager;

/// 收敛根子树中遗留的 Active child；失败只记录，不阻断本次进入。
pub(super) async fn settle_interrupted_children(
    resources: &Arc<dyn SessionResources>,
    manager: &SessionManager,
    sessions: &SharedSessions,
    root: &ThreadId,
) {
    let candidates = match resources.load_interrupted_threads(root).await {
        Ok(candidates) => candidates,
        Err(error) => {
            tracing::warn!(root_thread_id = %root, %error, "recovery: interrupted children query failed");
            return;
        }
    };
    for candidate in candidates {
        let thread = &candidate.thread_id;
        // 查询已排除根；这里只防御投影漂移，绝不收敛进入的根自身。
        if thread == root {
            continue;
        }
        // 工作意图决定终态；`working` 不是中断遗留，保持候选之外的语义。
        let status = match candidate.work_state {
            WorkState::Idle => AgentStatus::Error,
            WorkState::Stopped => AgentStatus::Cancelled,
            WorkState::Working => continue,
        };
        if live_in_this_process(manager, sessions, thread).await {
            continue;
        }
        match resources.close_settlement(thread).await {
            Ok(CloseSettlement::Finished) => {}
            // close 未结清（或无法证明已结清）：由 close 自己的结算负责，不抢先写终态。
            Ok(CloseSettlement::Pending | CloseSettlement::Unknown) => continue,
            Err(error) => {
                tracing::warn!(thread_id = %thread, %error, "recovery: close settlement query failed");
                continue;
            }
        }
        match resources
            .settle_terminal_without_result(thread, status, candidate.work_state)
            .await
        {
            Ok(outcome) => tracing::info!(
                root_thread_id = %root,
                thread_id = %thread,
                status = status.as_str(),
                ?outcome,
                "recovery: settled interrupted child"
            ),
            Err(error) => {
                tracing::warn!(thread_id = %thread, %error, "recovery: interrupted child settlement failed")
            }
        }
    }
}

/// 本进程是否真的在跑该 thread：父 turn 内的子 agent 登记，或已发布会话的
/// turn 登记（`SessionState.cancel_token`）。仅「有会话记录」不等于在跑。
async fn live_in_this_process(
    manager: &SessionManager,
    sessions: &SharedSessions,
    thread: &ThreadId,
) -> bool {
    if manager.thread_has_runtime(thread) {
        return true;
    }
    let Some(session_id) = manager.session_id_for_thread(thread) else {
        return false;
    };
    let states = sessions.lock().await;
    states
        .get(&session_id)
        .is_some_and(|state| state.cancel_token.is_some() || state.closing)
}
