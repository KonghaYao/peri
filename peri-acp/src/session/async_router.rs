//! AsyncRouter — unified routing for async results into the Session inbox.
//!
//! Replaces the executor's direct push to the raw `v2_message_queue` with a
//! unified path through [`InboxHandle`], so that [`SessionInbox::await_wake`] is
//! properly triggered when the agent is idle.
//!
//! Two routing targets:
//! - **Background SubAgent results** (`route_bg_result`): completion notifications
//!   from `/bg` fork agents, pushed as `Defer` + `MessageSource::SubAgentComplete`.
//! - **Workflow events** (`route_workflow_event`): completion notifications from
//!   the workflow middleware subscriber, pushed as `Defer` + `MessageSource::WorkflowComplete`.
//!
//! Both use `Defer` semantics: consumed by `drain_all` during the Receive stage
//! (RCRA), or detectable by `drain_for_end` for external callers.

use peri_agent::agent::events::BackgroundTaskResult;
use peri_agent::agent::session::inbox::InboxHandle;
use peri_agent::messages::{BaseMessage, MessageContent};
use peri_agent::session::MessageSource;
use peri_workflow::progress::PhaseSummary;
use tracing::debug;

/// Routes async results (bg SubAgent completion, workflow events) into the Session inbox.
///
/// Holds an [`InboxHandle`] which wraps the session-shared `MessageQueue` + wake
/// `Notify`. Every route call pushes a `Defer` message and triggers the wake, so
/// that an idle `run_session_loop` resumes via [`SessionInbox::await_wake`].
#[derive(Clone)]
pub struct AsyncRouter {
    inbox: InboxHandle,
}

impl AsyncRouter {
    /// Create a new AsyncRouter from the given inbox handle.
    ///
    /// The handle is typically obtained from `SessionInbox::handle()` during
    /// session initialization.
    pub fn new(inbox: InboxHandle) -> Self {
        Self { inbox }
    }

    /// Route a background SubAgent result into the session inbox.
    ///
    /// Converts the [`BackgroundTaskResult`] into a notification string via
    /// [`BackgroundTaskResult::to_notification`] and pushes it as a `Defer`
    /// message with `MessageSource::SubAgentComplete`.
    ///
    /// This replaces the executor's direct `v2_message_queue.push(QueuedMessage::new(
    /// Defer, SubAgentComplete, human(result.to_notification())))` — the only
    /// difference is that this path also triggers the inbox wake `Notify`.
    pub fn route_bg_result(&self, result: &BackgroundTaskResult) {
        tracing::info!(
            task_id = %result.task_id,
            agent_name = %result.agent_name,
            success = result.success,
            output_len = result.output.len(),
            "[bg-diag] route_bg_result: calling push_defer"
        );
        let msg = BaseMessage::human(MessageContent::text(result.to_notification()));
        self.inbox.push_defer(MessageSource::SubAgentComplete, msg);
        debug!(
            task_id = %result.task_id,
            agent_name = %result.agent_name,
            success = result.success,
            "AsyncRouter: routed bg SubAgent result to inbox"
        );
    }

    /// Route a workflow completion event into the session inbox.
    ///
    /// Formats the workflow metadata (name, duration, agent count, tool calls)
    /// into a human-readable notification string and pushes it as a `Defer`
    /// message with `MessageSource::WorkflowComplete`.
    ///
    /// This replaces the executor's direct `notify_queue.push(QueuedMessage::new(
    /// Defer, WorkflowComplete, human(notif_text)))` inside the workflow
    /// notification subscriber task.
    pub fn route_workflow_event(
        &self,
        run_id: &str,
        workflow_name: &str,
        duration_ms: u64,
        agent_count: usize,
        tool_calls_count: usize,
        phase_summaries: &[PhaseSummary],
    ) {
        let mut phase_lines = String::new();
        for s in phase_summaries {
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
        // 不包裹 <system-reminder>：append_messages_to_transcript 统一包裹所有 Defer/Info
        let notif_text = format!(
            "Workflow '{}' completed. ({}ms, {} agents, {} tool calls)\n\
            {}Results saved to .claude/workflow-runs/{}/state.json",
            workflow_name, duration_ms, agent_count, tool_calls_count, phase_lines, run_id,
        );
        let msg = BaseMessage::human(MessageContent::text(notif_text));
        self.inbox.push_defer(MessageSource::WorkflowComplete, msg);
        debug!(
            run_id = %run_id,
            workflow_name = %workflow_name,
            "AsyncRouter: routed workflow event to inbox"
        );
    }
}

impl std::fmt::Debug for AsyncRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncRouter").finish()
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "async_router_test.rs"]
mod tests;
