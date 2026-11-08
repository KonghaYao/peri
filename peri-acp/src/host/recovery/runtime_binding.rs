//! 恢复 child 的实际接收会话运行登记（设计 §5.3/§5.4）。
//!
//! 只登记到 live 且未 closing 的实际接收会话：父级 TaskManager 的任务投影、父级
//! `active_agents` 的持久化取消策略，以及二者与 child turn 取消令牌的绑定。
//! 登记发生在 child turn 真正获得准入之后；早失败、未准入或父级未 live 都不登记，
//! child 终态结算时收尾，不留 pending 任务。父级之后进入时对已在运行的 child 做
//! adoption；不冷装配祖先链。

use peri_acp_types::event::BackgroundTaskResult;
use peri_acp_types::tasks::NoopTaskManager;
use peri_acp_types::thread::{AgentStatus, ThreadId};
use tokio_util::sync::CancellationToken;

use super::super::{AcpServerConfig, SharedSessions};
use crate::session::RecoveryDelivery;

/// 恢复 child 的父级任务终态；`None`（早失败/结算未提交）按取消收尾。
pub(super) enum RecoveryTaskTerminal {
    Completed { success: bool, output: String },
    Cancelled,
}

/// 恢复 turn 准入后的登记入口：读取该 child 的实际收件人与本轮取消令牌。
///
/// 返回 `Ok(false)` 表示没有可登记的父级（未 live / 无恢复投递），不是失败；
/// 父级 live 但登记失败返回 `Err`，调用方撤销本次启动资格并保留下一次重试机会。
pub(crate) async fn register_recovered_child_for_turn(
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    child_session_id: &str,
) -> Result<bool, String> {
    let Some(parent) = cfg
        .session_manager
        .get_session(child_session_id)
        .and_then(|session| session.recovery_parent())
    else {
        return Ok(false);
    };
    let (child, cancel_token) = {
        let states = sessions.lock().await;
        let Some(state) = states.get(child_session_id) else {
            return Ok(false);
        };
        let Some(cancel_token) = state.cancel_token.clone() else {
            return Ok(false);
        };
        (ThreadId::from(state.thread_id.clone()), cancel_token)
    };
    register(
        cfg,
        sessions,
        child_session_id,
        &parent,
        &child,
        &cancel_token,
    )
    .await
}

/// 父级之后进入时的 adoption：只接管已在运行的恢复 child，不启动新执行。
pub(crate) async fn adopt_recovered_child(
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    child_session_id: &str,
) -> bool {
    let Some(parent) = cfg
        .session_manager
        .get_session(child_session_id)
        .and_then(|session| session.recovery_parent())
    else {
        return false;
    };
    let (child, cancel_token) = {
        let states = sessions.lock().await;
        let Some(state) = states.get(child_session_id) else {
            return false;
        };
        let Some(cancel_token) = state.cancel_token.clone() else {
            return false;
        };
        (ThreadId::from(state.thread_id.clone()), cancel_token)
    };
    match register(
        cfg,
        sessions,
        child_session_id,
        &parent,
        &child,
        &cancel_token,
    )
    .await
    {
        Ok(registered) => registered,
        Err(error) => {
            tracing::warn!(
                child_thread_id = %child,
                parent_thread_id = %parent,
                %error,
                "recovery: parent task adoption failed"
            );
            false
        }
    }
}

/// 在实际接收会话登记任务与运行时；父级不是 live/未 closing 时跳过。
async fn register(
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    child_session_id: &str,
    parent: &ThreadId,
    child: &ThreadId,
    cancel_token: &CancellationToken,
) -> Result<bool, String> {
    let cancel_policy = cfg
        .session_resources
        .load_session_meta(child)
        .await
        .map_err(|error| error.to_string())?
        .cancel_policy;
    let Some(parent_session_id) = cfg.session_manager.session_id_for_thread(parent) else {
        return Ok(false);
    };
    let parent_open = {
        let states = sessions.lock().await;
        states
            .get(&parent_session_id)
            .is_some_and(|state| !state.closing)
    };
    if !parent_open {
        return Ok(false);
    }
    let Some(parent_session) = cfg.session_manager.get_session(&parent_session_id) else {
        return Ok(false);
    };
    let task_manager = parent_session.task_manager.clone();
    drop(parent_session);
    // 无任务投影的部署（print 等）不因登记能力缺失阻断 child 执行。
    if task_manager.as_any().is::<NoopTaskManager>() {
        return Ok(false);
    }

    let Some(child_session) = cfg.session_manager.get_session(child_session_id) else {
        return Ok(false);
    };
    let registered = child_session.register_recovery_task(
        parent.clone(),
        parent_session_id.clone(),
        child.clone(),
        cancel_token.clone(),
        &task_manager,
    )?;
    drop(child_session);
    if !registered {
        return Ok(false);
    }

    // Stop follows the saved policy; closing the session still cancels all children.
    let Some(mut parent_session) = cfg.session_manager.get_session_mut(&parent_session_id) else {
        abort_recovery_launch(cfg, child_session_id);
        return Ok(false);
    };
    parent_session.active_agents.insert(
        child.clone(),
        peri_acp_types::session::AgentRuntime {
            thread_id: child.clone(),
            cancel_token: cancel_token.clone(),
            cancel_policy,
            status: AgentStatus::Active,
        },
    );
    Ok(true)
}

/// child 终态结算后收尾父级任务投影与运行时登记。
pub(crate) fn finish_recovered_child(
    cfg: &AcpServerConfig,
    delivery: &RecoveryDelivery,
    terminal: Option<RecoveryTaskTerminal>,
) {
    let Some(binding) = delivery.task.as_ref() else {
        return;
    };
    let Some(mut parent_session) = cfg
        .session_manager
        .get_session_mut(&binding.parent_session_id)
    else {
        tracing::debug!(
            parent_thread_id = %binding.parent,
            "recovery task binding dropped with its parent session"
        );
        return;
    };
    let owned = parent_session
        .active_agents
        .get(&binding.child)
        .is_some_and(|runtime| runtime.cancel_token == binding.cancel_token);
    if owned {
        parent_session.active_agents.remove(&binding.child);
    }
    let task_manager = parent_session.task_manager.clone();
    drop(parent_session);

    // 本次恢复 turn 已结束（或被父级取消）：先清掉注册时记下的外部执行证据，
    // 否则父级 `shutdown()` 会因 unsettled external 永远报 Incomplete。
    task_manager.confirm_external_execution_stopped(&binding.ticket);

    match terminal {
        Some(RecoveryTaskTerminal::Completed { success, output }) => {
            let result = BackgroundTaskResult {
                task_id: binding.task_id.clone(),
                agent_name: "subagent".to_owned(),
                prompt_summary: "recovered subagent".to_owned(),
                success,
                output,
                tool_calls_count: 0,
                duration_ms: 0,
                timed_out: false,
                child_thread_id: Some(binding.child.to_string()),
                subagent_failure: None,
                shell_output: None,
            };
            if !task_manager.complete(&binding.ticket, result) {
                // 父级 Stop/close 已取消同一条登记：终态已成立，不重复写。
                tracing::debug!(
                    task_id = %binding.task_id,
                    "recovery task was already settled by its parent"
                );
            }
        }
        Some(RecoveryTaskTerminal::Cancelled) | None => {
            if let Err(error) = task_manager.cancel(&binding.task_id) {
                // 已由父级 Stop/close 结算：取消是幂等收尾。
                tracing::debug!(
                    task_id = %binding.task_id,
                    %error,
                    "recovery task cancellation was already settled"
                );
            }
        }
    }
}

/// 早失败（未进入终态结算）：结束父任务投影，不投递任何结果事实。
pub(crate) fn finish_recovered_child_without_result(
    cfg: &AcpServerConfig,
    delivery: &RecoveryDelivery,
) {
    finish_recovered_child(cfg, delivery, None);
}

/// 取走并收尾本会话的恢复投递；用于启动准入后、模型调用前的失败回退。
pub(crate) fn abort_recovery_launch(cfg: &AcpServerConfig, child_session_id: &str) {
    let delivery = cfg
        .session_manager
        .get_session(child_session_id)
        .and_then(|session| session.take_recovery_parent());
    if let Some(delivery) = delivery {
        finish_recovered_child_without_result(cfg, &delivery);
    }
}
