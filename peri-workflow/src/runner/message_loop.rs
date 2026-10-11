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

use super::limits::RunLimitState;
use super::run_protocol::{
    parse_agent_task_params, parse_run_scoped, JournalAppendParams, JournalTruncateParams,
};
use super::terminal::finalize_workflow;
use super::{AgentTaskPublish, AgentTaskSink, WorkflowInput, WorkflowResult};
use crate::journal::WorkflowJournalStore;
use crate::progress::WorkflowProgressStore;
use crate::protocol::*;
use crate::rpc::{IncomingMessage, RpcChannel};

pub(super) struct MessageLoop {
    pub(super) run_scope: Arc<super::scope::RunScope>,
    /// `agent/task` 发布面（装配期绑定；未接线时一律拒绝并记录）。
    pub(super) agent_task_sink: Option<Arc<dyn AgentTaskSink>>,
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
}

impl MessageLoop {
    pub(super) async fn run(self) {
        let Self {
            run_scope,
            agent_task_sink,
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
                    let error = format!("workflow exceeded maxElapsedMs ({maximum})");
                    limit_state.set_breach(error.clone());
                    final_result.error = Some(error);
                    break;
                }
                match peri_time::timeout(Duration::from_millis(remaining), msg_rx.recv()).await {
                    Ok(message) => message,
                    Err(_) => {
                        let error = format!("workflow exceeded maxElapsedMs ({maximum})");
                        limit_state.set_breach(error.clone());
                        final_result.error = Some(error);
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
                        let (task_id, params) = match parse_agent_task_params(params, &run_id) {
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
                                        reason: Some("invalid-params".into()),
                                        detail: Some(error.clone()),
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
                        match sink.publish_agent_task(task_id.clone(), params) {
                            AgentTaskPublish::Accepted => {}
                            AgentTaskPublish::Closed => {
                                // 关闭后不得登记（design §4.4 第 ① 步）：就地结算 Node
                                // 等待方——任务不进入任务表，不能靠任务表终态投递收口。
                                let result = AgentRunResult::Dead {
                                    reason: Some("killed".into()),
                                    detail: Some(
                                        "workflow instance is closed and no longer accepts new tasks"
                                            .into(),
                                    ),
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
                            AgentTaskPublish::LimitBreach(error) => {
                                // 与旧链同语义（agent/run 的 maxAgents 分支）：run 立即
                                // 以限额错误终止，后续 done 不得覆写。
                                limit_state.set_breach(error.clone());
                                final_result.error = Some(error);
                                break;
                            }
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
                            if let Ok(parsed) =
                                parse_run_scoped::<JournalAppendParams>(Some(p), &run_id)
                            {
                                if let Err(e) = journal_store.append(&parsed.run_id, &parsed.entry)
                                {
                                    warn!(target: "workflow", run_id = %parsed.run_id, error = %e, "journal/append: write failed");
                                }
                            }
                        }
                    }
                    "journal/truncate" => {
                        if let Some(p) = params {
                            if let Ok(parsed) =
                                parse_run_scoped::<JournalTruncateParams>(Some(p), &run_id)
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
                        if let Ok(done) = parse_run_scoped::<WorkflowDoneParams>(params, &run_id) {
                            if done.status != "completed" {
                                warn!(
                                    target: "workflow",
                                    run_id = %done.run_id,
                                    status = %done.status,
                                    "workflow ended non-completed"
                                );
                            }
                            let processed_return_value = done.return_value.map(|mut v| {
                                if v.is_object() {
                                    let journal_for_extract = Arc::clone(&journal_store);
                                    let _extracted = crate::journal::extract_long_texts(
                                        &mut v,
                                        &done.run_id,
                                        &journal_for_extract,
                                        200,
                                    );
                                }
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
        let final_result = finalize_workflow(
            final_result,
            &input,
            &journal_store,
            &progress_store,
            started_at_iso,
            &host,
        );
        let _ = done_tx.send(Some(final_result));
    }
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

#[cfg(test)]
#[path = "message_loop_test.rs"]
mod tests;
