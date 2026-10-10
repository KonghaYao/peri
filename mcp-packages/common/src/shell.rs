#[cfg(windows)]
use std::process::Stdio;
use std::sync::Arc;

use peri_agent::agent::async_tasks::truncate_bytes;
use tokio::io::AsyncReadExt;

use super::shell_output::{ShellOutputCapture, ShellOutputWriter};

/// Keeps an external shell's cleanup evidence across foreground/background handoff.
pub struct ShellExecutionGuard {
    ownership: Option<Box<dyn peri_acp_types::tasks::ExternalExecutionGuard>>,
    tree: Option<Arc<peri_process::ProcessTree>>,
    child: Option<tokio::process::Child>,
    stopped: bool,
    pid: Option<u32>,
    registration: Option<(Arc<dyn peri_acp_types::tasks::TaskManager>, String)>,
}

impl ShellExecutionGuard {
    pub fn new(ownership: Option<Box<dyn peri_acp_types::tasks::ExternalExecutionGuard>>) -> Self {
        Self {
            ownership,
            tree: None,
            child: None,
            stopped: false,
            pid: None,
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
        self.pid = child.id();
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

    pub fn scoped_cancel_callback(
        &self,
        registry: Arc<peri_agent::agent::async_tasks::BackgroundTaskRegistry>,
    ) -> Option<Box<dyn FnOnce() + Send + Sync>> {
        #[cfg(unix)]
        {
            let tree = Arc::clone(self.tree.as_ref()?);
            let pid = self.pid?;
            Some(Box::new(move || {
                registry.spawn_execution_cleanup(async move {
                    if tree.is_stopped() {
                        return;
                    }
                    kill_process_group(pid, "TERM");
                    let deadline = peri_time::monotonic_now() + std::time::Duration::from_secs(2);
                    while !tree.is_stopped() {
                        if peri_time::monotonic_now() >= deadline {
                            tree.terminate();
                            return;
                        }
                        peri_time::sleep(std::time::Duration::from_millis(20)).await;
                    }
                });
            }))
        }
        #[cfg(not(unix))]
        {
            let _ = registry;
            self.cancel_callback()
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
            peri_time::sleep(std::time::Duration::from_millis(20)).await;
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
                    peri_time::timeout(std::time::Duration::from_secs(3), cleanup).await,
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
                // 指引指代 builtin 文件工具时必须用**模型面名字**（裸名已无提供面）；
                // 查表未命中的兜底不含工具名，不得回落到裸名。
                "\n\n[Full output saved to {} — {}]",
                file_path.display(),
                peri_acp_types::builtin_mcp::effective_name_of("workspace", "Read").map_or(
                    "read the file to view complete content".to_string(),
                    |name| format!("use `{name}` to view complete content"),
                ),
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

/// The caller owns this future until the TERM/KILL sequence has completed.
pub async fn terminate_process_group(pid: u32) {
    kill_process_group(pid, "TERM");
    let deadline = peri_time::monotonic_now() + std::time::Duration::from_secs(2);
    while !process_group_stopped(pid) {
        if peri_time::monotonic_now() >= deadline {
            kill_process_group(pid, "KILL");
            return;
        }
        peri_time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// 同步路径流式捕获的共享缓冲上限（2MB）；超过后继续排空管道（丢弃新内容），
/// 防止子进程写管道时阻塞
const MAX_PARTIAL_CAPTURE_BYTES: usize = 2 * 1024 * 1024;

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

#[cfg(test)]
#[path = "shell_test.rs"]
mod tests;

#[cfg(test)]
#[path = "shell_lifecycle_test.rs"]
mod lifecycle_tests;
