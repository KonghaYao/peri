use std::process::Stdio;
use std::sync::Arc;

use futures::FutureExt;
use peri_acp_types::tasks::{BgRegistryEvent, BgShellHandle, BgTaskKind, BgTaskRegistration};

use crate::agent::events::BackgroundTaskResult;

use super::registry::{
    BackgroundRegistryError, BackgroundTask, BackgroundTaskRegistry, BackgroundTaskStatus,
    BgCancelHandle, BgTaskInfo,
};
use super::shell::{bg_shell_task_id, finalize_bg_shell, kill_process_group, shell_command};
use super::{QueuedSubagentMessage, ShellOutputCapture, SubagentMessageError};

// ── TaskManager（per-session 聚合）────────────────────────────────────────────

/// per-session 后台任务管理器（L1 迁移点：Agent 层 async tasks manager）。
///
/// 聚合 `BackgroundTaskRegistry` 与 bg shell 实际执行（进程 spawn/进程组/
/// 超时/输出收集）。随 session 创建/销毁；`cancel_all` 供 session 销毁时
/// 取消所有 owned 任务（§9 销毁顺序：取消 owned tasks）。
///
/// `set_event_sender`/`clear_event_sender` 为过渡态事件桥接（供 ACP executor
/// 注入 `BgRegistryEvent` 泵），暂不依赖 M-event-chain。
pub struct TaskManager {
    registry: Arc<BackgroundTaskRegistry>,
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
                    .map(BgCancelHandle::Pid)
                    .ok_or_else(|| "bg shell register: pid 缺失".to_string())?,
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
    /// 其余按 kind 分发（Abort 优雅退出 + 超时 abort 兜底 / Kill 闭包 / Pid 进程组）。
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

    /// 启动后台 shell 任务（run_in_background 路径）。
    ///
    /// 进程 spawn（经 [`shell_command`] 统一 wrapper）/ 进程组 / 超时 / 输出收集
    /// 全部在 Agent 层完成；任务启动即注册（BgTaskStarted 立即推送），完成时
    /// [`finalize_bg_shell`] 收尾（输出引用 → 完成认领 → 回调 → 终态提交）。
    ///
    /// `timeout_ms`：`None` = 不超时（后台语义：跑完为止）；`Some(ms)` 超时后
    /// kill 整个进程组并等待 child 与输出管道收尾。
    ///
    /// 返回 [`BgShellHandle`]（`task_id` 格式 `shell-{uuid v7}` + 进程 PID）；
    /// PID 供工具层回显，LLM 可经另一个 shell `kill` 进程组终止任务。
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::type_complexity)]
    pub fn spawn_shell(
        &self,
        command: String,
        cwd: String,
        timeout_ms: Option<u64>,
        on_bg_complete: Option<Arc<dyn Fn(&BackgroundTaskResult, BgTaskKind) + Send + Sync>>,
    ) -> Result<BgShellHandle, Box<dyn std::error::Error + Send + Sync>> {
        let ownership = self.registry.scope.begin_external()?;
        let mut execution = super::shell::ShellExecutionGuard::new(Some(ownership));
        let _admission = self.registry.scope.admit()?;
        let task_id = bg_shell_task_id();
        let registry = Arc::clone(&self.registry);
        let command_owned = command;
        let on_bg_complete_cb = on_bg_complete;
        let task_id_for_return = task_id.clone();

        // 同步 spawn：PID 必须在返回前确定，供工具层回显给 LLM 管理任务
        let mut cmd = shell_command(&command_owned, &[]);
        cmd.current_dir(&cwd)
            // stdin 重定向为 null：后台任务同样不依赖终端输入（与 Bash 工具
            // 同步路径一致），否则读 stdin 的进程会永远阻塞等待 EOF。
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);

        execution.prepare(&mut cmd)?;
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                // Register first, then use the same callback boundary as all
                // other shell completions. A callback panic must not skip the
                // registry terminal transition.
                let bg_task = BackgroundTask {
                    id: task_id.clone(),
                    agent_name: "bg-shell".to_string(),
                    prompt_summary: command_owned.chars().take(80).collect(),
                    status: BackgroundTaskStatus::Running,
                    started_at: std::time::Instant::now(),
                    chrono_started_at: chrono::Utc::now(),
                    kind: BgTaskKind::Shell,
                    cancel_handle: BgCancelHandle::Kill(None),
                    cancel_token: None,
                    pid: None,
                    output_preview: None,
                    agent_inbox: None,
                };
                if let Err(registration_error) = registry.register_admitted(bg_task) {
                    // No registered task can publish a completion. Return the
                    // failure now instead of promising an eventual notification.
                    return Err(format!("Failed to spawn: {e}; {registration_error}").into());
                }
                finalize_bg_shell(
                    &registry,
                    &on_bg_complete_cb,
                    task_id.clone(),
                    command_owned.chars().take(80).collect(),
                    false,
                    format!("Failed to spawn: {e}"),
                    0,
                    false,
                    None,
                );
                return Ok(BgShellHandle {
                    task_id: task_id_for_return,
                    pid: None,
                    stdout_log: None,
                    stderr_log: None,
                });
            }
        };
        let pid = child
            .id()
            .expect("bg shell: child.id() returned None after successful spawn");
        if let Err(error) = execution.attach(&child) {
            self.registry.scope.spawn_admitted(async move {
                let _ = child.kill().await;
                execution.confirm_stopped();
            });
            return Err(error.into());
        }

        // Create durable files before the readers start. The full stream is
        // therefore retained even when the in-memory preview reaches 2 MiB.
        let mut output_capture = ShellOutputCapture::new(&format!("bg-{task_id}"));
        let stdout_log_path = output_capture.stdout_path();
        let stderr_log_path = output_capture.stderr_path();

        // 任务启动即注册：推送 BgTaskStarted 事件，运行期间 TUI 展示栏可见。
        // 完成时 finalize_bg_shell 只调 complete()，不再重复注册。
        let bg_task = BackgroundTask {
            id: task_id.clone(),
            agent_name: "bg-shell".to_string(),
            prompt_summary: command_owned.chars().take(80).collect(),
            status: BackgroundTaskStatus::Running,
            started_at: std::time::Instant::now(),
            chrono_started_at: chrono::Utc::now(),
            kind: BgTaskKind::Shell,
            cancel_handle: execution
                .cancel_callback()
                .map_or(BgCancelHandle::Pid(pid), |kill| {
                    BgCancelHandle::Kill(Some(kill))
                }),
            cancel_token: None,
            pid: Some(pid),
            output_preview: None,
            agent_inbox: None,
        };
        if let Err(error) = registry.register_admitted(bg_task) {
            kill_process_group(pid, "KILL");
            self.registry.scope.spawn_admitted(async move {
                let _ = child.wait().await;
                execution.confirm_stopped();
                output_capture.cleanup().await;
            });
            return Err(error.into());
        }

        // The returned handle publishes live output paths. They outlive the
        // worker and remain readable even after cancellation/completion.
        output_capture.retain_files();
        let stdout_writer = output_capture.stdout_writer();
        let stderr_writer = output_capture.stderr_writer();
        let output_capture = Arc::new(output_capture);

        self.registry.scope.spawn_admitted(async move {
            // 外層 catch_unwind 保護：確保任何意外 panic 也會調用 registry.complete()，
            // 防止 bg shell 任務殘留在狀態欄。
            let started = std::time::Instant::now();
            let result = std::panic::AssertUnwindSafe(async {
                // 流式读取 stdout/stderr：tee 到日志文件（运行期 agent 可读）+ 内存缓冲
                // （wait_with_output 内部消费管道无法 tee，故显式 take pipe 自行读取）
                let stdout_reader = tokio::io::BufReader::new(
                    child.stdout.take().expect("bg shell: stdout is piped"),
                );
                let stderr_reader = tokio::io::BufReader::new(
                    child.stderr.take().expect("bg shell: stderr is piped"),
                );
                let stdout_buf = Arc::new(std::sync::Mutex::new(String::new()));
                let stderr_buf = Arc::new(std::sync::Mutex::new(String::new()));
                let drain_stdout = tokio::spawn(super::shell::tee_pipe_with_output(
                    stdout_reader,
                    stdout_buf.clone(),
                    stdout_writer,
                    Arc::clone(&output_capture),
                    "stdout",
                ));
                let drain_stderr = tokio::spawn(super::shell::tee_pipe_with_output(
                    stderr_reader,
                    stderr_buf.clone(),
                    stderr_writer,
                    Arc::clone(&output_capture),
                    "stderr",
                ));

                // 超时包裹 wait（后台未显式传 timeout 或 timeout=0 时不超时）
                let wait_result = match timeout_ms {
                    None => child.wait().await.map(Some),
                    Some(ms) => {
                        match tokio::time::timeout(
                            std::time::Duration::from_millis(ms),
                            child.wait(),
                        )
                        .await
                        {
                            Ok(status) => status.map(Some),
                            Err(_elapsed) => {
                                // 超时：kill 整个进程组（bash 为组长，负号 PID 语义），
                                // 等待实际终态后再发完成事件。
                                kill_process_group(pid, "KILL");
                                let exit_status = child.wait().await.ok();
                                if exit_status.is_none() {
                                    output_capture.mark_incomplete("process exit status unavailable");
                                }
                                if let Err(error) = drain_stdout.await {
                                    output_capture.record_task_error("stdout", error);
                                }
                                if let Err(error) = drain_stderr.await {
                                    output_capture.record_task_error("stderr", error);
                                }
                                execution.wait_for_exit().await;
                                execution.confirm_stopped();
                                registry.confirm_external_stopped(&task_id);
                                finalize_bg_shell(
                                    &registry,
                                    &on_bg_complete_cb,
                                    task_id.clone(),
                                    command_owned.chars().take(80).collect(),
                                    false,
                                    format!(
                                        "Command timed out after {}s; process group was terminated.",
                                        ms as f64 / 1000.0
                                    ),
                                    started.elapsed().as_millis() as u64,
                                    true,
                                    Some(output_capture.finish(
                                        exit_status.as_ref().and_then(std::process::ExitStatus::code),
                                    )),
                                );
                                return;
                            }
                        }
                    }
                };

                let output = match wait_result {
                    Ok(Some(status)) => {
                        if let Err(error) = drain_stdout.await {
                            output_capture.record_task_error("stdout", error);
                        }
                        if let Err(error) = drain_stderr.await {
                            output_capture.record_task_error("stderr", error);
                        }
                        let success = status.success();
                        let stdout = match stdout_buf.lock() {
                            Ok(g) => g.clone(),
                            Err(poisoned) => poisoned.into_inner().clone(),
                        };
                        let stderr = match stderr_buf.lock() {
                            Ok(g) => g.clone(),
                            Err(poisoned) => poisoned.into_inner().clone(),
                        };
                        let mut combined = String::new();
                        if !stdout.is_empty() {
                            combined.push_str(&stdout);
                        }
                        if !stderr.is_empty() {
                            if !combined.is_empty() {
                                combined.push('\n');
                            }
                            combined.push_str("[stderr]\n");
                            combined.push_str(&stderr);
                        }
                        if combined.is_empty() {
                            combined = format!("[exit code: {}]", status.code().unwrap_or(-1));
                        }
                        (success, combined, status.code())
                    }
                    Err(e) => {
                        tracing::error!(task_id = %task_id, error = %e, "background shell wait failed");
                        // A failed wait does not prove that the process or
                        // its pipe readers stopped. Settle both before
                        // publishing the failure and output references.
                        kill_process_group(pid, "KILL");
                        let _ = child.wait().await;
                        if let Err(error) = drain_stdout.await {
                            output_capture.record_task_error("stdout", error);
                        }
                        if let Err(error) = drain_stderr.await {
                            output_capture.record_task_error("stderr", error);
                        }
                        (false, format!("Command failed: {e}"), None)
                    }
                    // unreachable: child.wait() 恒返回 Ok(ExitStatus)
                    Ok(None) => {
                        unreachable!("bg shell: child.wait returned Ok(None)")
                    }
                };

                execution.wait_for_exit().await;
                execution.confirm_stopped();
                // Process cleanup is independent of the visible task. Cancel
                // may already have removed it before completion can claim it.
                registry.confirm_external_stopped(&task_id);
                // 回调通知 + 完成（任务在启动时已注册，与 promote 续跑共用收尾逻辑）
                finalize_bg_shell(
                    &registry,
                    &on_bg_complete_cb,
                    task_id.clone(),
                    command_owned.chars().take(80).collect(),
                    output.0,
                    output.1,
                    started.elapsed().as_millis() as u64,
                    false,
                    Some(output_capture.finish(output.2)),
                );
            })
            .catch_unwind()
            .await;
            if let Err(panic_err) = result {
                kill_process_group(pid, "KILL");
                let _ = child.wait().await;
                execution.wait_for_exit().await;
                execution.confirm_stopped();
                registry.confirm_external_stopped(&task_id);
                // spawn 閉包內部 panic：嘗試用現有 task_id 發送失敗事件
                let panic_msg = if let Some(s) = panic_err.downcast_ref::<String>() {
                    s.clone()
                } else if let Some(s) = panic_err.downcast_ref::<&str>() {
                    s.to_string()
                } else {
                    "unknown panic".to_string()
                };
                output_capture.mark_incomplete("background shell worker panicked");
                // The task was registered before the worker was spawned. Do
                // not re-register here: cancellation may already have
                // removed the entry, and `complete` will then suppress the
                // duplicate terminal event as intended.
                finalize_bg_shell(
                    &registry,
                    &on_bg_complete_cb,
                    task_id.clone(),
                    command_owned.chars().take(80).collect(),
                    false,
                    format!("Background shell task panicked: {panic_msg}"),
                    started.elapsed().as_millis() as u64,
                    false,
                    Some(output_capture.finish(None)),
                );
            }
        });

        Ok(BgShellHandle {
            task_id: task_id_for_return,
            pid: Some(pid),
            stdout_log: stdout_log_path,
            stderr_log: stderr_log_path,
        })
    }
}
