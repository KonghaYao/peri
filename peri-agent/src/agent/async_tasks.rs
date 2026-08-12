//! async tasks manager（Agent 层，per-session 实例化）。
//!
//! 3.0 归位（L1）：`BackgroundTaskRegistry` 定义与 bg shell 实际执行
//! （进程 spawn/进程组/超时/输出收集）自 `peri-middlewares` 迁入本模块。
//! `TaskManager` 是 per-session 聚合：registry + shell 执行 + 事件桥接
//! （`set_event_sender`/`clear_event_sender` 保留为过渡态，供 ACP executor
//! 注入 `BgRegistryEvent` 泵，暂不依赖 M-event-chain）。
//!
//! Middleware 只做任务定义与启动发起（经 `TaskManager` 接口），不持有管理权；
//! 任务生命周期（取消/超时/事件）跟随 session（随 session 创建/销毁）。
//!
//! Task 保持易失投影语义：不持久化，重启不复活。

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;

use futures::FutureExt;
use thiserror::Error;
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::agent::events::BackgroundTaskResult;

/// bg agent 取消的优雅退出窗口（秒）：cancel() 先 `token.cancel()` 让任务响应
/// 取消链走完整收尾；超过该窗口任务仍未结束才 abort 兜底。
const CANCEL_GRACE_SECS: u64 = 3;

/// 后台任务注册表错误（结构化，取代 String 错误）
///
/// 参考已有 `lsp/tool.rs:LspToolError` 模式：实现 `std::error::Error`，
/// 调用方可通过 `?` 自动转 `Box<dyn Error>` / `anyhow::Error`。
#[derive(Debug, Error)]
pub enum BackgroundRegistryError {
    #[error("Maximum {0} concurrent background tasks reached")]
    ConcurrentLimit(usize),
    #[error("Task {0} not found")]
    TaskNotFound(String),
    #[error("Task {0} cannot be cancelled: kill handle unavailable")]
    KillUnavailable(String),
    #[error("Kind concurrent limit reached: {kind} ({current}/{limit})")]
    KindConcurrentLimit {
        kind: String,
        current: usize,
        limit: usize,
    },
}

/// 后台任务类别（事实源 peri-acp-types::tasks）
pub use peri_acp_types::tasks::{BgShellHandle, BgTaskKind, BgTaskRegistration};

pub enum BgCancelHandle {
    /// bg agent：取消 tokio task。
    /// 持 `JoinHandle`（而非 `AbortHandle`）——取消时先 `token.cancel()` 让任务
    /// 优雅退出，再 await JoinHandle 等待其走完收尾，超时才 abort。
    Abort(tokio::task::JoinHandle<()>),
    /// workflow：kill 闭包——转发到 `WorkflowTaskRegistry::kill`（真正的 kill_tx 在其内部）。
    /// `None` 表示 kill 通道不可用（如 spawn 失败），此时 `cancel()` 返回明确错误
    /// 而非假装成功（issue 2026-08-05：Workflow 取消无效）。
    Kill(Option<Box<dyn FnOnce() + Send + Sync>>),
    /// bg shell：OS 进程 kill
    Pid(u32),
}

impl std::fmt::Debug for BgCancelHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BgCancelHandle::Abort(_) => f.write_str("Abort(_)"),
            BgCancelHandle::Kill(_) => f.write_str("Kill(_)"),
            BgCancelHandle::Pid(pid) => f.debug_tuple("Pid").field(pid).finish(),
        }
    }
}

/// 后台任务信息（注册表条目）
pub struct BackgroundTask {
    pub id: String,
    pub agent_name: String,
    pub prompt_summary: String,
    pub status: BackgroundTaskStatus,
    pub started_at: std::time::Instant,
    /// 任务创建时间（chrono UTC），用于 list_tasks_full().started_at 返回真实时间
    pub chrono_started_at: chrono::DateTime<chrono::Utc>,
    /// 任务类型
    pub kind: BgTaskKind,
    /// 按 kind 分发的取消句柄
    pub cancel_handle: BgCancelHandle,
    /// 取消令牌（仅 Agent 类任务）：cancel() 时先 `token.cancel()` 让工具层取消链
    /// 生效（run_react_loop 的 await 点响应后走完整收尾），超时再 abort 兜底。
    /// Shell/Workflow 类任务为 None（取消走 Pid/Kill 句柄）。
    pub cancel_token: Option<CancellationToken>,
    /// OS 进程 PID（仅 bg shell 有效）
    pub pid: Option<u32>,
    /// 输出预览（completed 时写入，最多 500 字符）
    pub output_preview: Option<String>,
}

/// 后台任务状态
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BackgroundTaskStatus {
    Running,
    Completed,
    Failed,
}

/// 后台任务信息 DTO（序列化用）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BgTaskInfo {
    pub task_id: String,
    pub kind: BgTaskKind,
    pub summary: String,
    pub status: BackgroundTaskStatus,
    pub started_at: String,
    pub duration_ms: u64,
    pub pid: Option<u32>,
    pub output_preview: Option<String>,
}

/// Registry → ACP 层事件桥接
/// 后台任务注册表事件（事实源 peri-acp-types::tasks）
pub use peri_acp_types::tasks::BgRegistryEvent;

/// 后台任务注册中心
pub struct BackgroundTaskRegistry {
    tasks: parking_lot::Mutex<HashMap<String, BackgroundTask>>,
    event_sender: parking_lot::RwLock<Option<tokio::sync::mpsc::UnboundedSender<BgRegistryEvent>>>,
    session_id: parking_lot::RwLock<String>,
}

impl Default for BackgroundTaskRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl BackgroundTaskRegistry {
    pub const SHELL_LIMIT: usize = 5;
    pub const AGENT_LIMIT: usize = 3;
    pub const WORKFLOW_LIMIT: usize = 3;

    pub fn new() -> Self {
        Self {
            tasks: parking_lot::Mutex::new(HashMap::new()),
            event_sender: parking_lot::RwLock::new(None),
            session_id: parking_lot::RwLock::new(String::new()),
        }
    }

    /// 设置 ACP 事件推送通道（由 executor 在 run_session_loop 调用）
    pub fn set_event_sender(
        &self,
        sender: tokio::sync::mpsc::UnboundedSender<BgRegistryEvent>,
        session_id: String,
    ) {
        *self.event_sender.write() = Some(sender);
        *self.session_id.write() = session_id;
    }

    /// 清除 ACP 事件推送通道（session 结束时调用）
    pub fn clear_event_sender(&self) {
        *self.event_sender.write() = None;
        self.session_id.write().clear();
    }

    /// 当前运行中的任务数
    pub fn active_count(&self) -> usize {
        self.tasks
            .lock()
            .values()
            .filter(|t| matches!(t.status, BackgroundTaskStatus::Running))
            .count()
    }

    /// 按类型统计运行中任务数
    pub fn count_by_kind(&self, kind: BgTaskKind) -> usize {
        self.tasks
            .lock()
            .values()
            .filter(|t| matches!(t.status, BackgroundTaskStatus::Running) && t.kind == kind)
            .count()
    }

    /// 按类型注册新任务（独立上限）
    pub fn register_with_kind(&self, task: BackgroundTask) -> Result<(), BackgroundRegistryError> {
        let limit = match task.kind {
            BgTaskKind::Shell => Self::SHELL_LIMIT,
            BgTaskKind::Agent => Self::AGENT_LIMIT,
            BgTaskKind::Workflow => Self::WORKFLOW_LIMIT,
        };

        let kind = task.kind;
        let task_id = task.id.clone();
        let summary = task.prompt_summary.clone();

        let mut tasks = self.tasks.lock();
        let current = tasks
            .values()
            .filter(|t| matches!(t.status, BackgroundTaskStatus::Running) && t.kind == kind)
            .count();
        if current >= limit {
            let kind_str = match kind {
                BgTaskKind::Shell => "shell",
                BgTaskKind::Agent => "agent",
                BgTaskKind::Workflow => "workflow",
            };
            return Err(BackgroundRegistryError::KindConcurrentLimit {
                kind: kind_str.to_string(),
                current,
                limit,
            });
        }

        tasks.insert(task.id.clone(), task);
        drop(tasks);

        // 推送 BgTaskStarted 事件
        self.push_event(BgRegistryEvent::Started {
            task_id,
            kind,
            summary,
            started_at: chrono::Utc::now().to_rfc3339(),
        });

        Ok(())
    }

    /// 任务完成时调用：更新状态 + 推送通知。
    ///
    /// 返回 `true` 表示条目存在且已处理；`false` 表示任务已不在 registry
    /// （如已被 cancel 移除后自然完成），此时不推送 Completed 事件——否则会
    /// 产生幽灵完成事件（issue 2026-08-05：kill 后仍推 bg-task-completed）。
    pub fn complete(&self, task_id: &str, result: BackgroundTaskResult) -> bool {
        tracing::info!(
            task_id = %task_id,
            agent_name = %result.agent_name,
            success = result.success,
            output_len = result.output.len(),
            "[bg-diag] registry.complete() called"
        );
        let duration_ms = result.duration_ms;
        let success = result.success;
        let output_preview: String = result.output.chars().take(500).collect();

        // 持锁：更新状态 + 清理所有非 Running 任务，防止 JoinHandle 长期驻留内存
        let mut tasks = self.tasks.lock();
        let kind = tasks.get(task_id).map(|task| task.kind);
        let existed = tasks.contains_key(task_id);
        if let Some(task) = tasks.get_mut(task_id) {
            task.status = if result.success {
                BackgroundTaskStatus::Completed
            } else {
                BackgroundTaskStatus::Failed
            };
            task.output_preview = Some(output_preview.clone());
        }
        tasks.retain(|_, t| matches!(t.status, BackgroundTaskStatus::Running));
        drop(tasks);

        // 已移除条目不推幽灵 Completed 事件（cancel 已通知过用户）。
        // warn 而非静默：任务不在 registry 却走到 complete()，通常是
        // task_id 碰撞覆盖注册（同毫秒 UUID v7 截断前缀）或双重 complete，
        // 会导致 TUI 任务条目残留（issue 2026-08-05）。
        if !existed {
            warn!(
                task_id = %task_id,
                agent_name = %result.agent_name,
                success,
                "background registry: complete() called for unknown task (collision or double-complete); \
                 Completed event suppressed"
            );
            return false;
        }

        // 推送 BgTaskCompleted 事件（携带完整 result 供下游注入主 agent inbox）
        self.push_event(BgRegistryEvent::Completed {
            task_id: task_id.to_string(),
            kind,
            success,
            output_preview,
            duration_ms,
            result,
        });
        true
    }

    /// 获取所有任务状态（UI 使用）
    pub fn list_tasks(&self) -> Vec<(String, BackgroundTaskStatus, String)> {
        self.tasks
            .lock()
            .values()
            .map(|t| (t.id.clone(), t.status.clone(), t.prompt_summary.clone()))
            .collect()
    }

    /// 获取完整任务信息（供 ACP Snapshot / TUI 面板使用）
    pub fn list_tasks_full(&self) -> Vec<BgTaskInfo> {
        self.tasks
            .lock()
            .values()
            .map(|t| BgTaskInfo {
                task_id: t.id.clone(),
                kind: t.kind,
                summary: t.prompt_summary.clone(),
                status: t.status.clone(),
                started_at: t.chrono_started_at.to_rfc3339(),
                duration_ms: t.started_at.elapsed().as_millis() as u64,
                pid: t.pid,
                output_preview: t.output_preview.clone(),
            })
            .collect()
    }

    /// 取消指定任务（按 BgCancelHandle 分发取消逻辑）
    pub fn cancel(&self, task_id: &str) -> Result<(), BackgroundRegistryError> {
        let mut tasks = self.tasks.lock();
        // 先校验取消句柄可用性：Kill(None) 表示 kill 通道不可用（如 workflow kill 闭包缺失、
        // shell spawn 失败），此时如实返回错误并保留条目，等待任务自然完成，
        // 而不是移除条目 + 发 cancelled 事件假装成功（issue 2026-08-05）。
        let handle_unavailable = matches!(
            tasks.get(task_id).map(|t| &t.cancel_handle),
            Some(BgCancelHandle::Kill(None))
        );
        if handle_unavailable {
            return Err(BackgroundRegistryError::KillUnavailable(
                task_id.to_string(),
            ));
        }
        if let Some(task) = tasks.remove(task_id) {
            match task.cancel_handle {
                BgCancelHandle::Abort(mut handle) => {
                    // S3.2：先触发工具层取消链——任务在下一个响应 cancel 的 await 点
                    // （reason LLM 调用 / 工具执行 / idle 等待）退出，走完整收尾
                    // （SubagentStopped / deregister / thread status / stop hooks）。
                    if let Some(token) = task.cancel_token.as_ref() {
                        token.cancel();
                    }
                    // 超时兜底：等待任务自然结束（grace 窗口内响应 cancel 则保留
                    // async 收尾），超时再 abort——否则"取消后任务继续跑"比 abort 更糟。
                    // abort 兜底路径：任务内同步收尾 guard（deregister_runtime 等）仍执行，
                    // async 收尾（update_thread_status / stop hooks）丢失并记日志。
                    match tokio::runtime::Handle::try_current() {
                        Ok(_) => {
                            let task_id_owned = task_id.to_string();
                            tokio::spawn(async move {
                                if tokio::time::timeout(
                                    std::time::Duration::from_secs(CANCEL_GRACE_SECS),
                                    &mut handle,
                                )
                                .await
                                .is_err()
                                {
                                    handle.abort();
                                    warn!(
                                        task_id = %task_id_owned,
                                        "bg task cancel: grace period elapsed, aborted task \
                                         (async cleanup lost: thread status / stop hooks; \
                                         sync cleanup guard still runs)"
                                    );
                                }
                            });
                        }
                        Err(_) => {
                            // 无 tokio runtime 上下文（防御；生产调用点均在 async 上下文）：
                            // 无法异步等待，直接 abort 兜底。
                            handle.abort();
                            warn!(
                                task_id = %task_id,
                                "bg task cancel: no tokio runtime for graceful wait, aborted task"
                            );
                        }
                    }
                }
                BgCancelHandle::Kill(Some(kill)) => {
                    // 触发 kill 闭包：workflow 场景转发到 WorkflowTaskRegistry::kill
                    kill();
                }
                BgCancelHandle::Kill(None) => {
                    // 上方已校验，理论不可达；防御性保留
                    unreachable!("Kill(None) checked before task removal");
                }
                BgCancelHandle::Pid(pid) => {
                    if pid == 0 {
                        // 防御性守卫：Pid(0) 会导致 kill -TERM 0 波及当前进程组
                        warn!(
                            task_id = %task_id,
                            "bg task cancel: pid is 0 (spawn likely failed), skipping kill"
                        );
                    } else {
                        // 杀整个进程组（bash 为组长），避免子进程孤儿存活
                        kill_process_group(pid, "TERM");
                    }
                }
            }
            drop(tasks);

            self.push_event(BgRegistryEvent::Cancelled {
                task_id: task_id.to_string(),
                reason: "user cancelled".to_string(),
            });

            Ok(())
        } else {
            Err(BackgroundRegistryError::TaskNotFound(task_id.to_string()))
        }
    }

    /// 清理已完成的任务
    pub fn cleanup_completed(&self) {
        self.tasks
            .lock()
            .retain(|_, t| matches!(t.status, BackgroundTaskStatus::Running));
    }
}

impl BackgroundTaskRegistry {
    /// 推送 registry 事件到 ACP 层（非阻塞，channel 满时静默丢弃）
    fn push_event(&self, event: BgRegistryEvent) {
        if let Some(sender) = self.event_sender.read().as_ref() {
            if sender.send(event).is_err() {
                warn!("background registry: event channel closed");
            }
        }
    }
}

// ── Cross-platform shell command spawning ────────────────────────────────────

// [TRAP] 所有子进程 spawn 必须通过 shell_command() 统一 wrapper
// 新增 spawn 时必须复用，禁止直接用 std::process::Command 裸调。

/// 向进程组发送信号（fire-and-forget，不等待结果）。
///
/// - **Unix**：执行 `kill -<SIG> -- -<pid>`——负号 PID 表示进程组，`--` 防止
///   PID 被解析为选项（macOS BSD kill 与 Linux GNU kill 均支持）。
///   前提：调用方 spawn 时已设置 `process_group(0)` 使 bash 成为进程组组长，
///   这样 TERM/KILL 会波及 shell 的全部子进程，避免孤儿进程存活。
/// - **Windows**：无 POSIX 信号/进程组，回退 `taskkill /T /F` 尽力杀进程树。
///
/// 用法示例：`kill_process_group(pid, "TERM")`。
pub fn kill_process_group(pid: u32, signal: &str) {
    if pid == 0 {
        // 防御性守卫：kill 0 会波及当前进程组
        return;
    }
    #[cfg(windows)]
    let _ = signal; // Windows 回退 taskkill /T /F，不使用信号参数
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("kill")
            .arg(format!("-{signal}"))
            .arg("--")
            .arg(format!("-{pid}"))
            // 静默：进程组可能已自然退出（kill 失败属预期），避免噪音日志
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .arg("/PID")
            .arg(pid.to_string())
            .arg("/T")
            .arg("/F")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

/// Escape an argument for PowerShell single-quoted literal string.
///
/// In PowerShell, single-quoted strings treat all characters literally except
/// the single quote itself, which is escaped by doubling (`''`). This prevents
/// metacharacters like `$`, `` ` ``, `@`, `(`, `)`, `|`, `;`, `&` from being
/// interpreted as code.
///
/// Returns the argument wrapped in single quotes with internal `'` doubled
/// if it contains characters that need escaping; otherwise returns as-is.
fn escape_powershell_arg(arg: &str) -> String {
    let needs_quoting = arg.is_empty()
        || arg.contains(' ')
        || arg.contains('\'')
        || arg.contains('$')
        || arg.contains('`')
        || arg.contains('(')
        || arg.contains(')')
        || arg.contains('{')
        || arg.contains('}')
        || arg.contains(';')
        || arg.contains('|')
        || arg.contains('&')
        || arg.contains('@')
        || arg.contains('#');
    if !needs_quoting {
        return arg.to_string();
    }
    // Escape internal single quotes by doubling, then wrap in single quotes
    format!("'{}'", arg.replace('\'', "''"))
}

/// Build a `tokio::process::Command` that executes the given command through the
/// platform shell.
///
/// - **Unix**: `bash -c "<command> <args...>"`
/// - **Windows**: `powershell -NoProfile -NonInteractive -NoLogo -Command <cmd>`
///
/// Semantics mirror `bash -c`/`cmd /C`: `command` is parsed by the shell as a
/// script (so users may use pipes, `;`, redirections, variables, etc.). `args`
/// are treated as literal parameter values and are escaped as PowerShell
/// single-quoted strings to prevent metacharacters (`$`, `` ` ``, `(`, `)`,
/// `{`, `}`, `;`, `|`, `&`, `@`, `#`) from being interpreted as code.
///
/// `command` is intentionally NOT escaped on Windows — wrapping it in single
/// quotes would turn it into a PowerShell string literal, which `-Command`
/// would then evaluate as an expression and echo back verbatim instead of
/// executing it (e.g. `ping -n 60 127.0.0.1` was returned unchanged).
///
/// `kill_on_drop` only terminates the PowerShell wrapper process — child
/// processes (including peri) are NOT killed.
///
/// Returns the `Command` object so callers can add custom configuration
/// (env, current_dir, stdin/stdout/stderr, kill_on_drop, etc.).
pub fn shell_command(command: &str, args: &[&str]) -> tokio::process::Command {
    if cfg!(target_os = "windows") {
        // command 直接作为 PowerShell 脚本拼接（与 bash -c / cmd /C 一致），
        // 让 shell 解析管道、分号、重定向等。绝不能用单引号包围——否则
        // PowerShell 会把它当作字符串字面量，-Command 会 echo 出字符串本身。
        // args 是字面参数值，用单引号 escape 防止 PowerShell 元字符注入。
        let mut shell_cmd = command.to_string();
        for arg in args {
            shell_cmd.push(' ');
            shell_cmd.push_str(&escape_powershell_arg(arg));
        }

        let mut cmd = tokio::process::Command::new("powershell");
        cmd.arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-NoLogo")
            .arg("-Command")
            .arg(&shell_cmd);
        cmd
    } else {
        let mut parts = vec![command.to_string()];
        for arg in args {
            if arg.contains(' ') || arg.contains('"') || arg.contains('\'') || arg.contains('\\') {
                parts.push(format!("'{}'", arg.replace('\'', "'\\''")));
            } else {
                parts.push(arg.to_string());
            }
        }
        let shell_cmd = parts.join(" ");
        let mut cmd = tokio::process::Command::new("bash");
        cmd.arg("-c").arg(&shell_cmd);
        cmd
    }
}

// ── 输出截断落盘（bg shell 执行链共用）───────────────────────────────────────

/// 当输出被截断时，将完整内容写入临时文件。
/// 返回追加到截断信息后的提示字符串。
/// 文件路径：`{temp_dir}/peri-tool-output-{uuid}.txt`
pub fn persist_truncated_output(full_content: &str) -> String {
    let id = uuid::Uuid::new_v4();
    let dir = std::env::temp_dir();
    let file_name = format!("peri-tool-output-{id}.txt");
    let file_path = dir.join(&file_name);

    match std::fs::write(&file_path, full_content) {
        Ok(_) => format!(
            "\n\n[Full output saved to {} — use Read tool to view complete content]",
            file_path.display()
        ),
        Err(e) => format!(
            "\n\n[Failed to save full output to {}: {e}]",
            file_path.display()
        ),
    }
}

/// 按字节截断字符串，确保不拆分 UTF-8 字符边界。
///
/// 与 `&s[..max_bytes]` 不同，此函数会从 `max_bytes` 位置向前搜索
/// 最近的字符边界，避免在多字节字符中间截断。
pub fn truncate_bytes(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

// ── bg shell 执行链 ──────────────────────────────────────────────────────────

/// 生成 bg shell 任务 id：`shell-{完整 UUID v7}`。
///
/// **禁止截断 UUID**（issue 2026-08-05）：UUID v7 前 48 位是毫秒时间戳，
/// 同一毫秒内生成的前 8 字符必然相同。agent 连续多次 `run_in_background`
/// Bash 调用落在同一毫秒时，截断前缀会导致 task_id 碰撞——registry 覆盖
/// 注册（Started 事件重复、cancel 句柄丢失），且首个 `complete()` 的 retain
/// 清理后其余 `complete()` 因 existed=false 静默跳过，TUI 残留任务条目。
/// 与 bg agent（`bg-{完整 UUID}`）保持一致，用完整 UUID（122 位熵）。
pub fn bg_shell_task_id() -> String {
    format!("shell-{}", uuid::Uuid::now_v7())
}

/// 解析 timeout 参数（None = 不超时）。
///
/// - **后台**：未传 → None（默认不超时，与"后台"语义一致）；显式 0 → None；
///   显式 >0 → clamp 到 [min, 600_000]
/// - **同步**：未传 → Some(15_000)；显式 0 → None；显式 >0 → clamp
pub fn parse_timeout(input: &serde_json::Value, is_background: bool) -> Option<u64> {
    let min = if cfg!(target_os = "windows") { 5000 } else { 1 };
    match input.get("timeout").and_then(|v| v.as_u64()) {
        None => {
            if is_background {
                None
            } else {
                Some(15_000)
            }
        }
        Some(0) => None,
        Some(ms) => Some(ms.clamp(min, 600_000)),
    }
}

/// 向进程组发送 TERM，2 秒后若仍存活则升级为 KILL（fire-and-forget 任务）。
/// 用于超时分支：TERM 无法终止的进程（如 trap 忽略 TERM）由 KILL 兜底。
pub fn kill_process_group_escalating(pid: u32) {
    kill_process_group(pid, "TERM");
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        kill_process_group(pid, "KILL");
    });
}

/// 将 stdout/stderr 管道流式读入共享缓冲。缓冲超过 `MAX_PARTIAL_CAPTURE_BYTES`
/// 后继续排空（丢弃新内容），防止子进程写满管道时阻塞。
pub async fn drain_pipe(
    mut reader: impl tokio::io::AsyncRead + Unpin,
    buf: Arc<std::sync::Mutex<String>>,
) {
    let mut chunk = [0u8; 8192];
    loop {
        let n = match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        let mut guard = match buf.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        if guard.len() < MAX_PARTIAL_CAPTURE_BYTES {
            let s = String::from_utf8_lossy(&chunk[..n]);
            let remaining = MAX_PARTIAL_CAPTURE_BYTES - guard.len();
            guard.push_str(&s[..s.len().min(remaining)]);
        }
    }
}

/// 同步路径流式捕获的共享缓冲上限（2MB）；超过后继续排空管道（丢弃新内容），
/// 防止子进程写管道时阻塞
const MAX_PARTIAL_CAPTURE_BYTES: usize = 2 * 1024 * 1024;

/// 将 stdout/stderr 管道流式读入共享缓冲，同时追加到日志文件（tee）。
/// 缓冲超过 `MAX_PARTIAL_CAPTURE_BYTES` 后继续排空（丢弃新内容），
/// 防止子进程写满管道时阻塞。日志文件写入失败仅降级（不影响执行链）。
/// `log: None` = 不落盘（等价于 [`drain_pipe`]）。
pub async fn tee_pipe(
    mut reader: impl tokio::io::AsyncRead + Unpin,
    buf: Arc<std::sync::Mutex<String>>,
    mut log: Option<std::fs::File>,
) {
    let mut chunk = [0u8; 8192];
    loop {
        let n = match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        if let Some(f) = log.as_mut() {
            use std::io::Write;
            let _ = f.write_all(&chunk[..n]);
        }
        let mut guard = match buf.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        if guard.len() < MAX_PARTIAL_CAPTURE_BYTES {
            let s = String::from_utf8_lossy(&chunk[..n]);
            let remaining = MAX_PARTIAL_CAPTURE_BYTES - guard.len();
            guard.push_str(&s[..s.len().min(remaining)]);
        }
    }
}

/// bg shell 结果收尾（bg 路径与同步超时 promote 续跑共用）：
/// 超长输出落盘 → 构造 BackgroundTaskResult → on_bg_complete 回调 → complete()。
/// 任务在启动时已注册（BgTaskStarted 已推送），此处只收尾，不再重复注册。
#[allow(clippy::too_many_arguments)]
#[allow(clippy::type_complexity)]
pub fn finalize_bg_shell(
    registry: &BackgroundTaskRegistry,
    on_bg_complete: &Option<Arc<dyn Fn(&BackgroundTaskResult, BgTaskKind) + Send + Sync>>,
    task_id: String,
    prompt_summary: String,
    success: bool,
    output: String,
    duration_ms: u64,
    timed_out: bool,
) {
    // 输出超长落盘（>100K 字符时截断 + 持久化完整内容到磁盘）
    const BG_OUTPUT_TRUNC_THRESHOLD: usize = 100_000;
    let output_str = if output.len() > BG_OUTPUT_TRUNC_THRESHOLD {
        let persist_hint = persist_truncated_output(&output);
        let truncated = truncate_bytes(&output, BG_OUTPUT_TRUNC_THRESHOLD);
        format!("{}{}", truncated, persist_hint)
    } else {
        output
    };
    let result = BackgroundTaskResult {
        task_id: task_id.clone(),
        agent_name: "bg-shell".to_string(),
        prompt_summary,
        success,
        output: output_str,
        tool_calls_count: 0,
        duration_ms,
        child_thread_id: None,
        timed_out,
    };
    // 回调通知 Agent inbox（在 registry.complete() 之前，与 execute_bg.rs 对齐）
    if let Some(ref cb) = on_bg_complete {
        cb(&result, BgTaskKind::Shell);
    }
    // 任务已在启动时注册（run_in_background / promote 路径），此处只收尾推送 Completed。
    registry.complete(&result.task_id.clone(), result);
}

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

#[cfg(test)]
#[path = "async_tasks_test.rs"]
mod tests;
