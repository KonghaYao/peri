use super::{pending_tasks_for_closed_epoch, McpClientPool, ScopeTaskRow, ScopeTaskSnapshot};
use peri_acp_types::{
    event::{BackgroundTaskResult, ShellOutput},
    mcp::McpSubscriptionPort,
    session::{MessageQueue, MessageSource, QueuedPayload, SessionInbox},
    tasks::BgTaskKind,
};
use std::sync::Arc;

#[test]
fn close_reconciliation_rejects_reopened_epoch_before_selecting_cancel_targets() {
    use rmcp::model::{DetailedTask, Task, TaskPayload, TaskStatus};
    let snapshot = |epoch, closing| ScopeTaskSnapshot {
        cursor: 2,
        epoch,
        closing,
        tasks: vec![ScopeTaskRow {
            task: DetailedTask::new(
                Task::new(
                    "new-epoch-task",
                    TaskStatus::Working,
                    "2026-01-01T00:00:00Z",
                    "2026-01-01T00:00:00Z",
                ),
                TaskPayload::Working,
            ),
            summary: "new execution".into(),
            terminal_transition_id: None,
        }],
    };
    assert!(pending_tasks_for_closed_epoch("workspace", 0, snapshot(1, false)).is_err());
    assert!(pending_tasks_for_closed_epoch("workspace", 0, snapshot(1, true)).is_err());
    assert_eq!(
        pending_tasks_for_closed_epoch("workspace", 0, snapshot(0, true)).unwrap(),
        vec!["new-epoch-task"]
    );
}

#[test]
fn workspace_shell_reminder_uses_file_references() {
    let pool = McpClientPool::new_pending();
    let inbox = SessionInbox::new(Arc::new(MessageQueue::new()));
    pool.register_inbox("session", inbox.handle());
    let result = BackgroundTaskResult {
        task_id: "mcp-opaque".into(),
        agent_name: "bg-shell".into(),
        prompt_summary: "sleep 1".into(),
        success: true,
        output: "Shell command completed; read output files.".into(),
        tool_calls_count: 0,
        duration_ms: 1000,
        timed_out: false,
        child_thread_id: None,
        subagent_failure: None,
        shell_output: Some(Box::new(ShellOutput {
            stdout_path: Some("/tmp/stdout.log".into()),
            stderr_path: None,
            complete: true,
            error: None,
            exit_code: Some(0),
        })),
    };
    pool.deliver_task_reminder(
        "session",
        "workspace",
        &result,
        BgTaskKind::Shell,
        peri_acp_types::messages::MessageId::new(),
    )
    .unwrap();
    let messages = inbox.queue().drain_all();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].source, MessageSource::ShellComplete);
    let QueuedPayload::SystemReminder(reminder) = &messages[0].payload else {
        panic!("reminder")
    };
    assert!(reminder.as_reminder().body.contains("/tmp/stdout.log"));
    assert!(!reminder.as_reminder().body.contains("structuredContent"));
}
