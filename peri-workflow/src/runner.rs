//! WorkflowRunner —— spawn node 子进程 + 消息循环 + agent 回调。

use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use serde_json::Value;
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot, watch};
use tracing::{debug, info, warn};

use crate::error::WorkflowError;
use crate::journal::WorkflowJournalStore;
use crate::progress::WorkflowProgressStore;
use crate::protocol::*;
use crate::rpc::{IncomingMessage, RpcChannel};

// ─── 运行时检测 ────────────────────────────────────────────

/// 检测当前环境是否安装了 bun。
/// 使用 OnceLock 缓存结果，仅首次调用时执行 `bun --version`。
static HAS_BUN: OnceLock<bool> = OnceLock::new();

fn has_bun() -> bool {
    *HAS_BUN.get_or_init(|| {
        let ok = std::process::Command::new("bun")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok();
        if ok {
            info!(target: "workflow", "detected bun runtime, will use bunx");
        }
        ok
    })
}

/// 检测当前环境是否安装了 npx。
/// 使用 OnceLock 缓存结果，仅首次调用时执行 `npx --version`。
static HAS_NPX: OnceLock<bool> = OnceLock::new();

fn has_npx() -> bool {
    *HAS_NPX.get_or_init(|| {
        let ok = std::process::Command::new("npx")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok();
        if ok {
            info!(target: "workflow", "detected npx runtime, will use npx");
        }
        ok
    })
}

/// 返回 workflow 触发命令：(binary, args)。
/// bun 环境下用 bunx，否则回退 npx。
/// 两者都不可用时返回可操作的错误信息。
fn workflow_cmd() -> Result<(&'static str, &'static [&'static str]), WorkflowError> {
    if has_bun() {
        return Ok(("bunx", &["-y", "@peri-code/workflow"]));
    }
    // 回退 npx 前先检查是否可用
    if has_npx() {
        return Ok(("npx", &["-y", "@peri-code/workflow"]));
    }
    Err(WorkflowError::SpawnFailed(
        "bun and npx are both unavailable. \
         Install Node.js (https://nodejs.org/) or Bun (https://bun.sh/) \
         to enable multi-agent workflow support."
            .to_string(),
    ))
}

// ─── Agent 回调 trait ─────────────────────────────────────────

/// Agent 回调执行器 trait（由 peri-acp 实现）
#[async_trait::async_trait]
pub trait AgentExecutor: Send + Sync {
    /// 执行单个 workflow agent，返回 AgentRunResult
    async fn execute(&self, params: AgentRunParams) -> AgentRunResult;
}

// ─── 公开类型 ──────────────────────────────────────────────────

/// Workflow 输入参数
#[derive(Debug, Clone)]
pub struct WorkflowInput {
    pub script: String,
    pub args: Option<Value>,
    pub max_concurrency: u32,
    pub budget_total: Option<u64>,
    pub workflow_name: String,
    pub resume_from: Option<String>,
}

/// Workflow 执行结果
#[derive(Debug, Clone)]
pub struct WorkflowResult {
    pub run_id: String,
    pub status: String,
    pub return_value: Option<Value>,
    pub error: Option<String>,
    /// Node 进程 stderr 的最后 20 行（仅 status 为 "failed"/"killed" 时可能有值）。
    /// 用于诊断脚本加载/沙箱导致的快速失败。
    pub stderr_tail: Option<String>,
}

// ─── Journal RPC 参数反序列化 ───────────────────────────────────

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct JournalAppendParams {
    run_id: String,
    entry: crate::protocol::JournalEntry,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct JournalTruncateParams {
    run_id: String,
}

// ─── WorkflowRunner ────────────────────────────────────────────

pub struct WorkflowRunner {
    agent_executor: Arc<dyn AgentExecutor>,
    cwd: String,
    /// 活跃 workflow run 的 RPC 通道（run_id → channel），供 kill_agent 查找（GAP-07）。
    active_channels: dashmap::DashMap<String, Arc<RpcChannel>>,
    /// 进度事件接收通道（从 workflow agent 内部发送，合并到 msg_loop）
    progress_rx: parking_lot::Mutex<
        Option<tokio::sync::mpsc::UnboundedReceiver<crate::protocol::ProgressEvent>>,
    >,
}

impl WorkflowRunner {
    pub fn new(
        agent_executor: Arc<dyn AgentExecutor>,
        cwd: &str,
        progress_rx: Option<tokio::sync::mpsc::UnboundedReceiver<crate::protocol::ProgressEvent>>,
    ) -> Self {
        Self {
            agent_executor,
            cwd: cwd.to_string(),
            active_channels: dashmap::DashMap::new(),
            progress_rx: parking_lot::Mutex::new(progress_rx),
        }
    }

    /// 返回工作目录路径。
    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    /// 杀死指定 workflow run 中的单个 agent（GAP-07）。
    ///
    /// 通过 `active_channels` 找到该 run 的 RpcChannel，
    /// 再通过 `pending_agents` 找到对应 agent 的 cancel 通道。
    /// 返回 `true` 表示成功杀死，`false` 表示 agent 不存在。
    pub async fn kill_agent(&self, run_id: &str, agent_id: u64) -> bool {
        if let Some(channel) = self.active_channels.get(run_id) {
            channel.kill_agent(run_id, agent_id).await
        } else {
            false
        }
    }

    /// 启动 workflow（后台执行，通过 channels 推送事件/通知）
    #[allow(clippy::too_many_arguments)]
    pub async fn run(
        &self,
        run_id: String,
        input: WorkflowInput,
        progress_store: Arc<WorkflowProgressStore>,
        journal_store: Arc<WorkflowJournalStore>,
        done_tx: watch::Sender<Option<WorkflowResult>>,
        kill_rx: oneshot::Receiver<()>,
    ) -> Result<(), WorkflowError> {
        // Helper: send failure result on done_tx before returning Err.
        // Ensures tool.rs's fast-failure detection always receives a result,
        // even when runner exits before spawning the msg_loop (e.g. binary not found).
        fn send_failure(tx: &watch::Sender<Option<WorkflowResult>>, rid: &str, e: &WorkflowError) {
            let _ = tx.send(Some(WorkflowResult {
                run_id: rid.to_string(),
                status: "failed".to_string(),
                return_value: None,
                error: Some(format!("{:#}", e)),
                stderr_tail: None,
            }));
        }

        // 1. Persist script
        match journal_store.init_run(&run_id, &input.script) {
            Ok(()) => {}
            Err(e) => {
                let err = WorkflowError::Io(e);
                send_failure(&done_tx, &run_id, &err);
                return Err(err);
            }
        }

        // 2. Resume: read old journal if resume_from is set
        let resume_entries = if let Some(ref old_run_id) = input.resume_from {
            journal_store.read_all(old_run_id).ok()
        } else {
            None
        };

        // 3. Spawn workflow runner（bun 环境用 bunx，否则 npx）
        let (cmd_bin, cmd_args) = match workflow_cmd() {
            Ok(c) => c,
            Err(e) => {
                send_failure(&done_tx, &run_id, &e);
                return Err(e);
            }
        };
        let mut child = match Command::new(cmd_bin)
            .args(cmd_args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                let err = WorkflowError::SpawnFailed(e.to_string());
                send_failure(&done_tx, &run_id, &err);
                return Err(err);
            }
        };

        let stdin = match child.stdin.take() {
            Some(s) => s,
            None => {
                let err = WorkflowError::SpawnFailed("no stdin".into());
                send_failure(&done_tx, &run_id, &err);
                return Err(err);
            }
        };
        let stdout = match child.stdout.take() {
            Some(s) => s,
            None => {
                let err = WorkflowError::SpawnFailed("no stdout".into());
                send_failure(&done_tx, &run_id, &err);
                return Err(err);
            }
        };
        let stderr = child.stderr.take();

        // 4. Create RPC channel + register for kill tracking (GAP-07)
        let channel = Arc::new(RpcChannel::new(stdin));
        self.active_channels
            .insert(run_id.clone(), Arc::clone(&channel));

        // 5. stdout reader → incoming messages
        let (msg_tx, mut msg_rx) = mpsc::channel::<IncomingMessage>(256);
        crate::rpc::spawn_stdout_reader(stdout, Arc::clone(&channel), msg_tx);

        // 6. stderr reader → tracing::debug + buffer for error reporting
        let stderr_lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        if let Some(stderr) = stderr {
            let stderr_lines = Arc::clone(&stderr_lines);
            tokio::spawn(async move {
                use tokio::io::{AsyncBufReadExt, BufReader};
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    debug!(target: "workflow:node", "{line}");
                    stderr_lines.lock().push(line);
                }
            });
        }

        // 7. Send workflow/start request
        let start_params = match serde_json::to_value(&WorkflowStartParams {
            run_id: run_id.clone(),
            script: input.script.clone(),
            args: input.args.clone(),
            budget_total: input.budget_total,
            max_concurrency: input.max_concurrency,
            resume: resume_entries,
            cwd: self.cwd.clone(),
        }) {
            Ok(v) => v,
            Err(e) => {
                let err = WorkflowError::from(e);
                send_failure(&done_tx, &run_id, &err);
                return Err(err);
            }
        };
        let _start_resp = match tokio::time::timeout(
            std::time::Duration::from_secs(15),
            channel.send_request("workflow/start", start_params),
        )
        .await
        {
            Ok(Ok(resp)) => resp,
            Ok(Err(rpc_err)) => {
                let err =
                    WorkflowError::SpawnFailed(format!("workflow/start RPC error: {rpc_err}"));
                send_failure(&done_tx, &run_id, &err);
                return Err(err);
            }
            Err(_timeout) => {
                let err = WorkflowError::SpawnFailed(
                    "workflow/start timed out (15s) — node process may have crashed".into(),
                );
                send_failure(&done_tx, &run_id, &err);
                return Err(err);
            }
        };

        // 记录 workflow 实际启动时间（用于 state.json），避免与完成时间仅差微秒
        let started_at_iso = chrono::Utc::now().to_rfc3339();

        // 8. Message loop (spawned task)
        let agent_executor = Arc::clone(&self.agent_executor);
        let channel_clone = Arc::clone(&channel);
        let journal_clone = Arc::clone(&journal_store);
        let progress_store_clone = Arc::clone(&progress_store);
        let run_id_clone = run_id.clone();
        let stderr_for_loop = Arc::clone(&stderr_lines);

        // Clone values needed by the kill branch before they're moved into msg_loop
        let kill_wf_name = input.workflow_name.clone();
        let kill_script = input.script.clone();
        let kill_started_at = started_at_iso.clone();

        // Clone done_tx for kill branch — must happen before async move consumes it
        let done_tx_for_kill = done_tx.clone();

        // 提取 progress_rx 供独立转发任务使用（Mutex::lock().take() 消费 Option 内的 receiver）
        let progress_rx_for_loop = self.progress_rx.lock().take();

        // 独立的 progress 转发任务：从 workflow agent 内部接收实时进度事件并写入 progress_store
        if let Some(mut progress_rx) = progress_rx_for_loop {
            let progress_store_for_progress = Arc::clone(&progress_store);
            let _progress_task = tokio::spawn(async move {
                while let Some(event) = progress_rx.recv().await {
                    progress_store_for_progress.apply_event(&event);
                }
            });
        }

        let mut msg_loop = tokio::spawn(async move {
            let mut final_result = WorkflowResult {
                run_id: run_id_clone.clone(),
                status: "failed".into(),
                return_value: None,
                error: None,
                stderr_tail: None,
            };

            while let Some(msg) = msg_rx.recv().await {
                match msg {
                    IncomingMessage::Request { id, method, params } => match method.as_str() {
                        "agent/run" => {
                            // Parse params, spawn agent execution
                            let params: AgentRunParams = params
                                .and_then(|p| serde_json::from_value(p).ok())
                                .unwrap_or_else(|| AgentRunParams {
                                    run_id: run_id_clone.clone(),
                                    agent_id: 0,
                                    prompt: String::new(),
                                    schema: None,
                                    model: None,
                                    max_tokens: None,
                                    agent_type: None,
                                    isolation: None,
                                    allowed_tools: None,
                                    label: None,
                                    phase: None,
                                });
                            // Extract run_id + agent_id for kill tracking before moving params
                            let agent_run_id = params.run_id.clone();
                            let agent_id_num = params.agent_id;
                            let exec = Arc::clone(&agent_executor);
                            let ch = Arc::clone(&channel_clone);
                            tokio::spawn(async move {
                                // Register agent for single-agent kill (GAP-07)
                                let cancel_rx = ch.register_agent(&agent_run_id, agent_id_num, id);

                                // Execute with cancel support
                                let result = tokio::select! {
                                    r = exec.execute(params) => r,
                                    _ = cancel_rx => {
                                        crate::protocol::AgentRunResult::Dead {
                                            reason: Some("killed".into()),
                                            detail: Some("agent killed by user".into()),
                                        }
                                    }
                                };

                                // Deregister (no-op if already removed by kill_agent)
                                ch.deregister_agent(&agent_run_id, agent_id_num);

                                // 仅 cancel_rx 的 killed 结果跳过响应（kill_agent 已发送 error response，
                                // 避免双重 JSON-RPC 响应违反协议规范）
                                // 其他 Dead 变体（no-structured-output / interrupted / runagent-threw）
                                // 来自 executor 自身错误，仍需正常发送响应，否则 Node Promise 永远 hang
                                if let Some(id) = id {
                                    let was_killed = matches!(
                                        result,
                                        AgentRunResult::Dead { reason: Some(ref r), .. } if r == "killed"
                                    );
                                    if !was_killed {
                                        let result_val = serde_json::to_value(&result)
                                            .unwrap_or_else(|_| {
                                                serde_json::json!({
                                                    "kind": "dead",
                                                    "reason": "runagent-threw",
                                                    "detail": "serialize failed"
                                                })
                                            });
                                        let _ = ch.send_response(id, result_val).await;
                                    }
                                }
                            });
                        }
                        "progress/event" => {
                            if let Some(p) = params {
                                match serde_json::from_value::<ProgressEvent>(p) {
                                    Ok(event) => progress_store_clone.apply_event(&event),
                                    Err(e) => {
                                        warn!(target: "workflow", error = %e, "progress/event: parse failed")
                                    }
                                }
                            }
                        }
                        "journal/append" => {
                            if let Some(p) = params {
                                if let Ok(parsed) = serde_json::from_value::<JournalAppendParams>(p)
                                {
                                    if let Err(e) =
                                        journal_clone.append(&parsed.run_id, &parsed.entry)
                                    {
                                        warn!(target: "workflow", run_id = %parsed.run_id, error = %e, "journal/append: write failed");
                                    }
                                }
                            }
                        }
                        "journal/truncate" => {
                            if let Some(p) = params {
                                if let Ok(parsed) =
                                    serde_json::from_value::<JournalTruncateParams>(p)
                                {
                                    if let Err(e) = journal_clone.truncate(&parsed.run_id) {
                                        warn!(target: "workflow", run_id = %parsed.run_id, error = %e, "journal/truncate: write failed");
                                    }
                                }
                            }
                        }
                        "log" => {
                            if let Some(p) = params {
                                let msg = p.get("message").and_then(|v| v.as_str()).unwrap_or("?");
                                let level =
                                    p.get("level").and_then(|v| v.as_str()).unwrap_or("info");
                                match level {
                                    "error" | "warn" => warn!(target: "workflow:node", "{msg}"),
                                    "info" => info!(target: "workflow:node", "{msg}"),
                                    _ => debug!(target: "workflow:node", "{msg}"),
                                }
                            }
                        }
                        "workflow/done" => {
                            if let Some(p) = params {
                                if let Ok(done) = serde_json::from_value::<WorkflowDoneParams>(p) {
                                    if done.status != "completed" {
                                        warn!(
                                            target: "workflow",
                                            run_id = %done.run_id,
                                            status = %done.status,
                                            error = ?done.error,
                                            "workflow ended non-completed"
                                        );
                                    }
                                    final_result = WorkflowResult {
                                        run_id: done.run_id.clone(),
                                        status: done.status.clone(),
                                        return_value: done.return_value.clone(),
                                        error: done.error.clone(),
                                        stderr_tail: None,
                                    };
                                    break;
                                }
                            }
                        }
                        _ => {
                            warn!(target: "workflow", "unknown method from node: {method}");
                            if let Some(id) = id {
                                let _ = channel_clone
                                    .send_error(id, ERR_METHOD_NOT_FOUND, "Method not found")
                                    .await;
                            }
                        }
                    },
                    IncomingMessage::Response { .. } => {
                        debug!(target: "workflow", "orphan response received");
                    }
                }
            }

            // Write state.json
            let stderr_tail = {
                let lines = stderr_for_loop.lock();
                if lines.is_empty() {
                    None
                } else {
                    Some(
                        lines
                            .iter()
                            .rev()
                            .take(20)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join("\n"),
                    )
                }
            };
            let state = crate::journal::RunState {
                run_id: final_result.run_id.clone(),
                workflow_name: input.workflow_name.clone(),
                status: final_result.status.clone(),
                return_value: final_result.return_value.clone(),
                script: input.script.clone(),
                started_at: started_at_iso,
                finished_at: Some(chrono::Utc::now().to_rfc3339()),
                error: final_result.error.clone(),
            };
            if let Err(e) = journal_clone.write_state(&final_result.run_id, &state) {
                warn!(target: "workflow", run_id = %final_result.run_id, error = %e, "write_state failed");
            }

            final_result.stderr_tail = stderr_tail;
            let _ = done_tx.send(Some(final_result));
        });

        // 9. Wait for kill signal or message loop completion
        let journal_clone2 = Arc::clone(&journal_store);
        let stderr_for_kill = Arc::clone(&stderr_lines);
        tokio::select! {
            _ = kill_rx => {
                // 超时保护：Node crash 时不会阻塞 (M-ARCH6)
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    channel.send_request("workflow/kill", serde_json::json!({"runId": run_id})),
                )
                .await;
                let _ = child.kill().await;

                // Abort msg_loop 防止 state.json 和 done_tx 被覆写为 "failed"
                // （msg_loop 检测到 stdout 关闭后会以默认 status="failed" 写 state.json + done_tx，
                //  而 watch channel 后到值会覆盖先到值使 kill 事实丢失）
                msg_loop.abort();

                // 写入 killed state.json
                let _stderr_tail = {
                    let lines = stderr_for_kill.lock();
                    if lines.is_empty() {
                        None
                    } else {
                        Some(
                            lines
                                .iter()
                                .rev()
                                .take(20)
                                .cloned()
                                .collect::<Vec<_>>()
                                .join("\n"),
                        )
                    }
                };
                let state = crate::journal::RunState {
                    run_id: run_id.clone(),
                    workflow_name: kill_wf_name,
                    status: "killed".to_string(),
                    return_value: None,
                    script: kill_script,
                    started_at: kill_started_at,
                    finished_at: Some(chrono::Utc::now().to_rfc3339()),
                    error: Some("workflow killed by user".to_string()),
                };
                let _ = journal_clone2.write_state(&run_id, &state);

                // 发送 done_tx（kill 分支作为唯一出口，确保通知任务收到 "killed" 状态）
                let killed_result = WorkflowResult {
                    run_id: run_id.clone(),
                    status: "killed".to_string(),
                    return_value: None,
                    error: Some("workflow killed by user".to_string()),
                    stderr_tail: _stderr_tail,
                };
                let _ = done_tx_for_kill.send(Some(killed_result));
            }
            _ = &mut msg_loop => {
                // Message loop completed naturally
            }
        }

        // Cleanup: ensure child process is terminated（防止僵尸进程）
        let _ = child.kill().await;

        // Cleanup: remove channel from active tracking (GAP-07)
        self.active_channels.remove(&run_id);

        // Cleanup old runs from journal
        let _ = journal_store.cleanup_old_runs();

        // Cleanup completed runs from progress store（防止内存泄漏 S-PERF4）
        progress_store.cleanup_completed();

        Ok(())
    }
}

#[cfg(test)]
#[path = "runner_test.rs"]
mod tests;
