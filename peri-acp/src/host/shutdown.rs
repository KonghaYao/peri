//! Transport EOF closes admission before cancelling and draining owned resources.

use std::collections::BTreeSet;
use std::sync::Arc;

use peri_acp_types::ports::{LspPoolPort, McpTaskOwnerPort};
use tokio_util::sync::CancellationToken;

use super::{
    connection::ConnectionContext, task_scope, AcpServerConfig, PromptLocks, SharedSessions,
};

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
    if let Some(relay) = cfg.mcp_apps_relay.as_ref() {
        relay.close_connection(&connection_id);
    }
    connection.lock().await.begin_close();
    task_owner.begin_shutdown();
    if let Some(dynamic_mcp) = cfg.dynamic_mcp.as_ref() {
        dynamic_mcp.begin_shutdown();
    }
    if let Some(pool) = cfg.mcp_pool.as_ref() {
        pool.begin_shutdown();
    }
    mcp_task_owner.begin_shutdown();
    cont_tx.take();
    let (local_ids, mut lsp_pools) = {
        let sessions = sessions.lock().await;
        let mut ids = Vec::with_capacity(sessions.len());
        let mut pools = Vec::new();
        for (session_id, state) in sessions.iter() {
            ids.push(session_id.clone());
            if let Some(token) = state.cancel_token.as_ref() {
                token.cancel();
            }
            if let Some(pool) = state.lsp_pool.as_ref() {
                pools.push(Arc::clone(pool));
            }
        }
        (ids, pools)
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
    let dynamic_report = if let Some(dynamic_mcp) = cfg.dynamic_mcp.as_ref() {
        let report = dynamic_mcp.shutdown().await;
        if let peri_acp_types::dynamic_mcp::DynamicMcpShutdownReport::Incomplete {
            unfinished_instances,
        } = report
        {
            tracing::warn!(unfinished_instances, "Dynamic MCP service drain incomplete");
        }
        report
    } else {
        peri_acp_types::dynamic_mcp::DynamicMcpShutdownReport::Complete
    };
    let _ = mcp_task_owner.shutdown().await;
    for session_id in &all_ids {
        if let Some(session) = cfg.session_manager.take_for_close(session_id) {
            closing_sessions.insert(session_id.clone(), session);
        }
    }
    let closing_ids: Vec<_> = closing_sessions.keys().cloned().collect();
    for session_id in closing_ids {
        let session = closing_sessions
            .get(&session_id)
            .expect("closing session retained");
        if session.close_resources().await
            == peri_acp_types::dynamic_mcp::DynamicMcpShutdownReport::Complete
        {
            closing_sessions.remove(&session_id);
        } else {
            tracing::warn!(session_id = %session_id, "session resources retained for shutdown retry");
        }
    }
    let session_close_failures = closing_sessions.len();
    {
        let mut sessions = sessions.lock().await;
        for (_, state) in sessions.drain() {
            if let Some(pool) = state.lsp_pool {
                lsp_pools.push(pool);
            }
        }
    }
    prompt_locks.lock().await.clear();
    let mut unique_lsp = Vec::<Arc<dyn LspPoolPort>>::new();
    for pool in lsp_pools {
        if !unique_lsp.iter().any(|known| Arc::ptr_eq(known, &pool)) {
            unique_lsp.push(pool);
        }
    }
    for pool in unique_lsp {
        pool.shutdown().await;
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
        dynamic_report,
        pool_report,
        session_close_failures,
    );
    match terminal_report {
        task_scope::HostTerminalShutdownReport::Complete { .. } => {
            tracing::info!(?terminal_report, "ACP host terminal shutdown complete");
        }
        task_scope::HostTerminalShutdownReport::Incomplete { .. } => {
            tracing::warn!(?terminal_report, "ACP host terminal shutdown incomplete");
        }
    }
    terminal_report
}
