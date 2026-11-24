//! Transport EOF closes admission before cancelling and draining owned resources.

use std::collections::BTreeSet;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use peri_acp_types::ports::McpTaskOwnerPort;
use tokio_util::sync::CancellationToken;

use super::connection::ConnectionContext;
use super::task_scope;
use super::AcpServerConfig;
use super::PromptLocks;
use super::SharedSessions;

/// 关闭阶段，按执行顺序排列（预算超时的诊断标签）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ShutdownPhase {
    Admission,
    TaskOwners,
    SessionTasks,
    SessionResources,
    SessionEnvironments,
    McpPool,
}

impl ShutdownPhase {
    fn code(self) -> u8 {
        match self {
            Self::Admission => 0,
            Self::TaskOwners => 1,
            Self::SessionTasks => 2,
            Self::SessionResources => 3,
            Self::SessionEnvironments => 4,
            Self::McpPool => 5,
        }
    }

    fn from_code(code: u8) -> Self {
        match code {
            0 => Self::Admission,
            1 => Self::TaskOwners,
            2 => Self::SessionTasks,
            3 => Self::SessionResources,
            4 => Self::SessionEnvironments,
            _ => Self::McpPool,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Admission => "admission",
            Self::TaskOwners => "host_and_mcp_task_owners",
            Self::SessionTasks => "session_task_drain",
            Self::SessionResources => "session_resource_close",
            Self::SessionEnvironments => "session_environment_shutdown",
            Self::McpPool => "mcp_pool_shutdown",
        }
    }
}

/// 关闭阶段进度：只用于预算超时时报告「卡在哪个阶段、还有几项没完成」。
///
/// 原子量不参与所有权裁决——预算到期后 `shutdown_host` 的 future 被丢弃，
/// 各阶段的实际 owner 仍留在 `HostExitContext` 里供重试。
#[derive(Debug)]
pub(super) struct ShutdownProgress {
    phase: AtomicU8,
    pending: AtomicUsize,
}

impl ShutdownProgress {
    pub(super) fn new() -> Self {
        Self {
            phase: AtomicU8::new(ShutdownPhase::Admission.code()),
            pending: AtomicUsize::new(0),
        }
    }

    fn enter(&self, phase: ShutdownPhase, pending: usize) {
        self.phase.store(phase.code(), Ordering::Relaxed);
        self.pending.store(pending, Ordering::Relaxed);
    }

    fn item_done(&self) {
        self.pending.fetch_sub(1, Ordering::Relaxed);
    }

    /// 预算到期：报告当前阶段与本阶段尚未观察完成的批内项目。
    pub(super) fn warn_budget_expired(&self) {
        let phase = ShutdownPhase::from_code(self.phase.load(Ordering::Relaxed));
        let pending = self.pending.load(Ordering::Relaxed);
        tracing::warn!(
            phase = phase.label(),
            pending,
            "ACP host shutdown budget expired; resource drain reported Incomplete"
        );
    }
}

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
    closing_sessions: &mut std::collections::BTreeMap<String, Arc<crate::session::AcpSession>>,
    progress: &ShutdownProgress,
) -> task_scope::HostTerminalShutdownReport {
    progress.enter(ShutdownPhase::Admission, 0);
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
    {
        let mut connection = connection.lock().await;
        // 实时流归连接：SessionState 可能仍持有 `Arc`，必须在连接收口时显式清空。
        connection.live.close_all();
        connection.begin_close();
    }
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

    // host task owner 与 MCP task owner 互不依赖：并行排空，墙钟接近 max 而非 sum。
    progress.enter(ShutdownPhase::TaskOwners, 2);
    let host_drain = async {
        let report = task_owner.shutdown().await;
        progress.item_done();
        report
    };
    let mcp_drain = async {
        let report = mcp_task_owner.shutdown().await;
        progress.item_done();
        report
    };
    let (host_report, _) = tokio::join!(host_drain, mcp_drain);
    if let task_scope::HostShutdownReport::Incomplete { unfinished } = host_report {
        tracing::warn!(unfinished, "ACP host task drain incomplete");
    }

    // 会话任务彼此独立：并行 drain，全部收口后才允许关闭会话资源。
    progress.enter(ShutdownPhase::SessionTasks, all_ids.len());
    let mut drained: BTreeSet<String> = BTreeSet::new();
    let mut drain_set = tokio::task::JoinSet::new();
    for session_id in &all_ids {
        let manager = cfg.session_manager.clone();
        let session_id = session_id.clone();
        drain_set.spawn(async move {
            let drained = manager.drain_session_tasks(&session_id).await;
            (session_id, drained)
        });
    }
    while let Some(joined) = drain_set.join_next().await {
        progress.item_done();
        match joined {
            Ok((session_id, true)) => {
                drained.insert(session_id);
            }
            Ok((_, false)) => {}
            Err(error) => tracing::warn!(%error, "session task drain task failed"),
        }
    }
    for session_id in &all_ids {
        if !drained.contains(session_id) {
            continue;
        }
        if let Some(session) = cfg.session_manager.take_for_close(session_id) {
            closing_sessions.insert(session_id.clone(), Arc::new(session));
        }
    }

    // 会话资源同样彼此独立：并行 close，失败项留在 map 里供重试。
    progress.enter(ShutdownPhase::SessionResources, closing_sessions.len());
    let mut close_set = tokio::task::JoinSet::new();
    for (session_id, session) in closing_sessions.iter() {
        let session = Arc::clone(session);
        let session_id = session_id.clone();
        close_set.spawn(async move { (session_id, session.close_resources().await) });
    }
    while let Some(joined) = close_set.join_next().await {
        progress.item_done();
        match joined {
            Ok((session_id, true)) => {
                closing_sessions.remove(&session_id);
                cfg.session_manager.forget_recovered_child(&session_id);
            }
            Ok((session_id, false)) => {
                tracing::warn!(session_id = %session_id, "session resources retained for shutdown retry");
            }
            Err(error) => tracing::warn!(%error, "session resource close task failed"),
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
    // 仍有未收口会话（drain/close 失败或记录仍在）时不关闭其环境：环境保留在同一
    // 退出 context 里供重试，不能把「还要重试」伪装成已关闭。
    let host_complete = matches!(host_report, task_scope::HostShutdownReport::Complete);
    let environment_total = environments.len();
    let ready_environments: Vec<_> = environments
        .into_iter()
        .filter(|(session_id, _)| {
            host_complete
                && cfg.session_manager.get_session(session_id).is_none()
                && !closing_sessions.contains_key(session_id)
        })
        .collect();
    // 未获准关闭的环境（host 未排空或会话仍需重试）同样计入失败：环境所有者留在
    // 退出 context 中，本次关闭不能宣称它已收口。
    let mut environment_failures = environment_total - ready_environments.len();
    progress.enter(ShutdownPhase::SessionEnvironments, ready_environments.len());
    let mut environment_set = tokio::task::JoinSet::new();
    for (session_id, environment) in ready_environments {
        environment_set.spawn(async move { (session_id, environment.shutdown().await) });
    }
    while let Some(joined) = environment_set.join_next().await {
        progress.item_done();
        match joined {
            Ok((_, true)) => {}
            Ok((session_id, false)) => {
                environment_failures += 1;
                tracing::warn!(session_id = %session_id, "session environment shutdown incomplete");
            }
            Err(error) => {
                environment_failures += 1;
                tracing::warn!(%error, "session environment shutdown task failed");
            }
        }
    }

    progress.enter(ShutdownPhase::McpPool, 1);
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
    progress.item_done();
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
