//! Workflow runner.

use super::{
    fmt, oneshot, prepare_workflow_command, scope, watch, AgentExecutor, AgentResultParams,
    AgentRunParams, AgentRunResult, Arc, Debug, Duration, Formatter, JsExecutionHost,
    JsProcessSpec, MessageLoop, RpcChannel, RunLimitBreach, RunLimitState, Value, WorkflowError,
    WorkflowJournalStore, WorkflowLimits, WorkflowProgressStore,
};

pub(super) const START_TIMEOUT: Duration = Duration::from_secs(15);

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
    LimitBreach(RunLimitBreach),
}

/// `tool/task` 的发布结果（wire 侧视角；issue #407）。
///
/// tool 任务没有 agent 槽位准入：不存在容量 / `maxAgents` 拒绝分支。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolTaskPublish {
    /// 已登记（重复发布幂等返回既有记录）。
    Accepted,
    /// 实例已关闭：不登记，发布方须就地结算 Node 等待方。
    Closed,
}

/// Node 任务发布面：`agent/task` / `tool/task` → 任务表（由 `WorkflowRuntime`
/// 装配期注入）。
///
/// 边界：wire 侧只依赖本 trait，不直连任务表；limits 准入（maxAgents）与关闭判定
/// 都在实现侧，与 run `maxConcurrency`/limits 使用同一执行权威（design §5.2 第 5 条）。
/// 两种任务共用同一张表（不是第二套任务通道）。
pub trait AgentTaskSink: Send + Sync {
    fn publish_agent_task(&self, task_id: String, params: AgentRunParams) -> AgentTaskPublish;

    /// 发布脚本工具调用任务；不参与 agent 槽位 / 限额准入。
    fn publish_tool_task(
        &self,
        task_id: String,
        run_id: String,
        tool_name: String,
        arguments: serde_json::Map<String, Value>,
        tool_seq: u64,
        agent_seq: u64,
    ) -> ToolTaskPublish;

    /// run 终止时仍处于 `executing` 的 tool 任务数（「未知副作用」判定）。
    ///
    /// 默认 `0`：未接入任务表的测试 sink 没有可观测的 tool 任务；生产 sink 走
    /// 任务表的单一计数。
    fn executing_tool_tasks(&self, _run_id: &str) -> usize {
        0
    }

    /// 该 run 是否已记录不可确认的脚本工具副作用（tool 终态 `outcome_unknown` /
    /// `task_accepted`）。默认 `false`（测试 sink 无任务表）。
    fn unknown_tool_side_effects(&self, _run_id: &str) -> bool {
        false
    }

    /// 记录该 run 存在不可确认的脚本工具副作用。默认无操作（测试 sink 无任务表）。
    fn mark_unknown_tool_side_effect(&self, _run_id: &str) {}
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
    /// 启动时捕获的 Git 观测窗口；缺失基线不是“无变化”。
    pub git_observation: crate::journal::GitObservation,
}

/// Workflow 执行结果
#[derive(Debug, Clone)]
#[must_use]
pub struct WorkflowResult {
    pub run_id: String,
    pub status: String,
    pub return_value: Option<Value>,
    pub error: Option<String>,
    pub post_processing_status: peri_acp_types::workflow::PostProcessingStatus,
    pub delivery_status: peri_acp_types::workflow::DeliveryStatus,
    /// 子进程是否产生过 stderr 的稳定诊断摘要；不承载原始 stderr 内容。
    pub stderr_tail: Option<String>,
    /// run 内存在不可确认的脚本工具副作用（观测事实；不把 run 升级为失败）。
    pub unknown_side_effects: bool,
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
    pub(super) agent_executor: Arc<dyn AgentExecutor>,
    pub(super) cwd: String,
    /// 活跃 workflow run 的 RPC 通道（run_id → channel），供 kill_agent 查找（GAP-07）。
    pub(super) active_channels: dashmap::DashMap<String, Arc<RpcChannel>>,
    /// `agent/task` 发布面（装配期由 `WorkflowRuntime` 绑定；未绑定 = 未接线，一律拒绝）。
    pub(super) agent_task_sink: parking_lot::RwLock<Option<Arc<dyn AgentTaskSink>>>,
    /// 活跃 run 的 limit 状态（tool 计数 / breach；run 结束时移除）。
    pub(super) run_limit_states: dashmap::DashMap<String, Arc<RunLimitState>>,
    /// 进度事件接收通道（从 workflow agent 内部发送，合并到 msg_loop）
    pub(super) progress_rx: Option<
        Arc<
            tokio::sync::Mutex<
                tokio::sync::mpsc::UnboundedReceiver<crate::protocol::ProgressEvent>,
            >,
        >,
    >,
}

impl WorkflowRunner {
    #[must_use]
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
    /// 返回是否已安排异步投递；不确认 Node 已收到通知。
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

    /// 投递 `tool/result` 通知（脚本工具调用的终态）。
    ///
    /// 与 [`Self::deliver_agent_result`] 同一条投递契约：任务表提交先于投递、投递是
    /// 通知而非可靠投递（Node 侧按 `taskId` 去重）；在提交临界区内调用，必须非阻塞。
    pub fn deliver_tool_result(
        &self,
        run_id: &str,
        task_id: &str,
        outcome: &peri_acp_types::workflow::WorkflowToolOutcome,
    ) -> bool {
        let Some(channel) = self
            .active_channels
            .get(run_id)
            .map(|entry| Arc::clone(entry.value()))
        else {
            tracing::debug!(
                target: "workflow.rpc",
                run_id,
                task_id,
                "tool/result delivery skipped: run channel is not active",
            );
            return false;
        };
        let params = match serde_json::to_value(crate::protocol::ToolResultParams {
            run_id: run_id.to_string(),
            task_id: task_id.to_string(),
            outcome: outcome.clone(),
        }) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(
                    target: "workflow.rpc",
                    run_id,
                    task_id,
                    error = %error,
                    "tool/result serialization failed; result not delivered",
                );
                return false;
            }
        };
        if tokio::runtime::Handle::try_current().is_err() {
            tracing::warn!(
                target: "workflow.rpc",
                run_id,
                task_id,
                "tool/result delivery skipped: no async runtime",
            );
            return false;
        }
        let delivery_run_id = run_id.to_string();
        let delivery_task_id = task_id.to_string();
        tokio::spawn(async move {
            if let Err(error) = channel.send_notification("tool/result", params).await {
                tracing::warn!(
                    target: "workflow.rpc",
                    run_id = %delivery_run_id,
                    task_id = %delivery_task_id,
                    error = %error,
                    "tool/result delivery failed",
                );
            }
        });
        true
    }

    /// 记录 run 内出现不可确认的脚本工具副作用（`outcome_unknown` / `task_accepted`）。
    ///
    /// 单一事实源是任务表；本入口经 sink 转发（runner 不持任务表句柄）。
    pub(crate) fn note_unknown_tool_side_effect(&self, run_id: &str) {
        if let Some(sink) = self.agent_task_sink() {
            sink.mark_unknown_tool_side_effect(run_id);
        }
    }

    /// run 终止时仍处于 `executing` 的 tool 任务数（未知副作用判定的第二来源）。
    pub(crate) fn executing_tool_tasks(&self, run_id: &str) -> usize {
        self.agent_task_sink()
            .map_or(0, |sink| sink.executing_tool_tasks(run_id))
    }

    /// 该 run 是否已记录不可确认的脚本工具副作用（任务表事实）。
    pub(crate) fn unknown_tool_side_effects(&self, run_id: &str) -> bool {
        self.agent_task_sink()
            .is_some_and(|sink| sink.unknown_tool_side_effects(run_id))
    }

    /// Preserve the first fatal host failure even if the engine later reports completion.
    pub(crate) fn note_run_failure(&self, run_id: &str, detail: &str) {
        if let Some(state) = self.run_limit_states.get(run_id) {
            state.set_breach(detail.to_string());
        }
    }

    /// 记录一次任务终态结果对 run limits 的影响（design §5.2 第 5 条：同一权威）。
    ///
    /// `task_id` 是去重身份：任务终态回投是投递重试，同一任务只计入首次；缓存命中
    /// 没有本次执行，不会走到这里，因此不会被重复计费。token 用量与 `budgetTotal`
    /// 比较，迭代耗尽取执行器解析的上限与计数（不写死、不解析错误文案）。
    /// 返回本次结算新产生的超限详情（若有）；breach 由消息循环在 `workflow/done`
    /// 读取并投影为 run 终态（`status = failed` + 限额错误）。
    pub(crate) fn note_task_execution(
        &self,
        run_id: &str,
        task_id: &str,
        agent_id: u64,
        result: &AgentRunResult,
    ) -> Option<String> {
        let state = self.run_limit_states.get(run_id)?;
        let tool_breach = result
            .tool_count()
            .and_then(|count| state.observe_tool_calls(run_id, task_id, agent_id, count));
        if let Some(tokens) = result.token_count() {
            state.observe_tokens(run_id, task_id, agent_id, tokens);
        }
        let iteration_breach = if result.iteration_limit_exhausted() {
            let limits = self.agent_executor.execution_limits(run_id, agent_id);
            state.observe_iteration_exhaustion(
                run_id,
                task_id,
                agent_id,
                limits.map(|limits| limits.iteration_limit),
                limits.and_then(|limits| limits.iterations_used),
            )
        } else {
            None
        };
        tool_breach.or(iteration_breach)
    }

    /// Bind workflow agent tools to the owning session's execution scope.
    pub fn bind_execution_manager(
        &self,
        manager: Arc<dyn peri_acp_types::tasks::TaskManager>,
    ) -> Result<(), String> {
        self.agent_executor.bind_execution_manager(manager)
    }

    /// 返回工作目录路径。
    #[must_use]
    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    /// 杀死指定 workflow run 中的单个 agent（GAP-07）。
    ///
    /// 通过 `active_channels` 找到该 run 的 RpcChannel，
    /// 再通过 `pending_agents` 找到对应 agent 的 cancel 通道。
    /// 返回 `true` 表示成功杀死，`false` 表示 agent 不存在。
    #[must_use]
    pub async fn kill_agent(&self, run_id: &str, agent_id: u64) -> bool {
        if let Some(channel) = self.active_channels.get(run_id) {
            channel.kill_agent(run_id, agent_id).await
        } else {
            false
        }
    }

    /// 启动 workflow（后台执行，通过 channels 推送事件/通知）
    ///
    /// # Errors
    ///
    /// Returns the filesystem, process startup, RPC, or execution failure that prevents the run from completing. Failure publication and process cleanup follow the same terminal path.
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
                crate::runner::terminal::send_failure(
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
        let (resume_entries, resume_tools) = if let Some(ref old_run_id) = input.resume_from {
            let entries = match journal_store.read_all_strict(old_run_id) {
                Ok(entries) => entries,
                Err(error) => {
                    let err = WorkflowError::Io(error);
                    crate::runner::terminal::send_failure(
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
            let agent_prefix = crate::runner::run_protocol::reusable_journal_prefix(entries);
            let replayed_agent_prefix_len =
                u64::try_from(agent_prefix.len()).expect("journal prefix length fits in u64");
            // tools.jsonl 与 journal.jsonl 同口径 fail closed：坏行意味着工具前缀
            // 不可确认；静默丢弃会让 Node 重放错位。
            let tool_entries = match journal_store.read_tools_strict(old_run_id) {
                Ok(entries) => entries,
                Err(error) => {
                    let err = WorkflowError::Io(error);
                    crate::runner::terminal::send_failure(
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
            (
                Some(agent_prefix),
                Some(crate::runner::run_protocol::replayable_tool_prefix(
                    tool_entries,
                    replayed_agent_prefix_len,
                )),
            )
        } else {
            (None, None)
        };

        // 3. Prepare the validated runtime artifact and command.
        let command = match prepare_workflow_command().await {
            Ok(command) => command,
            Err(e) => {
                crate::runner::terminal::send_failure(
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
                crate::runner::terminal::send_failure(
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
        let start_params =
            match serde_json::to_value(crate::runner::run_protocol::workflow_start_params(
                &run_id,
                &input,
                resume_entries,
                resume_tools,
                &self.cwd,
            )) {
                Ok(v) => v,
                Err(e) => {
                    self.active_channels.remove(&run_id);
                    host.kill()
                        .await
                        .map_err(|error| WorkflowError::CleanupFailed(error.to_string()))?;
                    let err = WorkflowError::from(e);
                    crate::runner::terminal::send_failure(
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
                crate::runner::terminal::send_killed(&done_tx, &journal_store, &progress_store, &run_id, &input, &started_at_iso, host.stderr_tail(), crate::runner::RunLimitReport::default());
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
                crate::runner::terminal::send_failure(
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
                crate::runner::terminal::send_failure(
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
        if let Err(err) = crate::runner::run_protocol::validate_start_ack(start_resp) {
            self.active_channels.remove(&run_id);
            host.kill()
                .await
                .map_err(|error| WorkflowError::CleanupFailed(error.to_string()))?;
            let stderr_tail = host.stderr_tail();
            crate::runner::terminal::send_failure(
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
        let limit_state = Arc::new(RunLimitState::new(
            input.limits.max_tool_calls,
            input.budget_total,
        ));
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
            agent_executor: Arc::clone(&self.agent_executor),
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
            attempts: crate::journal::AttemptTracker::default(),
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
                let mut limit_report = self
                    .run_limit_states
                    .get(&run_id)
                    .map(|state| state.report())
                    .unwrap_or_default();
                // run 终止时仍有 tool 任务处于 executing（无终态）⇒ 未知副作用；
                // 已回填 `outcome_unknown` / `task_accepted` 同样置位（任务表单一来源）。
                if self.unknown_tool_side_effects(&run_id)
                    || self.executing_tool_tasks(&run_id) > 0
                {
                    limit_report.unknown_side_effects = true;
                }
                crate::runner::terminal::send_killed(&done_tx_for_kill, &journal_clone2, &progress_store,
                    &run_id, &kill_input, &kill_started_at, host.stderr_tail(), limit_report);
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
        // 执行器持有的每次执行上限事实同样随 run 回收，避免跨 run 残留。
        self.agent_executor.forget_run(&run_id);

        // Cleanup old runs from journal
        let _ = journal_store.cleanup_old_runs();

        // Cleanup completed runs from progress store（防止内存泄漏 S-PERF4）
        progress_store.cleanup_completed();

        cleanup.map_err(|error| WorkflowError::CleanupFailed(error.to_string()))
    }
}

impl Debug for WorkflowRunner {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkflowRunner")
            .field("cwd", &self.cwd)
            .field("progress_rx_present", &self.progress_rx.is_some())
            .finish_non_exhaustive()
    }
}
