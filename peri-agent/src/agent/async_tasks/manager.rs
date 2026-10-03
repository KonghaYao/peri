use std::sync::Arc;

use peri_acp_types::tasks::{
    BgRegistryEvent, BgShellHandle, BgTaskKind, BgTaskRegistration, ExternalTaskRegistration,
    TaskChange, TaskSnapshot,
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
}

fn external_task_id(request: &ExternalTaskRegistration) -> String {
    let mut hasher = Sha256::new();
    for part in [
        &request.session_id,
        &request.owner_identity,
        &request.owner_task_id,
    ] {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    format!("mcp-{:x}", hasher.finalize())
}

fn external_started_at(request: &ExternalTaskRegistration) -> Result<chrono::DateTime<chrono::Utc>, BackgroundRegistryError> {
    request.started_at.as_deref().map_or_else(
        || Ok(chrono::Utc::now()),
        |timestamp| chrono::DateTime::parse_from_rfc3339(timestamp)
            .map(|parsed| parsed.with_timezone(&chrono::Utc))
            .map_err(|_| BackgroundRegistryError::InvalidStartedAt),
    )
}

fn external_delivery_id(
    task_id: &str,
    terminal_transition_id: &str,
) -> peri_acp_types::messages::MessageId {
    let mut hasher = Sha256::new();
    hasher.update((task_id.len() as u64).to_be_bytes());
    hasher.update(task_id.as_bytes());
    hasher.update((terminal_transition_id.len() as u64).to_be_bytes());
    hasher.update(terminal_transition_id.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    peri_acp_types::messages::MessageId::from(uuid::Uuid::from_bytes(bytes))
}

impl Default for TaskManager {
    fn default() -> Self {
        Self::new()
    }
}

impl peri_acp_types::tasks::TaskManager for TaskManager {
    fn restore_external_terminal(
        &self,
        request: ExternalTaskRegistration,
        terminal_transition_id: &str,
        result: BackgroundTaskResult,
    ) -> Result<String, String> {
        self.restore_external_terminal(request, terminal_transition_id, result)
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

    fn register_external(&self, request: ExternalTaskRegistration) -> Result<String, String> {
        self.register_external(request)
            .map_err(|error| error.to_string())
    }

    fn settle_external(
        &self,
        task_id: &str,
        terminal_transition_id: &str,
        result: BackgroundTaskResult,
    ) -> Result<bool, String> {
        self.settle_external(task_id, terminal_transition_id, result)
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
    fn confirm_external_execution_stopped(&self, task_id: &str) {
        self.registry.confirm_external_stopped(task_id);
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

    fn register(&self, request: BgTaskRegistration) -> Result<(), String> {
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
            started_at: std::time::Instant::now(),
            chrono_started_at: chrono::Utc::now(),
            kind: request.kind,
            cancel_handle,
            cancel_token: None,
            pid: request.pid,
            output_preview: None,
            agent_inbox: None,
        };
        self.register_with_kind(task).map_err(|e| e.to_string())
    }

    fn complete(&self, task_id: &str, result: BackgroundTaskResult) -> bool {
        self.complete(task_id, result)
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
    ) -> Result<Box<dyn peri_acp_types::tasks::ExternalExecutionGuard>, String> {
        self.registry.scope.begin_external()
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
            if self.registry.scope.wait().await && self.registry.external_settled() {
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
        on_bg_complete: Option<Arc<dyn Fn(&BackgroundTaskResult, BgTaskKind) + Send + Sync>>,
    ) -> Result<BgShellHandle, Box<dyn std::error::Error + Send + Sync>> {
        self.spawn_shell(command, cwd, timeout_ms, on_bg_complete)
    }

    fn finalize_bg_shell(
        &self,
        on_bg_complete: &Option<Arc<dyn Fn(&BackgroundTaskResult, BgTaskKind) + Send + Sync>>,
        task_id: String,
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
            task_id,
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
    pub fn mark_external_lost(&self, task_id: &str) -> bool {
        self.registry.mark_external_status(task_id, "lost")
    }

    pub fn mark_external_running(&self, task_id: &str) -> bool {
        self.registry.mark_external_status(task_id, "running")
    }
    pub fn snapshot(&self) -> TaskSnapshot {
        self.registry.snapshot()
    }

    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<TaskChange> {
        self.registry.subscribe_events()
    }

    pub fn register_external(
        &self,
        request: ExternalTaskRegistration,
    ) -> Result<String, BackgroundRegistryError> {
        let _registration = self.external_registration.lock();
        let task_id = external_task_id(&request);
        let started_at = external_started_at(&request)?;
        let cancel = Arc::clone(&request.cancel);
        let on_terminal = Arc::clone(&request.on_terminal);
        let task = BackgroundTask {
            id: task_id.clone(),
            agent_name: "mcp".into(),
            prompt_summary: request.summary,
            status: BackgroundTaskStatus::Running,
            started_at: std::time::Instant::now(),
            chrono_started_at: started_at,
            kind: request.kind,
            cancel_handle: BgCancelHandle::External {
                cancel: request.cancel,
                on_terminal: request.on_terminal,
            },
            cancel_token: None,
            pid: None,
            output_preview: None,
            agent_inbox: None,
        };
        match self.registry.register_with_kind(task) {
            Ok(()) => {}
            Err(BackgroundRegistryError::DuplicateTask(_)) => {
                self.registry
                    .refresh_external_callbacks(&task_id, cancel, on_terminal);
            }
            Err(error) => return Err(error),
        }
        Ok(task_id)
    }

    pub fn restore_external_terminal(
        &self,
        request: ExternalTaskRegistration,
        terminal_transition_id: &str,
        mut result: BackgroundTaskResult,
    ) -> Result<String, String> {
        if terminal_transition_id.is_empty() {
            return Err("terminal transition ID is required".into());
        }
        let _registration = self.external_registration.lock();
        let task_id = external_task_id(&request);
        let started_at = external_started_at(&request).map_err(|error| error.to_string())?;
        if let Some(status) = self.registry.projection_status(&task_id) {
            if status == "running" || status == "lost" {
                self.registry.refresh_external_callbacks(
                    &task_id,
                    Arc::clone(&request.cancel),
                    Arc::clone(&request.on_terminal),
                );
                self.settle_external(&task_id, terminal_transition_id, result)?;
            }
            return Ok(task_id);
        }
        result.task_id = task_id.clone();
        let delivery_id = external_delivery_id(&task_id, terminal_transition_id);
        (request.on_terminal)(&result, delivery_id)?;
        self.registry.restore_external_terminal(
            task_id.clone(),
            request.kind,
            request.summary,
            started_at,
            result,
        );
        Ok(task_id)
    }

    pub fn settle_external(
        &self,
        task_id: &str,
        terminal_transition_id: &str,
        mut result: BackgroundTaskResult,
    ) -> Result<bool, String> {
        if terminal_transition_id.is_empty() {
            return Err("terminal transition ID is required".into());
        }
        let Some(notify) = self.registry.external_notify(task_id) else {
            return Ok(false);
        };
        if !self.registry.claim_completion(task_id) {
            return Ok(false);
        }
        result.task_id = task_id.to_owned();
        let delivery_id = external_delivery_id(task_id, terminal_transition_id);
        if let Err(error) = notify(&result, delivery_id) {
            self.registry.reset_completion_claim(task_id);
            return Err(error);
        }
        Ok(self.registry.complete(task_id, result))
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
        }
    }

    pub fn with_shell_executor(executor: Arc<dyn ShellExecutor>) -> Self {
        Self {
            shell_executor: Some(executor),
            ..Self::new()
        }
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

    pub fn register_with_kind(&self, task: BackgroundTask) -> Result<(), BackgroundRegistryError> {
        self.registry.register_with_kind(task)
    }

    /// Send Info to a live child in this session. `None` means no registered
    /// receiver; an error must not fall through to resume or create an execution.
    pub fn send_subagent_message(
        &self,
        thread_id: &str,
        prompt: Option<&str>,
    ) -> Result<Option<QueuedSubagentMessage>, SubagentMessageError> {
        self.registry.send_subagent_message(thread_id, prompt)
    }

    pub fn complete(&self, task_id: &str, result: BackgroundTaskResult) -> bool {
        self.registry.complete(task_id, result)
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
        let ownership = self.registry.scope.begin_external()?;
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
