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
//! Both use `Defer` semantics: preserved in queue during `drain_for_receive`, consumed
//! + woken by `drain_for_end` when the loop reaches the End stage.

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
        let notif_text = format!(
            "<system-reminder>\n\
            Workflow '{}' completed. ({}ms, {} agents, {} tool calls)\n\
            {}Results saved to .claude/workflow-runs/{}/state.json\n\
            </system-reminder>",
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
mod tests {
    use super::*;
    use peri_agent::agent::session::inbox::SessionInbox;
    use std::sync::Arc;

    fn make_inbox() -> (SessionInbox, InboxHandle) {
        let queue = Arc::new(peri_agent::session::MessageQueue::new());
        let inbox = SessionInbox::new(queue);
        let handle = inbox.handle();
        (inbox, handle)
    }

    fn make_bg_result(task_id: &str, agent_name: &str, output: &str) -> BackgroundTaskResult {
        BackgroundTaskResult {
            task_id: task_id.to_string(),
            agent_name: agent_name.to_string(),
            prompt_summary: "test prompt".to_string(),
            success: true,
            output: output.to_string(),
            tool_calls_count: 3,
            duration_ms: 1500,
            child_thread_id: None,
        }
    }

    #[test]
    fn test_route_bg_result_pushes_defer() {
        let (inbox, handle) = make_inbox();
        let router = AsyncRouter::new(handle);
        let result = make_bg_result("abc123", "test-agent", "done");

        router.route_bg_result(&result);

        assert_eq!(inbox.queue().len(), 1);
        assert!(inbox.queue().has_wake_up(), "Defer should wake the inbox");
    }

    #[test]
    fn test_route_bg_result_uses_subagent_complete_source() {
        let (inbox, handle) = make_inbox();
        let router = AsyncRouter::new(handle);
        let result = make_bg_result("abc123", "test-agent", "done");

        router.route_bg_result(&result);

        let msgs = inbox.queue().drain_for_end().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].source, MessageSource::SubAgentComplete);
    }

    #[test]
    fn test_route_bg_result_notification_text_contains_task_info() {
        let (inbox, handle) = make_inbox();
        let router = AsyncRouter::new(handle);
        let result = make_bg_result("task-12345", "my-agent", "output text");

        router.route_bg_result(&result);

        let msgs = inbox.queue().drain_for_end().unwrap();
        let text = msgs[0].message.content();
        assert!(text.contains("task-12"), "should contain short task_id");
        assert!(text.contains("my-agent"), "should contain agent_name");
        assert!(text.contains("output text"), "should contain output");
    }

    #[test]
    fn test_route_workflow_event_pushes_defer() {
        let (inbox, handle) = make_inbox();
        let router = AsyncRouter::new(handle);

        router.route_workflow_event("wf-run-999", "deploy-pipeline", 5000, 4, 12, &[]);

        assert_eq!(inbox.queue().len(), 1);
        assert!(inbox.queue().has_wake_up(), "Defer should wake the inbox");
    }

    #[test]
    fn test_route_workflow_event_uses_workflow_complete_source() {
        let (inbox, handle) = make_inbox();
        let router = AsyncRouter::new(handle);

        router.route_workflow_event("wf-run-999", "deploy-pipeline", 5000, 4, 12, &[]);

        let msgs = inbox.queue().drain_for_end().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].source, MessageSource::WorkflowComplete);
    }

    #[test]
    fn test_route_workflow_event_notification_format() {
        let (inbox, handle) = make_inbox();
        let router = AsyncRouter::new(handle);

        router.route_workflow_event("wf-run-999", "deploy-pipeline", 5000, 4, 12, &[]);

        let msgs = inbox.queue().drain_for_end().unwrap();
        let text = msgs[0].message.content();
        assert!(text.contains("wf-run-"), "should contain short run_id");
        assert!(
            text.contains("deploy-pipeline"),
            "should contain workflow_name"
        );
        assert!(text.contains("5000ms"), "should contain duration");
        assert!(text.contains("4 agents"), "should contain agent count");
        assert!(
            text.contains("12 tool calls"),
            "should contain tool_calls_count"
        );
    }

    #[test]
    fn test_multiple_routes_accumulate_in_queue() {
        let (inbox, handle) = make_inbox();
        let router = AsyncRouter::new(handle);

        let result1 = make_bg_result("task-1", "agent-a", "output-a");
        let result2 = make_bg_result("task-2", "agent-b", "output-b");
        router.route_bg_result(&result1);
        router.route_workflow_event("wf-3", "test-wf", 100, 1, 2, &[]);
        router.route_bg_result(&result2);

        assert_eq!(inbox.queue().len(), 3);

        let msgs = inbox.queue().drain_for_end().unwrap();
        assert_eq!(msgs[0].source, MessageSource::SubAgentComplete);
        assert_eq!(msgs[1].source, MessageSource::WorkflowComplete);
        assert_eq!(msgs[2].source, MessageSource::SubAgentComplete);
    }
}
