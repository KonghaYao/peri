use super::{pending_tasks_for_closed_epoch, McpClientPool, ScopeTaskRow, ScopeTaskSnapshot};
use peri_acp_types::{
    event::{BackgroundTaskResult, ShellOutput},
    mcp::McpSubscriptionPort,
    messages::MessageId,
    session::{MessageQueue, MessageSource, QueuedPayload, SessionInbox},
    system_reminder::TrustedSystemReminder,
    tasks::{BgTaskKind, TaskTerminalDelivery},
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
            initiator_session_id: Some("session".into()),
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

#[tokio::test]
async fn test_unavailable_terminal_settlement_is_retryable() {
    use peri_acp_types::tasks::NoopTaskManager;
    use rmcp::model::{DetailedTask, Task, TaskPayload, TaskStatus};
    let pool = McpClientPool::new_pending();
    let task = DetailedTask::new(
        Task::new(
            "owner-task",
            TaskStatus::Completed,
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:00Z",
        ),
        TaskPayload::Completed {
            result: serde_json::Map::new(),
        },
    );
    for transition_id in ["terminal", "terminal"] {
        let error = pool
            .deliver_managed_task_status(
                "server",
                "task",
                transition_id,
                &task,
                false,
                &NoopTaskManager,
            )
            .await
            .unwrap_err();
        assert_eq!(error, "external terminal delivery is unavailable");
    }
}

fn shell_result() -> BackgroundTaskResult {
    BackgroundTaskResult {
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
    }
}

struct RecordingDelivery {
    delivered: parking_lot::Mutex<Vec<(MessageId, TrustedSystemReminder)>>,
}

impl RecordingDelivery {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            delivered: parking_lot::Mutex::new(Vec::new()),
        })
    }
}

impl TaskTerminalDelivery for RecordingDelivery {
    fn deliver<'a>(
        &'a self,
        delivery_id: MessageId,
        reminder: &'a TrustedSystemReminder,
        _source: MessageSource,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            self.delivered.lock().push((delivery_id, reminder.clone()));
            Ok(())
        })
    }
}

#[tokio::test]
async fn workspace_shell_reminder_uses_file_references() {
    let pool = McpClientPool::new_pending();
    let inbox = SessionInbox::new(Arc::new(MessageQueue::new()));
    pool.register_inbox("session", inbox.handle());
    pool.deliver_task_reminder(
        "session",
        "session",
        "workspace",
        &shell_result(),
        BgTaskKind::Shell,
        MessageId::new(),
        None,
    )
    .await
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

// [回归测试] 子会话发起的任务，终态提醒必须投给发起者而不是 root；
// root 的收件箱不得收到（历史故障：回执承诺投递发起者、实际只到 root）。
#[tokio::test]
async fn terminal_reminder_is_delivered_to_the_initiator_not_the_task_owner() {
    let pool = McpClientPool::new_pending();
    let owner_inbox = SessionInbox::new(Arc::new(MessageQueue::new()));
    pool.register_inbox("root", owner_inbox.handle());
    let delivery = RecordingDelivery::new();
    pool.deliver_task_reminder(
        "child",
        "root",
        "workspace",
        &shell_result(),
        BgTaskKind::Shell,
        MessageId::new(),
        Some(&(delivery.clone() as Arc<dyn TaskTerminalDelivery>)),
    )
    .await
    .unwrap();
    let delivered = delivery.delivered.lock();
    assert_eq!(delivered.len(), 1);
    let metadata = &delivered[0].1.as_reminder().metadata;
    assert_eq!(metadata["initiator"], "child");
    assert_eq!(metadata["task_owner"], "root");
    assert_eq!(metadata["delivery"], "initiator");
    assert!(owner_inbox.queue().drain_all().is_empty());
}

#[tokio::test]
async fn unknown_initiator_cannot_register_or_deliver_to_root() {
    let pool = Arc::new(McpClientPool::new_pending());
    let owner_inbox = SessionInbox::new(Arc::new(MessageQueue::new()));
    pool.register_inbox("root", owner_inbox.handle());
    let error = pool
        .register_external_task(
            "root",
            None,
            None,
            "workspace",
            "task",
            BgTaskKind::Shell,
            "summary",
            false,
            "2026-10-05T00:00:00Z",
        )
        .unwrap_err();
    assert!(error.contains("Unroutable"), "{error}");
    assert!(owner_inbox.queue().is_empty());
}

// 回归：投递失败必须报错（任务保持未结清），不能静默当成功。
#[tokio::test]
async fn undeliverable_terminal_reminder_reports_failure() {
    let pool = McpClientPool::new_pending();
    let error = pool
        .deliver_task_reminder(
            "child",
            "root",
            "workspace",
            &shell_result(),
            BgTaskKind::Shell,
            MessageId::new(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(error, "session inbox unavailable");
}

// 回归：canonical 提交失败时不得把提醒落入队列（MQ 入队不算送达）。
#[tokio::test]
async fn failed_canonical_commit_does_not_enqueue_the_reminder() {
    use std::pin::Pin;

    struct FailingDelivery;
    impl TaskTerminalDelivery for FailingDelivery {
        fn deliver<'a>(
            &'a self,
            _delivery_id: MessageId,
            _reminder: &'a TrustedSystemReminder,
            _source: MessageSource,
        ) -> Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
            Box::pin(async { Err("canonical reminder commit failed".into()) })
        }
    }

    let pool = McpClientPool::new_pending();
    let owner_inbox = SessionInbox::new(Arc::new(MessageQueue::new()));
    pool.register_inbox("root", owner_inbox.handle());
    let error = pool
        .deliver_task_reminder(
            "root",
            "root",
            "workspace",
            &shell_result(),
            BgTaskKind::Shell,
            MessageId::new(),
            Some(&(Arc::new(FailingDelivery) as Arc<dyn TaskTerminalDelivery>)),
        )
        .await
        .unwrap_err();
    assert_eq!(error, "canonical reminder commit failed");
    assert!(owner_inbox.queue().drain_all().is_empty());
}
