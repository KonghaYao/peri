use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use peri_agent::{
    agent::events::BackgroundTaskResult, middleware::r#trait::Middleware, tools::BaseTool,
};
use serde_json::Value;
use tokio::time::{timeout, Duration};
use tracing::warn;

use crate::subagent::{
    BackgroundTask, BackgroundTaskRegistry, BackgroundTaskStatus, BgCancelHandle, BgTaskKind,
};
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
- Output exceeding 65000 bytes is truncated
- Non-zero exit codes are reported
- Both stdout and stderr are captured"#;
pub struct BashTool {
    pub cwd: String,
    /// 后台任务注册表（用于 run_in_background 模式）
    pub bg_registry: Option<Arc<BackgroundTaskRegistry>>,
}

impl BashTool {
    pub fn new(cwd: impl Into<String>) -> Self {
        Self {
            cwd: cwd.into(),
            bg_registry: None,
        }
    }

    pub fn with_registry(mut self, registry: Arc<BackgroundTaskRegistry>) -> Self {
        self.bg_registry = Some(registry);
        self
    }
}

/// 输出最大字节数
const MAX_OUTPUT_CHARS: usize = 65_000;
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
                },
                "run_in_background": {
                    "type": "boolean",
                    "description": "If true, runs the command in the background and returns immediately with a task_id. Use for long-running servers (dev server, watcher, etc.). The task can be monitored in the Tasks panel."
                }
            },
            "required": ["command"]
        })
    }

    fn timeout(&self) -> Option<std::time::Duration> {
        None
    }

    async fn invoke(
        &self,
        input: Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let command = input["command"]
            .as_str()
            .ok_or("Missing command parameter")?;

        // ── 后台执行路径 ──
        let run_in_background = input["run_in_background"].as_bool().unwrap_or(false);
        if run_in_background {
            let registry = self.bg_registry.as_ref().ok_or(
                "run_in_background is not available: no background task registry configured",
            )?;

            let count = registry.count_by_kind(BgTaskKind::Shell);
            if count >= BackgroundTaskRegistry::SHELL_LIMIT {
                return Err(format!(
                    "已达到 shell 后台任务并发上限 ({}/{})，请等待现有任务完成",
                    count,
                    BackgroundTaskRegistry::SHELL_LIMIT
                )
                .into());
            }

            let task_id = format!(
                "shell-{}",
                uuid::Uuid::now_v7()
                    .to_string()
                    .chars()
                    .take(8)
                    .collect::<String>()
            );
            let command_owned = command.to_string();
            let cwd = self.cwd.clone();
            let registry_clone = Arc::clone(registry);
            let task_id_clone = task_id.clone();

            tokio::spawn(async move {
                let started = std::time::Instant::now();
                let mut cmd = crate::process::shell_command(&command_owned, &[]);
                cmd.current_dir(&cwd)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .kill_on_drop(true);
                #[cfg(unix)]
                cmd.process_group(0);

                let child = cmd.spawn();
                let child = match child {
                    Ok(c) => c,
                    Err(e) => {
                        let _task_id = task_id_clone.clone();
                        let result = BackgroundTaskResult {
                            task_id: task_id_clone.clone(),
                            agent_name: "bg-shell".to_string(),
                            prompt_summary: command_owned.chars().take(80).collect(),
                            success: false,
                            output: format!("Failed to spawn: {}", e),
                            tool_calls_count: 0,
                            duration_ms: started.elapsed().as_millis() as u64,
                            child_thread_id: None,
                        };
                        // 注册 + 立即完成
                        let bg_task = BackgroundTask {
                            id: result.task_id.clone(),
                            agent_name: "bg-shell".to_string(),
                            prompt_summary: command_owned.chars().take(80).collect(),
                            status: BackgroundTaskStatus::Running,
                            started_at: std::time::Instant::now(),
                            kind: BgTaskKind::Shell,
                            cancel_handle: BgCancelHandle::Pid(0),
                            pid: None,
                            output_preview: None,
                        };
                        let _ = registry_clone.register_with_kind(bg_task);
                        let complete_task_id = result.task_id.clone();
                        registry_clone.complete(&complete_task_id, result);
                        return;
                    }
                };
                let pid = child.id();

                let output = match child.wait_with_output().await {
                    Ok(out) => {
                        let success = out.status.success();
                        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
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
                            combined = format!("[exit code: {}]", out.status.code().unwrap_or(-1));
                        }
                        BackgroundTaskResult {
                            task_id: task_id_clone.clone(),
                            agent_name: "bg-shell".to_string(),
                            prompt_summary: command_owned.chars().take(80).collect(),
                            success,
                            output: combined,
                            tool_calls_count: 0,
                            duration_ms: started.elapsed().as_millis() as u64,
                            child_thread_id: None,
                        }
                    }
                    Err(e) => BackgroundTaskResult {
                        task_id: task_id_clone.clone(),
                        agent_name: "bg-shell".to_string(),
                        prompt_summary: command_owned.chars().take(80).collect(),
                        success: false,
                        output: format!("Command failed: {}", e),
                        tool_calls_count: 0,
                        duration_ms: started.elapsed().as_millis() as u64,
                        child_thread_id: None,
                    },
                };

                // 注册任务（向 registry 提供 pid 用于取消）
                let bg_task = BackgroundTask {
                    id: output.task_id.clone(),
                    agent_name: "bg-shell".to_string(),
                    prompt_summary: command_owned.chars().take(80).collect(),
                    status: BackgroundTaskStatus::Running,
                    started_at: std::time::Instant::now(),
                    kind: BgTaskKind::Shell,
                    cancel_handle: BgCancelHandle::Pid(pid.unwrap_or(0)),
                    pid,
                    output_preview: None,
                };
                if let Err(e) = registry_clone.register_with_kind(bg_task) {
                    warn!(error = %e, task_id = %output.task_id, "bg shell: register_with_kind failed");
                    return;
                }
                let complete_task_id = output.task_id.clone();
                registry_clone.complete(&complete_task_id, output);
            });

            return Ok(format!(
                "Background shell task started.\ntask_id: {}\nThe command is running in the background. Monitor in the Tasks panel.",
                task_id
            ));
        }

        // ── 同步执行路径（现有逻辑不变） ──
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

    fn output_char_limit(&self) -> Option<usize> {
        Some(10000)
    }
}

/// TerminalMiddleware - 与 TypeScript TerminalMiddleware 对齐
pub struct TerminalMiddleware {
    bg_registry: Option<Arc<BackgroundTaskRegistry>>,
}

impl TerminalMiddleware {
    pub fn new() -> Self {
        Self { bg_registry: None }
    }

    pub fn with_registry(mut self, registry: Arc<BackgroundTaskRegistry>) -> Self {
        self.bg_registry = Some(registry);
        self
    }

    pub fn build_tools(cwd: &str) -> Vec<Box<dyn BaseTool>> {
        vec![Box::new(BashTool::new(cwd))]
    }

    pub fn build_tools_with_registry(
        cwd: &str,
        registry: Option<Arc<BackgroundTaskRegistry>>,
    ) -> Vec<Box<dyn BaseTool>> {
        vec![Box::new(BashTool {
            cwd: cwd.to_string(),
            bg_registry: registry,
        })]
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
impl Middleware for TerminalMiddleware {
    fn collect_tools(&self, cwd: &str) -> Vec<Box<dyn BaseTool>> {
        vec![Box::new(BashTool {
            cwd: cwd.to_string(),
            bg_registry: self.bg_registry.clone(),
        })]
    }

    fn name(&self) -> &str {
        "TerminalMiddleware"
    }
}

#[cfg(test)]
#[path = "terminal_test.rs"]
mod tests;
