#[cfg(windows)]
use std::process::Stdio;
use std::sync::Arc;

use peri_acp_types::tasks::BgTaskKind;
use tokio::io::AsyncReadExt;

use crate::agent::events::BackgroundTaskResult;

use super::registry::BackgroundTaskRegistry;
use super::shell_output::{ShellOutputCapture, ShellOutputWriter};

/// Keeps an external shell's cleanup evidence across foreground/background handoff.
pub struct ShellExecutionGuard {
    ownership: Option<Box<dyn peri_acp_types::tasks::ExternalExecutionGuard>>,
    tree: Option<Arc<peri_process::ProcessTree>>,
    child: Option<tokio::process::Child>,
    stopped: bool,
    registration: Option<(Arc<dyn peri_acp_types::tasks::TaskManager>, String)>,
}

impl ShellExecutionGuard {
    pub fn new(ownership: Option<Box<dyn peri_acp_types::tasks::ExternalExecutionGuard>>) -> Self {
        Self {
            ownership,
            tree: None,
            child: None,
            stopped: false,
            registration: None,
        }
    }

    /// Establish OS process-tree ownership before the command can execute.
    pub fn prepare(&mut self, command: &mut tokio::process::Command) -> std::io::Result<()> {
        let tree = peri_process::ProcessTree::new()?;
        tree.prepare(command);
        self.tree = Some(Arc::new(tree));
        Ok(())
    }

    pub fn attach(&mut self, child: &tokio::process::Child) -> std::io::Result<()> {
        Arc::get_mut(
            self.tree
                .as_mut()
                .ok_or_else(|| std::io::Error::other("shell process tree was not prepared"))?,
        )
        .ok_or_else(|| std::io::Error::other("shell process tree already shared"))?
        .attach(child)
    }

    pub fn attach_owned(&mut self, child: tokio::process::Child) -> std::io::Result<()> {
        let result = self.attach(&child);
        self.child = Some(child);
        result
    }

    pub fn child_mut(&mut self) -> &mut tokio::process::Child {
        self.child.as_mut().expect("shell child must be attached")
    }

    /// Windows cancellation retains the exact job handle, never a reusable PID.
    pub fn cancel_callback(&self) -> Option<Box<dyn FnOnce() + Send + Sync>> {
        #[cfg(windows)]
        {
            self.tree.as_ref().map(|tree| {
                let tree = Arc::clone(tree);
                Box::new(move || tree.terminate()) as Box<dyn FnOnce() + Send + Sync>
            })
        }
        #[cfg(not(windows))]
        {
            None
        }
    }

    pub fn is_stopped(&self) -> bool {
        self.tree.as_ref().is_none_or(|tree| tree.is_stopped())
    }

    /// Keep proof of a registered process even if its follow-up task is rejected during close.
    pub fn track_registration(
        &mut self,
        manager: Arc<dyn peri_acp_types::tasks::TaskManager>,
        task_id: String,
    ) {
        self.registration = Some((manager, task_id));
    }

    /// A reaped command leader can leave a live, registered background process group.
    pub async fn wait_for_exit(&mut self) {
        while !self.is_stopped() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// Standalone tool callers without session ownership retain their original process semantics.
    pub fn release_unmanaged(&mut self) {
        if self.ownership.is_none() {
            if let Some(tree) = &mut self.tree {
                if !Arc::get_mut(tree).is_some_and(|tree| tree.disarm().is_ok()) {
                    return;
                }
            }
            self.stopped = true;
        }
    }

    /// Call only after the child and its pipe readers have been joined.
    pub fn confirm_stopped(&mut self) {
        if self.is_stopped() {
            self.stopped = true;
            if let Some(owner) = &mut self.ownership {
                owner.confirm_stopped();
            }
            if let Some((manager, id)) = self.registration.take() {
                manager.confirm_external_execution_stopped(&id);
            }
        }
    }
}

impl Drop for ShellExecutionGuard {
    fn drop(&mut self) {
        if self.stopped {
            return;
        }
        let Some(tree) = self.tree.take() else {
            if let Some(owner) = &mut self.ownership {
                owner.confirm_stopped();
            }
            return;
        };
        tree.terminate();
        let mut child = self.child.take();
        let mut ownership = self.ownership.take();
        let registration = self.registration.take();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            // 保留 Child 并显式回收；不能让进程组退出证据依赖 orphan reaper 的调度。
            runtime.spawn(async move {
                let cleanup = async {
                    if let Some(child) = &mut child {
                        child.wait().await?;
                    }
                    tree.wait_for_exit().await;
                    Ok::<(), std::io::Error>(())
                };
                if matches!(
                    tokio::time::timeout(std::time::Duration::from_secs(3), cleanup).await,
                    Ok(Ok(()))
                ) {
                    if let Some(owner) = &mut ownership {
                        owner.confirm_stopped();
                    }
                    if let Some((manager, id)) = registration {
                        manager.confirm_external_execution_stopped(&id);
                    }
                }
            });
        }
    }
}

fn process_group_stopped(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        if pid == 0 {
            return false;
        }
        // Probe only. ESRCH proves that no process remains in this owned group.
        let result = unsafe { libc::kill(-pid, 0) };
        result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        // Child exit alone cannot prove Windows descendant cleanup.
        false
    }
}

// ── Cross-platform shell command spawning ────────────────────────────────────

// [TRAP] 所有子进程 spawn 必须通过 shell_command() 统一 wrapper
// 新增 spawn 时必须复用，禁止直接用 std::process::Command 裸调。

/// 请求终止进程组；成功发送信号本身不代表进程已退出。
///
/// - **Unix**：直接调用 `kill(-pid, signal)`，负号 PID 表示进程组。
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
        let Ok(pid) = i32::try_from(pid) else {
            return;
        };
        let signal = match signal {
            "TERM" => libc::SIGTERM,
            "KILL" => libc::SIGKILL,
            _ => return,
        };
        // A direct syscall leaves no detached helper process after session drain.
        unsafe {
            libc::kill(-pid, signal);
        }
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
            .status();
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
    let (hint, _) = persist_truncated_output_with_ref(full_content);
    hint
}

/// Persist a full output and return both the display hint and durable path.
/// The caller should carry the path as typed evidence instead of recovering it
/// from rendered text.
pub fn persist_truncated_output_with_ref(full_content: &str) -> (String, Option<String>) {
    let id = uuid::Uuid::new_v4();
    let dir = std::env::temp_dir();
    let file_name = format!("peri-tool-output-{id}.txt");
    let file_path = dir.join(&file_name);

    match std::fs::write(&file_path, full_content) {
        Ok(_) => (
            format!(
                "\n\n[Full output saved to {} — use Read tool to view complete content]",
                file_path.display()
            ),
            Some(file_path.to_string_lossy().into_owned()),
        ),
        Err(e) => (
            format!(
                "\n\n[Failed to save full output to {}: {e}]",
                file_path.display()
            ),
            None,
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
    tokio::spawn(terminate_process_group(pid));
}

/// The caller owns this future until the TERM/KILL sequence has completed.
pub(super) async fn terminate_process_group(pid: u32) {
    kill_process_group(pid, "TERM");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while !process_group_stopped(pid) {
        if tokio::time::Instant::now() >= deadline {
            kill_process_group(pid, "KILL");
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
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
        append_preview(&mut guard, &chunk[..n]);
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
        append_preview(&mut guard, &chunk[..n]);
    }
}

/// Variant used by shell tasks whose output may be promoted to the
/// background. It writes every byte to the durable stream file while keeping
/// the bounded in-memory preview, and records read/write failures for the
/// typed completion evidence.
pub async fn tee_pipe_with_output(
    mut reader: impl tokio::io::AsyncRead + Unpin,
    buf: Arc<std::sync::Mutex<String>>,
    mut output: ShellOutputWriter,
    capture: Arc<ShellOutputCapture>,
    stream: &'static str,
) {
    let mut chunk = [0u8; 8192];
    loop {
        let n = match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(error) => {
                capture.record_read_error(stream, error);
                break;
            }
        };
        output.write_chunk(&chunk[..n]).await;
        let mut guard = match buf.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        append_preview(&mut guard, &chunk[..n]);
    }
    output.finish().await;
}

fn append_preview(preview: &mut String, bytes: &[u8]) {
    if preview.len() >= MAX_PARTIAL_CAPTURE_BYTES {
        return;
    }
    let remaining = MAX_PARTIAL_CAPTURE_BYTES - preview.len();
    let text = String::from_utf8_lossy(bytes);
    preview.push_str(&truncate_bytes(&text, remaining));
}

/// bg shell 结果收尾（bg 路径与同步超时 promote 续跑共用）：
/// 输出引用已就绪 → 认领完成 → on_bg_complete 回调 → complete()。
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
    shell_output: Option<peri_acp_types::event::ShellOutput>,
) {
    let result = BackgroundTaskResult {
        task_id: task_id.clone(),
        agent_name: "bg-shell".to_string(),
        prompt_summary,
        success,
        // Shell output is projected through the typed file reference below;
        // keep this field short so callback/reminder size is independent of
        // process output volume.
        output: if shell_output.is_some() {
            if success {
                "Shell command completed; read the output files as needed.".into()
            } else {
                "Shell command failed; read the output files as needed.".into()
            }
        } else {
            output
        },
        tool_calls_count: 0,
        duration_ms,
        child_thread_id: None,
        timed_out,
        subagent_failure: None,
        shell_output: shell_output.map(Box::new),
    };
    // Linearize completion against cancel before publishing anything. A
    // claimed task remains active until the callback and terminal commit
    // finish, so the idle loop cannot exit before its result is enqueued.
    if !registry.claim_completion(&task_id) {
        return;
    }
    // 回调通知 Agent inbox（在 registry.complete() 之前，与 execute_bg.rs 对齐）
    if let Some(ref cb) = on_bg_complete {
        let callback = std::panic::AssertUnwindSafe(|| cb(&result, BgTaskKind::Shell));
        if let Err(panic) = std::panic::catch_unwind(callback) {
            let detail = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(ToString::to_string))
                .unwrap_or_else(|| "unknown panic".to_string());
            tracing::error!(task_id = %result.task_id, error = %detail, "background shell completion callback panicked");
        }
    }
    // 任务已在启动时注册（run_in_background / promote 路径），此处只收尾推送 Completed。
    registry.complete(&result.task_id.clone(), result);
}

#[cfg(test)]
mod tests {
    use super::finalize_bg_shell;
    use crate::agent::async_tasks::{
        BackgroundTask, BackgroundTaskRegistry, BackgroundTaskStatus, BgCancelHandle,
    };
    use peri_acp_types::tasks::BgTaskKind;
    use std::sync::Arc;

    fn registered_shell() -> Arc<BackgroundTaskRegistry> {
        let registry = Arc::new(BackgroundTaskRegistry::new());
        registry
            .register_with_kind(BackgroundTask {
                id: "shell-panic".into(),
                agent_name: "bg-shell".into(),
                prompt_summary: "test".into(),
                status: BackgroundTaskStatus::Running,
                started_at: std::time::Instant::now(),
                chrono_started_at: chrono::Utc::now(),
                kind: BgTaskKind::Shell,
                cancel_handle: BgCancelHandle::Kill(Some(Box::new(|| {}))),
                cancel_token: None,
                pid: None,
                output_preview: None,
                agent_inbox: None,
            })
            .expect("register test shell");
        registry
    }

    #[test]
    fn callback_panic_still_completes_registered_shell() {
        let registry = registered_shell();
        let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
        registry.set_event_sender(sender, "fixture".into());
        let callback_registry = Arc::clone(&registry);
        let callback: peri_acp_types::tasks::OnBgCompleteFn = Arc::new(move |_, _| {
            assert_eq!(callback_registry.active_count(), 1);
            assert!(matches!(
                callback_registry.cancel("shell-panic"),
                Err(crate::agent::async_tasks::BackgroundRegistryError::TaskCompleting(_))
            ));
            panic!("callback failure");
        });
        finalize_bg_shell(
            &registry,
            &Some(callback),
            "shell-panic".into(),
            "test".into(),
            true,
            "completed".into(),
            1,
            false,
            None,
        );
        assert_eq!(registry.active_count(), 0);
        let peri_acp_types::tasks::BgRegistryEvent::Completed { result, .. } =
            events.try_recv().expect("one terminal event")
        else {
            panic!("completion claim must win over cancellation");
        };
        assert!(
            result.success,
            "callback panic must not change process outcome"
        );
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn cancelled_shell_does_not_publish_a_completion_callback() {
        let registry = registered_shell();
        registry.cancel("shell-panic").unwrap();
        let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let callback_called = Arc::clone(&called);
        let callback: peri_acp_types::tasks::OnBgCompleteFn = Arc::new(move |_, _| {
            callback_called.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        finalize_bg_shell(
            &registry,
            &Some(callback),
            "shell-panic".into(),
            "test".into(),
            true,
            "completed".into(),
            1,
            false,
            None,
        );
        assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(registry.active_count(), 0);
    }
}
