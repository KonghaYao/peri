use std::process::Stdio;
use std::sync::Arc;

use futures::FutureExt;
use peri_acp_types::tasks::{BgRegistryEvent, BgShellHandle, BgTaskKind, BgTaskRegistration};
use tracing::warn;

use crate::agent::events::BackgroundTaskResult;

use super::registry::{
    BackgroundRegistryError, BackgroundTask, BackgroundTaskRegistry, BackgroundTaskStatus,
    BgCancelHandle, BgTaskInfo,
};
use super::shell::{
    bg_shell_task_id, finalize_bg_shell, kill_process_group_escalating, shell_command, tee_pipe,
};

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
            BgTaskKind::Shell => request
                .pid
                .map(BgCancelHandle::Pid)
                .ok_or_else(|| "bg shell register: pid 缺失".to_string())?,
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
    /// [`finalize_bg_shell`] 收尾（超长输出落盘 → on_bg_complete 回调 → complete）。
    ///
    /// `timeout_ms`：`None` = 不超时（后台语义：跑完为止）；`Some(ms)` 超时后
    /// kill 整个进程组（TERM → 2s 后 KILL 升级）。
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

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                // spawn 失败：注册 + 立即按失败收尾（agent 仍收到失败通知，语义不变）
                let result = BackgroundTaskResult {
                    task_id: task_id.clone(),
                    agent_name: "bg-shell".to_string(),
                    prompt_summary: command_owned.chars().take(80).collect(),
                    success: false,
                    output: format!("Failed to spawn: {}", e),
                    tool_calls_count: 0,
                    duration_ms: 0,
                    child_thread_id: None,
                    timed_out: false,
                };
                // 回调通知 Agent inbox（在 registry 操作之前）
                if let Some(ref cb) = on_bg_complete_cb {
                    cb(&result, BgTaskKind::Shell);
                }
                // 注册 + 立即完成
                let bg_task = BackgroundTask {
                    id: result.task_id.clone(),
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
                };
                let _ = registry.register_with_kind(bg_task);
                let complete_task_id = result.task_id.clone();
                registry.complete(&complete_task_id, result);
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

        // 创建实时输出日志文件（尽力而为：创建失败仅降级为不落盘，不影响执行链）。
        // 运行期间 agent 可经 Read 工具读取；完成后文件保留。
        let stdout_log = std::env::temp_dir().join(format!("peri-bg-{task_id}.stdout.log"));
        let stderr_log = std::env::temp_dir().join(format!("peri-bg-{task_id}.stderr.log"));
        let stdout_log_file = std::fs::File::create(&stdout_log).ok();
        let stderr_log_file = std::fs::File::create(&stderr_log).ok();
        let stdout_log_path = stdout_log_file
            .as_ref()
            .map(|_| stdout_log.to_string_lossy().into_owned());
        let stderr_log_path = stderr_log_file
            .as_ref()
            .map(|_| stderr_log.to_string_lossy().into_owned());

        tokio::spawn(async move {
            // 外層 catch_unwind 保護：確保任何意外 panic 也會調用 registry.complete()，
            // 防止 bg shell 任務殘留在狀態欄。
            let started = std::time::Instant::now();
            let result = std::panic::AssertUnwindSafe(async {

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
                    cancel_handle: BgCancelHandle::Pid(pid),
                    cancel_token: None,
                    pid: Some(pid),
                    output_preview: None,
                };
                if let Err(e) = registry.register_with_kind(bg_task) {
                    // 并发上限已满：杀掉进程组，按失败收尾（防孤儿进程 + 推送 Completed）
                    warn!(error = %e, task_id = %task_id, "bg shell: register_with_kind failed at start, killing process group");
                    kill_process_group_escalating(pid);
                    let result = BackgroundTaskResult {
                        task_id: task_id.clone(),
                        agent_name: "bg-shell".to_string(),
                        prompt_summary: command_owned.chars().take(80).collect(),
                        success: false,
                        output: format!("Failed to register background task: {}", e),
                        tool_calls_count: 0,
                        duration_ms: started.elapsed().as_millis() as u64,
                        child_thread_id: None,
                        timed_out: false,
                    };
                    if let Some(ref cb) = on_bg_complete_cb {
                        cb(&result, BgTaskKind::Shell);
                    }
                    registry.complete(&result.task_id.clone(), result);
                    return;
                }

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
                let drain_stdout =
                    tokio::spawn(tee_pipe(stdout_reader, stdout_buf.clone(), stdout_log_file));
                let drain_stderr =
                    tokio::spawn(tee_pipe(stderr_reader, stderr_buf.clone(), stderr_log_file));

                // 超时包裹 wait（后台未显式传 timeout 或 timeout=0 时不超时）
                let wait_result = match timeout_ms {
                    None => child.wait().await.map(Some),
                    Some(ms) => {
                        match tokio::time::timeout(std::time::Duration::from_millis(ms), child.wait())
                            .await
                        {
                            Ok(status) => status.map(Some),
                            Err(_elapsed) => {
                                // 超时：kill 整个进程组（bash 为组长，负号 PID 语义），
                                // 2s 后若 TERM 无效再升级 KILL（fire-and-forget）
                                kill_process_group_escalating(pid);
                                // 构造超时错误结果
                                let result = BackgroundTaskResult {
                                    task_id: task_id.clone(),
                                    agent_name: "bg-shell".to_string(),
                                    prompt_summary: command_owned
                                        .chars()
                                        .take(80)
                                        .collect(),
                                    success: false,
                                    output: format!(
                                        "Command timed out after {}s.\nCommand: {}",
                                        ms as f64 / 1000.0,
                                        command_owned
                                    ),
                                    tool_calls_count: 0,
                                    duration_ms: started.elapsed().as_millis() as u64,
                                    child_thread_id: None,
                                    timed_out: true,
                                };
                                // 回调通知 Agent inbox（在 registry 操作之前）
                                if let Some(ref cb) = on_bg_complete_cb {
                                    cb(&result, BgTaskKind::Shell);
                                }
                                // 任务在启动时已注册，此处只收尾推送 Completed
                                let complete_task_id = result.task_id.clone();
                                registry.complete(&complete_task_id, result);
                                return;
                            }
                        }
                    }
                };

                let output = match wait_result {
                    Ok(Some(status)) => {
                        let _ = drain_stdout.await;
                        let _ = drain_stderr.await;
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
                            combined =
                                format!("[exit code: {}]", status.code().unwrap_or(-1));
                        }
                        (success, combined)
                    }
                    Err(e) => (false, format!("Command failed: {}", e)),
                    // unreachable: child.wait() 恒返回 Ok(ExitStatus)
                    Ok(None) => {
                        unreachable!("bg shell: child.wait returned Ok(None)")
                    }
                };

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
                );
            })
            .catch_unwind()
            .await;
            if let Err(panic_err) = result {
                // spawn 閉包內部 panic：嘗試用現有 task_id 發送失敗事件
                let panic_msg = if let Some(s) = panic_err.downcast_ref::<String>() {
                    s.clone()
                } else if let Some(s) = panic_err.downcast_ref::<&str>() {
                    s.to_string()
                } else {
                    "unknown panic".to_string()
                };
                let fallback = BackgroundTaskResult {
                    task_id: task_id.clone(),
                    agent_name: "bg-shell".to_string(),
                    prompt_summary: command_owned.chars().take(80).collect(),
                    success: false,
                    output: format!("Background shell task panicked: {}", panic_msg),
                    tool_calls_count: 0,
                    duration_ms: started.elapsed().as_millis() as u64,
                    child_thread_id: None,
                    timed_out: false,
                };
                // 嘗試註冊 + 完成（即使 register 失敗也調 complete，發送 cleanup 事件到 TUI）
                let bg_task = BackgroundTask {
                    id: fallback.task_id.clone(),
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
                };
                let _ = registry.register_with_kind(bg_task);
                let complete_task_id = fallback.task_id.clone();
                registry.complete(&complete_task_id, fallback);
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
