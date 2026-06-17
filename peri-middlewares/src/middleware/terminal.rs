use std::process::Stdio;

use async_trait::async_trait;
use peri_agent::{agent::state::State, middleware::r#trait::Middleware, tools::BaseTool};
use serde_json::Value;
use tokio::time::{timeout, Duration};

use crate::tools::output_persist::persist_truncated_output;
use crate::tools::output_truncate::truncate_bytes;

/// BashTool - 终端命令执行工具，与 TypeScript TerminalMiddleware 对齐
const BASH_DESCRIPTION: &str = r#"Executes a given shell command and returns its output.

Usage:
- The working directory persists between commands, but shell state does not. The shell environment is initialized from the user's profile (bash or zsh)
- IMPORTANT: Avoid using this tool to run find, grep, cat, head, tail, sed, awk, or echo commands, unless explicitly instructed or after you have verified that a dedicated tool cannot accomplish your task
- Instead, use the appropriate dedicated tool which will provide a much better experience for the user:
  - File search: Use Glob (NOT find or ls)
  - Content search: Use Grep (NOT grep or rg)
  - Read files: Use Read (NOT cat/head/tail)
  - Edit files: Use Edit (NOT sed/awk)
  - Write files: Use Write (NOT echo/cat with redirect)
- You can specify an optional timeout in milliseconds (up to 600000ms / 10 minutes). Default is 120000ms (2 minutes)
- When issuing multiple commands, use && to chain them together rather than using separate tool calls if the commands depend on each other
- For long running commands, consider using a timeout to avoid waiting indefinitely

Platform behavior:
- Windows: uses powershell -NoProfile -NoLogo -NonInteractive -Command to execute commands
- Unix/macOS: uses bash -c to execute commands
- On Unix, child processes run in their own process group; timeout kills the entire process tree
- On Windows, timeout only terminates the PowerShell wrapper; child processes (including peri) are NOT killed

Output handling:
- Output exceeding 2000 lines is truncated (head + tail preserved)
- Output exceeding 100000 bytes is truncated
- Non-zero exit codes are reported
- Both stdout and stderr are captured"#;
pub struct BashTool {
    pub cwd: String,
}

impl BashTool {
    pub fn new(cwd: impl Into<String>) -> Self {
        Self { cwd: cwd.into() }
    }
}

/// 输出最大字节数
const MAX_OUTPUT_CHARS: usize = 100_000;
/// 输出最大行数（在第 N 行截断后，若还有行数超过上限再截字节）
const MAX_OUTPUT_LINES: usize = 2_000;

fn truncate_output(output: &str) -> String {
    let lines: Vec<&str> = output.split('\n').collect();
    if lines.len() > MAX_OUTPUT_LINES {
        let total_lines = lines.len();
        // Persist full content before truncating
        let persist_hint = persist_truncated_output(output);
        let head_count = MAX_OUTPUT_LINES / 2;
        let tail_count = MAX_OUTPUT_LINES - head_count;
        let head: Vec<&str> = lines.iter().take(head_count).copied().collect();
        let tail: Vec<&str> = lines
            .iter()
            .skip(total_lines - tail_count)
            .copied()
            .collect();
        let mut result = head.join("\n");
        result.push_str(&format!(
            "\n\n... [{} lines truncated, showing head {} and tail {} of {} total lines] ...\n\n",
            total_lines - MAX_OUTPUT_LINES,
            head_count,
            tail_count,
            total_lines
        ));
        result.push_str(&tail.join("\n"));
        result.push_str(&persist_hint);
        // Check byte limit after adding hint
        if result.len() > MAX_OUTPUT_CHARS {
            let truncated = truncate_bytes(&result, MAX_OUTPUT_CHARS);
            return format!(
                "{}\n\n[Output truncated: exceeds {} byte limit]{}",
                truncated, MAX_OUTPUT_CHARS, persist_hint
            );
        }
        return result;
    }
    if output.len() > MAX_OUTPUT_CHARS {
        let persist_hint = persist_truncated_output(output);
        let truncated = truncate_bytes(output, MAX_OUTPUT_CHARS);
        return format!(
            "{}\n\n[Output truncated: exceeds {} byte limit]{}",
            truncated, MAX_OUTPUT_CHARS, persist_hint
        );
    }
    output.to_string()
}

#[async_trait::async_trait]
impl BaseTool for BashTool {
    fn name(&self) -> &str {
        "Bash"
    }

    fn description(&self) -> &str {
        BASH_DESCRIPTION
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The bash command (and optional arguments) to execute. This can be complex commands that use pipes, &&, or other shell features. For multiple dependent commands, chain them with && rather than making separate calls"
                },
                "timeout": {
                    "type": "number",
                    "description": "Optional timeout in milliseconds (default 120000, max 600000). If the command takes longer than this, it will be killed and a timeout error returned"
                }
            },
            "required": ["command"]
        })
    }

    async fn invoke(
        &self,
        input: Value,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let command = input["command"]
            .as_str()
            .ok_or("Missing command parameter")?;

        let timeout_ms = input["timeout"]
            .as_u64()
            .unwrap_or(120_000)
            .clamp(if cfg!(target_os = "windows") { 5000 } else { 1 }, 600_000);

        let result = timeout(Duration::from_millis(timeout_ms), {
            let mut cmd = crate::process::shell_command(command, &[]);
            cmd.current_dir(&self.cwd)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            #[cfg(unix)]
            cmd.process_group(0);
            cmd.output()
        })
        .await;

        match result {
            Err(_) => Err(format!(
                "Error: Command timed out after {} seconds.\nCommand: {command}",
                timeout_ms as f64 / 1000.0
            )
            .into()),
            Ok(Err(e)) => Err(format!("Error executing command: {e}").into()),
            Ok(Ok(out)) => {
                let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                let exit_code = out.status.code().unwrap_or(-1);

                let mut output = String::new();

                if !stdout.is_empty() {
                    output.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !output.is_empty() {
                        output.push('\n');
                    }
                    output.push_str("[stderr]\n");
                    output.push_str(&stderr);
                }
                if exit_code != 0 {
                    output.push_str(&format!("\n[Exit code: {exit_code}]"));
                }

                if output.is_empty() {
                    output = format!("[Command completed with exit code {exit_code}]");
                }

                // 截断过长输出，防止撑爆 LLM context window
                Ok(truncate_output(&output))
            }
        }
    }
}

/// TerminalMiddleware - 与 TypeScript TerminalMiddleware 对齐
pub struct TerminalMiddleware;

impl TerminalMiddleware {
    pub fn new() -> Self {
        Self
    }

    pub fn build_tools(cwd: &str) -> Vec<Box<dyn BaseTool>> {
        vec![Box::new(BashTool::new(cwd))]
    }

    pub fn tool_names() -> Vec<&'static str> {
        vec!["Bash"]
    }
}

impl Default for TerminalMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl<S: State> Middleware<S> for TerminalMiddleware {
    fn collect_tools(&self, cwd: &str) -> Vec<Box<dyn BaseTool>> {
        Self::build_tools(cwd)
    }

    fn name(&self) -> &str {
        "TerminalMiddleware"
    }
}

#[cfg(test)]
#[path = "terminal_test.rs"]
mod tests;
