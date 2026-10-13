use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use peri_acp_types::tasks::{BgTaskKind, BgTaskRegistration, OnBgCompleteFn, TaskManager as _};
use tokio::sync::oneshot;

use peri_acp_types::identity::AgentId;

use super::factory::ResumeClaim;
use super::lifecycle::drain_subagent_events;
use super::types::{SubagentFailure, SubagentLifecycleStart, SubagentLifecycleStop};
use super::util::extract_last_ai_text;
use super::v2_bridge::{forward_subagent_start_v1, forward_subagent_stop_v1, V2SubagentContext};
use super::{
    build_subagent_start_v2, build_subagent_stop_v2_with_failure, emit_subagent_start_v2,
    emit_subagent_stop_v2_with_failure, on_subagent_stop_handler, DeregisterGuard,
    SubagentStopV2Input,
};
use crate::agent::async_tasks::TaskManager;
use crate::agent::events::{AgentEventHandler, ExecutorEvent, FnEventHandler};
use crate::agent::stages::LoopResult;
use crate::agent::subagent_event_forwarder::spawn_subagent_event_forwarder_for_completion;
use crate::agent::LangfuseBridgeLike;
use crate::session::factory::{DeregisterRuntimeFn, RegisterRuntimeFn};
use crate::session::Session;
use peri_acp_types::session_resources::SessionResources;

#[cfg(test)]
#[path = "run_sync_test.rs"]
mod tests;

pub(super) enum SyncCompletion {
    Finished(bool),
    Pending(Vec<String>),
}

pub(super) fn new_sync_task_id() -> String {
    format!("sync-{}", uuid::Uuid::now_v7())
}

struct CancelSyncWorkerOnDrop {
    worker: Option<tokio::task::AbortHandle>,
    close_state: Arc<super::close::SubagentCloseState>,
    task_id: String,
}

impl Drop for CancelSyncWorkerOnDrop {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.abort();
            self.close_state.settle_sync_delivery(&self.task_id);
        }
    }
}

pub(super) fn handoff_event_handler(
    original: Option<Arc<dyn AgentEventHandler>>,
    background: Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>,
    handed_off: Arc<AtomicBool>,
) -> Option<Arc<dyn AgentEventHandler>> {
    if original.is_none() && background.is_none() {
        return None;
    }
    Some(Arc::new(FnEventHandler(move |event| {
        if handed_off.load(Ordering::Acquire) {
            if let Some(sender) = &background {
                if let Err(error) = sender.send(event) {
                    tracing::error!(%error, "sync child handoff event receiver closed");
                }
            } else if let Some(handler) = &original {
                handler.on_event(event);
            }
        } else if let Some(handler) = &original {
            handler.on_event(event);
        }
    })))
}

#[cfg(test)]
pub(super) async fn run_sync_with_handoff<Run, Execution>(
    session: Arc<Session>,
    child_thread_id: String,
    agent_name: String,
    on_complete: Option<OnBgCompleteFn>,
    run: Run,
) -> Result<SyncCompletion, Box<dyn std::error::Error + Send + Sync>>
where
    Run: FnOnce(oneshot::Sender<()>, Arc<AtomicBool>) -> Execution + Send + 'static,
    Execution:
        Future<Output = Result<bool, Box<dyn std::error::Error + Send + Sync>>> + Send + 'static,
{
    run_sync_with_registered_handoff(
        session,
        child_thread_id,
        agent_name,
        None,
        None,
        on_complete,
        run,
    )
    .await
}

pub(super) async fn run_sync_with_registered_handoff<Run, Execution>(
    session: Arc<Session>,
    child_thread_id: String,
    agent_name: String,
    parent_manager: Option<Arc<TaskManager>>,
    task_id: Option<String>,
    on_complete: Option<OnBgCompleteFn>,
    run: Run,
) -> Result<SyncCompletion, Box<dyn std::error::Error + Send + Sync>>
where
    Run: FnOnce(oneshot::Sender<()>, Arc<AtomicBool>) -> Execution + Send + 'static,
    Execution:
        Future<Output = Result<bool, Box<dyn std::error::Error + Send + Sync>>> + Send + 'static,
{
    let host = session
        .subagent_host()
        .ok_or("sync child host unavailable")?;
    let (handoff_sender, handoff_ready) = oneshot::channel();
    let handed_off = Arc::new(AtomicBool::new(false));
    let mut execution = Box::pin(run(handoff_sender, Arc::clone(&handed_off)));
    tokio::select! {
        biased;
        outcome = &mut execution => return Ok(SyncCompletion::Finished(outcome?)),
        _ = handoff_ready => {},
    }
    let (finished_sender, mut finished) = oneshot::channel();
    let delivery_available = on_complete.is_some();
    let worker_handed_off = Arc::clone(&handed_off);
    let settlement = Arc::new(parking_lot::Mutex::new(()));
    let worker_settlement = Arc::clone(&settlement);
    let worker_session = Arc::clone(&session);
    let worker_thread_id = child_thread_id.clone();
    let worker_task_id = task_id.clone().unwrap_or_else(new_sync_task_id);
    let worker_manager = parent_manager.clone();
    let worker_agent_name = agent_name.clone();
    let close_state = Arc::clone(&host.close_state);
    let worker_close_state = Arc::clone(&close_state);
    let close_task_id = worker_task_id.clone();
    let cancel_task_id = worker_task_id.clone();
    let worker = close_state.spawn_sync_worker(close_task_id, async move {
        let outcome = std::panic::AssertUnwindSafe(execution)
            .catch_unwind()
            .await
            .unwrap_or_else(|_| {
                tracing::error!(child_thread_id = %worker_thread_id, "sync child worker panicked");
                Err("sync child worker panicked".into())
            });
        let delivery_required = {
            let _settlement = worker_settlement.lock();
            let delivery_required = worker_handed_off.load(Ordering::Acquire);
            if !delivery_required {
                let _ = finished_sender.send(outcome);
                worker_close_state.settle_sync_delivery(&worker_task_id);
                return;
            }
            delivery_required
        };
        if delivery_required {
            let result = peri_acp_types::event::BackgroundTaskResult {
                task_id: worker_task_id.clone(),
                agent_name: worker_agent_name,
                prompt_summary: "synchronous sub-agent handoff".into(),
                success: matches!(&outcome, Ok(false)),
                output: match &outcome {
                    Ok(false) => extract_last_ai_text(&worker_session),
                    Ok(true) => "Sub-agent was interrupted after handoff".into(),
                    Err(error) => format!("Sub-agent failed after handoff: {error}"),
                },
                tool_calls_count: super::util::count_tool_calls_from_session(&worker_session),
                duration_ms: 0,
                timed_out: false,
                child_thread_id: Some(worker_thread_id.clone()),
                subagent_failure: None,
                shell_output: None,
            };
            if let Some(manager) = worker_manager {
                if let Some(delivery) = on_complete {
                    let delivery_close_state = Arc::clone(&worker_close_state);
                    let delivery_task_id = worker_task_id.clone();
                    let confirmed: OnBgCompleteFn = Arc::new(move |result, kind| {
                        match delivery(result, kind) {
                            Ok(()) => Ok(()),
                            Err(error) => {
                                delivery_close_state.record_sync_delivery_failure(&delivery_task_id, &error);
                                Err(error)
                            }
                        }
                    });
                    match manager.settle_completed(&worker_task_id, result, confirmed) {
                        Ok(true) => worker_close_state.settle_sync_delivery(&worker_task_id),
                        Ok(false) => {
                            worker_close_state.record_sync_delivery_failure(&worker_task_id, "parent task was already terminal before this delivery");
                        }
                        Err(error) => {
                            worker_close_state.record_sync_delivery_failure(&worker_task_id, &error.to_string());
                            tracing::error!(task_id = %worker_task_id, %error, "sync child parent delivery pending retry");
                            if matches!(error, crate::agent::async_tasks::BackgroundRegistryError::DeliveryFailed { .. }) {
                                let closing = worker_close_state.closing_token();
                                loop {
                                    if manager.snapshot().tasks.iter().any(|task| {
                                        task.task_id == worker_task_id && matches!(task.status.as_str(), "completed" | "failed")
                                    }) {
                                        worker_close_state.settle_sync_delivery(&worker_task_id);
                                        break;
                                    }
                                    tokio::select! {
                                        _ = closing.cancelled() => {
                                            tracing::error!(task_id = %worker_task_id, "sync child delivery remains unconfirmed during close");
                                            break;
                                        }
                                        _ = peri_time::sleep(Duration::from_secs(5)) => {}
                                    }
                                    manager.retry_pending_deliveries();
                                }
                            }
                        }
                    }
                }
            } else if let Some(delivery) = on_complete {
                let mut attempts = 0u32;
                let closing = worker_close_state.closing_token();
                loop {
                    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        delivery(&result, BgTaskKind::Agent)
                    })) {
                        Ok(Ok(())) => {
                            worker_close_state.settle_sync_delivery(&worker_task_id);
                            break;
                        }
                        rejected => {
                            attempts = attempts.saturating_add(1);
                            let error = match rejected {
                                Ok(Err(error)) => error,
                                Err(_) => "completion callback panicked".into(),
                                Ok(Ok(())) => unreachable!(),
                            };
                            tracing::error!(child_thread_id = %worker_thread_id, %error, attempts, "sync child handoff completion delivery failed; retaining result for retry");
                            worker_close_state.record_sync_delivery_failure(&worker_task_id, &error);
                            tokio::select! {
                                _ = closing.cancelled() => {
                                    tracing::error!(task_id = %worker_task_id, "sync child result not delivered before close");
                                    break;
                                }
                                _ = peri_time::sleep(Duration::from_secs(5)) => {}
                            }
                        }
                    }
                }
            }
        }
        let _ = finished_sender.send(outcome);
    }).map_err(|error| format!("sync child worker registration failed: {error}"))?;
    let mut cancel_on_drop = CancelSyncWorkerOnDrop {
        worker: Some(worker.abort_handle()),
        close_state,
        task_id: cancel_task_id,
    };

    tokio::select! {
        biased;
        result = &mut finished => Ok(SyncCompletion::Finished(result.map_err(|_| "sync child worker lost")??)),
        _ = peri_time::sleep(Duration::from_secs(120)) => {
            let pending = host.task_manager.as_ref().map(|manager| {
                manager.pending_handoff_tasks().into_iter().map(|task| {
                    format!("{} ({:?}; owner: {})", task.task_id, task.kind,
                        task.owner_session_id.as_deref().unwrap_or(&child_thread_id))
                }).collect::<Vec<_>>()
            }).unwrap_or_default();
            if pending.is_empty() || !delivery_available {
                return Ok(SyncCompletion::Finished(finished.await.map_err(|_| "sync child worker lost")??));
            }
            let registered = {
                let _settlement = settlement.lock();
                if let Ok(result) = finished.try_recv() {
                    return Ok(SyncCompletion::Finished(result?));
                }
                if let Some(manager) = &parent_manager {
                    let id = task_id.as_deref().ok_or("sync child task ID unavailable")?;
                    match manager.register(BgTaskRegistration {
                        task_id: id.to_owned(),
                        kind: BgTaskKind::Agent,
                        summary: format!("synchronous sub-agent {agent_name}"),
                        kill: None,
                        pid: None,
                    }) {
                        Ok(()) => {
                            handed_off.store(true, Ordering::Release);
                            true
                        }
                        Err(error) => {
                            tracing::error!(%error, task_id = %id, "sync child parent task registration failed; waiting for completion");
                            false
                        }
                    }
                } else if task_id.is_some() {
                    tracing::error!(%child_thread_id, "sync child parent task manager unavailable; waiting for completion");
                    false
                } else {
                    handed_off.store(true, Ordering::Release);
                    true
                }
            };
            if !registered {
                return Ok(SyncCompletion::Finished(finished.await.map_err(|_| "sync child worker lost")??));
            }
            cancel_on_drop.worker = None;
            tracing::warn!(%child_thread_id, pending = ?pending, "sync child execution handed off to session-owned worker");
            Ok(SyncCompletion::Pending(pending))
        },
    }
}

// ─── 同步运行 ────────────────────────────────────────────────────────────────

/// 同步子 agent：当前调用内 run_react_loop，完成后收尾。
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_sync_subagent(
    child_thread_id: &str,
    agent_name: &str,
    cwd: &str,
    max_iterations: usize,
    event_handler: Option<Arc<dyn AgentEventHandler>>,
    on_subagent_start: Option<SubagentLifecycleStart>,
    on_subagent_stop: Option<SubagentLifecycleStop>,
    session_resources: Option<Arc<dyn SessionResources>>,
    register_runtime: Option<RegisterRuntimeFn>,
    deregister_runtime: Option<DeregisterRuntimeFn>,
    langfuse_bridge: Option<Arc<dyn LangfuseBridgeLike>>,
    parent_agent_id: Option<AgentId>,
    parent_tool_call_id: Option<String>,
    v2_ctx: V2SubagentContext,
    session: Arc<Session>,
    mut resume_claim: Option<ResumeClaim>,
    handoff_start: Option<tokio::sync::oneshot::Sender<()>>,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    let agent_name = agent_name.to_string();
    let cwd = cwd.to_string();

    // 启动注册（active_agents，与 DeregisterGuard drop 配对）
    if let Some(register) = &register_runtime {
        register(
            child_thread_id.to_string(),
            (*session.config().cancel_token).clone(),
            "cascade".into(),
        );
    }
    let mut deregister_guard = DeregisterGuard {
        thread_id: child_thread_id.to_string(),
        deregister: deregister_runtime,
    };

    // The resumed claim crosses the first execution await with us. Its Drop
    // records cancellation without duplicating lifecycle Stop/hook delivery.
    if let Some(claim) = &mut resume_claim {
        claim.mark_running(session.clone());
    }
    let stop_resources = if resume_claim.is_some() {
        None // 认领持有终态写入（finish 内定向写状态），此处不重复写。
    } else {
        session_resources
    };

    // 真实子会话身份：hook 载荷 agent_id 的来源（不是 agent 名）。
    if let Some(on_start) = &on_subagent_start {
        on_start(child_thread_id, &agent_name, &cwd);
    }
    emit_subagent_start_v2(
        &v2_ctx.event_bus,
        v2_ctx.context.turn_id(),
        parent_agent_id,
        v2_ctx.agent_id,
        &agent_name,
        false,
        parent_tool_call_id.clone(),
    );
    forward_subagent_start_v1(
        event_handler.as_ref(),
        build_subagent_start_v2(
            v2_ctx.context.turn_id(),
            parent_agent_id,
            v2_ctx.agent_id,
            &agent_name,
            false,
            parent_tool_call_id,
        ),
    );

    // v2 事件转发器：子 EventBus → 父事件 handler（TUI 可见子 agent 工具调用/AI 文本）
    let forwarder_handle = spawn_subagent_event_forwarder_for_completion(
        v2_ctx.event_handles,
        event_handler.clone(),
        langfuse_bridge.clone(),
        child_thread_id.to_string(),
    );
    let child_agent_id = v2_ctx.agent_id;
    let event_bus = v2_ctx.event_bus;

    // 运行 v2 ReAct 循环
    let execution_turn = v2_ctx.context.session.turn.clone();
    let mut loop_result =
        std::panic::AssertUnwindSafe(super::child_runner::run_child_with_handoff_signal(
            v2_ctx.context,
            max_iterations,
            &session,
            handoff_start,
        ))
        .catch_unwind()
        .await
        .unwrap_or_else(|_| {
            LoopResult::Error(crate::error::AgentError::Other(anyhow::anyhow!(
                "sync child execution panicked"
            )))
        });
    if let Err(error) = super::lifecycle::flush_session_history(&session).await {
        loop_result = LoopResult::Error(error);
    }
    if let LoopResult::Error(error) = &loop_result {
        if !execution_turn.cancel_token.is_cancelled()
            && !matches!(error, crate::error::AgentError::Interrupted)
        {
            tracing::error!(
                session_id = %child_thread_id,
                turn_id = %execution_turn.turn_id(),
                agent_name,
                category = error.category_name(),
                error = %error.user_facing_message(),
                causes = ?error.cause_chain(),
                "sync child execution failed"
            );
        }
    }
    let subagent_turn_id = Some(execution_turn.turn_id());
    if let Err(error) = super::close::settle_explicit_close(
        &session,
        matches!(&loop_result, LoopResult::Interrupted),
    )
    .await
    {
        deregister_guard.deregister = None;
        if let Some(mut claim) = resume_claim.take() {
            claim.release().await?.accept()?;
        }
        return Err(error.into());
    }

    // v2 SubagentStop（C3）：一个 emit 点覆盖 Completed / Interrupted / Error 三路
    let mut stop_failure = match &loop_result {
        LoopResult::Error(error) => {
            SubagentFailure::safe_failure_from_error(child_thread_id, error)
        }
        _ => None,
    };
    let (mut stop_result, mut stop_is_error) = match &loop_result {
        LoopResult::Completed => (
            extract_last_ai_text(&session)
                .chars()
                .take(500)
                .collect::<String>(),
            false,
        ),
        LoopResult::Interrupted => ("interrupted".to_string(), true),
        LoopResult::Error(e) => (
            format!(
                "{} execution failed: {}",
                agent_name,
                e.user_facing_message()
            )
            .chars()
            .take(500)
            .collect::<String>(),
            true,
        ),
    };
    // v2 SubagentStop（C3）：一个 emit 点覆盖 Completed / Interrupted / Error 三路。
    // 恢复路径复用本 Stop 发射点（R-L4）：stop_result 不含 child_thread_id——
    // issue 验收仅要求工具返回文本与 bg 通知文本携带 thread_id，事件侧不加。
    if let Some(subagent_turn_id) = subagent_turn_id {
        emit_subagent_stop_v2_with_failure(
            &event_bus,
            SubagentStopV2Input {
                turn_id: subagent_turn_id,
                parent_agent_id,
                child_agent_id,
                agent_name: &agent_name,
                result: &stop_result,
                is_error: stop_is_error,
                subagent_failure: stop_failure.clone(),
            },
        );
    }
    // Close the child producer after the terminal v2 event. Awaiting the
    // forwarder drains queued render/observe events (including visible deltas)
    // before the synchronous v1 stop reaches the parent handler.
    if let Err(error) = drain_subagent_events(event_bus, forwarder_handle, langfuse_bridge).await {
        tracing::error!(thread_id = %child_thread_id, error = %error.user_facing_message(), causes = ?error.cause_chain(), "sync child event forwarding barrier incomplete");
        if matches!(loop_result, LoopResult::Completed) {
            stop_result = format!(
                "{agent_name} execution failed: {}",
                error.user_facing_message()
            )
            .chars()
            .take(500)
            .collect();
            stop_is_error = true;
            stop_failure = SubagentFailure::safe_failure_from_error(child_thread_id, &error);
            loop_result = LoopResult::Error(error);
        }
    }
    // v1 协议化载体直发（SubagentStopped）：与 Started 同源（v2 事件构造 +
    // observe_event_to_executor 同步映射），保证 Stopped 在 turn 收尾前到达
    // 父协议化链路（TUI 容器销毁 / depth 配对）。
    if let Some(subagent_turn_id) = subagent_turn_id {
        forward_subagent_stop_v1(
            event_handler.as_ref(),
            build_subagent_stop_v2_with_failure(
                subagent_turn_id,
                parent_agent_id,
                child_agent_id,
                &agent_name,
                &stop_result,
                stop_is_error,
                stop_failure,
            ),
        );
    }

    let (final_text, interrupted) = match loop_result {
        LoopResult::Completed => {
            let text = extract_last_ai_text(&session);
            (text, false)
        }
        LoopResult::Interrupted => (String::new(), true),
        LoopResult::Error(e) => {
            // child_thread_id 前缀：错误路径（LLM 网络错误等）必须可恢复——主 agent
            // 凭返回值中的 thread_id 找回执行现场（与 define.rs 成功路径
            // `child_thread_id: {id}\n{result}` 格式一致，多行展示）
            let failure = SubagentFailure::completed(child_thread_id, &agent_name, e);
            let error_summary = failure.to_string();
            let error_result: String = error_summary.chars().take(500).collect();
            // 统一后处理（hook + thread_store；v1 协议化直发已在 emit_subagent_stop_v2
            // 之后经 forward_subagent_stop_v1 发出）
            on_subagent_stop_handler(
                &on_subagent_stop,
                &stop_resources,
                &agent_name,
                child_thread_id,
                &error_result,
                true,
                &cwd,
            )
            .await;
            if let Some(claim) = resume_claim.take() {
                claim
                    .finish(peri_acp_types::thread::AgentStatus::Error)
                    .await?;
            }
            return Err(Box::new(failure));
        }
    };

    let output_summary: String = if interrupted {
        "interrupted".to_string()
    } else {
        final_text.chars().take(500).collect()
    };
    on_subagent_stop_handler(
        &on_subagent_stop,
        &stop_resources,
        &agent_name,
        child_thread_id,
        &output_summary,
        interrupted,
        &cwd,
    )
    .await;
    if let Some(claim) = resume_claim.take() {
        claim
            .finish(if interrupted {
                peri_acp_types::thread::AgentStatus::Error
            } else {
                peri_acp_types::thread::AgentStatus::Done
            })
            .await?;
    }

    Ok(interrupted)
}
