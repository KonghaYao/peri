//! WorkflowRunner —— 公开运行入口、子进程所有权与取消收敛。

mod agent_dispatch;
mod artifact;
mod limits;
mod message_loop;
mod run_protocol;
mod terminal;

use std::sync::Arc;
use std::time::Duration;

use peri_js_runtime::{JsExecutionHost, JsProcessSpec};
use serde_json::Value;
use tokio::sync::{oneshot, watch};

use artifact::prepare_workflow_command;
use message_loop::MessageLoop;
use run_protocol::{validate_start_ack, workflow_start_params};
use terminal::send_failure;

use crate::error::WorkflowError;
use crate::journal::WorkflowJournalStore;
use crate::progress::WorkflowProgressStore;
use crate::protocol::{ProgressEvent, WorkflowLimits};
use crate::rpc::RpcChannel;

pub(crate) use artifact::WORKFLOW_ARTIFACT_BYTES;

const START_TIMEOUT: Duration = Duration::from_secs(15);

// ─── Agent 回调 trait（3.0 批 2 波 1 迁入 peri-acp-types）────────────

pub use peri_acp_types::workflow::AgentExecutor;

// ─── 公开类型 ──────────────────────────────────────────────────

/// Workflow 输入参数
#[derive(Debug, Clone)]
pub struct WorkflowInput {
    pub script: String,
    pub args: Option<Value>,
    pub max_concurrency: u32,
    pub budget_total: Option<u64>,
    pub limits: WorkflowLimits,
    pub workflow_name: String,
    pub resume_from: Option<String>,
    pub write_intent: Option<peri_acp_types::workflow::WorkflowWriteIntent>,
    pub git_baseline: Option<crate::journal::GitBaseline>,
}

/// Workflow 执行结果
#[derive(Debug, Clone)]
pub struct WorkflowResult {
    pub run_id: String,
    pub status: String,
    pub return_value: Option<Value>,
    pub error: Option<String>,
    pub post_processing_status: peri_acp_types::workflow::PostProcessingStatus,
    pub delivery_status: peri_acp_types::workflow::DeliveryStatus,
    /// 子进程是否产生过 stderr 的稳定诊断摘要；不承载原始 stderr 内容。
    pub stderr_tail: Option<String>,
}

/// 读取 workflow 终态：若 fast-path 已经写入当前 watch 值则立即返回，
/// 否则等待首次变化。sender 未发布终态即关闭时返回 `None`。
pub async fn receive_workflow_result(
    rx: &mut watch::Receiver<Option<WorkflowResult>>,
) -> Option<WorkflowResult> {
    loop {
        if let Some(result) = rx.borrow().clone() {
            return Some(result);
        }
        if rx.changed().await.is_err() {
            return rx.borrow().clone();
        }
    }
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
        let started_at_iso = chrono::Utc::now().to_rfc3339();

        // 1. Persist script
        match journal_store.init_run(&run_id, &input.script) {
            Ok(()) => {}
            Err(e) => {
                let err = WorkflowError::Io(e);
                send_failure(
                    &done_tx,
                    Some(&journal_store),
                    &run_id,
                    &input,
                    &started_at_iso,
                    &err,
                    None,
                );
                return Err(err);
            }
        }

        // 2. Resume: read old journal if resume_from is set
        let resume_entries = if let Some(ref old_run_id) = input.resume_from {
            journal_store.read_all(old_run_id).ok()
        } else {
            None
        };

        // 3. Prepare the validated runtime artifact and command.
        let command = match prepare_workflow_command().await {
            Ok(command) => command,
            Err(e) => {
                send_failure(
                    &done_tx,
                    Some(&journal_store),
                    &run_id,
                    &input,
                    &started_at_iso,
                    &e,
                    None,
                );
                return Err(e);
            }
        };
        let host = match JsExecutionHost::spawn(JsProcessSpec::new(command.program, command.args)) {
            Ok(host) => Arc::new(host),
            Err(error) => {
                let err = WorkflowError::from(error);
                send_failure(
                    &done_tx,
                    Some(&journal_store),
                    &run_id,
                    &input,
                    &started_at_iso,
                    &err,
                    None,
                );
                return Err(err);
            }
        };

        // 4. Register channel for Workflow agent kill tracking (GAP-07)
        let channel = Arc::new(RpcChannel::new(host.channel()));
        self.active_channels
            .insert(run_id.clone(), Arc::clone(&channel));

        // 5. Generic host owns stdout/stderr readers; Adapter consumes routed messages.
        let msg_rx = host
            .take_incoming()
            .await
            .expect("new JavaScript host must expose its incoming receiver");

        // 7. Send workflow/start request
        let start_params = match serde_json::to_value(workflow_start_params(
            &run_id,
            &input,
            resume_entries,
            &self.cwd,
        )) {
            Ok(v) => v,
            Err(e) => {
                self.active_channels.remove(&run_id);
                let _ = host.kill().await;
                let err = WorkflowError::from(e);
                send_failure(
                    &done_tx,
                    Some(&journal_store),
                    &run_id,
                    &input,
                    &started_at_iso,
                    &err,
                    None,
                );
                return Err(err);
            }
        };
        let start_resp = match tokio::time::timeout(
            START_TIMEOUT,
            channel.send_request("workflow/start", start_params),
        )
        .await
        {
            Ok(Ok(resp)) => resp,
            Ok(Err(_rpc_error)) => {
                self.active_channels.remove(&run_id);
                let _ = host.kill().await;
                let stderr_tail = host.stderr_tail();
                let err = WorkflowError::SpawnFailed("workflow/start RPC failed".into());
                send_failure(
                    &done_tx,
                    Some(&journal_store),
                    &run_id,
                    &input,
                    &started_at_iso,
                    &err,
                    stderr_tail,
                );
                return Err(err);
            }
            Err(_timeout) => {
                self.active_channels.remove(&run_id);
                let _ = host.kill().await;
                let stderr_tail = host.stderr_tail();
                let err = WorkflowError::SpawnFailed(
                    "workflow/start timed out (15s) — node process may have crashed".into(),
                );
                send_failure(
                    &done_tx,
                    Some(&journal_store),
                    &run_id,
                    &input,
                    &started_at_iso,
                    &err,
                    stderr_tail,
                );
                return Err(err);
            }
        };
        if let Err(err) = validate_start_ack(start_resp) {
            self.active_channels.remove(&run_id);
            let _ = host.kill().await;
            let stderr_tail = host.stderr_tail();
            send_failure(
                &done_tx,
                Some(&journal_store),
                &run_id,
                &input,
                &started_at_iso,
                &err,
                stderr_tail,
            );
            return Err(err);
        }

        // 8. Message loop (spawned task)
        let run_started = std::time::Instant::now();

        // Clone values needed by the kill branch before they're moved into msg_loop
        let kill_wf_name = input.workflow_name.clone();
        let kill_script = input.script.clone();
        let kill_limits = input.limits.clone();
        let kill_budget_total = input.budget_total;
        let kill_write_intent = input.write_intent.clone();
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

        let message_loop = MessageLoop {
            agent_executor: Arc::clone(&self.agent_executor),
            channel: Arc::clone(&channel),
            journal_store: Arc::clone(&journal_store),
            progress_store: Arc::clone(&progress_store),
            run_id: run_id.clone(),
            host: Arc::clone(&host),
            input,
            started_at_iso,
            run_started,
            msg_rx,
            done_tx,
        };
        let mut msg_loop = tokio::spawn(message_loop.run());

        // 9. Wait for kill signal or message loop completion
        let journal_clone2 = Arc::clone(&journal_store);
        tokio::select! {
            biased;
            _ = kill_rx => {
                // 超时保护：Node crash 时不会阻塞 (M-ARCH6)
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    channel.send_request("workflow/kill", serde_json::json!({"runId": run_id})),
                )
                .await;
                let _ = host.kill().await;

                // Abort msg_loop 防止 state.json 和 done_tx 被覆写为 "failed"
                // （msg_loop 检测到 stdout 关闭后会以默认 status="failed" 写 state.json + done_tx，
                //  而 watch channel 后到值会覆盖先到值使 kill 事实丢失）
                msg_loop.abort();
                let _ = (&mut msg_loop).await;

                // 写入 killed state.json
                let stderr_tail = host.stderr_tail();
                let state = crate::journal::RunState {
                    run_id: run_id.clone(),
                    workflow_name: kill_wf_name,
                    status: "killed".to_string(),
                    execution_status: peri_acp_types::workflow::ExecutionStatus::Killed,
                    acceptance_status: peri_acp_types::workflow::AcceptanceStatus::Unknown,
                    post_processing_status: peri_acp_types::workflow::PostProcessingStatus::Blocked,
                    delivery_status: peri_acp_types::workflow::DeliveryStatus::Blocked,
                    write_intent: kill_write_intent,
                    limits: kill_limits,
                    budget_total: kill_budget_total,
                    attempts: journal_clone2.read_attempts(&run_id).unwrap_or_default(),
                    return_value: None,
                    script: kill_script,
                    started_at: kill_started_at,
                    finished_at: Some(chrono::Utc::now().to_rfc3339()),
                    error: Some("workflow killed by user".to_string()),
                };
                let _ = journal_clone2.write_state(&run_id, &state);

                // 复用 reducer 标记 Killed 终态（与 Node 正常路径 run_done 同一状态机入口）：
                // msg_loop 已被 abort，Node 侧 run_done 不会到达；不标记则 progress_store 会
                // 永久保留 Running 条目（workflow/list_runs 幽灵 running）
                progress_store.apply_event(&ProgressEvent::RunDone {
                    run_id: run_id.clone(),
                    status: "killed".to_string(),
                    return_value: None,
                    error: Some("workflow killed by user".to_string()),
                });

                // 发送 done_tx（kill 分支作为唯一出口，确保通知任务收到 "killed" 状态）
                let killed_result = WorkflowResult {
                    run_id: run_id.clone(),
                    status: "killed".to_string(),
                    return_value: None,
                    error: Some("workflow killed by user".to_string()),
                    post_processing_status: peri_acp_types::workflow::PostProcessingStatus::Blocked,
                    delivery_status: peri_acp_types::workflow::DeliveryStatus::Blocked,
                    stderr_tail,
                };
                let _ = done_tx_for_kill.send(Some(killed_result));
            }
            _ = &mut msg_loop => {
                // Message loop completed naturally
            }
        }

        // Cleanup: ensure child process is terminated（防止僵尸进程）
        let _ = host.kill().await;

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
