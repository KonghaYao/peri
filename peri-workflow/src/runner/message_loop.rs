//! Node domain dispatch；由 runner 持有 task handle 并决定终止顺序。

use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Duration;

use peri_js_runtime::JsExecutionHost;
use tokio::sync::watch;
use tracing::{debug, warn};

use super::agent_dispatch::AgentDispatcher;
use super::run_protocol::{parse_run_scoped, JournalAppendParams, JournalTruncateParams};
use super::terminal::finalize_workflow;
use super::{AgentExecutor, WorkflowInput, WorkflowResult};
use crate::journal::WorkflowJournalStore;
use crate::progress::WorkflowProgressStore;
use crate::protocol::*;
use crate::rpc::{IncomingMessage, RpcChannel};

pub(super) struct MessageLoop {
    pub(super) agent_executor: Arc<dyn AgentExecutor>,
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
            agent_executor,
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
        let live_agent_attempts = Arc::new(AtomicU64::new(0));
        let observed_tool_calls = Arc::new(AtomicU64::new(0));
        let limit_breach = Arc::new(parking_lot::Mutex::new(None::<String>));
        let run_limits = input.limits.clone();
        let agent_dispatcher = AgentDispatcher {
            run_id: run_id.clone(),
            agent_executor,
            channel: Arc::clone(&channel),
            progress_store: Arc::clone(&progress_store),
            run_limits: run_limits.clone(),
            run_started,
            live_agent_attempts,
            observed_tool_calls,
            limit_breach: Arc::clone(&limit_breach),
        };
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
                    *limit_breach.lock() = Some(error.clone());
                    final_result.error = Some(error);
                    break;
                }
                match tokio::time::timeout(Duration::from_millis(remaining), msg_rx.recv()).await {
                    Ok(message) => message,
                    Err(_) => {
                        let error = format!("workflow exceeded maxElapsedMs ({maximum})");
                        *limit_breach.lock() = Some(error.clone());
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
                    "agent/run" => {
                        if let Err(error) = agent_dispatcher.dispatch(id, params).await {
                            final_result.error = Some(error);
                            break;
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
                                status: if limit_breach.lock().is_some() {
                                    "failed".to_string()
                                } else {
                                    done.status.clone()
                                },
                                return_value: processed_return_value,
                                error: limit_breach.lock().clone().or(done.error.clone()),
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

#[cfg(test)]
#[path = "message_loop_test.rs"]
mod tests;
