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

/// Keep the current child run responsible for its own task directory after a
/// bounded idle handoff. A completed RCRA pass is only a delegation terminal
/// once its child tasks and their required queue messages have settled.
pub(super) async fn run_child_until_terminal(
    mut context: StageContext,
    max_iterations: usize,
    session: &Arc<Session>,
) -> LoopResult {
    let Some(manager) = session
        .subagent_host()
        .and_then(|host| host.task_manager.clone())
    else {
        return run_react_loop(context, max_iterations).await;
    };
    let mut activity = manager.registry().subscribe_activity();
    let execution = context.session.turn.execution_binding();
    let cancel = context.session.turn.cancel_token.clone();
    let hook_stop_requested = Arc::new(AtomicBool::new(false));
    let shell_reminded = Arc::new(AtomicBool::new(false));
    context.async_ctx.hook_stop_requested = Some(Arc::clone(&hook_stop_requested));
    // Child task settlement is owned below. A live shell must reach the child
    // terminal decision immediately, before the general 120-second idle wait.
    let original_idle_probe = context.async_ctx.idle_should_wait.take();
    context.async_ctx.idle_should_wait = Some({
        let manager = Arc::clone(&manager);
        let shell_reminded = Arc::clone(&shell_reminded);
        Arc::new(move || {
            if !shell_reminded.load(Ordering::Acquire) && !active_shell_tasks(&manager).is_empty() {
                return false;
            }
            original_idle_probe.as_ref().is_some_and(|probe| probe())
        })
    });
    let mut result = run_react_loop(context.clone(), max_iterations).await;

    loop {
        if !matches!(result, LoopResult::Completed) {
            return result;
        }
        if cancel.is_cancelled() {
            return LoopResult::Interrupted;
        }
        let stopped_by_hook = hook_stop_requested.load(Ordering::Acquire);
        if stopped_by_hook {
            shell_reminded.store(true, Ordering::Release);
        }
        if !stopped_by_hook && context.session.queue.has_required_for_run(&execution) {
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
                        return LoopResult::Error(crate::error::AgentError::Other(anyhow::anyhow!(
                            error
                        )))
                    }
                };
                context.session.queue.push(
                    QueuedMessage::system_reminder(
                        MessageKind::Defer,
                        MessageSource::SystemInjected,
                        reminder,
                    )
                    .with_policy(MessagePolicy::continue_current_run(execution.clone())),
                );
                result = run_react_loop(context.clone(), remaining).await;
                continue;
            }
            tracing::warn!(task_ids = ?shells.iter().map(|task| &task.task_id).collect::<Vec<_>>(), "child shell reminder skipped because model budget is exhausted");
        }
        if manager.active_count() == 0 {
            if activity.has_changed().unwrap_or(false) {
                activity.borrow_and_update();
                continue;
            }
            if !stopped_by_hook && context.session.queue.has_required_for_run(&execution) {
                continue;
            }
            return LoopResult::Completed;
        }
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return LoopResult::Interrupted,
            _ = context.session.queue.await_wake_for_run(&execution), if !stopped_by_hook => {},
            changed = activity.changed() => {
                if changed.is_err() {
                    return LoopResult::Error(crate::error::AgentError::Other(
                        anyhow::anyhow!("child task directory activity stream closed"),
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "child_runner_test.rs"]
mod tests;
