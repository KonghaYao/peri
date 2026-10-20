use super::{
    lost_owner_retry_due, pending_tasks_for_closed_epoch, McpClientPool, ScopeTaskRow,
    ScopeTaskSnapshot, LOST_ABANDON_ATTEMPTS,
};
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
fn unobservable_owner_reaches_retry_limit_and_resets_after_recovery() {
    let mut attempts = 0;
    for _ in 1..LOST_ABANDON_ATTEMPTS {
        assert!(!lost_owner_retry_due(&mut attempts));
    }
    assert!(lost_owner_retry_due(&mut attempts));
    assert_eq!(attempts, LOST_ABANDON_ATTEMPTS);

    attempts = 0;
    assert!(!lost_owner_retry_due(&mut attempts));
    assert_eq!(attempts, 1);
}

#[test]
fn unobservable_owner_retry_counter_saturates() {
    let mut attempts = usize::MAX;
    assert!(lost_owner_retry_due(&mut attempts));
    assert_eq!(attempts, usize::MAX);
}

#[test]
fn close_reconciliation_rejects_reopened_epoch_before_selecting_cancel_targets() {
    use rmcp::model::{DetailedTask, Task, TaskPayload, TaskStatus};
    let snapshot = |epoch, closing| ScopeTaskSnapshot {
        cursor: 2,
        epoch,
        closing,
        resources_settled: Some(false),
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
fn missing_resource_settlement_cannot_be_inferred_from_empty_directory() {
    let snapshot = ScopeTaskSnapshot {
        cursor: 2,
        epoch: 0,
        closing: true,
        resources_settled: None,
        tasks: vec![],
    };
    assert!(pending_tasks_for_closed_epoch("workspace", 0, snapshot)
        .unwrap_err()
        .contains("Incomplete"));
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
        "shell-remote",
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

// T16/T17：提醒正文必须自足关联完整公开 id、命令、终态与输出证据；
// 远端 id 只能留在 metadata，不得替换公开 id。
#[tokio::test]
async fn async_result_shell_reminder_body_keeps_full_identity_and_evidence() {
    let pool = McpClientPool::new_pending();
    let inbox = SessionInbox::new(Arc::new(MessageQueue::new()));
    pool.register_inbox("session", inbox.handle());
    let mut result = shell_result();
    result.task_id = "mcp-01a2b3c4d5e6f7a8".into();
    result.success = false;
    result.shell_output = Some(Box::new(ShellOutput {
        stdout_path: Some("/tmp/failed.stdout.log".into()),
        stderr_path: Some("/tmp/failed.stderr.log".into()),
        complete: true,
        error: None,
        exit_code: Some(7),
    }));
    pool.deliver_task_reminder(
        "session",
        "session",
        "workspace",
        &result,
        BgTaskKind::Shell,
        MessageId::new(),
        None,
        "shell-remote",
    )
    .await
    .unwrap();
    let messages = inbox.queue().drain_all();
    let QueuedPayload::SystemReminder(reminder) = &messages[0].payload else {
        panic!("reminder")
    };
    let reminder = reminder.as_reminder();
    assert!(reminder.body.contains("task_id: mcp-01a2b3c4d5e6f7a8"));
    assert!(
        !reminder.body.contains("shell-remote"),
        "公开正文不得用远端 id 替换会话侧 id: {}",
        reminder.body
    );
    assert!(reminder.body.contains("command: sleep 1"));
    assert!(reminder.body.contains("state: failed"));
    assert!(reminder.body.contains("exit_code: 7"));
    assert!(reminder.body.contains("stdout: /tmp/failed.stdout.log"));
    assert!(reminder.body.contains("stderr: /tmp/failed.stderr.log"));
    assert_eq!(reminder.summary.as_deref(), Some("Shell task failed"));
    assert_eq!(
        reminder.metadata["remote_task_id"].as_str(),
        Some("shell-remote")
    );
}

// unresolved：body 与标题都不得声称成功或远端已停止。
#[tokio::test]
async fn async_result_unresolved_reminder_reports_unknown_side_effects() {
    let pool = McpClientPool::new_pending();
    let inbox = SessionInbox::new(Arc::new(MessageQueue::new()));
    pool.register_inbox("session", inbox.handle());
    let mut result = shell_result();
    result.success = false;
    result.timed_out = true;
    result.shell_output = None;
    result.output = "The task owner stayed unobservable and the task was abandoned as \
                     unresolved. Its remote side effects are unknown; check the owner directly \
                     before assuming it stopped."
        .into();
    pool.deliver_task_reminder(
        "session",
        "session",
        "workspace",
        &result,
        BgTaskKind::Shell,
        MessageId::new(),
        None,
        "shell-remote",
    )
    .await
    .unwrap();
    let messages = inbox.queue().drain_all();
    let QueuedPayload::SystemReminder(reminder) = &messages[0].payload else {
        panic!("reminder")
    };
    let reminder = reminder.as_reminder();
    assert!(reminder.body.contains("state: unresolved"));
    assert!(reminder.body.contains("结果未知"));
    assert!(reminder.body.contains("副作用未知"));
    assert!(!reminder.body.contains("进程组已终止"));
    assert!(!reminder.body.contains("state: succeeded"));
    assert_eq!(reminder.summary.as_deref(), Some("Shell task unresolved"));
}

// 非 shell 失败同样不得用 "completed" 标题投影成功。
#[tokio::test]
async fn async_result_failed_mcp_reminder_is_not_projected_as_completed() {
    let pool = McpClientPool::new_pending();
    let inbox = SessionInbox::new(Arc::new(MessageQueue::new()));
    pool.register_inbox("session", inbox.handle());
    let mut result = shell_result();
    result.agent_name = "mcp".into();
    result.success = false;
    result.shell_output = None;
    result.output = "remote task failed".into();
    pool.deliver_task_reminder(
        "session",
        "session",
        "remote",
        &result,
        BgTaskKind::Mcp,
        MessageId::new(),
        None,
        "remote-task",
    )
    .await
    .unwrap();
    let messages = inbox.queue().drain_all();
    let QueuedPayload::SystemReminder(reminder) = &messages[0].payload else {
        panic!("reminder")
    };
    let reminder = reminder.as_reminder();
    assert_eq!(reminder.summary.as_deref(), Some("MCP task failed"));
    assert!(!reminder.body.contains("completed"));
}

// [回归测试] 完成提醒必须沿用启动回执的会话侧公共 id（`mcp-...`），
// 不能被远端原始 id（`shell-...`）覆盖；否则任务面板/回执与完成通知无法对应。
#[tokio::test]
async fn terminal_reminder_keeps_the_public_task_id_instead_of_the_remote_shell_id() {
    let pool = Arc::new(McpClientPool::new_pending());
    pool.clients.write().insert(
        "workspace".into(),
        Arc::new(crate::mcp::client::McpClientHandle {
            name: "workspace".into(),
            version: None,
            connected_at: None,
            protocol_version: None,
            cache_version: None,
            peer: None,
            tools: vec![],
            resources: vec![],
            status: crate::mcp::client::ClientStatus::Connected,
            oauth_status: Default::default(),
            source: None,
            url: None,
            skills_capable: false,
        }),
    );
    let manager: Arc<dyn peri_acp_types::tasks::TaskManager> =
        Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    pool.bind_session_task_manager("session", &manager);

    let delivery = RecordingDelivery::new();
    let (manager, request) = pool
        .external_task_registration(
            "session",
            Some("session"),
            Some(delivery.clone() as Arc<dyn TaskTerminalDelivery>),
            "workspace",
            "shell-remote",
            BgTaskKind::Shell,
            "sleep 1",
            false,
            "2026-10-19T00:00:00Z",
        )
        .unwrap();
    let on_terminal = Arc::clone(&request.on_terminal);
    let public_id = manager.register_external(request).unwrap();
    assert!(public_id.starts_with("mcp-"), "{public_id}");

    // `settle_external` 的契约：回调收到的 result 已带会话侧公共 id。
    let mut result = shell_result();
    result.task_id = public_id.clone();
    on_terminal(&result, MessageId::new()).await.unwrap();

    let delivered = delivery.delivered.lock();
    assert_eq!(delivered.len(), 1);
    let metadata = &delivered[0].1.as_reminder().metadata;
    assert_eq!(metadata["task_id"].as_str(), Some(public_id.as_str()));
    assert_eq!(metadata["remote_task_id"].as_str(), Some("shell-remote"));
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
        "shell-remote",
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
            "shell-remote",
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
            "shell-remote",
        )
        .await
        .unwrap_err();
    assert_eq!(error, "canonical reminder commit failed");
    assert!(owner_inbox.queue().drain_all().is_empty());
}
