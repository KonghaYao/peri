//! AsyncRouter — unified routing for async results into the Session inbox.
//!
//! L5：自 `peri-acp/src/session/async_router.rs` 物理迁入（仅依赖 peri-acp-types，
//! 干净迁入；ACP 侧保留 re-export 桥）。
//!
//! Replaces the executor's direct push to the raw `v2_message_queue` with a
//! unified path through [`InboxHandle`], so that [`SessionInbox::await_wake`] is
//! properly triggered when the agent is idle.
//!
//! Two routing targets:
//! - **Background task results** (`route_bg_result`): completion notifications
//!   from independent agents or background shells, pushed as `Defer` with a
//!   source derived from [`BgTaskKind`].
//! - **Workflow events** (`route_workflow_event`): completion notifications from
//!   the workflow middleware subscriber, pushed as `Defer` + `MessageSource::WorkflowComplete`.
//!
//! Both use `Defer` semantics: consumed by `drain_all` during the Receive stage
//! (RCRA), or detectable by `drain_for_end` for external callers.

use peri_acp_types::event::BackgroundTaskResult;
use peri_acp_types::messages::{BaseMessage, MessageContent};
use peri_acp_types::session::InboxHandle;
use peri_acp_types::session::MessageSource;
use peri_acp_types::tasks::BgTaskKind;
use peri_acp_types::workflow::PhaseSummary;
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

    /// Route a background task result into the session inbox.
    ///
    /// Converts the [`BackgroundTaskResult`] into a notification string via
    /// [`BackgroundTaskResult::to_notification`] and pushes it as a `Defer`
    /// message. Independent agents use `MessageSource::SubAgentComplete`; shells
    /// use `MessageSource::ShellComplete`; workflow results retain their existing
    /// `MessageSource::WorkflowComplete` source.
    ///
    /// This replaces the executor's direct `v2_message_queue.push(QueuedMessage::new(
    /// Defer, ..., human(result.to_notification())))` — the only difference is
    /// that this path also triggers the inbox wake `Notify`.
    pub fn route_bg_result(&self, result: &BackgroundTaskResult, kind: BgTaskKind) {
        tracing::info!(
            task_id = %result.task_id,
            agent_name = %result.agent_name,
            success = result.success,
            output_len = result.output.len(),
            "[bg-diag] route_bg_result: calling push_defer"
        );
        let msg = BaseMessage::human(MessageContent::text(result.to_notification()));
        let source = match kind {
            BgTaskKind::Agent => MessageSource::SubAgentComplete,
            BgTaskKind::Shell => MessageSource::ShellComplete,
            BgTaskKind::Workflow => MessageSource::WorkflowComplete,
        };
        self.inbox.push_defer(source, msg);
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
    /// `status` 区分 completed / killed / failed 文本——kill/failed 不得显示为
    /// "completed"（幽灵完成事件，issue 2026-08-05）。
    ///
    /// This replaces the executor's direct `notify_queue.push(QueuedMessage::new(
    /// Defer, WorkflowComplete, human(notif_text)))` inside the workflow
    /// notification subscriber task.
    #[allow(clippy::too_many_arguments)]
    pub fn route_workflow_event(
        &self,
        run_id: &str,
        workflow_name: &str,
        status: &str,
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
        let status_word = match status {
            "completed" => "completed",
            "killed" => "killed",
            _ => "failed",
        };
        // 不包裹 <system-reminder>：append_messages_to_transcript 统一包裹所有 Defer/Info
        let notif_text = format!(
            "Workflow '{}' {status_word}. ({}ms, {} agents, {} tool calls)\n\
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
