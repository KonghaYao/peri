//! Session workflow notification consumer — Path B (Defer) then Path A (`TaskManager::complete`).
//!
//! Extracted from `exec/executor/agent_build.rs` so ordering invariants (#117) are unit-testable.

use peri_acp_types::event::BackgroundTaskResult;
use peri_acp_types::messages::{BaseMessage, MessageContent};
use peri_acp_types::session::{MessageKind, MessageQueue, MessageSource, QueuedMessage};
use peri_acp_types::tasks::TaskManager;
use peri_acp_types::workflow::WorkflowTaskResult;

use crate::session::async_router::AsyncRouter;

/// Apply a broadcast [`WorkflowTaskResult`]: push Defer (and wake when `router` is set), then
/// complete the background workflow task in [`TaskManager`].
///
/// **Order matters** (#117): `notify_bg.complete` must run only after Defer is queued, so
/// `idle_should_wait` (`active_count > 0`) stays true until the inbox can wake the ReAct loop.
pub fn apply_workflow_task_result(
    task_result: &WorkflowTaskResult,
    router: Option<&AsyncRouter>,
    fallback_queue: Option<&MessageQueue>,
    notify_bg: &dyn TaskManager,
) {
    if let Some(router) = router {
        router.route_workflow_task_result(task_result);
    } else if let Some(fallback_queue) = fallback_queue {
        push_workflow_defer_fallback(fallback_queue, task_result);
    }

    let bg = BackgroundTaskResult {
        task_id: task_result.run_id.clone(),
        agent_name: format!("workflow:{}", task_result.workflow_name),
        prompt_summary: task_result.workflow_name.clone(),
        success: task_result.agent_facing_success(),
        output: format!(
            "Workflow '{}' finished with status {:?} ({}ms, {} agents, {} tool calls). \
             Results in .claude/workflow-runs/{}/state.json",
            task_result.workflow_name,
            task_result.status,
            task_result.duration_ms,
            task_result.agent_count,
            task_result.tool_calls_count,
            task_result.run_id
        ),
        tool_calls_count: task_result.tool_calls_count,
        duration_ms: task_result.duration_ms,
        child_thread_id: None,
        timed_out: false,
    };
    notify_bg.complete(&task_result.run_id, bg);
}

fn push_workflow_defer_fallback(queue: &MessageQueue, task_result: &WorkflowTaskResult) {
    let mut phase_lines = String::new();
    for s in &task_result.phase_summaries {
        let token_info = if s.token_count > 0 {
            format!(", {} tokens", s.token_count)
        } else {
            String::new()
        };
        let dur_info = if let Some(d) = s.duration_ms {
            format!(", {}ms", d)
        } else {
            String::new()
        };
        phase_lines.push_str(&format!(
            "- {}: {} agents{}{}\n",
            s.name, s.agent_count, token_info, dur_info
        ));
    }
    let status_word = task_result.notification_status_phrase();
    let notif_text = format!(
        "Workflow '{}' {status_word}. ({}ms, {} agents, {} tool calls)\n\
        {}Results saved to .claude/workflow-runs/{}/state.json",
        task_result.workflow_name,
        task_result.duration_ms,
        task_result.agent_count,
        task_result.tool_calls_count,
        phase_lines,
        task_result.run_id,
    );
    queue.push(QueuedMessage::new(
        MessageKind::Defer,
        MessageSource::WorkflowComplete,
        BaseMessage::human(MessageContent::text(notif_text)),
    ));
}

#[cfg(test)]
#[path = "workflow_completion_test.rs"]
mod tests;
