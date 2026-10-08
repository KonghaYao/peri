use std::sync::Arc;

use super::super::Session;
use crate::agent::stages::{run_react_loop, LoopResult, StageContext};

/// Keep the current child run responsible for its own task directory after a
/// bounded idle handoff. A completed RCRA pass is only a delegation terminal
/// once its child tasks and their required queue messages have settled.
pub(super) async fn run_child_until_terminal(
    context: StageContext,
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
    let mut result = run_react_loop(context.clone(), max_iterations).await;

    loop {
        if !matches!(result, LoopResult::Completed) {
            return result;
        }
        if cancel.is_cancelled() {
            return LoopResult::Interrupted;
        }
        if context.session.queue.has_required_for_run(&execution) {
            let remaining = max_iterations.saturating_sub(context.session.turn.current_step());
            result = run_react_loop(context.clone(), remaining).await;
            continue;
        }
        if manager.active_count() == 0 {
            if activity.has_changed().unwrap_or(false) {
                activity.borrow_and_update();
                continue;
            }
            if context.session.queue.has_required_for_run(&execution) {
                continue;
            }
            return LoopResult::Completed;
        }
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return LoopResult::Interrupted,
            _ = context.session.queue.await_wake_for_run(&execution) => {},
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
