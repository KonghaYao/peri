//! Cross-platform shell command spawning.
//!
//! On Unix, wraps commands in `bash -c "<command> <args...>"`.
//! On Windows, wraps commands in PowerShell `-NoProfile -NonInteractive -NoLogo -Command`.

/// Build a `tokio::process::Command` that executes the given command through the
/// platform shell.
///
/// - **Unix**: `bash -c "<command> <args...>"`
/// - **Windows**: `powershell -NoProfile -NonInteractive -NoLogo -Command <cmd>`
///
/// On Windows, tokio auto-quotes arguments containing spaces (producing
/// `-Command "the command"`), which PowerShell's `-Command` parameter strips
/// and executes correctly. `kill_on_drop` only terminates the PowerShell wrapper
/// process — child processes (including peri) are NOT killed.
///
/// Returns the `Command` object so callers can add custom configuration
/// (env, current_dir, stdin/stdout/stderr, kill_on_drop, etc.).
pub fn shell_command(command: &str, args: &[&str]) -> tokio::process::Command {
    if cfg!(target_os = "windows") {
        // Build the full command string (command + args), pass directly to -Command.
        // tokio auto-quotes args with spaces via double quotes, which PowerShell
        // strips per -Command semantics, then executes the unquoted command.
        let mut shell_cmd = command.to_string();
        for arg in args {
            shell_cmd.push(' ');
            shell_cmd.push_str(arg);
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
