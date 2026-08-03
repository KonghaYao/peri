use std::process::Stdio;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::FutureExt;
use peri_agent::{
    agent::events::BackgroundTaskResult, middleware::r#trait::Middleware, tools::BaseTool,
};
use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::time::{timeout, Duration};
use tracing::warn;

use crate::process::kill_process_group;
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
    pub on_bg_complete: Option<Arc<dyn Fn(&BackgroundTaskResult) + Send + Sync>>,
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
/// 同步路径流式捕获的共享缓冲上限（2MB）；超过后继续排空管道（丢弃新内容），
/// 防止子进程写管道时阻塞
const MAX_PARTIAL_CAPTURE_BYTES: usize = 2 * 1024 * 1024;

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

/// 解析 timeout 参数（None = 不超时）。
///
/// - **后台**：未传 → None（默认不超时，与"后台"语义一致）；显式 0 → None；
///   显式 >0 → clamp 到 [min, 600_000]
/// - **同步**：未传 → Some(15_000)；显式 0 → None；显式 >0 → clamp
fn parse_timeout(input: &Value, is_background: bool) -> Option<u64> {
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
fn kill_process_group_escalating(pid: u32) {
    kill_process_group(pid, "TERM");
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        kill_process_group(pid, "KILL");
    });
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

/// 将 stdout/stderr 管道流式读入共享缓冲。缓冲超过 `MAX_PARTIAL_CAPTURE_BYTES`
/// 后继续排空（丢弃新内容），防止子进程写满管道时阻塞。
async fn drain_pipe(mut reader: impl tokio::io::AsyncRead + Unpin, buf: Arc<Mutex<String>>) {
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

/// bg shell 结果收尾（bg 路径与同步超时 promote 续跑共用）：
/// 超长输出落盘 → 构造 BackgroundTaskResult → on_bg_complete 回调 → 注册 → complete()。
#[allow(clippy::too_many_arguments)]
fn finalize_bg_shell(
    registry: &BackgroundTaskRegistry,
    on_bg_complete: &Option<Arc<dyn Fn(&BackgroundTaskResult) + Send + Sync>>,
    task_id: String,
    prompt_summary: String,
    success: bool,
    output: String,
    duration_ms: u64,
    cancel_handle: BgCancelHandle,
    pid: Option<u32>,
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
        cb(&result);
    }
    // 注册任务（向 registry 提供 pid 用于取消）
    let bg_task = BackgroundTask {
        id: result.task_id.clone(),
        agent_name: "bg-shell".to_string(),
        prompt_summary: result.prompt_summary.clone(),
        status: BackgroundTaskStatus::Running,
        started_at: std::time::Instant::now(),
        chrono_started_at: chrono::Utc::now(),
        kind: BgTaskKind::Shell,
        cancel_handle,
        pid,
        output_preview: None,
    };
    // register 失败时仍需调 complete()，确保 bg-task-completed 事件推送到 TUI，
    // 否则任务会残留在状态栏。complete() 在 task 未注册时也能安全处理（仅 push_event）。
    if let Err(e) = registry.register_with_kind(bg_task) {
        warn!(error = %e, task_id = %result.task_id, "bg shell: register_with_kind failed (callback already fired)");
    }
    let complete_task_id = result.task_id.clone();
    registry.complete(&complete_task_id, result);
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
                    "description": "Optional timeout in milliseconds (default 15s for foreground; background tasks run until completion unless timeout is explicitly set; 0 = no timeout; max 600000). If the command takes longer than this, the entire process group is killed and a timeout error returned. The short foreground default encourages efficient commands — for long-running tasks (builds, installs), set a higher timeout or use run_in_background."
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
            let registry = Arc::clone(self.bg_registry.as_ref().ok_or(
                "run_in_background is not available: no background task registry configured",
            )?);

            // timeout 参数解析：未传/显式 0 → 不超时（后台语义：跑完为止）
            let timeout_opt = parse_timeout(&input, true);

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
                // 外層 catch_unwind 保護：確保任何意外 panic 也會調用 registry.complete()，
                // 防止 bg shell 任務殘留在狀態欄。
                let started = std::time::Instant::now();
                let result = std::panic::AssertUnwindSafe(async {
                    let mut cmd = crate::process::shell_command(&command_owned, &[]);
                    cmd.current_dir(&cwd)
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .kill_on_drop(true);
                    #[cfg(unix)]
                    cmd.process_group(0);

                    let child = match cmd.spawn() {
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
                                timed_out: false,
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

                    // 超时包裹 wait_with_output（后台未显式传 timeout 或 timeout=0 时不超时）
                    let wait_future = child.wait_with_output();
                    let wait_result = match timeout_opt {
                        None => wait_future.await.map(Some),
                        Some(ms) => {
                            match tokio::time::timeout(Duration::from_millis(ms), wait_future)
                                .await
                            {
                                Ok(output_result) => output_result.map(Some),
                                Err(_elapsed) => {
                                    // 超时：kill 整个进程组（bash 为组长，负号 PID 语义），
                                    // 2s 后若 TERM 无效再升级 KILL（fire-and-forget）
                                    kill_process_group_escalating(pid.expect(
                                        "bg shell: child.id() returned None after successful spawn",
                                    ));
                                    // 构造超时错误结果
                                    let result = BackgroundTaskResult {
                                        task_id: task_id.clone(),
                                        agent_name: "bg-shell".to_string(),
                                        prompt_summary: command_owned
                                            .chars()
                                            .take(80)
                                            .collect(),
                                        success: false,
                                        output: format!(
                                            "Command timed out after {}s.\nCommand: {}",
                                            ms as f64 / 1000.0,
                                            command_owned
                                        ),
                                        tool_calls_count: 0,
                                        duration_ms: started.elapsed().as_millis() as u64,
                                        child_thread_id: None,
                                        timed_out: true,
                                    };
                                    // 回调通知 Agent inbox（在 registry 操作之前）
                                    if let Some(ref cb) = on_bg_complete_cb {
                                        cb(&result);
                                    }
                                    let bg_task = BackgroundTask {
                                        id: result.task_id.clone(),
                                        agent_name: "bg-shell".to_string(),
                                        prompt_summary: command_owned
                                            .chars()
                                            .take(80)
                                            .collect(),
                                        status: BackgroundTaskStatus::Running,
                                        started_at: std::time::Instant::now(),
                                        chrono_started_at: chrono::Utc::now(),
                                        kind: BgTaskKind::Shell,
                                        cancel_handle: BgCancelHandle::Pid(pid.expect(
                                            "bg shell: child.id() returned None after successful spawn",
                                        )),
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
                                combined =
                                    format!("[exit code: {}]", out.status.code().unwrap_or(-1));
                            }
                            (success, combined)
                        }
                        Err(e) => (false, format!("Command failed: {}", e)),
                        // unreachable: wait_with_output() always returns Ok(Output)
                        Ok(None) => {
                            unreachable!("bg shell: wait_with_output returned Ok(None)")
                        }
                    };

                    // 回调通知 + 注册 + 完成（与 promote 续跑共用收尾逻辑）
                    finalize_bg_shell(
                        &registry,
                        &on_bg_complete_cb,
                        task_id.clone(),
                        command_owned.chars().take(80).collect(),
                        output.0,
                        output.1,
                        started.elapsed().as_millis() as u64,
                        BgCancelHandle::Pid(pid.expect(
                            "bg shell: child.id() returned None after successful spawn",
                        )),
                        pid,
                        false,
                    );
                })
                .catch_unwind()
                .await;
                if let Err(panic_err) = result {
                    // spawn 閉包內部 panic：嘗試用現有 task_id 發送失敗事件
                    let panic_msg = if let Some(s) = panic_err.downcast_ref::<String>() {
                        s.clone()
                    } else if let Some(s) = panic_err.downcast_ref::<&str>() {
                        s.to_string()
                    } else {
                        "unknown panic".to_string()
                    };
                    let fallback = BackgroundTaskResult {
                        task_id: task_id.clone(),
                        agent_name: "bg-shell".to_string(),
                        prompt_summary: command_owned.chars().take(80).collect(),
                        success: false,
                        output: format!("Background shell task panicked: {}", panic_msg),
                        tool_calls_count: 0,
                        duration_ms: started.elapsed().as_millis() as u64,
                        child_thread_id: None,
                        timed_out: false,
                    };
                    // 嘗試註冊 + 完成（即使 register 失敗也調 complete，發送 cleanup 事件到 TUI）
                    let bg_task = BackgroundTask {
                        id: fallback.task_id.clone(),
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
                    let complete_task_id = fallback.task_id.clone();
                    registry.complete(&complete_task_id, fallback);
                }
            });

            return Ok(format!(
                "Background shell task started.\ntask_id: {}\nThe command is running in the background. Monitor in the Tasks panel.",
                task_id_for_return
            ));
        }

        // ── 同步执行路径 ──
        let timeout_opt = parse_timeout(&input, false);

        let mut cmd = crate::process::shell_command(command, &[]);
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

                    if let Some(registry) = self.bg_registry.as_ref() {
                        // ── 有注册表：不杀进程，promote 为后台任务续跑 ──
                        let task_id = format!(
                            "shell-{}",
                            uuid::Uuid::now_v7()
                                .to_string()
                                .chars()
                                .take(8)
                                .collect::<String>()
                        );
                        let bg_task = BackgroundTask {
                            id: task_id.clone(),
                            agent_name: "bg-shell".to_string(),
                            prompt_summary: command.chars().take(80).collect(),
                            status: BackgroundTaskStatus::Running,
                            started_at: std::time::Instant::now(),
                            chrono_started_at: chrono::Utc::now(),
                            kind: BgTaskKind::Shell,
                            cancel_handle: BgCancelHandle::Pid(pid),
                            pid: Some(pid),
                            output_preview: None,
                        };
                        match registry.register_with_kind(bg_task) {
                            Ok(()) => {
                                let registry = registry.clone();
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
                                    finalize_bg_shell(
                                        &registry,
                                        &on_bg_complete_cb,
                                        task_id_owned,
                                        command_owned.chars().take(80).collect(),
                                        success,
                                        combined,
                                        started.elapsed().as_millis() as u64,
                                        BgCancelHandle::Pid(pid),
                                        Some(pid),
                                        false,
                                    );
                                });
                                return Err(format!(
                                    "Command timed out after {:.1}s; the process is now running as a background task.\ntask_id: {task_id}\nThe process is now running as a background task; you will be notified when it completes.\n{partial_hint}\nCommand that timed out: {command}",
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
                        // ── 无注册表：杀进程组 + 部分输出落盘 ──
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
    bg_registry: Option<Arc<BackgroundTaskRegistry>>,
    on_bg_complete: Option<Arc<dyn Fn(&BackgroundTaskResult) + Send + Sync>>,
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
