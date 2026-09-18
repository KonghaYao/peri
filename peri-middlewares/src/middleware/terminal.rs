use std::process::Stdio;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use peri_acp_types::tasks::TaskManager;
use peri_agent::agent::async_tasks::{
    bg_shell_task_id, kill_process_group, parse_timeout, shell_command, tee_pipe_with_output,
    truncate_bytes, BgTaskKind, ShellExecutionGuard, ShellOutputCapture,
};
use peri_agent::{
    agent::events::BackgroundTaskResult,
    middleware::r#trait::Middleware,
    tools::{BaseTool, ToolExecutionEvidence, ToolExecutionStatus, ToolOutput},
};
use serde_json::Value;
use tokio::time::{timeout, Duration};
use tracing::warn;

use crate::tools::output_persist::persist_truncated_output_with_ref;

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

#[cfg(test)]
fn truncate_output(output: &str) -> String {
    truncate_output_with_ref(output).0
}

fn truncate_output_with_ref(output: &str) -> (String, Option<String>) {
    let lines: Vec<&str> = output.split('\n').collect();
    if lines.len() > MAX_OUTPUT_LINES {
        let total_lines = lines.len();
        // Persist full content before truncating
        let (persist_hint, output_ref) = persist_truncated_output_with_ref(output);
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
            return (
                format!(
                    "{}\n\n[Output truncated: exceeds {} byte limit]{}",
                    truncated,
                    MAX_OUTPUT_CHARS,
                    persist_hint.as_str()
                ),
                output_ref,
            );
        }
        return (result, output_ref);
    }
    if output.len() > MAX_OUTPUT_CHARS {
        let (persist_hint, output_ref) = persist_truncated_output_with_ref(output);
        let truncated = truncate_bytes(output, MAX_OUTPUT_CHARS);
        return (
            format!(
                "{}\n\n[Output truncated: exceeds {} byte limit]{}",
                truncated,
                MAX_OUTPUT_CHARS,
                persist_hint.as_str()
            ),
            output_ref,
        );
    }
    (output.to_string(), None)
}

/// Bash declares a 10k character model projection. Persist the source before
/// applying that final bound, including outputs that are below the historical
/// 65k/2000-line middleware threshold.
fn bounded_bash_output(output: &str) -> (String, Option<String>) {
    const BASH_OUTPUT_LIMIT: usize = 10_000;
    // Reserve room for the typed execution summary appended by canonical
    // dispatch. This keeps near-limit outputs from being truncated a second
    // time after Bash has already decided whether to persist the source.
    const EXECUTION_SUMMARY_BUDGET: usize = 512;
    if output.chars().count() <= BASH_OUTPUT_LIMIT - EXECUTION_SUMMARY_BUDGET {
        return truncate_output_with_ref(output);
    }
    let (hint, output_ref) = persist_truncated_output_with_ref(output);
    let marker = format!("\n\n[Output truncated at {BASH_OUTPUT_LIMIT} chars]");
    let content_limit = BASH_OUTPUT_LIMIT
        .saturating_sub(marker.chars().count() + hint.chars().count() + EXECUTION_SUMMARY_BUDGET);
    let truncated = truncate_bytes(output, content_limit);
    (format!("{truncated}{marker}{hint}"), output_ref)
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
fn persist_partial_output_with_ref(output: &str) -> (String, Option<String>) {
    let id = uuid::Uuid::new_v4();
    let dir = std::env::temp_dir();
    let file_name = format!("peri-tool-output-{id}.txt");
    let file_path = dir.join(&file_name);

    match std::fs::write(&file_path, output) {
        Ok(_) => (
            format!(
                "\n\n[Partial output saved to {} — use Read tool to view captured output so far]",
                file_path.display()
            ),
            Some(file_path.to_string_lossy().into_owned()),
        ),
        Err(e) => (
            format!(
                "\n\n[Failed to save partial output to {}: {e}]",
                file_path.display()
            ),
            None,
        ),
    }
}

/// 超时时刻采集进程状态快照（`ps -o pid,stat,etime,command`），供诊断定位。
/// 非 Unix 或 ps 不可用时返回 None（降级：文案中省略该行）。
fn process_status_snapshot(pid: u32) -> Option<String> {
    #[cfg(unix)]
    {
        let out = std::process::Command::new("ps")
            .args(["-o", "pid=,stat=,etime=,command=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
    #[cfg(not(unix))]
    {
        // Windows 无 ps；显式消费 pid 消除 unused_variable（-D warnings）。
        let _ = pid;
        None
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
                    "description": "Optional timeout in milliseconds (default 15s for foreground; no default timeout for explicit background tasks; 0 = no timeout; max 600000). Foreground timeout returns an error: if background task registration succeeds, the process continues in the background with a task_id and pid, without a new timeout; otherwise termination is requested. A positive timeout on an explicit background task requests termination when reached. Check the returned process status before retrying; do not duplicate a task that is still running. For builds, installs, or tests, set a higher timeout (e.g. 300000 for 5 minutes)."
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

    async fn invoke_output(
        &self,
        input: Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<ToolOutput, Box<dyn std::error::Error + Send + Sync>> {
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

            let command = command.to_string();
            let cwd = self.cwd.clone();
            let on_complete = self.on_bg_complete.clone();
            // The synchronous port returns a PID and already-created output
            // paths. Keep its process/file setup off the async runtime.
            let handle = tokio::task::spawn_blocking(move || {
                task_manager.spawn_shell(command, cwd, timeout_opt, on_complete)
            })
            .await??;

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
                         completion notifications include output file paths for reading on demand",
                    );
                }
                None => msg.push_str(
                    "\n(process failed to spawn — a failure notification will arrive shortly)",
                ),
            }
            return Ok(ToolOutput::with_execution(
                msg,
                ToolExecutionEvidence {
                    status: if handle.pid.is_some() {
                        ToolExecutionStatus::Running
                    } else {
                        ToolExecutionStatus::Unknown
                    },
                    exit_code: None,
                    output_ref: None,
                    output_truncated: false,
                    task_id: Some(handle.task_id),
                },
            ));
        }

        // ── 同步执行路径 ──
        let timeout_opt = parse_timeout(&input, false);
        let ownership = self
            .task_manager
            .as_ref()
            .map(|manager| manager.begin_external_execution())
            .transpose()?;
        let mut execution = ShellExecutionGuard::new(ownership);

        let mut cmd = shell_command(command, &[]);
        cmd.current_dir(&self.cwd)
            // stdin 重定向为 null：Bash 工具是非交互执行，命令不应依赖终端输入。
            // 否则读 stdin 的进程（交互式命令、stdio 服务）会永远阻塞等待 EOF，
            // 表现为挂死到超时；null 使它们立即 EOF 快速失败，错误立刻可见。
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Promotion moves the Child; cancellation drops it and must stop the leader.
        cmd.kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);

        execution.prepare(&mut cmd)?;
        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return Err(format!("Error executing command: {e}").into()),
        };
        let pid = child
            .id()
            .expect("shell_command spawn succeeded but child.id() is None");
        if let Err(error) = execution.attach_owned(child) {
            let _ = execution.child_mut().kill().await;
            execution.confirm_stopped();
            return Err(error.into());
        }

        // Start durable capture before either pipe reader is spawned. A
        // timeout promotion can therefore retain output beyond the bounded
        // in-memory preview without reconstructing it after the fact.
        let mut output_capture =
            tokio::task::spawn_blocking(|| ShellOutputCapture::new("foreground-shell")).await?;
        let stdout_writer = output_capture.stdout_writer();
        let stderr_writer = output_capture.stderr_writer();
        let output_capture = Arc::new(output_capture);

        // 流式读取 stdout/stderr 到共享缓冲（超时时部分输出不再全丢）
        let stdout_buf = Arc::new(Mutex::new(String::new()));
        let stderr_buf = Arc::new(Mutex::new(String::new()));
        let child = execution.child_mut();
        let stdout_reader =
            tokio::io::BufReader::new(child.stdout.take().expect("shell_command stdout is piped"));
        let stderr_reader =
            tokio::io::BufReader::new(child.stderr.take().expect("shell_command stderr is piped"));
        let drain_stdout = tokio::spawn(tee_pipe_with_output(
            stdout_reader,
            stdout_buf.clone(),
            stdout_writer,
            Arc::clone(&output_capture),
            "stdout",
        ));
        let drain_stderr = tokio::spawn(tee_pipe_with_output(
            stderr_reader,
            stderr_buf.clone(),
            stderr_writer,
            Arc::clone(&output_capture),
            "stderr",
        ));

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
                    let (partial_hint, partial_ref) = persist_partial_output_with_ref(&partial);
                    // 诊断信号：进程是否产生过任何输出（区分"慢但活跃"与"挂起/无进展"）
                    let has_output = !partial_stdout.is_empty() || !partial_stderr.is_empty();
                    let ps_line = process_status_snapshot(pid)
                        .map(|s| format!("Process state: {s}"))
                        .unwrap_or_else(|| "Process state: unavailable".to_string());

                    if let Some(task_manager) = self.task_manager.as_ref() {
                        // ── 有 TaskManager：不杀进程，promote 为后台任务续跑 ──
                        let task_id = bg_shell_task_id();
                        let register_result =
                            task_manager.register(peri_acp_types::tasks::BgTaskRegistration {
                                task_id: task_id.clone(),
                                kind: BgTaskKind::Shell,
                                summary: command.chars().take(80).collect(),
                                pid: Some(pid),
                                kill: execution.cancel_callback(),
                            });
                        match register_result {
                            Ok(()) => {
                                let task_manager = task_manager.clone();
                                execution.track_registration(task_manager.clone(), task_id.clone());
                                let on_bg_complete_cb = self.on_bg_complete.clone();
                                let command_owned = command.to_string();
                                let task_id_owned = task_id.clone();
                                let background_output = Arc::clone(&output_capture);
                                // 续跑任务：继续读 pipe 至 EOF → wait → finalize → 通知 Agent
                                let task_owner = task_manager.clone();
                                task_owner.spawn_owned(Box::pin(async move {
                                    let started = std::time::Instant::now();
                                    if let Err(error) = drain_stdout.await {
                                        background_output.record_task_error("stdout", error);
                                    }
                                    if let Err(error) = drain_stderr.await {
                                        background_output.record_task_error("stderr", error);
                                    }
                                    // wait 失败（极罕见）按失败完成，保证 finalize 一定执行
                                    let (success, exit_code) =
                                        match execution.child_mut().wait().await {
                                            Ok(s) => (s.success(), s.code()),
                                            Err(e) => {
                                                warn!(
                                                    error = %e,
                                                    task_id = %task_id_owned,
                                                    "promoted bg shell: wait failed"
                                                );
                                                kill_process_group(pid, "KILL");
                                                let _ = execution.child_mut().wait().await;
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
                                    execution.wait_for_exit().await;
                                    execution.confirm_stopped();
                                    task_manager.finalize_bg_shell(
                                        &on_bg_complete_cb,
                                        task_id_owned,
                                        command_owned.chars().take(80).collect(),
                                        success,
                                        combined,
                                        started.elapsed().as_millis() as u64,
                                        false,
                                        Some(background_output.finish(exit_code)),
                                    );
                                }))?;
                                output_capture.retain_files();
                                if has_output {
                                    // 有部分输出：进程在产生进展，续跑是合理的
                                    return Ok(ToolOutput::with_execution(format!(
                                        "Command timed out after {:.1}s. The process is still running and has been promoted to a background task (it was producing output, so it is likely progressing).\ntask_id: {task_id}\npid: {pid}\n{ps_line}\n- It continues running in the background; you will be notified when it completes.\n- Kill it: run `kill {pid}` in another shell command (`kill -- -{pid}` kills the whole process group including child processes)\n{partial_hint}\nCommand that timed out: {command}",
                                        ms as f64 / 1000.0
                                    ), ToolExecutionEvidence {
                                        status: ToolExecutionStatus::RunningAfterTimeout,
                                        exit_code: None,
                                        output_ref: partial_ref.clone(),
                                        output_truncated: true,
                                        task_id: Some(task_id),
                                    }));
                                }
                                // 无输出：进程可能挂起（等输入/资源）而非正常变慢——
                                // 仍 promote（避免误杀静默启动的慢任务），但如实说明不确定性
                                return Ok(ToolOutput::with_execution(format!(
                                    "Command timed out after {:.1}s with no output produced. The process is still running and has been promoted to a background task, but it may never complete on its own.\ntask_id: {task_id}\npid: {pid}\n{ps_line}\nLikely causes:\n- The command is waiting for input or for a resource (network, lock, another process) that will never arrive.\n- It is a long-running service/daemon; it should have been started with run_in_background: true.\n- It is a slow command still in a silent startup phase (e.g. compile/install with no output yet).\nIf it does not complete on its own, terminate it: run `kill {pid}` in another shell command (`kill -- -{pid}` kills the whole process group including child processes)\n{partial_hint}\nCommand that timed out: {command}",
                                    ms as f64 / 1000.0
                                ), ToolExecutionEvidence {
                                    status: ToolExecutionStatus::RunningAfterTimeout,
                                    exit_code: None,
                                    output_ref: partial_ref,
                                    output_truncated: true,
                                    task_id: Some(task_id),
                                }));
                            }
                            Err(e) => {
                                // 注册失败（SHELL_LIMIT 满）→ 回退杀进程组路径
                                kill_process_group(pid, "KILL");
                                let _ = execution.child_mut().wait().await;
                                let _ = drain_stdout.await;
                                let _ = drain_stderr.await;
                                execution.confirm_stopped();
                                output_capture.cleanup().await;
                                return Ok(ToolOutput::with_execution(format!(
                                    "Command timed out after {:.1}s and could not be promoted to a background task: {e}. The process group has been terminated.\n{ps_line}\n{partial_hint}\nCommand that timed out: {command}",
                                    ms as f64 / 1000.0
                                ), ToolExecutionEvidence {
                                    status: ToolExecutionStatus::TimedOut,
                                    exit_code: None,
                                    output_ref: partial_ref.clone(),
                                    output_truncated: true,
                                    task_id: None,
                                }));
                            }
                        }
                    } else {
                        // ── 无 TaskManager：杀进程组 + 部分输出落盘 ──
                        kill_process_group(pid, "KILL");
                        let _ = execution.child_mut().wait().await;
                        let _ = drain_stdout.await;
                        let _ = drain_stderr.await;
                        execution.confirm_stopped();
                        output_capture.cleanup().await;
                        return Ok(ToolOutput::with_execution(format!(
                            "Command timed out after {:.1}s. The default timeout is deliberately short (15s) to encourage efficient commands.\n\
                             {ps_line}\n\
                             Options:\n\
                             - Optimize the command: avoid scanning large directories (e.g. use `find . -maxdepth 3` instead of `find /Users/...`), add `| head`, or use fd/rg instead of find/grep.\n\
                             - Increase timeout: set `timeout` parameter to a larger value (e.g. `timeout: 120000` for 2 minutes).\n\
                             - Use background mode: set `run_in_background: true` for long-running servers/builds/installs.\n\
                             {partial_hint}\n\
                             Command that timed out: {command}",
                            ms as f64 / 1000.0
                        ), ToolExecutionEvidence {
                            status: ToolExecutionStatus::TimedOut,
                            exit_code: None,
                            output_ref: partial_ref,
                            output_truncated: true,
                            task_id: None,
                        }));
                    }
                }
            },
        };

        // 正常完成路径：等待管道排空，合并输出，保持既有格式与截断逻辑
        match wait_result {
            Ok(status) => {
                if let Err(error) = drain_stdout.await {
                    output_capture.record_task_error("stdout", error);
                }
                if let Err(error) = drain_stderr.await {
                    output_capture.record_task_error("stderr", error);
                }
                let stdout = match stdout_buf.lock() {
                    Ok(g) => g.clone(),
                    Err(poisoned) => poisoned.into_inner().clone(),
                };
                let stderr = match stderr_buf.lock() {
                    Ok(g) => g.clone(),
                    Err(poisoned) => poisoned.into_inner().clone(),
                };
                let mut output = merge_output(&stdout, &stderr, status.code());
                let mut background_task_id = None;
                if execution.is_stopped() {
                    execution.confirm_stopped();
                    output_capture.cleanup().await;
                } else if let Some(manager) = &self.task_manager {
                    // A successful shell can leave `command &` running with redirected pipes.
                    // Register that group before returning so normal completion does not kill it.
                    let task_id = bg_shell_task_id();
                    background_task_id = Some(task_id.clone());
                    manager.register(peri_acp_types::tasks::BgTaskRegistration {
                        task_id: task_id.clone(),
                        kind: BgTaskKind::Shell,
                        summary: command.chars().take(80).collect(),
                        pid: Some(pid),
                        kill: execution.cancel_callback(),
                    })?;
                    execution.track_registration(manager.clone(), task_id.clone());
                    let task_manager = manager.clone();
                    let on_complete = self.on_bg_complete.clone();
                    let summary: String = command.chars().take(80).collect();
                    let task_id_for_wait = task_id.clone();
                    let background_output = Arc::clone(&output_capture);
                    manager.spawn_owned(Box::pin(async move {
                        let started = std::time::Instant::now();
                        execution.wait_for_exit().await;
                        execution.confirm_stopped();
                        task_manager.finalize_bg_shell(&on_complete, task_id_for_wait, summary, status.success(),
                            "Background processes have stopped; individual exit codes are unavailable.".into(),
                            started.elapsed().as_millis() as u64, false,
                            // Only the leader was waited. The remaining group
                            // has no observable exit code of its own.
                            Some(background_output.finish(None)));
                    }))?;
                    output_capture.retain_files();
                    output.push_str(&format!(
                        "\nRemaining processes continue as background task {task_id}."
                    ));
                } else {
                    execution.release_unmanaged();
                    output_capture.cleanup().await;
                }
                let (text, output_ref) = bounded_bash_output(&output);
                let execution_status = if background_task_id.is_some() {
                    ToolExecutionStatus::Running
                } else if command.contains('&') && status.success() {
                    // Without a task manager we cannot account for children
                    // left behind by shell background syntax.
                    ToolExecutionStatus::Unknown
                } else if status.success() {
                    ToolExecutionStatus::Completed
                } else {
                    ToolExecutionStatus::Failed
                };
                let output_truncated =
                    output_ref.is_some() || text.chars().count() < output.chars().count();
                Ok(ToolOutput::with_execution(
                    text,
                    ToolExecutionEvidence {
                        status: execution_status,
                        exit_code: status.code(),
                        output_truncated,
                        output_ref,
                        task_id: background_task_id,
                    },
                ))
            }
            Err(e) => {
                kill_process_group(pid, "KILL");
                let _ = execution.child_mut().wait().await;
                let _ = drain_stdout.await;
                let _ = drain_stderr.await;
                execution.confirm_stopped();
                output_capture.cleanup().await;
                Err(format!("Error executing command: {e}").into())
            }
        }
    }

    async fn invoke(
        &self,
        input: Value,
        ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        match self.invoke_output(input, ctx).await? {
            ToolOutput {
                text,
                execution: Some(evidence),
            } if matches!(
                evidence.status,
                ToolExecutionStatus::TimedOut | ToolExecutionStatus::RunningAfterTimeout
            ) =>
            {
                Err(text.into())
            }
            output => Ok(output.text),
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
