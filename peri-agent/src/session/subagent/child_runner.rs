use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use peri_acp_types::{
    session::{MessageKind, MessagePolicy, MessageSource, QueuedMessage},
    system_reminder::{ReminderCategory, ReminderDelivery, ReminderSeverity},
    tasks::BgTaskKind,
};

use super::super::Session;
use super::types::{ChildHandoffFailure, ChildHandoffReason, ChildHandoffReceipt, ChildRunOutcome};
use super::ParkedChildOwner;
use crate::agent::async_tasks::handoff::ChildSettlementWindow;
use crate::agent::async_tasks::{BackgroundTaskStatus, BgTaskInfo};
use crate::agent::stages::{run_react_loop, LoopResult, StageContext};

fn active_shell_tasks(manager: &crate::agent::async_tasks::TaskManager) -> Vec<BgTaskInfo> {
    let mut tasks: Vec<_> = manager
        .registry()
        .list_tasks_full()
        .into_iter()
        .filter(|task| {
            task.kind == BgTaskKind::Shell
                && matches!(
                    task.status,
                    BackgroundTaskStatus::Running | BackgroundTaskStatus::Completing
                )
        })
        .collect();
    tasks.sort_by(|left, right| left.task_id.cmp(&right.task_id));
    tasks
}

fn owner_of(session: &Arc<Session>) -> Option<Arc<ParkedChildOwner>> {
    session
        .subagent_host()
        .and_then(|host| host.child_owner.clone())
}

async fn has_obligations(
    manager: &crate::agent::async_tasks::TaskManager,
    context: &StageContext,
    execution: &peri_acp_types::session::ExecutionBinding,
) -> bool {
    manager.has_unsettled_tasks()
        || context
            .session
            .queue
            .has_required_for_run(execution)
            .await
            .unwrap_or(false)
        || !context.async_ctx.received_after_deadline.lock().is_empty()
        || !context.async_ctx.unprocessed_tool_calls.lock().is_empty()
}

fn received_after_deadline(context: &StageContext) -> Vec<peri_acp_types::messages::MessageId> {
    context.async_ctx.received_after_deadline.lock().clone()
}

fn pending_act_tool_calls(context: &StageContext) -> Vec<String> {
    context.async_ctx.unprocessed_tool_calls.lock().clone()
}

async fn commit_handoff(
    context: &StageContext,
    owner: &Arc<ParkedChildOwner>,
    reason: ChildHandoffReason,
    unprocessed_tool_calls: Vec<String>,
    handoff_start: &mut Option<tokio::sync::oneshot::Sender<ChildHandoffReceipt>>,
) -> Result<ChildHandoffReceipt, crate::agent::async_tasks::handoff::HandoffCommitError> {
    let receipt = super::commit_child_handoff(
        &context.session,
        owner,
        reason,
        received_after_deadline(context),
        unprocessed_tool_calls,
    )
    .await?;
    if let Some(sender) = handoff_start.take() {
        let _ = sender.send(receipt.clone());
    }
    Ok(receipt)
}

fn handoff_failure(
    cause: Option<crate::error::AgentError>,
    error: crate::agent::async_tasks::handoff::HandoffCommitError,
    receipt: Option<ChildHandoffReceipt>,
) -> ChildRunOutcome {
    ChildRunOutcome::HandoffFailed {
        cause,
        failure: ChildHandoffFailure {
            error,
            receipt,
            flush_error: None,
        },
    }
}

fn missing_handoff_owner() -> ChildRunOutcome {
    handoff_failure(
        None,
        crate::agent::async_tasks::handoff::HandoffCommitError::MissingCapability,
        None,
    )
}

/// Keep the current child run responsible for its own task directory after a
/// bounded idle handoff. A completed RCRA pass is only a delegation terminal
/// once its child tasks and their required queue messages have settled.
pub(super) async fn run_child_until_terminal(
    context: StageContext,
    max_iterations: usize,
    session: &Arc<Session>,
) -> ChildRunOutcome {
    run_child_with_handoff_signal(context, max_iterations, session, None).await
}

pub(super) async fn run_child_with_handoff_signal(
    context: StageContext,
    max_iterations: usize,
    session: &Arc<Session>,
    handoff_start: Option<tokio::sync::oneshot::Sender<ChildHandoffReceipt>>,
) -> ChildRunOutcome {
    // tracker 随 StageContext 一起被循环消费；先保留共享句柄，执行结束后
    // 才能把本轮真实占用快照交给宿主展示（sync/background 共用此入口）。
    let token_tracker = Arc::clone(&context.compact.token_tracker);
    let outcome =
        run_child_with_handoff_signal_inner(context, max_iterations, session, handoff_start).await;
    // 读锁 guard 不能跨 await：先取出快照再落库。
    let tokens = token_tracker.read().estimated_context_tokens();
    persist_context_usage(session, tokens).await;
    outcome
}

/// 只刷新 `threads.context_usage_tokens` 展示快照，不参与终态提交：
/// 写失败只记录，不影响子 Agent 的结算结果；无有效 usage 时写回 NULL。
async fn persist_context_usage(session: &Arc<Session>, tokens: Option<u64>) {
    let Some(thread_id) = session.store().thread_id.clone() else {
        return;
    };
    let Some(resources) = session
        .subagent_host()
        .and_then(|host| host.session_resources.clone())
    else {
        return;
    };
    if let Err(error) = resources
        .set_context_usage(&peri_acp_types::thread::ThreadId::from(thread_id), tokens)
        .await
    {
        tracing::warn!(%error, "child context usage write failed");
    }
}

async fn run_child_with_handoff_signal_inner(
    mut context: StageContext,
    max_iterations: usize,
    session: &Arc<Session>,
    mut handoff_start: Option<tokio::sync::oneshot::Sender<ChildHandoffReceipt>>,
) -> ChildRunOutcome {
    let Some(manager) = session
        .subagent_host()
        .and_then(|host| host.task_manager.clone())
    else {
        return match run_react_loop(context, max_iterations).await {
            LoopResult::Completed => ChildRunOutcome::Settled,
            LoopResult::SettlementDue { .. } => missing_handoff_owner(),
            LoopResult::HookStopped {
                unprocessed_tool_calls,
            } => {
                if unprocessed_tool_calls.is_empty() {
                    ChildRunOutcome::Settled
                } else {
                    missing_handoff_owner()
                }
            }
            LoopResult::Interrupted => ChildRunOutcome::Interrupted,
            LoopResult::Error(error) => ChildRunOutcome::Failed {
                error,
                residual: None,
            },
        };
    };
    let window = context
        .async_ctx
        .child_settlement
        .clone()
        .unwrap_or_else(|| Arc::new(ChildSettlementWindow::default()));
    context.async_ctx.child_settlement = Some(Arc::clone(&window));
    let mut activity = manager.registry().subscribe_activity();
    let execution = context.session.turn.execution_binding();
    let cancel = context.session.turn.cancel_token.clone();
    let hook_stop_requested = Arc::new(AtomicBool::new(false));
    let shell_reminded = Arc::new(AtomicBool::new(false));
    context.async_ctx.hook_stop_requested = Some(Arc::clone(&hook_stop_requested));
    // Child task settlement is owned below. Active tasks must reach the child
    // terminal decision before the general idle wait can consume another bound.
    let original_idle_probe = context.async_ctx.idle_should_wait.take();
    context.async_ctx.idle_should_wait = Some({
        let manager = Arc::clone(&manager);
        Arc::new(move || {
            if manager.has_unsettled_tasks() {
                return false;
            }
            original_idle_probe.as_ref().is_some_and(|probe| probe())
        })
    });
    let mut result = run_react_loop(context.clone(), max_iterations).await;

    loop {
        if cancel.is_cancelled() {
            return ChildRunOutcome::Interrupted;
        }
        let stopped_by_hook = hook_stop_requested.load(Ordering::Acquire);
        if stopped_by_hook {
            shell_reminded.store(true, Ordering::Release);
        }
        let mut hook_stopped_tool_calls: Option<Vec<String>> = None;
        match result {
            LoopResult::Interrupted => return ChildRunOutcome::Interrupted,
            LoopResult::SettlementDue {
                unprocessed_tool_calls,
            } => {
                if unprocessed_tool_calls.is_empty()
                    && !has_obligations(&manager, &context, &execution).await
                {
                    return ChildRunOutcome::Settled;
                }
                let Some(owner) = owner_of(session) else {
                    tracing::error!(
                        child_thread_id = %context.session.agent_id,
                        "handoff_capability_unavailable"
                    );
                    return missing_handoff_owner();
                };
                return match commit_handoff(
                    &context,
                    &owner,
                    ChildHandoffReason::SettlementDeadline,
                    unprocessed_tool_calls,
                    &mut handoff_start,
                )
                .await
                {
                    Ok(receipt) => ChildRunOutcome::HandedOff { receipt },
                    Err(error) => {
                        tracing::error!(%error, "child handoff persistence failed");
                        handoff_failure(None, error, owner.pending_receipt())
                    }
                };
            }
            LoopResult::Error(error) => {
                if matches!(error, crate::error::AgentError::Interrupted) {
                    return ChildRunOutcome::Interrupted;
                }
                let pending_tool_calls = pending_act_tool_calls(&context);
                if pending_tool_calls.is_empty()
                    && !has_obligations(&manager, &context, &execution).await
                {
                    return ChildRunOutcome::Failed {
                        error,
                        residual: None,
                    };
                }
                let Some(owner) = owner_of(session) else {
                    return handoff_failure(
                        Some(error),
                        crate::agent::async_tasks::handoff::HandoffCommitError::MissingCapability,
                        None,
                    );
                };
                return match commit_handoff(
                    &context,
                    &owner,
                    ChildHandoffReason::ExecutionFailed,
                    pending_tool_calls,
                    &mut handoff_start,
                )
                .await
                {
                    Ok(receipt) => ChildRunOutcome::Failed {
                        error,
                        residual: Some(receipt),
                    },
                    Err(commit_error) => {
                        tracing::error!(%commit_error, %error, "child handoff after failure was not committed");
                        handoff_failure(Some(error), commit_error, owner.pending_receipt())
                    }
                };
            }
            LoopResult::HookStopped {
                ref unprocessed_tool_calls,
            } => {
                if !unprocessed_tool_calls.is_empty() {
                    hook_stopped_tool_calls = Some(unprocessed_tool_calls.clone());
                }
            }
            LoopResult::Completed => {}
        }

        if let Some(unprocessed_tool_calls) = hook_stopped_tool_calls {
            let Some(owner) = owner_of(session) else {
                return missing_handoff_owner();
            };
            return match commit_handoff(
                &context,
                &owner,
                ChildHandoffReason::HookStop,
                unprocessed_tool_calls,
                &mut handoff_start,
            )
            .await
            {
                Ok(receipt) => ChildRunOutcome::HandedOff { receipt },
                Err(error) => {
                    tracing::error!(%error, "child hook-stop handoff persistence failed");
                    handoff_failure(None, error, owner.pending_receipt())
                }
            };
        }

        let obligations = has_obligations(&manager, &context, &execution).await;
        if !obligations {
            // 消费最后一次 activity 复核，防止终态与通知竞态被误判为已完成。
            if activity.has_changed().unwrap_or(false) {
                activity.borrow_and_update();
                continue;
            }
            return ChildRunOutcome::Settled;
        }
        if !window.is_started() {
            window.start();
        }
        if window.expired() && obligations {
            let Some(owner) = owner_of(session) else {
                return missing_handoff_owner();
            };
            let reason = if stopped_by_hook {
                ChildHandoffReason::HookStop
            } else {
                ChildHandoffReason::SettlementDeadline
            };
            return match commit_handoff(
                &context,
                &owner,
                reason,
                pending_act_tool_calls(&context),
                &mut handoff_start,
            )
            .await
            {
                Ok(receipt) => ChildRunOutcome::HandedOff { receipt },
                Err(error) => {
                    tracing::error!(%error, "child settlement handoff persistence failed");
                    handoff_failure(None, error, owner.pending_receipt())
                }
            };
        }

        if !stopped_by_hook
            && context
                .session
                .queue
                .has_required_for_run(&execution)
                .await
                .unwrap_or(false)
        {
            let remaining = max_iterations.saturating_sub(context.session.turn.current_step());
            result = run_react_loop(context.clone(), remaining).await;
            continue;
        }
        let shells = active_shell_tasks(&manager);
        if !shells.is_empty() && !shell_reminded.load(Ordering::Acquire) {
            let remaining = max_iterations.saturating_sub(context.session.turn.current_step());
            shell_reminded.store(true, Ordering::Release);
            if remaining > 0 {
                let facts: Vec<_> = shells
                    .iter()
                    .map(|task| {
                        let status = if matches!(task.status, BackgroundTaskStatus::Completing) {
                            "completing"
                        } else {
                            "running"
                        };
                        format!("{} ({status})", task.task_id)
                    })
                    .collect();
                let body = format!(
                    "子会话当前仍持有后台 shell 任务：{}。任务 ID 不是进程 PID。",
                    facts.join(", ")
                );
                let reminder = match crate::session::producer_reminders::try_trusted_reminder(
                    ReminderCategory::Task,
                    "subagent_task_lifecycle",
                    "active_background_shell_before_submit",
                    ReminderSeverity::Info,
                    ReminderDelivery::Required,
                    body,
                    Some("子会话有运行中的后台 shell".into()),
                    serde_json::json!({"tasks": shells.iter().map(|task| {
                        serde_json::json!({"task_id": task.task_id, "status": if matches!(task.status, BackgroundTaskStatus::Completing) { "completing" } else { "running" }})
                    }).collect::<Vec<_>>() }),
                ) {
                    Ok(reminder) => reminder,
                    Err(error) => {
                        return ChildRunOutcome::Failed {
                            error: crate::error::AgentError::Other(anyhow::anyhow!(error)),
                            residual: None,
                        }
                    }
                };
                let message = QueuedMessage::system_reminder(
                    MessageKind::Defer,
                    MessageSource::SystemInjected,
                    reminder,
                )
                .with_policy(MessagePolicy::continue_current_run(execution.clone()));
                if let Err(error) = context.session.queue.enqueue(message).await {
                    tracing::warn!(%error, "child shell reminder enqueue failed");
                }
                result = run_react_loop(context.clone(), remaining).await;
                continue;
            }
            tracing::warn!(task_ids = ?shells.iter().map(|task| &task.task_id).collect::<Vec<_>>(), "child shell reminder skipped because model budget is exhausted");
        }
        if manager.active_count() == 0 && activity.has_changed().unwrap_or(false) {
            activity.borrow_and_update();
            continue;
        }
        let deadline = window
            .deadline()
            .expect("settlement window starts before waiting");
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return ChildRunOutcome::Interrupted,
            _ = context.session.queue.await_wake_for_run(&execution), if !stopped_by_hook => {},
            changed = activity.changed() => {
                if changed.is_err() {
                    return ChildRunOutcome::Failed {
                        error: crate::error::AgentError::Other(
                            anyhow::anyhow!("child task directory activity stream closed"),
                        ),
                        residual: None,
                    };
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                let Some(owner) = owner_of(session) else {
                    return missing_handoff_owner();
                };
                let reason = if stopped_by_hook {
                    ChildHandoffReason::HookStop
                } else {
                    ChildHandoffReason::SettlementDeadline
                };
                return match commit_handoff(
                &context,
                &owner,
                reason,
                pending_act_tool_calls(&context),
                &mut handoff_start,
            )
            .await
            {
                    Ok(receipt) => ChildRunOutcome::HandedOff { receipt },
                    Err(error) => {
                        tracing::error!(%error, "child settlement handoff persistence failed");
                        handoff_failure(None, error, owner.pending_receipt())
                    }
                };
            }
        }
    }
}

#[cfg(test)]
#[path = "child_runner_test.rs"]
mod tests;
