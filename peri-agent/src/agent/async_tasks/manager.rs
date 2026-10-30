use std::sync::Arc;

use peri_acp_types::tasks::{
    BgRegistryEvent, BgShellHandle, BgTaskKind, BgTaskRegistration, ExternalTaskRegistration,
    TaskChange, TaskRegistrationTicket, TaskSnapshot,
};
use sha2::{Digest, Sha256};

use crate::agent::events::BackgroundTaskResult;

use super::registry::{
    BackgroundRegistryError, BackgroundTask, BackgroundTaskRegistry, BackgroundTaskStatus,
    BgCancelHandle, BgTaskInfo,
};
use super::shell::finalize_bg_shell;
use super::{QueuedSubagentMessage, ShellExecutor, SubagentMessageError};

// ── TaskManager（per-session 聚合）────────────────────────────────────────────

/// per-session 后台任务管理器（L1 迁移点：Agent 层 async tasks manager）。
///
/// 聚合 `BackgroundTaskRegistry` 与注入的 shell 执行环境端口。随 session 创建/销毁；`cancel_all` 供 session 销毁时
/// 取消所有 owned 任务（§9 销毁顺序：取消 owned tasks）。
///
/// Session consumers use `snapshot` plus `subscribe_events`; the legacy
/// single-sender hook remains only for older test adapters.
pub struct TaskManager {
    registry: Arc<BackgroundTaskRegistry>,
    shell_executor: Option<Arc<dyn ShellExecutor>>,
    external_registration: parking_lot::Mutex<()>,
    child_owners: parking_lot::RwLock<
        std::collections::HashMap<String, Arc<crate::session::subagent::ParkedChildOwner>>,
    >,
}

fn external_source_identity(request: &ExternalTaskRegistration) -> String {
    let mut hasher = Sha256::new();
    for part in [
        &request.session_id,
        &request.owner_identity,
        &request.owner_task_id,
    ] {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    format!("mcp-{hex}")
}

fn external_started_at(
    request: &ExternalTaskRegistration,
) -> Result<chrono::DateTime<chrono::Utc>, BackgroundRegistryError> {
    request.started_at.as_deref().map_or_else(
        || Ok(peri_time::now_wall().into()),
        |timestamp| {
            chrono::DateTime::parse_from_rfc3339(timestamp)
                .map(|parsed| parsed.with_timezone(&chrono::Utc))
                .map_err(|_| BackgroundRegistryError::InvalidStartedAt)
        },
    )
}

fn external_background_task(
    request: &ExternalTaskRegistration,
    task_id: String,
    started_at: chrono::DateTime<chrono::Utc>,
) -> BackgroundTask {
    BackgroundTask {
        id: task_id,
        agent_name: "mcp".into(),
        prompt_summary: request.summary.clone(),
        status: BackgroundTaskStatus::Running,
        started_at: peri_time::monotonic_now(),
        chrono_started_at: started_at,
        kind: request.kind,
        cancel_handle: BgCancelHandle::External {
            cancel: Arc::clone(&request.cancel),
            on_terminal: Arc::clone(&request.on_terminal),
        },
        cancel_token: None,
        pid: None,
        output_preview: None,
        agent_inbox: None,
        initiator_session_id: request.initiator_session_id.clone(),
        owner_session_id: Some(request.session_id.clone()),
        owner_identity: Some(request.owner_identity.clone()),
    }
}

struct ExternalCompletionClaim<'manager> {
    registry: &'manager BackgroundTaskRegistry,
    ticket: &'manager TaskRegistrationTicket,
}

impl Drop for ExternalCompletionClaim<'_> {
    fn drop(&mut self) {
        self.registry.reset_completion_claim(self.ticket);
    }
}

use super::delivery::terminal_delivery_id as external_delivery_id;

#[cfg(test)]
#[path = "external_settlement_test.rs"]
mod external_settlement_tests;

impl Default for TaskManager {
    fn default() -> Self {
        Self::new()
    }
}

impl peri_acp_types::tasks::TaskManager for TaskManager {
    fn child_task_manager(
        &self,
        child_id: &str,
    ) -> Option<Arc<dyn peri_acp_types::tasks::TaskManager>> {
        self.child_owner(child_id)
            .map(|owner| owner.manager.clone() as Arc<dyn peri_acp_types::tasks::TaskManager>)
    }

    fn close_child_resources(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + '_>> {
        Box::pin(async move {
            let owners = self.child_owners();
            for owner in owners {
                owner.close().await?;
                self.release_child_owner(&owner.child_thread_id);
            }
            Ok(())
        })
    }
    fn register_external_terminal(
        &self,
        request: ExternalTaskRegistration,
    ) -> Result<TaskRegistrationTicket, String> {
        self.register_external_terminal(request)
            .map_err(|error| error.to_string())
    }
    fn refresh_external(
        &self,
        ticket: &TaskRegistrationTicket,
        request: ExternalTaskRegistration,
    ) -> Result<(), String> {
        self.refresh_external(ticket, request)
            .map_err(|error| error.to_string())
    }
    fn external_task_ids(&self) -> Vec<String> {
        self.registry.external_task_ids()
    }
    fn has_unsettled_external(&self) -> bool {
        self.registry.has_unsettled_mcp()
    }
    fn mark_external_lost(&self, task_id: &str) -> bool {
        self.mark_external_lost(task_id)
    }
    fn mark_external_running(&self, task_id: &str) -> bool {
        self.mark_external_running(task_id)
    }
    fn snapshot(&self) -> TaskSnapshot {
        self.registry.snapshot()
    }

    fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<TaskChange> {
        self.registry.subscribe_events()
    }

    fn register_external(
        &self,
        request: ExternalTaskRegistration,
    ) -> Result<TaskRegistrationTicket, String> {
        self.register_external(request)
            .map_err(|error| error.to_string())
    }

    fn settle_external(
        &self,
        ticket: &TaskRegistrationTicket,
        terminal_transition_id: &str,
        result: BackgroundTaskResult,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<bool, String>> + Send + '_>>
    {
        let ticket = ticket.clone();
        let terminal_transition_id = terminal_transition_id.to_owned();
        Box::pin(async move {
            self.settle_external(&ticket, &terminal_transition_id, result)
                .await
        })
    }

    fn abandon_external(
        &self,
        ticket: &TaskRegistrationTicket,
        reason: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<bool, String>> + Send + '_>>
    {
        let ticket = ticket.clone();
        let reason = reason.to_owned();
        Box::pin(async move { self.abandon_external(&ticket, &reason).await })
    }

    fn cancel_async(
        &self,
        task_id: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + '_>> {
        let task_id = task_id.to_owned();
        Box::pin(async move {
            self.cancel_async(&task_id)
                .await
                .map_err(|error| error.to_string())
        })
    }
    fn confirm_external_execution_stopped(&self, ticket: &TaskRegistrationTicket) {
        self.registry.confirm_external_stopped(ticket);
    }
    fn is_execution_idle(&self) -> bool {
        self.registry.scope.is_idle() && self.registry.external_settled()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn set_event_sender(
        &self,
        sender: tokio::sync::mpsc::UnboundedSender<BgRegistryEvent>,
        session_id: String,
    ) {
        self.set_event_sender(sender, session_id);
    }

    fn active_count(&self) -> usize {
        self.active_count()
    }

    fn register(&self, request: BgTaskRegistration) -> Result<TaskRegistrationTicket, String> {
        let cancel_handle = match request.kind {
            BgTaskKind::Shell => match request.kill {
                Some(kill) => BgCancelHandle::Kill(Some(kill)),
                None => request
                    .pid
                    .and_then(|pid| {
                        self.shell_executor
                            .as_ref()
                            .and_then(|executor| {
                                executor.cancel_callback(pid, Arc::clone(&self.registry))
                            })
                            .map(|kill| BgCancelHandle::Kill(Some(kill)))
                    })
                    .ok_or_else(|| {
                        "bg shell register: execution-environment cancellation handle unavailable"
                            .to_string()
                    })?,
            },
            BgTaskKind::Workflow => BgCancelHandle::Kill(request.kill),
            BgTaskKind::Agent => BgCancelHandle::Kill(request.kill),
            BgTaskKind::Mcp => return Err("MCP tasks must use register_external".into()),
        };
        let task = BackgroundTask {
            id: request.task_id,
            agent_name: match request.kind {
                BgTaskKind::Shell => "bg-shell",
                BgTaskKind::Agent => "agent",
                BgTaskKind::Workflow => "workflow",
                BgTaskKind::Mcp => "mcp",
            }
            .to_string(),
            prompt_summary: request.summary,
            status: BackgroundTaskStatus::Running,
            started_at: peri_time::monotonic_now(),
            chrono_started_at: peri_time::now_wall().into(),
            kind: request.kind,
            cancel_handle,
            cancel_token: None,
            pid: request.pid,
            output_preview: None,
            agent_inbox: None,
            initiator_session_id: None,
            owner_session_id: None,
            owner_identity: None,
        };
        self.register_with_kind(task).map_err(|e| e.to_string())
    }

    fn complete(&self, ticket: &TaskRegistrationTicket, result: BackgroundTaskResult) -> bool {
        self.complete(ticket, result)
    }

    fn settle_completed(
        &self,
        ticket: &TaskRegistrationTicket,
        result: BackgroundTaskResult,
        delivery: peri_acp_types::tasks::OnBgCompleteFn,
    ) -> Result<bool, String> {
        self.settle_completed(ticket, result, delivery)
            .map_err(|error| error.to_string())
    }

    fn retry_pending_deliveries(&self) -> usize {
        self.retry_pending_deliveries()
    }

    fn cancel(&self, task_id: &str) -> Result<(), String> {
        self.cancel(task_id).map_err(|e| e.to_string())
    }

    fn cancel_all(&self) {
        self.cancel_all();
    }

    fn execution_cancel_token(&self) -> Option<tokio_util::sync::CancellationToken> {
        Some(self.registry.scope.cancel_token())
    }

    fn spawn_owned(
        &self,
        task: peri_acp_types::tasks::OwnedTaskFuture,
    ) -> Result<tokio::task::JoinHandle<()>, String> {
        self.registry.scope.spawn(task)
    }

    fn begin_external_execution(
        &self,
        scope: &str,
    ) -> Result<Box<dyn peri_acp_types::tasks::ExternalExecutionGuard>, String> {
        self.registry.scope.begin_external(scope)
    }

    fn resolve_external_execution_evidence(&self, scope: &str) -> usize {
        self.registry.scope.resolve_external_evidence(scope)
    }

    fn shutdown(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = peri_acp_types::tasks::TaskShutdownReport> + Send + '_,
        >,
    > {
        Box::pin(async move {
            self.registry.scope.close();
            // Deployment/transport release must never cancel remote MCP tasks.
            self.cancel_all();
            let execution_stopped = self.registry.scope.wait().await;
            self.registry.retry_pending_deliveries();
            if execution_stopped && self.registry.external_settled() {
                peri_acp_types::tasks::TaskShutdownReport::Complete
            } else {
                peri_acp_types::tasks::TaskShutdownReport::Incomplete
            }
        })
    }

    fn spawn_shell(
        &self,
        command: String,
        cwd: String,
        timeout_ms: Option<u64>,
        on_bg_complete: Option<peri_acp_types::tasks::OnBgCompleteFn>,
    ) -> Result<BgShellHandle, Box<dyn std::error::Error + Send + Sync>> {
        self.spawn_shell(command, cwd, timeout_ms, on_bg_complete)
    }

    fn finalize_bg_shell(
        &self,
        on_bg_complete: &Option<peri_acp_types::tasks::OnBgCompleteFn>,
        ticket: TaskRegistrationTicket,
        prompt_summary: String,
        success: bool,
        output: String,
        duration_ms: u64,
        timed_out: bool,
        shell_output: Option<peri_acp_types::event::ShellOutput>,
    ) {
        finalize_bg_shell(
            &self.registry,
            on_bg_complete,
            ticket,
            prompt_summary,
            success,
            output,
            duration_ms,
            timed_out,
            shell_output,
        );
    }
}

impl TaskManager {
    pub fn child_owner(
        &self,
        child_id: &str,
    ) -> Option<Arc<crate::session::subagent::ParkedChildOwner>> {
        let children: Vec<_> = self.child_owners.read().values().cloned().collect();
        for child in children {
            if child.child_thread_id == child_id {
                return Some(child);
            }
            if let Some(descendant) = child.manager.child_owner(child_id) {
                return Some(descendant);
            }
        }
        None
    }

    pub fn child_owners(&self) -> Vec<Arc<crate::session::subagent::ParkedChildOwner>> {
        self.child_owners.read().values().cloned().collect()
    }

    /// 只在 owner 已完成消息与资源结算后调用。
    pub fn release_child_owner(&self, child_id: &str) -> bool {
        self.child_owners.write().remove(child_id).is_some()
    }

    /// 父结果是否已由 registry 的单一 retry worker 接管。
    pub fn has_pending_delivery(&self, task_id: &str) -> bool {
        self.registry.has_pending_delivery(task_id)
    }

    pub fn has_pending_deliveries(&self) -> bool {
        self.registry.has_pending_deliveries()
    }

    pub fn has_unsettled_tasks(&self) -> bool {
        self.active_count() > 0 || !self.registry.external_settled()
    }

    pub(crate) fn register_child_owner(
        self: &Arc<Self>,
        owner: Arc<crate::session::subagent::ParkedChildOwner>,
    ) -> Result<(), String> {
        let _admission = self.registry.scope.admit()?;
        owner.attach_parent(self);
        let mut children = self.child_owners.write();
        if children.contains_key(&owner.child_thread_id) {
            return Err("child resource owner is already registered".into());
        }
        children.insert(owner.child_thread_id.clone(), owner);
        Ok(())
    }

    pub(crate) fn begin_session_close(&self) {
        self.registry.scope.close();
    }

    pub fn session_close_settled(&self) -> bool {
        self.registry.scope.is_closed()
            && self.registry.scope.is_idle()
            && self.registry.external_settled()
    }

    pub(crate) async fn wait_session_close(&self) -> bool {
        self.registry.scope.wait().await && self.registry.external_settled()
    }
    pub fn mark_external_lost(&self, task_id: &str) -> bool {
        self.registry.mark_external_status(task_id, "lost")
    }

    pub fn mark_external_running(&self, task_id: &str) -> bool {
        self.registry.mark_external_status(task_id, "running")
    }
    pub fn snapshot(&self) -> TaskSnapshot {
        self.registry.snapshot()
    }

    /// Unsettled tasks used by the bounded-wait handoff record.
    pub fn pending_handoff_tasks(&self) -> Vec<super::handoff::PendingHandoffTask> {
        self.registry.pending_handoff_tasks()
    }

    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<TaskChange> {
        self.registry.subscribe_events()
    }

    pub fn register_external(
        &self,
        request: ExternalTaskRegistration,
    ) -> Result<TaskRegistrationTicket, BackgroundRegistryError> {
        self.register_external_source(request, false)
    }

    pub fn register_external_terminal(
        &self,
        request: ExternalTaskRegistration,
    ) -> Result<TaskRegistrationTicket, BackgroundRegistryError> {
        self.register_external_source(request, true)
    }

    fn register_external_source(
        &self,
        request: ExternalTaskRegistration,
        terminal: bool,
    ) -> Result<TaskRegistrationTicket, BackgroundRegistryError> {
        let _registration = self.external_registration.lock();
        let started_at = external_started_at(&request)?;
        // A new source admission has a fresh public identity. Callback refresh
        // and terminal replay must retain its ticket, never reconstruct by ID.
        let task_id = format!("mcp-{}", uuid::Uuid::now_v7());
        self.registry.register_external_task(
            external_background_task(&request, task_id, started_at),
            external_source_identity(&request),
            terminal,
        )
    }

    pub fn refresh_external(
        &self,
        ticket: &TaskRegistrationTicket,
        request: ExternalTaskRegistration,
    ) -> Result<(), BackgroundRegistryError> {
        let _registration = self.external_registration.lock();
        self.registry.validate_ticket(ticket)?;
        if ticket.source_identity() != Some(external_source_identity(&request).as_str()) {
            return Err(BackgroundRegistryError::ExternalIdentityConflict(
                ticket.task_id().into(),
            ));
        }
        let recorded = ticket.initiator_session_id();
        if let (Some(recorded), Some(requested)) = (&recorded, &request.initiator_session_id) {
            if recorded != requested {
                return Err(BackgroundRegistryError::ExternalInitiatorConflict {
                    task_id: ticket.task_id().into(),
                    recorded: recorded.clone(),
                    requested: requested.clone(),
                });
            }
        }
        external_started_at(&request)?;
        if ticket.is_settled() || (recorded.is_some() && request.initiator_session_id.is_none()) {
            return Ok(());
        }
        let refresh = self.registry.external_callback_refresh_allowed(
            ticket.task_id(),
            request.initiator_session_id.as_deref(),
        )?;
        if refresh {
            self.registry.refresh_external_callbacks(
                ticket.task_id(),
                request.cancel,
                request.on_terminal,
                request.initiator_session_id,
            );
        }
        Ok(())
    }

    /// Bounded give-up for a task whose owner stayed unobservable: one final,
    /// explicitly marked reminder plus a terminal projection, so the task can
    /// neither stay active forever nor disappear silently.
    pub async fn abandon_external(
        &self,
        ticket: &TaskRegistrationTicket,
        reason: &str,
    ) -> Result<bool, String> {
        self.registry
            .validate_ticket(ticket)
            .map_err(|error| error.to_string())?;
        if ticket.is_settled() {
            return Ok(false);
        }
        let task_id = ticket.task_id();
        let notify = {
            let _registration = self.external_registration.lock();
            let Some(notify) = self.registry.external_notify(task_id) else {
                return Err(format!(
                    "external task {task_id} has no terminal delivery route"
                ));
            };
            self.registry
                .ensure_external_routable(task_id)
                .map_err(|error| error.to_string())?;
            if !self.registry.claim_completion(ticket) {
                return Ok(false);
            }
            notify
        };
        let _claim = ExternalCompletionClaim {
            registry: &self.registry,
            ticket,
        };
        tracing::warn!(task_id, reason, "abandoning unobservable external task");
        let mut result = BackgroundTaskResult {
            task_id: task_id.to_owned(),
            agent_name: "mcp".into(),
            prompt_summary: "unobservable task owner".into(),
            success: false,
            output: "The task owner stayed unobservable for the whole bounded retry window and \
                     the task was abandoned as unresolved. Its remote side effects are unknown; \
                     check the owner directly before assuming it stopped."
                .into(),
            tool_calls_count: 0,
            duration_ms: 0,
            timed_out: true,
            child_thread_id: None,
            subagent_failure: None,
            shell_output: None,
        };
        result.task_id = task_id.to_owned();
        // Stable delivery ID: retries of the give-up converge on one reminder.
        let delivery_id = external_delivery_id(task_id, "abandoned-unresolved");
        notify(&result, delivery_id).await?;
        if self.registry.complete(ticket, result) {
            Ok(true)
        } else {
            Err(format!(
                "external task {task_id} terminal projection was not committed"
            ))
        }
    }

    pub async fn settle_external(
        &self,
        ticket: &TaskRegistrationTicket,
        terminal_transition_id: &str,
        mut result: BackgroundTaskResult,
    ) -> Result<bool, String> {
        if terminal_transition_id.is_empty() {
            return Err("terminal transition ID is required".into());
        }
        self.registry
            .validate_ticket(ticket)
            .map_err(|error| error.to_string())?;
        let task_id = ticket.task_id();
        let already_settled = || ticket.is_settled();
        let notify = {
            let _registration = self.external_registration.lock();
            let Some(notify) = self.registry.external_notify(task_id) else {
                return if already_settled() {
                    Ok(false)
                } else {
                    Err(format!(
                        "external task {task_id} has no terminal delivery route"
                    ))
                };
            };
            self.registry
                .ensure_external_routable(task_id)
                .map_err(|error| error.to_string())?;
            if !self.registry.claim_completion(ticket) {
                return if already_settled() {
                    Ok(false)
                } else {
                    Err(format!(
                        "external task {task_id} settlement is still pending"
                    ))
                };
            }
            notify
        };
        let _claim = ExternalCompletionClaim {
            registry: &self.registry,
            ticket,
        };
        result.task_id = task_id.to_owned();
        let delivery_id = external_delivery_id(task_id, terminal_transition_id);
        notify(&result, delivery_id).await?;
        if self.registry.complete(ticket, result) {
            Ok(true)
        } else {
            Err(format!(
                "external task {task_id} terminal projection was not committed"
            ))
        }
    }

    pub async fn cancel_async(&self, task_id: &str) -> Result<(), BackgroundRegistryError> {
        if let Some(cancel) = self.registry.external_cancel(task_id) {
            cancel()
                .await
                .map_err(|_| BackgroundRegistryError::ExternalCancelFailed(task_id.into()))
        } else {
            self.registry.cancel(task_id)
        }
    }
    pub fn new() -> Self {
        Self {
            registry: Arc::new(BackgroundTaskRegistry::new()),
            shell_executor: None,
            external_registration: parking_lot::Mutex::new(()),
            child_owners: parking_lot::RwLock::default(),
        }
    }

    pub fn with_shell_executor(executor: Arc<dyn ShellExecutor>) -> Self {
        Self {
            shell_executor: Some(executor),
            ..Self::new()
        }
    }

    /// 作用域**已关闭**的实例：任何后台接管（`spawn_owned` /
    /// `begin_external_execution`）都被准入拒绝，调用方按「未执行 + 告警」处理。
    ///
    /// 装配面用于「拿到的任务管理器不是本部署的具体类型」这一失败分支：它**不**
    /// 冒充会话级管理器——既不建游离作用域，也不静默接收任务；与真正拿不到
    /// 会话级管理器的后果（后台 hook 不进入会话作用域）相比，显式不执行是
    /// fail-closed 的诚实结果。
    pub fn without_session_scope() -> Self {
        let manager = Self::new();
        manager.registry.scope.close();
        manager
    }

    /// 访问底层 registry（workflow 适配 / ACP 侧 Snapshot 等场景）
    pub fn registry(&self) -> &Arc<BackgroundTaskRegistry> {
        &self.registry
    }

    // ── 事件桥接（过渡态，ACP executor 注入 BgRegistryEvent 泵）──

    pub fn set_event_sender(
        &self,
        sender: tokio::sync::mpsc::UnboundedSender<BgRegistryEvent>,
        session_id: String,
    ) {
        self.registry.set_event_sender(sender, session_id);
    }

    pub fn clear_event_sender(&self) {
        self.registry.clear_event_sender();
    }

    // ── registry 委托（Middleware 经 TaskManager 发起，不直接持有 registry）──

    pub fn active_count(&self) -> usize {
        self.registry.active_count()
    }

    pub fn count_by_kind(&self, kind: BgTaskKind) -> usize {
        self.registry.count_by_kind(kind)
    }

    pub fn register_with_kind(
        &self,
        task: BackgroundTask,
    ) -> Result<TaskRegistrationTicket, BackgroundRegistryError> {
        self.registry.register_with_kind(task)
    }

    pub(crate) fn register_pending_start(
        &self,
        task: BackgroundTask,
    ) -> Result<TaskRegistrationTicket, BackgroundRegistryError> {
        self.registry.register_pending_start(task)
    }

    pub(crate) fn mark_execution_started(
        &self,
        ticket: &TaskRegistrationTicket,
    ) -> Result<(), BackgroundRegistryError> {
        self.registry.mark_execution_started(ticket)
    }

    pub(crate) fn withdraw_unstarted_registration(
        &self,
        ticket: &TaskRegistrationTicket,
    ) -> Result<bool, BackgroundRegistryError> {
        self.registry.withdraw_unstarted_registration(ticket)
    }

    /// Send Defer to a live child in this session. `None` means no registered
    /// receiver; an error must not fall through to resume or create an execution.
    pub fn send_subagent_message(
        &self,
        thread_id: &str,
        prompt: Option<&str>,
    ) -> Result<Option<QueuedSubagentMessage>, SubagentMessageError> {
        self.registry.send_subagent_message(thread_id, prompt)
    }

    pub fn complete(&self, ticket: &TaskRegistrationTicket, result: BackgroundTaskResult) -> bool {
        self.registry.complete(ticket, result)
    }

    pub fn settle_completed(
        &self,
        ticket: &TaskRegistrationTicket,
        result: BackgroundTaskResult,
        delivery: peri_acp_types::tasks::OnBgCompleteFn,
    ) -> Result<bool, BackgroundRegistryError> {
        self.registry.settle_completed(ticket, result, delivery)
    }

    pub fn retry_pending_deliveries(&self) -> usize {
        self.registry.retry_pending_deliveries()
    }

    pub fn cancel(&self, task_id: &str) -> Result<(), BackgroundRegistryError> {
        self.registry.cancel(task_id)
    }

    pub fn list_tasks(&self) -> Vec<(String, BackgroundTaskStatus, String)> {
        self.registry.list_tasks()
    }

    pub fn list_tasks_full(&self) -> Vec<BgTaskInfo> {
        self.registry.list_tasks_full()
    }

    pub fn cleanup_completed(&self) {
        self.registry.cleanup_completed();
    }

    /// 取消全部运行中任务（session 销毁时调用，§9 销毁顺序「取消 owned tasks」）。
    ///
    /// 逐条 `cancel()`：不可取消条目（Kill(None)）如实保留（等待自然完成），
    /// 其余按 kind 分发（Abort 优雅退出 + 超时 abort 兜底 / 执行环境 Kill 闭包）。
    pub fn cancel_all(&self) {
        let task_ids = self.registry.local_task_ids();
        for task_id in task_ids {
            if let Err(e) = self.registry.cancel(&task_id) {
                tracing::warn!(
                    task_id = %task_id,
                    error = %e,
                    "task_manager.cancel_all: cancel failed (entry kept)"
                );
            }
        }
    }

    pub fn spawn_shell(
        &self,
        command: String,
        cwd: String,
        timeout_ms: Option<u64>,
        on_bg_complete: Option<peri_acp_types::tasks::OnBgCompleteFn>,
    ) -> Result<BgShellHandle, Box<dyn std::error::Error + Send + Sync>> {
        let executor = self
            .shell_executor
            .as_ref()
            .ok_or("shell execution environment is not configured")?;
        let ownership = self.registry.scope.begin_external("workspace")?;
        let _admission = self.registry.scope.admit()?;
        executor.spawn(
            Arc::clone(&self.registry),
            ownership,
            command,
            cwd,
            timeout_ms,
            on_bg_complete,
        )
    }
}
