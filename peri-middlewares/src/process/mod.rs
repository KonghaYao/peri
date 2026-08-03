//! Cross-platform shell command spawning.
//!
//! On Unix, wraps commands in `bash -c "<command> <args...>"`.
//! On Windows, wraps commands in PowerShell `-NoProfile -NonInteractive -NoLogo -Command`.

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
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .arg("/PID")
            .arg(pid.to_string())
            .arg("/T")
            .arg("/F")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
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

#[cfg(test)]
mod process_test;
