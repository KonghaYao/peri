use std::process::Stdio;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use peri_acp_types::tasks::TaskManager;
use peri_agent::agent::async_tasks::{
    bg_shell_task_id, drain_pipe, kill_process_group_escalating, parse_timeout, shell_command,
    truncate_bytes, BgTaskKind,
};
use peri_agent::{
    agent::events::BackgroundTaskResult, middleware::r#trait::Middleware, tools::BaseTool,
};
use serde_json::Value;
use tokio::time::{timeout, Duration};
use tracing::warn;

use crate::tools::output_persist::persist_truncated_output;

/// BashTool - 终端命令执行工具，与 TypeScript TerminalMiddleware 对齐
const BASH_DESCRIPTION: &str = include_str!("descriptions/bash.md");
pub struct BashTool {
    pub cwd: String,
    /// 后台任务管理器（Agent 层 per-session TaskManager；用于 run_in_background 模式）
    pub task_manager: Option<Arc<dyn TaskManager>>,
    /// bg shell 完成时的同步回调（在 registry.complete() 之前调用）。
    /// 第二参为任务 kind（bg shell 恒为 Shell，供 continuation scheduler 过滤）。
    pub on_bg_complete: Option<Arc<dyn Fn(&BackgroundTaskResult, BgTaskKind) + Send + Sync>>,
}

impl BashTool {
    pub fn new(cwd: impl Into<String>) -> Self {
        Self {
            cwd: cwd.into(),
            task_manager: None,
            on_bg_complete: None,
        }
    }

    pub fn with_task_manager(mut self, task_manager: Arc<dyn TaskManager>) -> Self {
        self.task_manager = Some(task_manager);
        self
    }

    pub fn with_on_bg_complete(
        mut self,
        cb: Arc<dyn Fn(&BackgroundTaskResult, BgTaskKind) + Send + Sync>,
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

/// 合并 stdout/stderr 为既有输出格式：stderr 加 `[stderr]` 前缀；
/// 非零退出码追加 `[Exit code: N]`；空输出时给占位说明。
/// `exit_code: None` 用于超时部分输出（进程未退出，退出码未知）。
fn merge_output(stdout: &str, stderr: &str, exit_code: Option<i32>) -> String {
    let mut output = String::new();
    if !stdout.is_empty() {
        output.push_str(stdout);
    }
    if !stderr.is_empty() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str("[stderr]\n");
        output.push_str(stderr);
    }
    if let Some(code) = exit_code {
        if code != 0 {
            output.push_str(&format!("\n[Exit code: {code}]"));
        }
    }
    if output.is_empty() {
        output = match exit_code {
            Some(code) => format!("[Command completed with exit code {code}]"),
            None => "[no output captured yet]".to_string(),
        };
    }
    output
}

/// 将超时前捕获的部分输出写入临时文件，返回提示字符串（含 "partial output" 字样）。
/// 文件路径：`{temp_dir}/peri-tool-output-{uuid}.txt`
fn persist_partial_output(output: &str) -> String {
    let id = uuid::Uuid::new_v4();
    let dir = std::env::temp_dir();
    let file_name = format!("peri-tool-output-{id}.txt");
    let file_path = dir.join(&file_name);

    match std::fs::write(&file_path, output) {
        Ok(_) => format!(
            "\n\n[Partial output saved to {} — use Read tool to view captured output so far]",
            file_path.display()
        ),
        Err(e) => format!(
            "\n\n[Failed to save partial output to {}: {e}]",
            file_path.display()
        ),
    }
}

#[async_trait::async_trait]
impl BaseTool for BashTool {
    fn name(&self) -> &str {
        "Bash"
    }

    fn is_direct(&self) -> bool {
        true
    }

    /// 执行类工具分组（design v2 §2.5.1：同类工具按 namespace 组织声明段）。
    fn namespace(&self) -> Option<&str> {
        Some("execution")
    }

    /// 提示词层声明模板（design v2 §2.5.3）。
    /// title 不覆盖——走 `BaseTool::tool_description` 默认路径由 name 推导。
    fn prompt_declaration(&self) -> Option<String> {
        Some(
            "Run a shell command → `{{name}}` ({{title}}). Prefer the purpose-built tools above when applicable: they give structured output and enforce permission rules."
                .to_string(),
        )
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
                    "description": "Optional timeout in milliseconds (default 15s for foreground; background tasks run until completion unless timeout is explicitly set; 0 = no timeout; max 600000). If the command takes longer than this, the entire process group is killed and a timeout error returned. For builds, installs, or tests, set a higher timeout (e.g. 300000 for 5 minutes) rather than automatically switching to background."
                },
                "run_in_background": {
                    "type": "boolean",
                    "description": "If true, runs the command in the background and returns immediately with a task_id. Only use for long-running servers, watchers, or daemons. For builds/installs/tests, prefer a longer timeout instead."
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
            // 任务发起（Agent 层 TaskManager::spawn_shell 承载实际执行：
            // 进程 spawn/进程组/超时/输出收集/注册/完成收尾全部在 Agent 层完成）。
            let task_manager = Arc::clone(self.task_manager.as_ref().ok_or(
                "run_in_background is not available: no background task manager configured",
            )?);

            // timeout 参数解析：未传/显式 0 → 不超时（后台语义：跑完为止）
            let timeout_opt = parse_timeout(&input, true);

            let handle = task_manager.spawn_shell(
                command.to_string(),
                self.cwd.clone(),
                timeout_opt,
                self.on_bg_complete.clone(),
            )?;

            let mut msg = format!(
                "Background shell task started.\ntask_id: {}\nThe command is running in the background.",
                handle.task_id
            );
            match handle.pid {
                Some(pid) => {
                    msg.push_str(&format!(
                        "\npid: {pid}\n\
                         - Kill it: run `kill {pid}` in another shell command (`kill -- -{pid}` kills the whole process group including child processes)\n\
                         - Live output: Read the log file {}",
                        handle
                            .stdout_log
                            .as_deref()
                            .unwrap_or("<unavailable>")
                    ));
                    if let Some(stderr_log) = handle.stderr_log.as_deref() {
                        msg.push_str(&format!(" (stderr: {stderr_log})"));
                    }
                    if handle.stdout_log.is_some() {
                        msg.push_str(
                            " — it appends while the command runs (use the Read tool to view)",
                        );
                    }
                    msg.push_str(
                        "\n- Monitor: check the Tasks panel for status and output preview; \
                         the captured output is returned via a completion notification when the task finishes",
                    );
                }
                None => msg.push_str(
                    "\n(process failed to spawn — a failure notification will arrive shortly)",
                ),
            }
            return Ok(msg);
        }

        // ── 同步执行路径 ──
        let timeout_opt = parse_timeout(&input, false);

        let mut cmd = shell_command(command, &[]);
        cmd.current_dir(&self.cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // 注意：不设 kill_on_drop——超时 promote 转后台时 child 不能被 drop 误杀
        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return Err(format!("Error executing command: {e}").into()),
        };
        let pid = child
            .id()
            .expect("shell_command spawn succeeded but child.id() is None");

        // 流式读取 stdout/stderr 到共享缓冲（超时时部分输出不再全丢）
        let stdout_buf = Arc::new(Mutex::new(String::new()));
        let stderr_buf = Arc::new(Mutex::new(String::new()));
        let stdout_reader =
            tokio::io::BufReader::new(child.stdout.take().expect("shell_command stdout is piped"));
        let stderr_reader =
            tokio::io::BufReader::new(child.stderr.take().expect("shell_command stderr is piped"));
        let drain_stdout = tokio::spawn(drain_pipe(stdout_reader, stdout_buf.clone()));
        let drain_stderr = tokio::spawn(drain_pipe(stderr_reader, stderr_buf.clone()));

        let wait_result = match timeout_opt {
            None => child.wait().await,
            Some(ms) => match timeout(Duration::from_millis(ms), child.wait()).await {
                Ok(status) => status,
                Err(_elapsed) => {
                    // ── 超时分支 ──
                    // 先捕获当前已产生的部分输出并落盘
                    let partial_stdout = match stdout_buf.lock() {
                        Ok(g) => g.clone(),
                        Err(poisoned) => poisoned.into_inner().clone(),
                    };
                    let partial_stderr = match stderr_buf.lock() {
                        Ok(g) => g.clone(),
                        Err(poisoned) => poisoned.into_inner().clone(),
                    };
                    let partial = merge_output(&partial_stdout, &partial_stderr, None);
                    let partial_hint = persist_partial_output(&partial);

                    if let Some(task_manager) = self.task_manager.as_ref() {
                        // ── 有 TaskManager：不杀进程，promote 为后台任务续跑 ──
                        let task_id = bg_shell_task_id();
                        let register_result =
                            task_manager.register(peri_acp_types::tasks::BgTaskRegistration {
                                task_id: task_id.clone(),
                                kind: BgTaskKind::Shell,
                                summary: command.chars().take(80).collect(),
                                pid: Some(pid),
                                kill: None,
                            });
                        match register_result {
                            Ok(()) => {
                                let task_manager = task_manager.clone();
                                let on_bg_complete_cb = self.on_bg_complete.clone();
                                let command_owned = command.to_string();
                                let task_id_owned = task_id.clone();
                                // 续跑任务：继续读 pipe 至 EOF → wait → finalize → 通知 Agent
                                tokio::spawn(async move {
                                    let started = std::time::Instant::now();
                                    let _ = drain_stdout.await;
                                    let _ = drain_stderr.await;
                                    // wait 失败（极罕见）按失败完成，保证 finalize 一定执行
                                    let (success, exit_code) = match child.wait().await {
                                        Ok(s) => (s.success(), s.code()),
                                        Err(e) => {
                                            warn!(
                                                error = %e,
                                                task_id = %task_id_owned,
                                                "promoted bg shell: wait failed"
                                            );
                                            (false, None)
                                        }
                                    };
                                    let stdout = match stdout_buf.lock() {
                                        Ok(g) => g.clone(),
                                        Err(poisoned) => poisoned.into_inner().clone(),
                                    };
                                    let stderr = match stderr_buf.lock() {
                                        Ok(g) => g.clone(),
                                        Err(poisoned) => poisoned.into_inner().clone(),
                                    };
                                    let combined = merge_output(&stdout, &stderr, exit_code);
                                    task_manager.finalize_bg_shell(
                                        &on_bg_complete_cb,
                                        task_id_owned,
                                        command_owned.chars().take(80).collect(),
                                        success,
                                        combined,
                                        started.elapsed().as_millis() as u64,
                                        false,
                                    );
                                });
                                return Err(format!(
                                    "Command timed out after {:.1}s; the process is now running as a background task.\ntask_id: {task_id}\npid: {pid}\nThe process is now running as a background task; you will be notified when it completes.\n- Kill it: run `kill {pid}` in another shell command (`kill -- -{pid}` kills the whole process group including child processes)\n{partial_hint}\nCommand that timed out: {command}",
                                    ms as f64 / 1000.0
                                )
                                .into());
                            }
                            Err(e) => {
                                // 注册失败（SHELL_LIMIT 满）→ 回退杀进程组路径
                                kill_process_group_escalating(pid);
                                return Err(format!(
                                    "Command timed out after {:.1}s and could not be promoted to a background task: {e}. The process group has been terminated.\n{partial_hint}\nCommand that timed out: {command}",
                                    ms as f64 / 1000.0
                                )
                                .into());
                            }
                        }
                    } else {
                        // ── 无 TaskManager：杀进程组 + 部分输出落盘 ──
                        kill_process_group_escalating(pid);
                        return Err(format!(
                            "Command timed out after {:.1}s. The default timeout is deliberately short (15s) to encourage efficient commands.\n\
                             Options:\n\
                             - Optimize the command: avoid scanning large directories (e.g. use `find . -maxdepth 3` instead of `find /Users/...`), add `| head`, or use fd/rg instead of find/grep.\n\
                             - Increase timeout: set `timeout` parameter to a larger value (e.g. `timeout: 120000` for 2 minutes).\n\
                             - Use background mode: set `run_in_background: true` for long-running servers/builds/installs.\n\
                             {partial_hint}\n\
                             Command that timed out: {command}",
                            ms as f64 / 1000.0
                        )
                        .into());
                    }
                }
            },
        };

        // 正常完成路径：等待管道排空，合并输出，保持既有格式与截断逻辑
        match wait_result {
            Ok(status) => {
                let _ = drain_stdout.await;
                let _ = drain_stderr.await;
                let stdout = match stdout_buf.lock() {
                    Ok(g) => g.clone(),
                    Err(poisoned) => poisoned.into_inner().clone(),
                };
                let stderr = match stderr_buf.lock() {
                    Ok(g) => g.clone(),
                    Err(poisoned) => poisoned.into_inner().clone(),
                };
                let output = merge_output(&stdout, &stderr, status.code());
                Ok(truncate_output(&output))
            }
            Err(e) => Err(format!("Error executing command: {e}").into()),
        }
    }

    fn output_char_limit(&self) -> Option<usize> {
        Some(10000)
    }
}

/// TerminalMiddleware - 与 TypeScript TerminalMiddleware 对齐
pub struct TerminalMiddleware {
    task_manager: Option<Arc<dyn TaskManager>>,
    on_bg_complete: Option<Arc<dyn Fn(&BackgroundTaskResult, BgTaskKind) + Send + Sync>>,
}

impl TerminalMiddleware {
    pub fn new() -> Self {
        Self {
            task_manager: None,
            on_bg_complete: None,
        }
    }

    pub fn with_task_manager(mut self, task_manager: Arc<dyn TaskManager>) -> Self {
        self.task_manager = Some(task_manager);
        self
    }

    pub fn with_on_bg_complete(
        mut self,
        cb: Arc<dyn Fn(&BackgroundTaskResult, BgTaskKind) + Send + Sync>,
    ) -> Self {
        self.on_bg_complete = Some(cb);
        self
    }

    pub fn build_tools(cwd: &str) -> Vec<Box<dyn BaseTool>> {
        vec![Box::new(BashTool::new(cwd))]
    }

    pub fn build_tools_with_registry(
        cwd: &str,
        task_manager: Option<Arc<dyn TaskManager>>,
    ) -> Vec<Box<dyn BaseTool>> {
        vec![Box::new(BashTool {
            cwd: cwd.to_string(),
            task_manager,
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
            task_manager: self.task_manager.clone(),
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
