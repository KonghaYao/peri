//! Transport EOF closes admission before cancelling and draining owned resources.

use std::collections::BTreeSet;
use std::sync::Arc;

use peri_acp_types::ports::McpTaskOwnerPort;
use tokio_util::sync::CancellationToken;

use super::{
    connection::ConnectionContext, task_scope, AcpServerConfig, PromptLocks, SharedSessions,
};

/// 当前全部会话环境（连接关闭时逐个收口的输入）。
async fn session_environments(
    sessions: &SharedSessions,
) -> Vec<Arc<super::workspace::SessionEnvironment>> {
    sessions
        .lock()
        .await
        .values()
        .filter_map(|state| state.environment.clone())
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn shutdown_host(
    task_owner: &mut task_scope::HostTaskOwner,
    mcp_task_owner: &mut dyn McpTaskOwnerPort,
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    prompt_locks: &PromptLocks,
    cont_tx: &mut Option<
        Arc<tokio::sync::mpsc::UnboundedSender<crate::session::executor::ContinuationRequest>>,
    >,
    connection: &Arc<tokio::sync::Mutex<ConnectionContext>>,
    connection_cancellation: &CancellationToken,
    closing_sessions: &mut std::collections::BTreeMap<String, crate::session::AcpSession>,
) -> task_scope::HostTerminalShutdownReport {
    // Transport EOF is the host's single ownership transaction.
    connection_cancellation.cancel();
    let connection_id = connection.lock().await.id().to_string();
    // 连接关闭：每个会话环境各自的路由与 relay 都要收口——该连接在本会话的
    // endpoint 置离线（其 View 附件随之 detach，进入有界重挂接窗口），relay 侧
    // 同步撤该连接附件的交付资格。**不撤销 record**（裁决 D6：record 终态撤销只在
    // `apps/close` / session close / incarnation 换代 / 保留到期），也不取消已
    // dispatch 的调用（由 invocation owner 收尾，R7/A8）。
    for environment in session_environments(sessions).await {
        environment
            .interaction_router()
            .retire_connection(&connection_id);
        if let Some(relay) = environment.cfg.mcp_apps_relay.as_ref() {
            relay.close_connection(&connection_id);
        }
        // Apps 账本：销毁该连接的 Host 操作域（已销毁的 namespace 不得重建）；
        // 附件 namespace 留在重挂接窗口内，不销毁。
        if let Some(facade) = environment.apps_facade() {
            facade.on_connection_retired(&connection_id);
        }
    }
    // 无会话环境（host 级装配）时仍按既有 host 级 relay 收口。
    if sessions.lock().await.is_empty() {
        if let Some(relay) = cfg.mcp_apps_relay.as_ref() {
            relay.close_connection(&connection_id);
        }
    }
    connection.lock().await.begin_close();
    task_owner.begin_shutdown();
    if let Some(pool) = cfg.mcp_pool.as_ref() {
        pool.begin_shutdown();
    }
    mcp_task_owner.begin_shutdown();
    cont_tx.take();
    let local_ids = {
        let sessions = sessions.lock().await;
        let mut ids = Vec::with_capacity(sessions.len());
        for (session_id, state) in sessions.iter() {
            ids.push(session_id.clone());
            if let Some(token) = state.cancel_token.as_ref() {
                token.cancel();
            }
        }
        ids
    };
    let mut all_ids: BTreeSet<String> = local_ids.into_iter().collect();
    all_ids.extend(cfg.session_manager.session_ids());
    for session_id in &all_ids {
        cfg.session_manager.pre_close_session(session_id);
    }
    let host_report = task_owner.shutdown().await;
    if let task_scope::HostShutdownReport::Incomplete { unfinished } = host_report {
        tracing::warn!(unfinished, "ACP host task drain incomplete");
    }
    let _ = mcp_task_owner.shutdown().await;
    for session_id in &all_ids {
        if !cfg.session_manager.drain_session_tasks(session_id).await {
            continue;
        }
        if let Some(session) = cfg.session_manager.take_for_close(session_id) {
            closing_sessions.insert(session_id.clone(), session);
        }
    }
    let closing_ids: Vec<_> = closing_sessions.keys().cloned().collect();
    for session_id in closing_ids {
        let session = closing_sessions
            .get(&session_id)
            .expect("closing session retained");
        if session.close_resources().await {
            closing_sessions.remove(&session_id);
        } else {
            tracing::warn!(session_id = %session_id, "session resources retained for shutdown retry");
        }
    }
    let session_close_failures = closing_sessions.len() + cfg.session_manager.session_ids().len();
    let environments = {
        let mut sessions = sessions.lock().await;
        sessions
            .values_mut()
            .filter_map(|state| {
                state.closing = true;
                state
                    .environment
                    .clone()
                    .map(|environment| (state.session_id.clone(), environment))
            })
            .collect::<Vec<_>>()
    };
    let mut environment_failures = 0;
    for (session_id, environment) in environments {
        if !matches!(host_report, task_scope::HostShutdownReport::Complete)
            || cfg.session_manager.get_session(&session_id).is_some()
            || closing_sessions.contains_key(&session_id)
            || !environment.shutdown().await
        {
            environment_failures += 1;
        }
    }
    let pool_report = if let Some(pool) = cfg.mcp_pool.as_ref() {
        let report = pool.shutdown().await;
        if let peri_acp_types::ports::McpPoolShutdownReport::Incomplete {
            settled_services,
            unfinished_services,
            failed_services,
        } = report
        {
            tracing::warn!(
                settled_services,
                unfinished_services,
                failed_services,
                "MCP pool service drain incomplete"
            );
        }
        report
    } else {
        peri_acp_types::ports::McpPoolShutdownReport::Complete {
            settled_services: 0,
            failed_services: 0,
        }
    };
    let terminal_report = task_scope::HostTerminalShutdownReport::aggregate(
        host_report,
        pool_report,
        session_close_failures + environment_failures,
    );
    match terminal_report {
        task_scope::HostTerminalShutdownReport::Complete { .. } => {
            sessions.lock().await.clear();
            prompt_locks.lock().await.clear();
            tracing::info!(?terminal_report, "ACP host terminal shutdown complete");
        }
        task_scope::HostTerminalShutdownReport::Incomplete { .. } => {
            tracing::warn!(?terminal_report, "ACP host terminal shutdown incomplete");
        }
    }
    terminal_report
}
