//! Bash 进程执行器：**唯一** spawn `bash -c` 并持有 `process::Child` 的地方。
//!
//! 单进程形态（D-003）下不存在第二个任务表：本模块只负责"把一次启动变成受监督的
//! 进程生命周期"（spawn、进程组、日志 tee、TERM→KILL 升级、状态机），任务归属、
//! 容量、TTL 与事件由 [`crate::tasks::registry::TaskRegistry`] 的唯一任务表持有。
//! 二者是同一进程内的普通 Rust 调用：没有信封、没有帧、没有请求配对、没有超时兜底。
//!
//! 与源实现（`peri-middlewares/src/middleware/terminal.rs` +
//! `peri-agent/src/agent/async_tasks/shell.rs`）的对应关系：
//!
//! | 源行为 | 本模块 |
//! | --- | --- |
//! | `shell_command` → `bash -c`，`stdin(null)`，`process_group(0)` | [`BashTasks::start`] |
//! | `drain_pipe`/`tee_pipe`（2 MiB 上限、继续排空） | `spawn_drain` |
//! | `bg_shell_task_id`（`shell-<UUIDv7>`） | 注册表铸造（本模块只接受） |
//! | `kill_process_group` / `kill_process_group_escalating`（TERM→2s→KILL） | [`BashTask::terminate_group`] |
//! | 前台超时 promote 为后台 | `mode=Foreground` 的 await 到期分支 |
//! | 后台 timeout 到期终止 | `mode=Background` 的定时终止分支 |
//! | 终态输出 = merge + truncate（落盘全量） | 监督任务收尾时调用
//!   [`crate::tools::bash::limits`] 的纯函数 |
//!
//! 两处**记录在案**的实现差异（同语义、不同手段）：
//!
//! 1. 信号使用 `libc::kill` 而非 spawn `kill` 二进制：每个信号不必额外 fork 一个进程，
//!    进程组语义（`-- -pgid`）完全一致。
//! 2. 前台同步路径也写日志文件（源只在后台/提升路径 tee）：前台超时提升后，
//!    日志已包含此前输出，Read 立即可读。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::io::AsyncReadExt;
use tokio::process::Child;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::capability::RootDir;
use crate::error::WorkerError;
use crate::tasks::log::{DirOutputPersist, LogStore, LogStream, OutputPersist};
use crate::tasks::params::{
    BashMode, BashStartPayload, BashTaskState, TaskLogPayload, MAX_LOG_READ_BYTES,
};
use crate::tools::bash::limits::{merge_output, truncate_bytes, truncate_output};
use crate::wire::TaskStatus;

/// 前台同步路径的部分输出捕获上限（源：`MAX_PARTIAL_CAPTURE_BYTES`，2 MiB）。
pub const MAX_PARTIAL_CAPTURE_BYTES: usize = 2 * 1024 * 1024;

/// TERM 到 KILL 的升级窗口（源：`kill_process_group_escalating` 的 2s）。
pub const KILL_ESCALATION: Duration = Duration::from_secs(2);

/// 关闭时等待进程组退出的有界时间。
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Bash 任务执行配置。
#[derive(Clone)]
pub struct BashTaskConfig {
    /// 默认工作目录（**宿主工作区根**；`Bash` 的 cwd）。
    pub workspace: PathBuf,
    /// 日志目录（工作区根内的私有目录；`Read` 能读到它）。
    pub log_dir: PathBuf,
    /// 截断输出落盘 sink。
    pub persist: Arc<dyn OutputPersist>,
    /// 日志存储（capability 模式下每个文件操作都经授权根解析）。
    pub logs: LogStore,
}

impl BashTaskConfig {
    /// 使用给定工作区与日志目录（**不经过** capability 校验；测试夹具用）。
    pub fn new(workspace: impl Into<PathBuf>, log_dir: impl Into<PathBuf>) -> Self {
        let log_dir = log_dir.into();
        Self {
            workspace: workspace.into(),
            persist: Arc::new(DirOutputPersist::new(log_dir.clone())),
            logs: LogStore::new(log_dir.clone()),
            log_dir,
        }
    }

    /// 让日志与截断落盘走 capability 校验（**生产唯一路径**）。
    ///
    /// `log_subdir` 是相对授权根的日志目录（生产为 `.local-mcp/logs`）。构造后：
    /// 日志目录被换成符号链接、或目标越出授权根，都会被 [`crate::capability`] 拒绝，
    /// 且不写任何文件（见 F2）。
    pub fn with_capability(mut self, root: Arc<RootDir>, log_subdir: impl Into<String>) -> Self {
        let log_subdir = log_subdir.into();
        let logs = LogStore::rooted(Arc::clone(&root), log_subdir.clone());
        self.log_dir = logs.dir().to_path_buf();
        self.persist = Arc::new(DirOutputPersist::rooted(root, log_subdir));
        self.logs = logs;
        self
    }
}

/// 任务层操作失败。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BashTaskError {
    /// 日志句柄与任务不匹配（猜测/重放）。
    #[error("log handle mismatch for task: {task_id}")]
    HandleMismatch {
        /// 任务 id。
        task_id: String,
    },
    /// spawn 失败（源文案 `Error executing command: {e}` 由工具层拼装）。
    #[error("{message}")]
    Spawn {
        /// 失败说明。
        message: String,
    },
    /// 日志读取失败。
    #[error("log read failed: {message}")]
    Log {
        /// 失败说明。
        message: String,
    },
}

impl From<BashTaskError> for WorkerError {
    fn from(error: BashTaskError) -> Self {
        WorkerError::Protocol {
            reason: error.to_string(),
        }
    }
}

/// 任务终态（运行中由 `terminal == None` 表示，不需要单独变体）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lifecycle {
    Completed,
    Failed,
    Killed,
    TimedOut,
}

impl Lifecycle {
    fn to_status(self) -> TaskStatus {
        match self {
            Self::Completed => TaskStatus::Completed,
            Self::Failed => TaskStatus::Failed,
            Self::Killed => TaskStatus::Killed,
            Self::TimedOut => TaskStatus::TimedOut,
        }
    }
}

/// 单个任务的终态信息。
struct TerminalInfo {
    lifecycle: Lifecycle,
    exit_code: Option<i32>,
    ended_at: String,
    final_output: String,
    truncated: bool,
    persisted_path: Option<String>,
}

/// 单个 Bash 任务（进程 + 状态机 + 日志句柄）。
///
/// 生命周期只有一处权威：任务自身；注册表持有 `Arc<BashTask>` 并读取它，不复制状态。
///
/// **内嵌 API 边界**：本类型与它的方法只供同进程内的唯一任务表
/// （[`crate::tasks::registry::TaskRegistry`]）、嵌入方与集成测试使用，MCP wire 上
/// 不可达；模型侧的可见面只有 `Bash` 工具的结果与 `sandbox://tasks` 资源。
pub struct BashTask {
    task_id: String,
    log_handle: String,
    pid: u32,
    pgid: u32,
    /// 日志路径；capability 拒绝该路径（例如目录被换成符号链接）时为 `None`——
    /// 此时不写任何越界文件，返回文本会显示 `<unavailable>`（见 F2）。
    stdout_log: Option<PathBuf>,
    stderr_log: Option<PathBuf>,
    started_at: std::time::Instant,
    started_rfc3339: String,
    capture_limit: usize,
    persist: Arc<dyn OutputPersist>,
    stdout: Mutex<String>,
    stderr: Mutex<String>,
    terminal: Mutex<Option<TerminalInfo>>,
    terminal_at: Mutex<Option<std::time::Instant>>,
    promoted: Mutex<bool>,
    timed_out: Mutex<bool>,
    cancelled: Mutex<bool>,
    process_state: Mutex<Option<String>>,
    notify: Notify,
}

impl BashTask {
    /// 任务 id。
    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    /// 日志句柄（调用方按 id 操作任务时必须回带它）。
    pub fn log_handle(&self) -> &str {
        &self.log_handle
    }

    /// 是否已进入终态。
    pub fn is_terminal(&self) -> bool {
        self.terminal.lock().is_some()
    }

    fn capture(&self, stream: LogStream, chunk: &[u8]) {
        let target = match stream {
            LogStream::Stdout => &self.stdout,
            LogStream::Stderr => &self.stderr,
        };
        let mut guard = target.lock();
        if guard.len() < self.capture_limit {
            let text = String::from_utf8_lossy(chunk);
            let remaining = self.capture_limit - guard.len();
            guard.push_str(&truncate_bytes(&text, remaining));
        }
    }

    /// 构造状态快照（`final_output` 只在终态出现）。
    pub fn snapshot(&self) -> BashTaskState {
        let terminal = self.terminal.lock();
        let stdout = self.stdout.lock().clone();
        let stderr = self.stderr.lock().clone();
        let (status, exit_code, ended_at, final_output, truncated, persisted_path) =
            match terminal.as_ref() {
                Some(info) => (
                    info.lifecycle.to_status(),
                    info.exit_code,
                    Some(info.ended_at.clone()),
                    Some(info.final_output.clone()),
                    info.truncated,
                    info.persisted_path.clone(),
                ),
                None => (TaskStatus::Running, None, None, None, false, None),
            };
        BashTaskState {
            task_id: self.task_id.clone(),
            status,
            pid: Some(self.pid),
            pgid: Some(self.pgid),
            stdout_log: self
                .stdout_log
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()),
            stderr_log: self
                .stderr_log
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()),
            exit_code,
            stdout,
            stderr,
            promoted: *self.promoted.lock(),
            timed_out: *self.timed_out.lock() || status == TaskStatus::TimedOut,
            cancelled: *self.cancelled.lock(),
            elapsed_ms: self.started_at.elapsed().as_millis() as u64,
            started_at: self.started_rfc3339.clone(),
            ended_at,
            final_output,
            truncated,
            persisted_path,
            process_state: self.process_state.lock().clone(),
        }
    }

    /// 标记前台提升（超时后进程继续存活）。
    pub(crate) fn mark_promoted(&self) {
        *self.promoted.lock() = true;
        *self.process_state.lock() = process_status_snapshot(self.pid);
    }

    /// 等待终态或到期（`deadline = None` 表示无限等待）。
    pub async fn wait_terminal(&self, deadline: Option<tokio::time::Instant>) {
        loop {
            if self.is_terminal() {
                return;
            }
            let notified = self.notify.notified();
            if self.is_terminal() {
                return;
            }
            match deadline {
                None => notified.await,
                Some(deadline) => {
                    if tokio::time::timeout_at(deadline, notified).await.is_err() {
                        return;
                    }
                }
            }
        }
    }

    /// TERM → 2s → KILL 的进程组终止序列（幂等；终态直接返回）。
    ///
    /// 等待有界：KILL 之后仍给监督任务一次机会观察退出；若进程组仍未被回收
    /// （例如不可中断睡眠），如实标记 `Killed`，不假装进程已消失。
    pub async fn terminate_group(&self, timed_out: bool) {
        if self.is_terminal() {
            return;
        }
        if timed_out {
            *self.timed_out.lock() = true;
        }
        send_signal_quietly(self.pgid, libc::SIGTERM);
        let term_deadline = tokio::time::Instant::now() + KILL_ESCALATION;
        self.wait_terminal(Some(term_deadline)).await;
        if !self.is_terminal() {
            send_signal_quietly(self.pgid, libc::SIGKILL);
            let kill_deadline = tokio::time::Instant::now() + KILL_ESCALATION;
            self.wait_terminal(Some(kill_deadline)).await;
        }
        if !self.is_terminal() {
            self.mark_terminal(Lifecycle::Killed, None);
        }
    }

    fn mark_terminal(&self, lifecycle: Lifecycle, exit_code: Option<i32>) {
        let stdout = self.stdout.lock().clone();
        let stderr = self.stderr.lock().clone();
        let merged = merge_output(&stdout, &stderr, exit_code);
        let shaped = truncate_output(&merged, self.persist.as_ref());
        let mut guard = self.terminal.lock();
        if guard.is_none() {
            *self.terminal_at.lock() = Some(std::time::Instant::now());
            *guard = Some(TerminalInfo {
                lifecycle,
                exit_code,
                ended_at: chrono::Utc::now().to_rfc3339(),
                final_output: shaped.text,
                truncated: shaped.truncated,
                persisted_path: shaped.persisted_path,
            });
        }
        drop(guard);
        self.notify.notify_waiters();
    }
}

/// Bash 进程执行器：spawn 工厂 + 日志存储（不持有任务表）。
pub struct BashTasks {
    config: BashTaskConfig,
    logs: LogStore,
}

impl BashTasks {
    /// 新建执行器。
    pub fn new(config: BashTaskConfig) -> Self {
        let logs = config.logs.clone();
        Self { config, logs }
    }

    /// 默认工作目录（宿主工作区根）；`Bash` 的 cwd 与终端指引都以它为准。
    pub fn workspace(&self) -> &Path {
        &self.config.workspace
    }

    /// 日志存储句柄（供注册表回收与诊断）。
    pub fn logs(&self) -> &LogStore {
        &self.logs
    }

    /// 启动任务并返回受监督的进程句柄。
    ///
    /// - `mode = Foreground`：等待到终态或 `await_ms` 到期；到期**不杀进程**，
    ///   只置 `promoted` 并返回（注册表决定是否保留该任务）。
    /// - `mode = Background`：立即返回；`timeout_ms` 到期时终止进程组。
    ///
    /// 返回的 `Arc<BashTask>` 是进程生命周期的唯一句柄：注册表把它登记进自己的表，
    /// 之后的状态查询、日志读取与终止都直接作用于同一对象。
    pub async fn start(
        &self,
        payload: BashStartPayload,
        cancel: Option<CancellationToken>,
    ) -> Result<Arc<BashTask>, BashTaskError> {
        let task = self.spawn(&payload)?;
        match payload.mode {
            BashMode::Background => {
                if let Some(ms) = payload.timeout_ms {
                    spawn_deadline_kill(Arc::clone(&task), Duration::from_millis(ms));
                }
                Ok(task)
            }
            BashMode::Foreground => {
                let deadline = payload
                    .await_ms
                    .map(|ms| tokio::time::Instant::now() + Duration::from_millis(ms));
                wait_with_cancel(&task, deadline, cancel).await;
                if !task.is_terminal() {
                    // 前台超时：进程继续存活，等待注册表决定保留或终止。
                    task.mark_promoted();
                }
                Ok(task)
            }
        }
    }

    /// 读取任务日志尾部（必须同时给出 task id 与句柄）。
    pub fn read_log(
        &self,
        task: &Arc<BashTask>,
        log_handle: &str,
        stream: LogStream,
        max_bytes: usize,
    ) -> Result<TaskLogPayload, BashTaskError> {
        if task.log_handle != log_handle {
            return Err(BashTaskError::HandleMismatch {
                task_id: task.task_id.clone(),
            });
        }
        let max_bytes = max_bytes.min(MAX_LOG_READ_BYTES);
        self.logs
            .read_tail(&task.log_handle, stream, max_bytes)
            .map(|chunk| TaskLogPayload {
                content: chunk.content,
                total_bytes: chunk.total_bytes,
                truncated: chunk.truncated,
            })
            .map_err(|error| BashTaskError::Log {
                message: error.to_string(),
            })
    }

    /// 回收终态任务的日志文件（注册表 TTL 到期时调用）；运行中任务拒绝回收。
    pub fn forget(&self, task: &Arc<BashTask>) -> bool {
        if !task.is_terminal() {
            return false;
        }
        if let Err(error) = self.logs.remove(&task.log_handle) {
            tracing::warn!(
                task_id = %task.task_id,
                error = %error,
                "回收任务日志失败"
            );
        }
        true
    }

    /// 关闭一批任务：TERM 全部 → 2s → KILL 全部 → 有界等待（进程退出前的收尾）。
    ///
    /// 只作用于传入的句柄（调用方 = 唯一任务表的所有者），因此不会有"进程还在跑但
    /// 没人知道"的任务。
    pub async fn shutdown(&self, running: &[Arc<BashTask>]) {
        let running: Vec<Arc<BashTask>> = running
            .iter()
            .filter(|task| !task.is_terminal())
            .cloned()
            .collect();
        if running.is_empty() {
            return;
        }
        for task in &running {
            send_signal_quietly(task.pgid, libc::SIGTERM);
        }
        let term_deadline = tokio::time::Instant::now() + KILL_ESCALATION;
        for task in &running {
            task.wait_terminal(Some(term_deadline)).await;
        }
        for task in &running {
            if !task.is_terminal() {
                send_signal_quietly(task.pgid, libc::SIGKILL);
            }
        }
        let kill_deadline = tokio::time::Instant::now() + SHUTDOWN_GRACE;
        for task in &running {
            task.wait_terminal(Some(kill_deadline)).await;
        }
    }

    /// spawn `bash -c <command>` 并启动排空/监督任务。
    fn spawn(&self, payload: &BashStartPayload) -> Result<Arc<BashTask>, BashTaskError> {
        let cwd = if payload.cwd.is_empty() {
            self.config.workspace.clone()
        } else {
            PathBuf::from(&payload.cwd)
        };
        // 日志文件由 capability 层解析并创建（F2）：目录被换成符号链接、越出授权根
        // 都在这里失败。句柄非法是调用方错误（注册表铸的句柄不合法）→ 直接拒绝；
        // 路径被 capability 拒绝 → 降级为「本次任务无日志」，但绝不写到别处。
        let stdout_log = self.open_log(&payload.log_handle, LogStream::Stdout)?;
        let stderr_log = self.open_log(&payload.log_handle, LogStream::Stderr)?;

        let mut command = tokio::process::Command::new("bash");
        command
            .arg("-c")
            .arg(&payload.command)
            .current_dir(&cwd)
            // stdin 为 null：非交互执行，读 stdin 的进程立即 EOF 快速失败。
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        // 不设 kill_on_drop：前台超时提升后进程必须继续存活。
        #[cfg(unix)]
        command.process_group(0);

        let mut child = command.spawn().map_err(|error| BashTaskError::Spawn {
            message: error.to_string(),
        })?;
        let pid = child.id().ok_or_else(|| BashTaskError::Spawn {
            message: "spawned process has no pid".to_string(),
        })?;

        let task = Arc::new(BashTask {
            task_id: payload.task_id.clone(),
            log_handle: payload.log_handle.clone(),
            pid,
            pgid: pid,
            stdout_log: stdout_log.as_ref().map(|(path, _)| path.clone()),
            stderr_log: stderr_log.as_ref().map(|(path, _)| path.clone()),
            started_at: std::time::Instant::now(),
            started_rfc3339: chrono::Utc::now().to_rfc3339(),
            capture_limit: MAX_PARTIAL_CAPTURE_BYTES,
            persist: Arc::clone(&self.config.persist),
            stdout: Mutex::new(String::new()),
            stderr: Mutex::new(String::new()),
            terminal: Mutex::new(None),
            terminal_at: Mutex::new(None),
            promoted: Mutex::new(false),
            timed_out: Mutex::new(false),
            cancelled: Mutex::new(false),
            process_state: Mutex::new(None),
            notify: Notify::new(),
        });

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let mut drains = Vec::new();
        if let Some(reader) = stdout {
            drains.push(spawn_drain(
                Arc::clone(&task),
                LogStream::Stdout,
                reader,
                stdout_log.map(|(_, file)| file),
            ));
        }
        if let Some(reader) = stderr {
            drains.push(spawn_drain(
                Arc::clone(&task),
                LogStream::Stderr,
                reader,
                stderr_log.map(|(_, file)| file),
            ));
        }
        spawn_supervisor(task.clone(), child, drains);
        Ok(task)
    }

    /// 打开（必要时创建）一个日志流，返回（展示路径，追加写句柄）。
    ///
    /// 非法句柄是调用方错误；capability 拒绝走降级路径（`Ok(None)`）并留下明确日志——
    /// 降级的含义是"本次任务没有日志文件"，而不是"写到别处"。
    fn open_log(
        &self,
        handle: &str,
        stream: LogStream,
    ) -> Result<Option<(PathBuf, std::fs::File)>, BashTaskError> {
        match self.logs.create(handle, stream) {
            Ok(opened) => Ok(Some(opened)),
            Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {
                Err(BashTaskError::Log {
                    message: error.to_string(),
                })
            }
            Err(error) => {
                tracing::warn!(
                    stream = %stream.as_str(),
                    error = %error,
                    "任务日志路径未通过 capability 校验；本次任务不写日志（不降级到越界路径）"
                );
                Ok(None)
            }
        }
    }
}

/// 排空一个流：全量 tee 到日志文件，同时按上限进入捕获缓冲（超出继续排空）。
///
/// 日志句柄由 [`BashTasks::open_log`] 在 spawn 前经 capability 创建并持有：写入只
/// 经过这个已打开的 fd，后续任何路径替换都无法把它重定向到别处（见 F2）。
fn spawn_drain<R>(
    task: Arc<BashTask>,
    stream: LogStream,
    mut reader: R,
    log_file: Option<std::fs::File>,
) -> tokio::task::JoinHandle<()>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        // 日志写入失败只降级（不影响执行链，源 `tee_pipe` 同语义）。
        let mut log = log_file.map(tokio::fs::File::from_std);
        let mut chunk = [0u8; 8192];
        loop {
            match reader.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Some(file) = log.as_mut() {
                        use tokio::io::AsyncWriteExt;
                        let _ = file.write_all(&chunk[..n]).await;
                    }
                    task.capture(stream, &chunk[..n]);
                }
            }
        }
        if let Some(file) = log.as_mut() {
            use tokio::io::AsyncWriteExt;
            let _ = file.flush().await;
        }
    })
}

/// 监督任务：等待子进程退出、排空管道，然后落终态（退出码/时间/合并截断输出）。
///
/// 排空在 `child.wait()` 之后**有界**等待（[`KILL_ESCALATION`]）：源实现是无限等待，
/// 若某个子孙进程握着管道不放，无界等待会让任务永远到不了终态；有界等待保证
/// 状态诚实（输出可能少掉最后一段，但任务不会卡在 Running）。
fn spawn_supervisor(
    task: Arc<BashTask>,
    mut child: Child,
    drains: Vec<tokio::task::JoinHandle<()>>,
) {
    tokio::spawn(async move {
        let wait_result = child.wait().await;
        if !drains.is_empty() {
            let drain_deadline = tokio::time::Instant::now() + KILL_ESCALATION;
            let drain_all = async {
                for handle in drains {
                    let _ = handle.await;
                }
            };
            let _ = tokio::time::timeout_at(drain_deadline, drain_all).await;
        }
        let (lifecycle, exit_code) = match wait_result {
            Ok(status) => {
                let code = status.code();
                let lifecycle = if *task.timed_out.lock() {
                    Lifecycle::TimedOut
                } else if *task.cancelled.lock() {
                    Lifecycle::Killed
                } else if status.success() {
                    Lifecycle::Completed
                } else {
                    Lifecycle::Failed
                };
                (lifecycle, code)
            }
            Err(error) => {
                tracing::warn!(task_id = %task.task_id, error = %error, "等待子进程失败");
                (Lifecycle::Failed, None)
            }
        };
        task.mark_terminal(lifecycle, exit_code);
    });
}

/// `mode = Background` 的到期终止（正向 timeout 请求终止，源描述一致）。
fn spawn_deadline_kill(task: Arc<BashTask>, after: Duration) {
    tokio::spawn(async move {
        tokio::time::sleep(after).await;
        if task.is_terminal() {
            return;
        }
        *task.timed_out.lock() = true;
        task.terminate_group(false).await;
    });
}

/// 前台等待：终态、`deadline` 到期或请求取消（取消会终止进程组）。
async fn wait_with_cancel(
    task: &Arc<BashTask>,
    deadline: Option<tokio::time::Instant>,
    cancel: Option<CancellationToken>,
) {
    match cancel {
        None => task.wait_terminal(deadline).await,
        Some(token) => {
            let notified = token.cancelled();
            tokio::pin!(notified);
            let wait = task.wait_terminal(deadline);
            tokio::pin!(wait);
            tokio::select! {
                () = &mut wait => {}
                () = &mut notified => {
                    if !task.is_terminal() {
                        *task.cancelled.lock() = true;
                        task.terminate_group(false).await;
                    }
                }
            }
        }
    }
}

/// 向进程组发送信号（`-- -pgid` 语义；pid=0 防御性跳过）。
#[cfg(unix)]
fn send_signal_quietly(pgid: u32, signal: libc::c_int) {
    if pgid == 0 {
        return;
    }
    // SAFETY: `libc::kill` 只读取参数；负 pid 表示进程组，pgid 来自本进程 spawn
    // 的组长 pid，且 0 已被上面拒绝（避免波及自身进程组）。失败（组已退出）按
    // 预期忽略，与源实现"静默"语义一致。
    unsafe {
        libc::kill(-(pgid as i32), signal);
    }
}

/// 非 Unix：没有进程组语义（DESIGN-001 §5.4 明确 Windows 不受支持）。
#[cfg(not(unix))]
fn send_signal_quietly(_pgid: u32, _signal: i32) {}

/// 采集进程状态快照（源：`terminal.rs::process_status_snapshot`）。
///
/// 尽力而为：非 Unix 或 `ps` 不可用时返回 `None`，调用方在文案中省略该行。
/// 只读、无副作用，且与目标进程同一 PID 命名空间（单进程本机形态）。
fn process_status_snapshot(pid: u32) -> Option<String> {
    #[cfg(unix)]
    {
        let output = std::process::Command::new("ps")
            .args(["-o", "pid=,stat=,etime=,command=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let snapshot = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if snapshot.is_empty() {
            None
        } else {
            Some(snapshot)
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        None
    }
}
