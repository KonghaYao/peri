use std::sync::Arc;

use peri_acp_types::tasks::{BgRegistryEvent, BgShellHandle, BgTaskKind, BgTaskRegistration};

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
/// `set_event_sender`/`clear_event_sender` 为过渡态事件桥接（供 ACP executor
/// 注入 `BgRegistryEvent` 泵），暂不依赖 M-event-chain。
pub struct TaskManager {
    registry: Arc<BackgroundTaskRegistry>,
    shell_executor: Option<Arc<dyn ShellExecutor>>,
}

impl Default for TaskManager {
    fn default() -> Self {
        Self::new()
    }
}

impl peri_acp_types::tasks::TaskManager for TaskManager {
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
        };
        let task = BackgroundTask {
            id: request.task_id,
            agent_name: match request.kind {
                BgTaskKind::Shell => "bg-shell",
                BgTaskKind::Agent => "agent",
                BgTaskKind::Workflow => "workflow",
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
    pub fn new() -> Self {
        Self {
            registry: Arc::new(BackgroundTaskRegistry::new()),
            shell_executor: None,
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
        let task_ids: Vec<String> = self
            .registry
            .list_tasks()
            .into_iter()
            .map(|(id, _, _)| id)
            .collect();
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
