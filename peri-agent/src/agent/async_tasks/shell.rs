use std::process::Stdio;
use std::sync::Arc;

use peri_acp_types::tasks::BgTaskKind;
use tokio::io::AsyncReadExt;

use crate::agent::events::BackgroundTaskResult;

use super::registry::BackgroundTaskRegistry;

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
