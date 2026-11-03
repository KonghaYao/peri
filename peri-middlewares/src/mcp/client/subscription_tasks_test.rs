use super::{lost_owner_retry_due, McpClientPool, LOST_ABANDON_ATTEMPTS};
use peri_acp_types::{
    event::{BackgroundTaskResult, ShellOutput},
    mcp::McpSubscriptionPort,
    messages::MessageId,
    monitor::{monitor_alert_delivery_id, MONITOR_META_KEY},
    plugin::ConfigSource,
    session::{
        MessageKind, MessageQueue, MessageSource, QueuedMessage, QueuedPayload, SessionInbox,
    },
    system_reminder::TrustedSystemReminder,
    tasks::{
        BgRegistryEvent, BgShellHandle, BgTaskKind, BgTaskRegistration, ExternalTaskRegistration,
        OnBgCompleteFn, TaskManager, TaskRegistrationTicket, TaskSnapshot, TaskTerminalDelivery,
    },
};
use rmcp::{
    model::{
        DetailedTask, GetTaskParams, GetTaskResult, JsonObject, MetaObject, ServerCapabilities,
        ServerConfig, Task, TaskPayload, TaskStatus,
    },
    service::{RequestContext, RoleServer},
    ServerHandler,
};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

use crate::mcp::client::output_store::tests::Wire;
use crate::mcp::McpTaskOwner;

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

#[tokio::test]
async fn test_unavailable_terminal_settlement_is_retryable() {
    use peri_acp_types::tasks::NoopTaskManager;
    use rmcp::model::{DetailedTask, Task, TaskPayload, TaskStatus};
    let pool = McpClientPool::new_pending();
    let owner = peri_agent::agent::async_tasks::TaskManager::new();
    let ticket = owner
        .register(peri_acp_types::tasks::BgTaskRegistration {
            task_id: "task".into(),
            kind: BgTaskKind::Agent,
            summary: "fixture".into(),
            pid: None,
            kill: None,
        })
        .unwrap();
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
                &ticket,
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
    let messages = inbox.queue_for_test().drain_all_sync();
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
    let messages = inbox.queue_for_test().drain_all_sync();
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
    let messages = inbox.queue_for_test().drain_all_sync();
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
    let messages = inbox.queue_for_test().drain_all_sync();
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
            call_gate: Default::default(),
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
    let ticket = manager.register_external(request).unwrap();
    let public_id = ticket.task_id().to_owned();
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
    assert!(owner_inbox.queue_for_test().drain_all_sync().is_empty());
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
    assert!(owner_inbox.queue_for_test().is_empty_sync());
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
    assert!(owner_inbox.queue_for_test().drain_all_sync().is_empty());
}

/// Close settlement has one shared deadline: a pending external cancel on the
/// first task must not grant every later task its own 20-second window.
#[tokio::test(start_paused = true)]
async fn cancel_session_external_tasks_uses_one_total_deadline() {
    fn pending_registration(owner_task_id: &str) -> ExternalTaskRegistration {
        ExternalTaskRegistration {
            session_id: "session".into(),
            initiator_session_id: Some("session".into()),
            owner_identity: "fixture".into(),
            owner_task_id: owner_task_id.into(),
            kind: BgTaskKind::Mcp,
            summary: "pending cancel".into(),
            started_at: Some("2026-10-24T00:00:00Z".into()),
            cancel: Arc::new(|| Box::pin(std::future::pending::<Result<(), String>>())),
            on_terminal: Arc::new(|_, _| Box::pin(async { Ok(()) })),
        }
    }

    let pool = McpClientPool::new_pending();
    let manager: Arc<dyn TaskManager> =
        Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    pool.bind_session_task_manager("session", &manager);
    for owner_task_id in ["owner-1", "owner-2"] {
        manager
            .register_external(pending_registration(owner_task_id))
            .unwrap();
    }

    let started = tokio::time::Instant::now();
    let error = pool
        .cancel_session_external_tasks("session")
        .await
        .unwrap_err();
    let elapsed = started.elapsed();
    assert!(
        elapsed <= std::time::Duration::from_secs(21),
        "per-task waits must share one budget, elapsed {elapsed:?}"
    );
    assert!(
        elapsed >= std::time::Duration::from_secs(20),
        "the shared budget should be spent before giving up, elapsed {elapsed:?}"
    );
    assert!(error.contains("cancellation timed out"), "{error}");
    assert!(error.contains("cancellation budget exhausted"), "{error}");
    assert!(
        manager.has_unsettled_external(),
        "a timed-out cancel must not fabricate a terminal state"
    );
    assert!(
        manager
            .snapshot()
            .tasks
            .iter()
            .all(|task| task.status == "running"),
        "both tasks stay running after bounded cancellation failure"
    );
}

/// [回归测试] 订阅重试用原票据确认已接纳终态，不查询已淘汰的 UI 快照行。
#[tokio::test]
async fn task_subscription_confirms_terminal_after_snapshot_eviction() {
    use rmcp::model::{DetailedTask, Task, TaskPayload, TaskStatus};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let pool = McpClientPool::new_pending();
    let manager = peri_agent::agent::async_tasks::TaskManager::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let delivered = calls.clone();
    let ticket = manager
        .register_external(ExternalTaskRegistration {
            session_id: "session".into(),
            initiator_session_id: Some("session".into()),
            owner_identity: "server".into(),
            owner_task_id: "remote".into(),
            kind: BgTaskKind::Mcp,
            summary: "remote".into(),
            started_at: Some("2026-01-01T00:00:00Z".into()),
            cancel: Arc::new(|| Box::pin(async { Ok(()) })),
            on_terminal: Arc::new(move |_, _| {
                delivered.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(()) })
            }),
        })
        .unwrap();
    let task = DetailedTask::new(
        Task::new(
            "remote",
            TaskStatus::Completed,
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:00Z",
        ),
        TaskPayload::Completed {
            result: serde_json::Map::new(),
        },
    );
    pool.deliver_managed_task_status("server", &ticket, "terminal", &task, false, &manager)
        .await
        .unwrap();
    for index in 0..600 {
        let later = manager
            .register(peri_acp_types::tasks::BgTaskRegistration {
                task_id: format!("later-{index}"),
                kind: BgTaskKind::Agent,
                summary: "later".into(),
                pid: None,
                kill: None,
            })
            .unwrap();
        manager.confirm_external_execution_stopped(&later);
        let mut result = shell_result();
        result.task_id = later.task_id().to_owned();
        assert!(manager.complete(&later, result));
    }
    assert!(!manager
        .snapshot()
        .tasks
        .iter()
        .any(|record| record.task_id == ticket.task_id()));
    pool.deliver_managed_task_status("server", &ticket, "terminal", &task, false, &manager)
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

fn handle_with_source(
    name: &str,
    source: Option<ConfigSource>,
) -> Arc<crate::mcp::client::McpClientHandle> {
    Arc::new(crate::mcp::client::McpClientHandle {
        call_gate: Default::default(),
        name: name.into(),
        version: None,
        connected_at: None,
        protocol_version: None,
        cache_version: None,
        peer: None,
        tools: vec![],
        resources: vec![],
        status: crate::mcp::client::ClientStatus::Connected,
        oauth_status: Default::default(),
        source,
        url: None,
        skills_capable: false,
    })
}

/// Register one external task through the production registration path, settle
/// it, and return the owner's queued messages plus the published task status.
async fn settle_registered_task(
    server: &str,
    source: Option<ConfigSource>,
    owner_scoped: bool,
    success: bool,
) -> (Vec<QueuedMessage>, String) {
    let pool = Arc::new(McpClientPool::new_pending());
    let inbox = SessionInbox::new(Arc::new(MessageQueue::new()));
    pool.register_inbox("session", inbox.handle());
    pool.clients
        .write()
        .insert(server.into(), handle_with_source(server, source));
    let manager: Arc<dyn TaskManager> =
        Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    pool.bind_session_task_manager("session", &manager);

    let (manager, request) = pool
        .external_task_registration(
            "session",
            Some("session"),
            None,
            server,
            "remote-1",
            BgTaskKind::Mcp,
            "fixture task",
            owner_scoped,
            "2026-11-01T00:00:00Z",
        )
        .expect("registration");
    let ticket = manager.register_external(request).expect("register");
    let mut result = shell_result();
    result.success = success;
    result.output = if success {
        "Command exited successfully.".into()
    } else {
        "Command exited with status 3.".into()
    };
    assert!(
        manager
            .settle_external(&ticket, "remote-1:terminal", result)
            .await
            .expect("settle"),
        "terminal projection must be committed"
    );
    let status = manager
        .snapshot()
        .tasks
        .into_iter()
        .find(|record| record.task_id == ticket.task_id())
        .map(|record| record.status)
        .expect("terminal task record");
    (inbox.queue_for_test().drain_all_sync(), status)
}

/// B9: monitor 的终态（成功/失败）只更新任务投影，不向 owner 投递任何 reminder。
#[tokio::test]
async fn monitor_terminal_settlement_never_wakes_the_owner() {
    for success in [true, false] {
        let (messages, status) = settle_registered_task(
            "monitor",
            Some(ConfigSource::Builtin {
                instance: "monitor".into(),
            }),
            true,
            success,
        )
        .await;
        assert!(
            messages.is_empty(),
            "monitor terminal settlement must not queue a reminder (success={success}): {messages:?}"
        );
        assert_eq!(
            status,
            if success { "completed" } else { "failed" },
            "terminal projection must still be observable through the task manager"
        );
    }
}

/// B9 回归：非 monitor 实例的终态投递保持不变。
#[tokio::test]
async fn non_monitor_terminal_settlement_still_wakes_the_owner() {
    let (messages, status) = settle_registered_task("workflow", None, false, true).await;
    assert_eq!(status, "completed");
    assert_eq!(
        messages.len(),
        1,
        "non-monitor terminal delivery must be unchanged"
    );
    assert_eq!(messages[0].kind, MessageKind::Defer);
    assert_eq!(messages[0].source, MessageSource::McpNotification);
}

/// B9：身份以 `ConfigSource::Builtin { instance }` 为准，不依赖 server key 字面量。
#[tokio::test]
async fn monitor_instance_alias_terminal_settlement_never_wakes_the_owner() {
    let (messages, status) = settle_registered_task(
        "monitor-alias",
        Some(ConfigSource::Builtin {
            instance: "monitor".into(),
        }),
        true,
        true,
    )
    .await;
    assert!(
        messages.is_empty(),
        "monitor instance identity must suppress terminal delivery regardless of server key"
    );
    assert_eq!(status, "completed");
}
// Regression fixtures for the terminal path of the monitor task subscription.
//
// A non-builtin server named `monitor` matches the identity fallback but has no
// obligation to project `_meta["peri/monitor"]`. The subscription loop must
// still settle such a terminal task; only the alert reminder is lost.

/// Alert projection returned by the fixture's `tasks/get`.
#[derive(Clone, Copy)]
enum TerminalMeta {
    /// No `_meta` at all (name-fallback monitor server without the projection).
    Missing,
    /// `_meta["peri/monitor"]` present but with a wrong field type.
    Malformed,
    /// Healthy projection with `hits` matched events.
    Hits(u64),
}

/// A `tasks/get` server that always reports one completed task.
struct TerminalMonitorServer {
    meta: TerminalMeta,
    polls: Arc<AtomicUsize>,
}

impl ServerHandler for TerminalMonitorServer {
    fn get_info(&self) -> ServerConfig {
        let mut config = ServerConfig::default();
        config.capabilities = ServerCapabilities::builder().enable_tasks().build();
        config
    }

    async fn get_task(
        &self,
        _: GetTaskParams,
        _: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, rmcp::ErrorData> {
        self.polls.fetch_add(1, Ordering::SeqCst);
        let mut result = GetTaskResult::new(DetailedTask::new(
            Task::new(
                "mon-1",
                TaskStatus::Completed,
                "2026-11-01T00:00:00Z",
                "2026-11-01T00:00:01Z",
            ),
            TaskPayload::Completed {
                result: serde_json::Map::new(),
            },
        ));
        let mut fields = JsonObject::new();
        match self.meta {
            TerminalMeta::Missing => {}
            TerminalMeta::Malformed => {
                fields.insert(
                    MONITOR_META_KEY.into(),
                    serde_json::json!({ "alertMatched": "not-a-number" }),
                );
            }
            TerminalMeta::Hits(hits) => {
                fields.insert(
                    MONITOR_META_KEY.into(),
                    serde_json::json!({
                        "alertMatched": hits,
                        "lastAlertSeq": 7,
                        "label": "build"
                    }),
                );
            }
        }
        if !fields.is_empty() {
            result.meta = Some(MetaObject(fields));
        }
        Ok(result)
    }
}

/// Concrete manager behind the decorator.
type ConcreteTasks = peri_agent::agent::async_tasks::TaskManager;

/// Task manager decorator: records whether the alert reminder was already in the
/// inbox when settlement started, and can fail the first settlement attempt.
struct RecordingManager {
    inner: Arc<ConcreteTasks>,
    queue: Arc<MessageQueue>,
    fail_first_settle: bool,
    settle_attempts: AtomicUsize,
    alert_before_settle: AtomicBool,
}

impl RecordingManager {
    fn new(inner: Arc<ConcreteTasks>, queue: Arc<MessageQueue>, fail_first_settle: bool) -> Self {
        Self {
            inner,
            queue,
            fail_first_settle,
            settle_attempts: AtomicUsize::new(0),
            alert_before_settle: AtomicBool::new(false),
        }
    }
}

fn has_monitor_alert(queue: &MessageQueue) -> bool {
    queue.snapshot_sync().iter().any(|message| {
        message.kind == MessageKind::Defer && message.source == MessageSource::McpNotification
    })
}

impl TaskManager for RecordingManager {
    fn register_external(
        &self,
        request: ExternalTaskRegistration,
    ) -> Result<TaskRegistrationTicket, String> {
        <ConcreteTasks as TaskManager>::register_external(self.inner.as_ref(), request)
    }

    fn settle_external(
        &self,
        ticket: &TaskRegistrationTicket,
        terminal_transition_id: &str,
        result: BackgroundTaskResult,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<bool, String>> + Send + '_>>
    {
        if has_monitor_alert(&self.queue) {
            self.alert_before_settle.store(true, Ordering::SeqCst);
        }
        let attempt = self.settle_attempts.fetch_add(1, Ordering::SeqCst);
        if attempt == 0 && self.fail_first_settle {
            return Box::pin(async { Err("fixture: first settlement fails".to_owned()) });
        }
        <ConcreteTasks as TaskManager>::settle_external(
            self.inner.as_ref(),
            ticket,
            terminal_transition_id,
            result,
        )
    }

    fn has_unsettled_external(&self) -> bool {
        <ConcreteTasks as TaskManager>::has_unsettled_external(self.inner.as_ref())
    }

    fn mark_external_running(&self, task_id: &str) -> bool {
        <ConcreteTasks as TaskManager>::mark_external_running(self.inner.as_ref(), task_id)
    }

    fn mark_external_lost(&self, task_id: &str) -> bool {
        <ConcreteTasks as TaskManager>::mark_external_lost(self.inner.as_ref(), task_id)
    }

    fn external_task_ids(&self) -> Vec<String> {
        <ConcreteTasks as TaskManager>::external_task_ids(self.inner.as_ref())
    }

    fn snapshot(&self) -> TaskSnapshot {
        <ConcreteTasks as TaskManager>::snapshot(self.inner.as_ref())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn set_event_sender(
        &self,
        sender: tokio::sync::mpsc::UnboundedSender<BgRegistryEvent>,
        session_id: String,
    ) {
        <ConcreteTasks as TaskManager>::set_event_sender(self.inner.as_ref(), sender, session_id);
    }

    fn active_count(&self) -> usize {
        <ConcreteTasks as TaskManager>::active_count(self.inner.as_ref())
    }

    fn register(&self, request: BgTaskRegistration) -> Result<TaskRegistrationTicket, String> {
        <ConcreteTasks as TaskManager>::register(self.inner.as_ref(), request)
    }

    fn complete(&self, ticket: &TaskRegistrationTicket, result: BackgroundTaskResult) -> bool {
        <ConcreteTasks as TaskManager>::complete(self.inner.as_ref(), ticket, result)
    }

    fn settle_completed(
        &self,
        ticket: &TaskRegistrationTicket,
        result: BackgroundTaskResult,
        delivery: OnBgCompleteFn,
    ) -> Result<bool, String> {
        <ConcreteTasks as TaskManager>::settle_completed(
            self.inner.as_ref(),
            ticket,
            result,
            delivery,
        )
    }

    fn retry_pending_deliveries(&self) -> usize {
        <ConcreteTasks as TaskManager>::retry_pending_deliveries(self.inner.as_ref())
    }

    fn cancel(&self, task_id: &str) -> Result<(), String> {
        <ConcreteTasks as TaskManager>::cancel(self.inner.as_ref(), task_id)
    }

    fn cancel_all(&self) {
        <ConcreteTasks as TaskManager>::cancel_all(self.inner.as_ref());
    }

    fn spawn_shell(
        &self,
        command: String,
        cwd: String,
        timeout_ms: Option<u64>,
        on_bg_complete: Option<OnBgCompleteFn>,
    ) -> Result<BgShellHandle, Box<dyn std::error::Error + Send + Sync>> {
        <ConcreteTasks as TaskManager>::spawn_shell(
            self.inner.as_ref(),
            command,
            cwd,
            timeout_ms,
            on_bg_complete,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn finalize_bg_shell(
        &self,
        on_bg_complete: &Option<OnBgCompleteFn>,
        ticket: TaskRegistrationTicket,
        prompt_summary: String,
        success: bool,
        output: String,
        duration_ms: u64,
        timed_out: bool,
        shell_output: Option<ShellOutput>,
    ) {
        <ConcreteTasks as TaskManager>::finalize_bg_shell(
            self.inner.as_ref(),
            on_bg_complete,
            ticket,
            prompt_summary,
            success,
            output,
            duration_ms,
            timed_out,
            shell_output,
        );
    }
}

struct TerminalMonitorFixture {
    wire: Wire,
    pool: Arc<McpClientPool>,
    owner: McpTaskOwner,
    queue: Arc<MessageQueue>,
    manager: Arc<RecordingManager>,
    polls: Arc<AtomicUsize>,
}

/// Start a terminal monitor task on a real host subscription.
///
/// The connection is intentionally non-builtin and only matches the monitor
/// identity through the reserved name, which is the name-fallback regression.
async fn terminal_monitor_fixture(
    meta: TerminalMeta,
    fail_first_settle: bool,
) -> TerminalMonitorFixture {
    let polls = Arc::new(AtomicUsize::new(0));
    let wire = Wire::connect(TerminalMonitorServer {
        meta,
        polls: Arc::clone(&polls),
    })
    .await;
    let (owner, spawner) = McpTaskOwner::new();
    let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
    wire.install(&pool, "monitor", false);
    let queue = Arc::new(MessageQueue::new());
    pool.register_inbox("session", SessionInbox::new(Arc::clone(&queue)).handle());
    let manager = Arc::new(RecordingManager::new(
        Arc::new(peri_agent::agent::async_tasks::TaskManager::new()),
        Arc::clone(&queue),
        fail_first_settle,
    ));
    let bound: Arc<dyn TaskManager> = Arc::clone(&manager) as Arc<dyn TaskManager>;
    pool.bind_session_task_manager("session", &bound);
    let ticket = pool
        .register_external_task(
            "session",
            Some("session"),
            None,
            "monitor",
            "mon-1",
            BgTaskKind::Mcp,
            "monitor task",
            false,
            "2026-11-01T00:00:00Z",
        )
        .expect("external task registration");
    pool.spawn_managed_task_subscription(
        "monitor".into(),
        "session".into(),
        "mon-1".into(),
        ticket,
        false,
        false,
        wire.client.peer().clone(),
    )
    .expect("subscription admission");
    TerminalMonitorFixture {
        wire,
        pool,
        owner,
        queue,
        manager,
        polls,
    }
}

async fn shutdown_fixture(fixture: TerminalMonitorFixture) {
    let TerminalMonitorFixture {
        wire,
        pool,
        mut owner,
        ..
    } = fixture;
    pool.clients.write().clear();
    wire.close().await;
    drop(pool);
    owner.begin_shutdown();
    let _ = owner.shutdown().await;
}

async fn wait_settled(manager: &RecordingManager) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while TaskManager::has_unsettled_external(manager) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the task subscription must settle the terminal monitor task"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_subscription_exit(owner: &McpTaskOwner) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while owner.active_count() > 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the task subscription must exit after settlement"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn reminder_body(message: &QueuedMessage) -> String {
    match &message.payload {
        QueuedPayload::SystemReminder(reminder) => reminder.as_reminder().body.clone(),
        other => panic!("expected a canonical reminder, got {other:?}"),
    }
}

/// 终态无 `_meta`：warn 后跳过提醒，仍结算并退出订阅循环。
#[tokio::test]
async fn terminal_monitor_without_meta_still_settles() {
    let fixture = terminal_monitor_fixture(TerminalMeta::Missing, false).await;
    wait_settled(&fixture.manager).await;
    wait_subscription_exit(&fixture.owner).await;
    assert_eq!(
        fixture.polls.load(Ordering::SeqCst),
        1,
        "the terminal observation must settle directly instead of re-polling"
    );
    assert!(
        fixture.queue.drain_all_sync().is_empty(),
        "a missing alert projection must not deliver a reminder"
    );
    assert!(
        fixture
            .manager
            .snapshot()
            .tasks
            .iter()
            .all(|task| task.status == "completed"),
        "the terminal projection must still reach the task manager"
    );
    shutdown_fixture(fixture).await;
}

/// 终态 `_meta` 字段类型错误：与缺失同样只跳过提醒，不阻塞结算。
#[tokio::test]
async fn terminal_monitor_with_malformed_meta_still_settles() {
    let fixture = terminal_monitor_fixture(TerminalMeta::Malformed, false).await;
    wait_settled(&fixture.manager).await;
    wait_subscription_exit(&fixture.owner).await;
    assert_eq!(
        fixture.polls.load(Ordering::SeqCst),
        1,
        "a malformed projection must not keep the terminal task polling"
    );
    assert!(
        fixture.queue.drain_all_sync().is_empty(),
        "a malformed alert projection must not deliver a reminder"
    );
    assert!(
        fixture
            .manager
            .snapshot()
            .tasks
            .iter()
            .all(|task| task.status == "completed"),
        "the terminal projection must still reach the task manager"
    );
    shutdown_fixture(fixture).await;
}

/// 终态有新命中：提醒在 `settle_external` 入口前已经进入 inbox。
#[tokio::test]
async fn terminal_alert_enters_the_inbox_before_settlement() {
    let fixture = terminal_monitor_fixture(TerminalMeta::Hits(2), false).await;
    wait_settled(&fixture.manager).await;
    wait_subscription_exit(&fixture.owner).await;
    assert!(
        fixture.manager.alert_before_settle.load(Ordering::SeqCst),
        "the terminal alert must be delivered before settlement starts"
    );
    let messages = fixture.queue.drain_all_sync();
    assert_eq!(messages.len(), 1, "one alert observation: {messages:?}");
    assert_eq!(messages[0].kind, MessageKind::Defer);
    assert_eq!(messages[0].source, MessageSource::McpNotification);
    assert_eq!(
        messages[0].delivery_id,
        Some(monitor_alert_delivery_id("mon-1", 2)),
        "the reminder keeps its canonical delivery id"
    );
    let body = reminder_body(&messages[0]);
    assert!(body.contains("mon-1"), "{body}");
    assert!(body.contains("2 time(s)"), "{body}");
    shutdown_fixture(fixture).await;
}

/// 结算失败重试：第二次结算复用终态缓存，且不重复投递同一提醒。
#[tokio::test]
async fn failed_settlement_retry_does_not_duplicate_the_alert() {
    let fixture = terminal_monitor_fixture(TerminalMeta::Hits(1), true).await;
    wait_settled(&fixture.manager).await;
    wait_subscription_exit(&fixture.owner).await;
    assert_eq!(
        fixture.manager.settle_attempts.load(Ordering::SeqCst),
        2,
        "the first settlement fails and the retry succeeds"
    );
    assert_eq!(
        fixture.polls.load(Ordering::SeqCst),
        1,
        "the retry reuses the cached terminal observation"
    );
    let messages = fixture.queue.drain_all_sync();
    assert_eq!(
        messages.len(),
        1,
        "a retried settlement must not deliver the alert twice: {messages:?}"
    );
    assert_eq!(
        messages[0].delivery_id,
        Some(monitor_alert_delivery_id("mon-1", 1))
    );
    shutdown_fixture(fixture).await;
}
