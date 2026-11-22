//! Node domain dispatch；由 runner 持有 task handle 并决定终止顺序。
//!
//! P6 wire：`agent/task`（通知）经 [`super::AgentTaskSink`] 发布为任务表记录，宿主
//! 消费者执行后由任务表终态路径投递 `agent/result`；本循环不再就地执行 agent，
//! 只保留发布准入（关闭 / limits）与 run 终态投影。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use peri_js_runtime::JsExecutionHost;
use tokio::sync::watch;
use tracing::{debug, warn};

use crate::journal::WorkflowJournalStore;
use crate::progress::WorkflowProgressStore;
use crate::protocol::AgentResultParams;
use crate::protocol::WorkflowDoneParams;
use crate::protocol::ERR_INTERNAL;
use crate::protocol::ERR_INVALID_PARAMS;
use crate::protocol::ERR_METHOD_NOT_FOUND;
use crate::rpc::{IncomingMessage, RpcChannel};
use crate::runner::limits::RunLimitState;

use crate::runner::run_protocol::JournalAppendParams;
use crate::runner::run_protocol::JournalTruncateParams;

use crate::runner::AgentExecutor;
use crate::runner::AgentTaskPublish;
use crate::runner::AgentTaskSink;
use crate::runner::ToolTaskPublish;
use crate::runner::WorkflowInput;
use crate::runner::WorkflowResult;
use peri_acp_types::workflow::AgentRunResult;
use peri_acp_types::workflow::ProgressEvent;
use peri_acp_types::workflow::WorkflowToolErrorCode;
use peri_acp_types::workflow::WorkflowToolOutcome;

pub(super) struct MessageLoop {
    pub(super) run_scope: Arc<crate::runner::scope::RunScope>,
    /// `agent/task` 发布面（装配期绑定；未接线时一律拒绝并记录）。
    pub(super) agent_task_sink: Option<Arc<dyn AgentTaskSink>>,
    /// agent 执行器：读取每次执行实际解析的上限与用量（attempt 富化用）。
    pub(super) agent_executor: Arc<dyn AgentExecutor>,
    /// run limit 状态（tool 计数 / breach）；与投递路径共享同一实例。
    pub(super) limit_state: Arc<RunLimitState>,
    pub(super) channel: Arc<RpcChannel>,
    pub(super) journal_store: Arc<WorkflowJournalStore>,
    pub(super) progress_store: Arc<WorkflowProgressStore>,
    pub(super) run_id: String,
    pub(super) host: Arc<JsExecutionHost>,
    pub(super) input: WorkflowInput,
    pub(super) started_at_iso: String,
    pub(super) run_started: std::time::Instant,
    pub(super) msg_rx: tokio::sync::mpsc::Receiver<IncomingMessage>,
    pub(super) done_tx: watch::Sender<Option<WorkflowResult>>,
    pub(super) attempts: crate::journal::AttemptTracker,
}

impl MessageLoop {
    pub(super) async fn run(self) {
        let Self {
            run_scope,
            agent_task_sink,
            agent_executor,
            limit_state,
            channel,
            journal_store,
            progress_store,
            run_id,
            host,
            input,
            started_at_iso,
            run_started,
            mut msg_rx,
            done_tx,
            mut attempts,
        } = self;
        let run_limits = input.limits.clone();
        let mut final_result = WorkflowResult {
            run_id: run_id.clone(),
            status: "failed".into(),
            return_value: None,
            error: None,
            post_processing_status: peri_acp_types::workflow::PostProcessingStatus::Blocked,
            delivery_status: peri_acp_types::workflow::DeliveryStatus::Blocked,
            stderr_tail: None,
            unknown_side_effects: false,
        };

        let mut msg_count: usize = 0;
        let mut request_count: usize = 0;
        let mut response_count: usize = 0;
        let mut method_counts: HashMap<String, usize> = HashMap::new();

        loop {
            let next = if let Some(maximum) = run_limits.max_elapsed_ms {
                let elapsed = run_started.elapsed().as_millis() as u64;
                let remaining = maximum.saturating_sub(elapsed);
                if remaining == 0 {
                    // 超时是后到的失败：已记录的 breach/错误才是首个诊断。
                    limit_state.record_run_breach(crate::runner::RunLimitBreach::new(
                        peri_acp_types::workflow::LimitScope::ElapsedMs,
                        Some(maximum),
                        Some(elapsed),
                        format!("workflow exceeded maxElapsedMs ({maximum})"),
                    ));
                    final_result.error = final_result.error.take().or_else(|| limit_state.breach());
                    break;
                }
                match peri_time::timeout(Duration::from_millis(remaining), msg_rx.recv()).await {
                    Ok(message) => message,
                    Err(_) => {
                        // 超时是后到的失败：已记录的 breach/错误才是首个诊断。
                        limit_state.record_run_breach(crate::runner::RunLimitBreach::new(
                            peri_acp_types::workflow::LimitScope::ElapsedMs,
                            Some(maximum),
                            // 等待已耗尽剩余预算，检测时点的观测值即上限。
                            Some(maximum),
                            format!("workflow exceeded maxElapsedMs ({maximum})"),
                        ));
                        final_result.error =
                            final_result.error.take().or_else(|| limit_state.breach());
                        break;
                    }
                }
            } else {
                msg_rx.recv().await
            };
            let Some(msg) = next else { break };
            msg_count += 1;
            match &msg {
                IncomingMessage::Request { method, .. } => {
                    request_count += 1;
                    *method_counts.entry(method.clone()).or_default() += 1;
                }
                IncomingMessage::Response { .. } => {
                    response_count += 1;
                }
                IncomingMessage::ProtocolError(_) | IncomingMessage::ResourceLimit { .. } => {}
            }
            match msg {
                IncomingMessage::Request { id, method, params } => match method.as_str() {
                    // P6 wire：Node 以通知发布任务（design §5.2 第 1 条）。宿主不再就地
                    // 执行；消费者经 `resources/read` 发现任务并回填 `WorkflowAgentResult`，
                    // 终态由任务表投递 `agent/result`。
                    "agent/task" => {
                        let rejected_task_id = params
                            .as_ref()
                            .and_then(|value| value.get("taskId"))
                            .and_then(|value| value.as_str())
                            .map(str::to_string);
                        let (task_id, params) =
                            match crate::runner::run_protocol::parse_agent_task_params(
                                params, &run_id,
                            ) {
                                Ok(parsed) => parsed,
                                Err(error) => {
                                    warn!(
                                        target: "workflow.rpc",
                                        run_id = %run_id,
                                        error = %error,
                                        "agent/task rejected invalid params",
                                    );
                                    if let Some(id) = id {
                                        let _ = channel
                                            .send_error(
                                                id,
                                                ERR_INVALID_PARAMS,
                                                "invalid agent/task params",
                                            )
                                            .await;
                                    } else if let Some(rejected) = rejected_task_id {
                                        // 通知形态没有请求 id 可回：能取到 taskId 时就地结算，
                                        // 不留悬挂 promise（被拒绝的任务不会进入任务表）。
                                        let result = AgentRunResult::Dead {
                                            scope_close_incomplete: false,
                                            reason: Some("invalid-params".into()),
                                            detail: Some(error.clone()),
                                            duration_ms: Some(0),
                                            token_count: None,
                                        };
                                        if let Err(error) =
                                            send_agent_result(&channel, &run_id, &rejected, &result)
                                                .await
                                        {
                                            warn!(
                                                target: "workflow.rpc",
                                                run_id = %run_id,
                                                task_id = %rejected,
                                                error = %error,
                                                "agent/result delivery failed for a rejected task",
                                            );
                                        }
                                    }
                                    continue;
                                }
                            };
                        let Some(sink) = agent_task_sink.as_ref() else {
                            // 装配不变量：sink 先于 run 绑定；走到这里说明装配被破坏。
                            warn!(
                                target: "workflow.rpc",
                                run_id = %run_id,
                                task_id = %task_id,
                                "agent/task rejected: workflow task channel is not wired",
                            );
                            if let Some(id) = id {
                                let _ = channel
                                    .send_error(
                                        id,
                                        ERR_INTERNAL,
                                        "workflow task channel is not wired",
                                    )
                                    .await;
                            }
                            continue;
                        };
                        let agent_id = params.agent_id;
                        match sink.publish_agent_task(task_id.clone(), params) {
                            AgentTaskPublish::Accepted => {
                                attempts.record_dispatch(&run_id, agent_id, &task_id);
                            }
                            AgentTaskPublish::Closed => {
                                // 关闭后不得登记（design §4.4 第 ① 步）：就地结算 Node
                                // 等待方——任务不进入任务表，不能靠任务表终态投递收口。
                                let result = AgentRunResult::Dead {
                                    scope_close_incomplete: false,
                                    reason: Some("killed".into()),
                                    detail: Some(
                                        "workflow instance is closed and no longer accepts new tasks"
                                            .into(),
                                    ),
                                    duration_ms: Some(0),
                                    token_count: None,
                                };
                                if let Err(error) =
                                    send_agent_result(&channel, &run_id, &task_id, &result).await
                                {
                                    warn!(
                                        target: "workflow.rpc",
                                        run_id = %run_id,
                                        task_id = %task_id,
                                        error = %error,
                                        "agent/result delivery failed for a closed instance",
                                    );
                                }
                            }
                            AgentTaskPublish::LimitBreach(breach) => {
                                // 与旧链同语义（agent/run 的 maxAgents 分支）：run 立即
                                // 以限额错误终止，后续 done 不得覆写。
                                limit_state.record_run_breach(breach);
                                final_result.error = limit_state.breach();
                                break;
                            }
                        }
                    }
                    // 脚本工具调用（issue #407）：与 agent 任务共用任务表；宿主消费者
                    // claim 后经 canonical 调用管线执行，`WorkflowToolResult` 回填终态。
                    "tool/task" => {
                        let rejected_task_id = params
                            .as_ref()
                            .and_then(|value| value.get("taskId"))
                            .and_then(|value| value.as_str())
                            .map(str::to_string);
                        let rejected_tool_name = params
                            .as_ref()
                            .and_then(|value| value.get("toolName"))
                            .and_then(|value| value.as_str())
                            .unwrap_or("<unknown>")
                            .to_string();
                        let parsed = match crate::runner::run_protocol::parse_tool_task_params(
                            params, &run_id,
                        ) {
                            Ok(parsed) => parsed,
                            Err(rejection) => {
                                warn!(
                                    target: "workflow.rpc",
                                    run_id = %run_id,
                                    error = %rejection.message,
                                    "tool/task rejected invalid params",
                                );
                                if let Some(id) = id {
                                    let _ = channel
                                        .send_error(
                                            id,
                                            ERR_INVALID_PARAMS,
                                            "invalid tool/task params",
                                        )
                                        .await;
                                } else if let Some(rejected) = rejected_task_id {
                                    // 通知形态没有请求 id：就地结算 Node 等待方，不留悬挂 promise
                                    // （被拒绝的任务不进入任务表）。
                                    let outcome = WorkflowToolOutcome::error(
                                        rejection.code,
                                        rejected_tool_name,
                                        rejection.message,
                                    );
                                    if let Err(error) =
                                        send_tool_result(&channel, &run_id, &rejected, &outcome)
                                            .await
                                    {
                                        warn!(
                                            target: "workflow.rpc",
                                            run_id = %run_id,
                                            task_id = %rejected,
                                            error = %error,
                                            "tool/result delivery failed for a rejected task",
                                        );
                                    }
                                }
                                continue;
                            }
                        };
                        let Some(sink) = agent_task_sink.as_ref() else {
                            warn!(
                                target: "workflow.rpc",
                                run_id = %run_id,
                                task_id = %parsed.task_id,
                                "tool/task rejected: workflow task channel is not wired",
                            );
                            if let Some(id) = id {
                                let _ = channel
                                    .send_error(
                                        id,
                                        ERR_INTERNAL,
                                        "workflow task channel is not wired",
                                    )
                                    .await;
                            } else {
                                let outcome = WorkflowToolOutcome::error(
                                    WorkflowToolErrorCode::ToolUnavailable,
                                    &parsed.tool_name,
                                    "workflow task channel is not wired",
                                );
                                let _ =
                                    send_tool_result(&channel, &run_id, &parsed.task_id, &outcome)
                                        .await;
                            }
                            continue;
                        };
                        match sink.publish_tool_task(
                            parsed.task_id.clone(),
                            parsed.run_id.clone(),
                            parsed.tool_name.clone(),
                            parsed.arguments,
                            parsed.tool_seq,
                            parsed.agent_seq,
                        ) {
                            ToolTaskPublish::Accepted => {}
                            ToolTaskPublish::Closed => {
                                // 关闭后不得登记（design §4.4 第 ① 步）：就地结算等待方。
                                let outcome = WorkflowToolOutcome::error(
                                    WorkflowToolErrorCode::ToolUnavailable,
                                    &parsed.tool_name,
                                    "workflow instance is closed and no longer accepts new tasks",
                                );
                                if let Err(error) =
                                    send_tool_result(&channel, &run_id, &parsed.task_id, &outcome)
                                        .await
                                {
                                    warn!(
                                        target: "workflow.rpc",
                                        run_id = %run_id,
                                        task_id = %parsed.task_id,
                                        error = %error,
                                        "tool/result delivery failed for a closed instance",
                                    );
                                }
                            }
                        }
                    }
                    // 工具调用回放（issue #407 阶段 2）：Node 重放旧 run 的前缀时逐条上报，
                    // 宿主把 entry 追加进当前 run 的 tools.jsonl。解析 / 写入失败只记录，
                    // 不终止 run（下一次 resume 只是无法复用这条前缀）。
                    "tool/replayed" => {
                        match crate::runner::run_protocol::parse_run_scoped::<
                            crate::protocol::ToolReplayedParams,
                        >(params, &run_id)
                        {
                            Ok(parsed) => {
                                if let Err(error) =
                                    journal_store.append_tool(&parsed.run_id, &parsed.entry)
                                {
                                    tracing::error!(
                                        target: "workflow.rpc",
                                        run_id = %parsed.run_id,
                                        task_id = %parsed.entry.task_id,
                                        error = %error,
                                        "tool/replayed: tools.jsonl append failed",
                                    );
                                }
                            }
                            Err(reason) => warn!(
                                target: "workflow.rpc",
                                run_id = %run_id,
                                error = reason,
                                "tool/replayed rejected",
                            ),
                        }
                    }
                    "progress/event" => {
                        if let Some(p) = params {
                            match serde_json::from_value::<ProgressEvent>(p.clone()) {
                                Ok(event) if event.run_id() == run_id => {
                                    debug!(
                                        target: "workflow.rpc",
                                        run_id = %run_id,
                                        "progress/event: applied to store",
                                    );
                                    progress_store.apply_event(&event);
                                }
                                Ok(_) => {
                                    warn!(target: "workflow", "progress/event rejected for inactive run");
                                }
                                Err(_) => {
                                    warn!(target: "workflow", "progress/event: invalid parameters")
                                }
                            }
                        }
                    }
                    "journal/append" => {
                        if let Some(p) = params {
                            if let Ok(mut parsed) = crate::runner::run_protocol::parse_run_scoped::<
                                JournalAppendParams,
                            >(Some(p), &run_id)
                            {
                                attempts.annotate(&mut parsed.entry);
                                // 执行事实按 agent 身份取（`annotate` 已回填可证身份）。
                                let agent_identity = parsed
                                    .entry
                                    .attempt
                                    .as_ref()
                                    .and_then(|attempt| attempt.agent_id)
                                    .unwrap_or(parsed.entry.seq);
                                let limits =
                                    agent_executor.execution_limits(&run_id, agent_identity);
                                enrich_attempt_execution(
                                    &mut parsed.entry,
                                    limits,
                                    input.budget_total,
                                );
                                if let Err(e) = journal_store.append(&parsed.run_id, &parsed.entry)
                                {
                                    warn!(target: "workflow", run_id = %parsed.run_id, error = %e, "journal/append: write failed");
                                }
                            }
                        }
                    }
                    "journal/truncate" => {
                        if let Some(p) = params {
                            if let Ok(parsed) = crate::runner::run_protocol::parse_run_scoped::<
                                JournalTruncateParams,
                            >(Some(p), &run_id)
                            {
                                if let Err(e) = journal_store.truncate(&parsed.run_id) {
                                    warn!(target: "workflow", run_id = %parsed.run_id, error = %e, "journal/truncate: write failed");
                                }
                            }
                        }
                    }
                    "log" => {
                        // Node log bodies may contain user script data or credentials.
                        debug!(target: "workflow:node", "workflow node log received");
                    }
                    "workflow/done" => {
                        if let Ok(done) = crate::runner::run_protocol::parse_run_scoped::<
                            WorkflowDoneParams,
                        >(params, &run_id)
                        {
                            if done.status != "completed" {
                                warn!(
                                    target: "workflow",
                                    run_id = %done.run_id,
                                    status = %done.status,
                                    "workflow ended non-completed"
                                );
                            }
                            let processed_return_value = done.return_value.map(|mut v| {
                                let journal_for_extract = Arc::clone(&journal_store);
                                let _extracted = crate::journal::extract_long_texts(
                                    &mut v,
                                    &done.run_id,
                                    &journal_for_extract,
                                    peri_acp_types::workflow::LONGTEXT_EXTRACT_THRESHOLD_BYTES,
                                );
                                v
                            });
                            final_result = WorkflowResult {
                                run_id: done.run_id.clone(),
                                status: if limit_state.breach().is_some() {
                                    "failed".to_string()
                                } else {
                                    done.status.clone()
                                },
                                return_value: processed_return_value,
                                error: limit_state.breach().or(done.error.clone()),
                                post_processing_status:
                                    peri_acp_types::workflow::PostProcessingStatus::Blocked,
                                delivery_status: peri_acp_types::workflow::DeliveryStatus::Blocked,
                                stderr_tail: None,
                                unknown_side_effects: false,
                            };
                            break;
                        }
                    }
                    _ => {
                        warn!(target: "workflow", "unknown method from node: {method}");
                        if let Some(id) = id {
                            let _ = channel
                                .send_error(id, ERR_METHOD_NOT_FOUND, "Method not found")
                                .await;
                        }
                    }
                },
                IncomingMessage::Response { .. } => {
                    debug!(target: "workflow", "orphan response received");
                }
                IncomingMessage::ProtocolError(error) => {
                    final_result.error = Some(error);
                    break;
                }
                IncomingMessage::ResourceLimit { .. } => {
                    final_result.error = Some("JavaScript RPC resource limit exceeded".into());
                    break;
                }
            }
        }

        tracing::info!(
            target: "workflow",
            run_id = %run_id,
            total_msgs = msg_count,
            requests = request_count,
            responses = response_count,
            method_count = method_counts.len(),
            final_status = %final_result.status,
            "msg_loop exiting — summary"
        );

        run_scope.drain().await;
        // 未知副作用来自任务表（单一事实源）：已回填 `outcome_unknown` /
        // `task_accepted`，或 run 终止时仍有 tool 任务处于 `executing`（无终态）。
        // 此时任务表尚未被 run 终止路径取消（先读后取消），事实仍可观测。
        let mut limit_report = limit_state.report();
        if agent_task_sink.as_ref().is_some_and(|sink| {
            sink.unknown_tool_side_effects(&run_id) || sink.executing_tool_tasks(&run_id) > 0
        }) {
            limit_report.unknown_side_effects = true;
        }
        let final_result = crate::runner::terminal::finalize_workflow(
            final_result,
            &input,
            &journal_store,
            &progress_store,
            started_at_iso,
            &host,
            limit_report,
        );
        let _ = done_tx.send(Some(final_result));
    }
}

/// 给 attempt 补执行结果：`disposition` 保持来源语义，dead/limited 单独可见。
///
/// 缓存命中（recovered）没有本次执行，不写执行事实，也不参与计费；旧 journal
/// 不含 `execution`，只能在写入时按当时的执行事实生成，不回填历史。
fn enrich_attempt_execution(
    entry: &mut crate::protocol::JournalEntry,
    limits: Option<peri_acp_types::workflow::AgentExecutionLimits>,
    token_budget: Option<u64>,
) {
    use peri_acp_types::workflow::AttemptExecution;
    use peri_acp_types::workflow::AttemptOutcome;

    let Some(attempt) = entry.attempt.as_mut() else {
        return;
    };
    if attempt.disposition != peri_acp_types::workflow::AttemptDisposition::Produced {
        return;
    }
    let limited = entry.result.iteration_limit_exhausted();
    let (outcome, reason) = match &entry.result {
        AgentRunResult::Ok { .. } => (AttemptOutcome::Ok, None),
        AgentRunResult::Skipped => (AttemptOutcome::Skipped, None),
        AgentRunResult::Dead { reason, .. } if limited => (AttemptOutcome::Limited, reason.clone()),
        AgentRunResult::Dead { reason, .. } => (AttemptOutcome::Dead, reason.clone()),
    };
    attempt.execution = Some(AttemptExecution {
        outcome,
        reason,
        iteration_limit: limits.map(|limits| limits.iteration_limit),
        iteration_limit_source: limits.map(|limits| limits.iteration_limit_source),
        iterations_used: limits.and_then(|limits| limits.iterations_used),
        token_budget,
        token_count: entry.result.token_count(),
    });
}

/// 就地投递 `agent/result`（关闭拒绝路径；正常终态由任务表终态 sink 投递）。
async fn send_agent_result(
    channel: &RpcChannel,
    run_id: &str,
    task_id: &str,
    result: &AgentRunResult,
) -> Result<(), crate::error::WorkflowError> {
    let params = AgentResultParams {
        run_id: run_id.to_string(),
        task_id: task_id.to_string(),
        result: result.clone(),
    };
    let params = serde_json::to_value(params)
        .map_err(|error| crate::error::WorkflowError::SpawnFailed(error.to_string()))?;
    channel.send_notification("agent/result", params).await
}

/// 就地投递 `tool/result`（拒绝 / 关闭路径；正常终态由任务表终态 sink 投递）。
async fn send_tool_result(
    channel: &RpcChannel,
    run_id: &str,
    task_id: &str,
    outcome: &WorkflowToolOutcome,
) -> Result<(), crate::error::WorkflowError> {
    let params = crate::protocol::ToolResultParams {
        run_id: run_id.to_string(),
        task_id: task_id.to_string(),
        outcome: outcome.clone(),
    };
    let params = serde_json::to_value(params)
        .map_err(|error| crate::error::WorkflowError::SpawnFailed(error.to_string()))?;
    channel.send_notification("tool/result", params).await
}

#[cfg(test)]
#[path = "message_loop_test.rs"]
mod tests;
