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
const BASH_DESCRIPTION: &str = include_str!("descriptions/bash.md");
pub struct BashTool {
    pub cwd: String,
    /// 后台任务注册表（用于 run_in_background 模式）
    pub bg_registry: Option<Arc<BackgroundTaskRegistry>>,
    /// bg shell 完成时的同步回调（在 registry.complete() 之前调用）
    pub on_bg_complete:
        Option<Arc<dyn Fn(&BackgroundTaskResult) + Send + Sync>>,
}

impl BashTool {
    pub fn new(cwd: impl Into<String>) -> Self {
        Self {
            cwd: cwd.into(),
            bg_registry: None,
            on_bg_complete: None,
        }
    }

    pub fn with_registry(mut self, registry: Arc<BackgroundTaskRegistry>) -> Self {
        self.bg_registry = Some(registry);
        self
    }

    pub fn with_on_bg_complete(
        mut self,
        cb: Arc<dyn Fn(&BackgroundTaskResult) + Send + Sync>,
    ) -> Self {
        self.on_bg_complete = Some(cb);
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

    fn is_direct(&self) -> bool {
        true
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
                    "description": "Optional timeout in milliseconds (default 15000, max 600000). If the command takes longer than this, it will be killed and a timeout error returned. The default is deliberately short — prefer targeted commands. For long-running tasks (builds, installs), set a higher timeout or use run_in_background."
                },
                "run_in_background": {
                    "type": "boolean",
                    "description": "If true, runs the command in the background and returns immediately with a task_id. Use for long-running servers (dev server, watcher, etc.). The task can be monitored in the Tasks panel."
                }
            },
            "required": ["command"]
        })
    }

    fn aliases(&self) -> &[&str] {
        &["Shell"]
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
            let registry = Arc::clone(
                self.bg_registry
                    .as_ref()
                    .ok_or("run_in_background is not available: no background task registry configured")?,
            );

            // timeout 参数解析（与同步 Bash 对齐，含 clamp）
            let timeout_ms: u64 = input["timeout"]
                .as_u64()
                .unwrap_or(15_000)
                .clamp(if cfg!(target_os = "windows") { 5000 } else { 1 }, 600_000);

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
            let on_bg_complete_cb = self.on_bg_complete.clone();
            let task_id_for_return = task_id.clone();

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
                        let result = BackgroundTaskResult {
                            task_id: task_id.clone(),
                            agent_name: "bg-shell".to_string(),
                            prompt_summary: command_owned.chars().take(80).collect(),
                            success: false,
                            output: format!("Failed to spawn: {}", e),
                            tool_calls_count: 0,
                            duration_ms: started.elapsed().as_millis() as u64,
                            child_thread_id: None,
                        };
                        // 回调通知 Agent inbox（在 registry 操作之前）
                        if let Some(ref cb) = on_bg_complete_cb {
                            cb(&result);
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
                            pid: None,
                            output_preview: None,
                        };
                        let _ = registry.register_with_kind(bg_task);
                        let complete_task_id = result.task_id.clone();
                        registry.complete(&complete_task_id, result);
                        return;
                    }
                };
                let pid = child.id();

                // 超时包裹 wait_with_output
                let wait_future = child.wait_with_output();
                let wait_result = if timeout_ms == 0 {
                    // 无超时：兼容长期运行的服务器/构建场景
                    wait_future.await.map(Some)
                } else {
                    match tokio::time::timeout(
                        Duration::from_millis(timeout_ms),
                        wait_future,
                    )
                    .await
                    {
                        Ok(output_result) => output_result.map(Some),
                        Err(_elapsed) => {
                            // 超时：显式 kill 子进程（通过 pid，因 wait_with_output() 已 moved child）
                            // 与 background.rs:294 的 cancel() 对齐——使用 kill 命令发送 SIGTERM
                            let _ = std::process::Command::new("kill")
                                .arg("-TERM")
                                .arg(pid.unwrap_or(0).to_string())
                                .spawn();
                            // 构造超时错误结果
                            let result = BackgroundTaskResult {
                                task_id: task_id.clone(),
                                agent_name: "bg-shell".to_string(),
                                prompt_summary: command_owned.chars().take(80).collect(),
                                success: false,
                                output: format!(
                                    "Command timed out after {}s.\nCommand: {}",
                                    timeout_ms as f64 / 1000.0,
                                    command_owned
                                ),
                                tool_calls_count: 0,
                                duration_ms: started.elapsed().as_millis() as u64,
                                child_thread_id: None,
                            };
                            // 回调通知 Agent inbox（在 registry 操作之前）
                            if let Some(ref cb) = on_bg_complete_cb {
                                cb(&result);
                            }
                            let bg_task = BackgroundTask {
                                id: result.task_id.clone(),
                                agent_name: "bg-shell".to_string(),
                                prompt_summary: command_owned.chars().take(80).collect(),
                                status: BackgroundTaskStatus::Running,
                                started_at: std::time::Instant::now(),
                                chrono_started_at: chrono::Utc::now(),
                                kind: BgTaskKind::Shell,
                                cancel_handle: BgCancelHandle::Pid(
                        pid.expect("bg shell: child.id() returned None after successful spawn"),
                    ),
                                pid,
                                output_preview: None,
                            };
                            if let Err(e) = registry.register_with_kind(bg_task) {
                                warn!(error = %e, task_id = %result.task_id, "bg shell timeout: register_with_kind failed");
                            }
                            let complete_task_id = result.task_id.clone();
                            registry.complete(&complete_task_id, result);
                            return;
                        }
                    }
                };

                let output = match wait_result {
                    Ok(Some(out)) => {
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
                        // 输出超长落盘（>100K 字符时截断 + 持久化完整内容到磁盘）
                        const BG_OUTPUT_TRUNC_THRESHOLD: usize = 100_000;
                        let output_str = if combined.len() > BG_OUTPUT_TRUNC_THRESHOLD {
                            let persist_hint = persist_truncated_output(&combined);
                            let truncated = truncate_bytes(&combined, BG_OUTPUT_TRUNC_THRESHOLD);
                            format!("{}{}", truncated, persist_hint)
                        } else {
                            combined
                        };
                        BackgroundTaskResult {
                            task_id: task_id.clone(),
                            agent_name: "bg-shell".to_string(),
                            prompt_summary: command_owned.chars().take(80).collect(),
                            success,
                            output: output_str,
                            tool_calls_count: 0,
                            duration_ms: started.elapsed().as_millis() as u64,
                            child_thread_id: None,
                        }
                    }
                    Err(e) => BackgroundTaskResult {
                        task_id: task_id.clone(),
                        agent_name: "bg-shell".to_string(),
                        prompt_summary: command_owned.chars().take(80).collect(),
                        success: false,
                        output: format!("Command failed: {}", e),
                        tool_calls_count: 0,
                        duration_ms: started.elapsed().as_millis() as u64,
                        child_thread_id: None,
                    },
                    // unreachable: wait_with_output() always returns Ok(Output)
                    Ok(None) => unreachable!("bg shell: wait_with_output returned Ok(None)"),
                };

                // 回调通知 Agent inbox（在 registry.complete() 之前，与 execute_bg.rs 对齐）
                if let Some(ref cb) = on_bg_complete_cb {
                    cb(&output);
                }

                // 注册任务（向 registry 提供 pid 用于取消）
                let bg_task = BackgroundTask {
                    id: output.task_id.clone(),
                    agent_name: "bg-shell".to_string(),
                    prompt_summary: command_owned.chars().take(80).collect(),
                    status: BackgroundTaskStatus::Running,
                    started_at: std::time::Instant::now(),
                    chrono_started_at: chrono::Utc::now(),
                    kind: BgTaskKind::Shell,
                    cancel_handle: BgCancelHandle::Pid(
                        pid.expect("bg shell: child.id() returned None after successful spawn"),
                    ),
                    pid,
                    output_preview: None,
                };
                if let Err(e) = registry.register_with_kind(bg_task) {
                    // register 失败时仅回调已完成，不再调 complete()（review M-4:
                    // complete() 的 registry 状态更新是 no-op，语义模糊，回调已正确触发）
                    warn!(error = %e, task_id = %output.task_id, "bg shell: register_with_kind failed (callback already fired)");
                    return;
                }
                let complete_task_id = output.task_id.clone();
                registry.complete(&complete_task_id, output);
            });

            return Ok(format!(
                "Background shell task started.\ntask_id: {}\nThe command is running in the background. Monitor in the Tasks panel.",
                task_id_for_return
            ));
        }

        // ── 同步执行路径（现有逻辑不变） ──
        let timeout_ms = input["timeout"]
            .as_u64()
            .unwrap_or(15_000)
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
                "Command timed out after {}s. The default timeout is deliberately short (15s) to encourage efficient commands.\n\
                 Options:\n\
                 - Optimize the command: avoid scanning large directories (e.g. use `find . -maxdepth 3` instead of `find /Users/...`), add `| head`, or use fd/rg instead of find/grep.\n\
                 - Increase timeout: set `timeout` parameter to a larger value (e.g. `timeout: 120000` for 2 minutes).\n\
                 - Use background mode: set `run_in_background: true` for long-running servers/builds/installs.\n\
                 Command that timed out: {command}",
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
    on_bg_complete:
        Option<Arc<dyn Fn(&BackgroundTaskResult) + Send + Sync>>,
}

impl TerminalMiddleware {
    pub fn new() -> Self {
        Self {
            bg_registry: None,
            on_bg_complete: None,
        }
    }

    pub fn with_registry(mut self, registry: Arc<BackgroundTaskRegistry>) -> Self {
        self.bg_registry = Some(registry);
        self
    }

    pub fn with_on_bg_complete(
        mut self,
        cb: Arc<dyn Fn(&BackgroundTaskResult) + Send + Sync>,
    ) -> Self {
        self.on_bg_complete = Some(cb);
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
            on_bg_complete: None,
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
            on_bg_complete: self.on_bg_complete.clone(),
        })]
    }

    fn name(&self) -> &str {
        "TerminalMiddleware"
    }
}

#[cfg(test)]
#[path = "terminal_test.rs"]
mod tests;
