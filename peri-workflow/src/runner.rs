//! WorkflowRunner —— 公开运行入口、子进程所有权与取消收敛。
//!
//! P6 wire：Node → 宿主 `agent/task`（通知）经 [`AgentTaskSink`] 发布为任务表记录；
//! 宿主的终态提交（[`Self::deliver_agent_result`]）向 Node 投递 `agent/result`（通知）。

mod artifact;
mod limits;
mod message_loop;
mod run_protocol;
mod scope;
mod terminal;

use std::sync::Arc;
use std::time::Duration;

use peri_js_runtime::{JsExecutionHost, JsProcessSpec};
use serde_json::Value;
use tokio::sync::{oneshot, watch};

use message_loop::MessageLoop;
use run_protocol::{reusable_journal_prefix, validate_start_ack, workflow_start_params};
use terminal::{send_failure, send_killed};

use crate::error::WorkflowError;
use crate::journal::WorkflowJournalStore;
use crate::progress::WorkflowProgressStore;
use crate::protocol::{AgentResultParams, AgentRunParams, AgentRunResult, WorkflowLimits};
use crate::rpc::RpcChannel;

pub(crate) use artifact::prepare_workflow_command;
pub(crate) use artifact::WorkflowCommand;
#[cfg(test)]
pub(crate) use artifact::{repository_package_dir, repository_workflow_command};
pub(crate) use limits::RunLimitState;

const START_TIMEOUT: Duration = Duration::from_secs(15);

// ─── Agent 回调 trait（3.0 批 2 波 1 迁入 peri-acp-types）────────────

pub use peri_acp_types::workflow::AgentExecutor;

// ─── agent/task 发布面（P6：Node → 宿主任务发布）──────────────────

/// `agent/task` 的发布结果（wire 侧视角；design §5.2 第 1 条）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentTaskPublish {
    /// 已登记（重复发布幂等返回既有记录）；消费与投递由任务表驱动。
    Accepted,
    /// 实例已关闭（任务表 `PublishOutcome::Closed`）：拒绝登记，发布方须就地结算
    /// Node 等待方，不留悬挂 promise（design §4.4 关闭顺序第 ① 步）。
    Closed,
    /// 超出 run limits（如 maxAgents）：与旧链同语义——run 以限额错误终止。
    LimitBreach(String),
}

/// Node 任务发布面：`agent/task` → 任务表（由 `WorkflowRuntime` 装配期注入）。
///
/// 边界：wire 侧只依赖本 trait，不直连任务表；limits 准入（maxAgents）与关闭判定
/// 都在实现侧，与 run `maxConcurrency`/limits 使用同一执行权威（design §5.2 第 5 条）。
pub trait AgentTaskSink: Send + Sync {
    fn publish_agent_task(&self, task_id: String, params: AgentRunParams) -> AgentTaskPublish;
}

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
    /// `agent/task` 发布面（装配期由 `WorkflowRuntime` 绑定；未绑定 = 未接线，一律拒绝）。
    agent_task_sink: parking_lot::RwLock<Option<Arc<dyn AgentTaskSink>>>,
    /// 活跃 run 的 limit 状态（tool 计数 / breach；run 结束时移除）。
    run_limit_states: dashmap::DashMap<String, Arc<RunLimitState>>,
    /// 进度事件接收通道（从 workflow agent 内部发送，合并到 msg_loop）
    progress_rx: Option<
        Arc<
            tokio::sync::Mutex<
                tokio::sync::mpsc::UnboundedReceiver<crate::protocol::ProgressEvent>,
            >,
        >,
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
            agent_task_sink: parking_lot::RwLock::new(None),
            run_limit_states: dashmap::DashMap::new(),
            progress_rx: progress_rx.map(|rx| Arc::new(tokio::sync::Mutex::new(rx))),
        }
    }

    /// 绑定 `agent/task` 发布面（`WorkflowRuntime` 构造期一次绑定）。
    pub fn bind_agent_task_sink(&self, sink: Arc<dyn AgentTaskSink>) {
        *self.agent_task_sink.write() = Some(sink);
    }

    pub(crate) fn agent_task_sink(&self) -> Option<Arc<dyn AgentTaskSink>> {
        self.agent_task_sink.read().clone()
    }

    /// 投递 `agent/result` 通知（design §5.2 第 4 条：先提交任务表，再投递）。
    ///
    /// 通知只是结算信号、不承担可靠投递：Node 侧按 `taskId` 去重，迟到/重复投递被
    /// 忽略。投递在消息循环之外发生（宿主提交路径），因此以 spawn 移交、不阻塞提交
    /// （任务表 CAS 临界区内调用，必须非阻塞）；失败只记录。
    /// 返回是否找到该 run 的活跃通道。
    pub fn deliver_agent_result(
        &self,
        run_id: &str,
        task_id: &str,
        result: &AgentRunResult,
    ) -> bool {
        let Some(channel) = self
            .active_channels
            .get(run_id)
            .map(|entry| Arc::clone(entry.value()))
        else {
            // run 已结束（通道已回收）属预期路径：终态仍提交任务表，只是不再有等待方。
            tracing::debug!(
                target: "workflow.rpc",
                run_id,
                task_id,
                "agent/result delivery skipped: run channel is not active",
            );
            return false;
        };
        let params = match serde_json::to_value(AgentResultParams {
            run_id: run_id.to_string(),
            task_id: task_id.to_string(),
            result: result.clone(),
        }) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(
                    target: "workflow.rpc",
                    run_id,
                    task_id,
                    error = %error,
                    "agent/result serialization failed; result not delivered",
                );
                return false;
            }
        };
        if tokio::runtime::Handle::try_current().is_err() {
            tracing::warn!(
                target: "workflow.rpc",
                run_id,
                task_id,
                "agent/result delivery skipped: no async runtime",
            );
            return false;
        }
        let delivery_run_id = run_id.to_string();
        let delivery_task_id = task_id.to_string();
        tokio::spawn(async move {
            if let Err(error) = channel.send_notification("agent/result", params).await {
                tracing::warn!(
                    target: "workflow.rpc",
                    run_id = %delivery_run_id,
                    task_id = %delivery_task_id,
                    error = %error,
                    "agent/result delivery failed",
                );
            }
        });
        true
    }

    /// 记录一次任务结果的 tool 调用数（design §5.2 第 5 条：与 run limits 同一权威）。
    ///
    /// 返回首次超限的 breach 详情（若有）；breach 由消息循环在 `workflow/done`
    /// 读取并投影为 run 终态（`status = failed` + 限额错误）。
    pub(crate) fn note_task_tool_calls(&self, run_id: &str, count: u64) -> Option<String> {
        self.run_limit_states
            .get(run_id)
            .and_then(|state| state.observe_tool_calls(count))
    }

    /// Bind workflow agent tools to the owning session's execution scope.
    pub fn bind_execution_manager(
        &self,
        manager: Arc<dyn peri_acp_types::tasks::TaskManager>,
    ) -> Result<(), String> {
        self.agent_executor.bind_execution_manager(manager)
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
        mut kill_rx: oneshot::Receiver<()>,
    ) -> Result<(), WorkflowError> {
        let started_at_iso = peri_time::now_utc_rfc3339();

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
            let entries = match journal_store.read_all_strict(old_run_id) {
                Ok(entries) => entries,
                Err(error) => {
                    let err = WorkflowError::Io(error);
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
            Some(reusable_journal_prefix(entries))
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
        let host = match JsExecutionHost::spawn(
            JsProcessSpec::new(command.program, command.args).with_cwd(&self.cwd),
        ) {
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
                host.kill()
                    .await
                    .map_err(|error| WorkflowError::CleanupFailed(error.to_string()))?;
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
        let start_request = peri_time::timeout(
            START_TIMEOUT,
            channel.send_request("workflow/start", start_params),
        );
        let start_result = tokio::select! {
            biased;
            _ = &mut kill_rx => {
                self.active_channels.remove(&run_id);
                host.kill().await.map_err(|error| WorkflowError::CleanupFailed(error.to_string()))?;
                send_killed(&done_tx, &journal_store, &progress_store, &run_id, &input, &started_at_iso, host.stderr_tail());
                return Ok(());
            }
            result = start_request => result,
        };
        let start_resp = match start_result {
            Ok(Ok(resp)) => resp,
            Ok(Err(_rpc_error)) => {
                self.active_channels.remove(&run_id);
                host.kill()
                    .await
                    .map_err(|error| WorkflowError::CleanupFailed(error.to_string()))?;
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
                host.kill()
                    .await
                    .map_err(|error| WorkflowError::CleanupFailed(error.to_string()))?;
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
            host.kill()
                .await
                .map_err(|error| WorkflowError::CleanupFailed(error.to_string()))?;
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
        let run_started = peri_time::monotonic_now();
        let limit_state = Arc::new(RunLimitState::new(input.limits.max_tool_calls));
        self.run_limit_states
            .insert(run_id.clone(), Arc::clone(&limit_state));

        let kill_input = input.clone();
        let kill_started_at = started_at_iso.clone();

        // Clone done_tx for kill branch — must happen before async move consumes it
        let done_tx_for_kill = done_tx.clone();

        let run_scope = Arc::new(scope::RunScope::new());
        // A live run forwards shared progress. Its successor can take over the
        // receiver after cancellation; no detached session-lifetime task remains.
        if let Some(progress_rx) = self.progress_rx.clone() {
            let progress_store = Arc::clone(&progress_store);
            run_scope.spawn(async move {
                let mut progress_rx = progress_rx.lock().await;
                while let Some(event) = progress_rx.recv().await {
                    progress_store.apply_event(&event);
                }
            });
        }

        let message_loop = MessageLoop {
            run_scope: Arc::clone(&run_scope),
            agent_task_sink: self.agent_task_sink(),
            limit_state: Arc::clone(&limit_state),
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
                run_scope.cancel();
                // 超时保护：Node crash 时不会阻塞 (M-ARCH6)
                let _ = peri_time::timeout(
                    std::time::Duration::from_secs(5),
                    channel.send_request("workflow/kill", serde_json::json!({"runId": run_id})),
                )
                .await;

                // Abort msg_loop 防止 state.json 和 done_tx 被覆写为 "failed"
                // （msg_loop 检测到 stdout 关闭后会以默认 status="failed" 写 state.json + done_tx，
                //  而 watch channel 后到值会覆盖先到值使 kill 事实丢失）
                msg_loop.abort();
                let _ = (&mut msg_loop).await;

                run_scope.drain().await;
                send_killed(&done_tx_for_kill, &journal_clone2, &progress_store,
                    &run_id, &kill_input, &kill_started_at, host.stderr_tail());
            }
            _ = &mut msg_loop => {
                // Message loop completed naturally
            }
        }

        // Cleanup: ensure child process is terminated（防止僵尸进程）
        run_scope.drain().await;
        let cleanup = host.kill().await;

        // Cleanup: remove channel from active tracking (GAP-07)
        self.active_channels.remove(&run_id);

        // Cleanup: drop per-run limit state (计数随 run 结束)
        self.run_limit_states.remove(&run_id);

        // Cleanup old runs from journal
        let _ = journal_store.cleanup_old_runs();

        // Cleanup completed runs from progress store（防止内存泄漏 S-PERF4）
        progress_store.cleanup_completed();

        cleanup.map_err(|error| WorkflowError::CleanupFailed(error.to_string()))
    }
}

#[cfg(test)]
#[path = "runner_test.rs"]
mod tests;
