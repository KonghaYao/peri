use super::*;

// Regression fixtures for the terminal path of the monitor task subscription.
//
// A non-builtin server named `monitor` matches the identity fallback but has no
// obligation to project `_meta["peri/monitor"]`. The subscription loop must
// still deliver the terminal reminder and settle such a task; only the merged
// alert counts are lost.

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

/// Task manager decorator: can fail the first settlement attempt so the
/// subscription retries against the cached terminal observation.
struct RecordingManager {
    inner: Arc<ConcreteTasks>,
    fail_first_settle: bool,
    settle_attempts: AtomicUsize,
}

impl RecordingManager {
    fn new(inner: Arc<ConcreteTasks>, fail_first_settle: bool) -> Self {
        Self {
            inner,
            fail_first_settle,
            settle_attempts: AtomicUsize::new(0),
        }
    }
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

/// 终态无 `_meta`：warn 后只投递不含计数的终态提醒，仍结算并退出订阅循环。
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
    let messages = fixture.queue.drain_all_sync();
    assert_eq!(
        messages.len(),
        1,
        "a missing alert projection must still deliver the terminal reminder: {messages:?}"
    );
    assert_eq!(messages[0].kind, MessageKind::Defer);
    let body = reminder_body(&messages[0]);
    assert!(body.contains("finished with status Completed"), "{body}");
    assert!(
        !body.contains("time(s)"),
        "a missing projection must not invent alert counts: {body}"
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

/// 终态 `_meta` 字段类型错误：与缺失同样只省略计数，不阻塞终态提醒与结算。
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
    let messages = fixture.queue.drain_all_sync();
    assert_eq!(
        messages.len(),
        1,
        "a malformed alert projection must still deliver the terminal reminder: {messages:?}"
    );
    let body = reminder_body(&messages[0]);
    assert!(body.contains("finished with status Completed"), "{body}");
    assert!(
        !body.contains("time(s)"),
        "a malformed projection must not invent alert counts: {body}"
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

/// 终态有新命中：未报告计数并入唯一一条终态提醒。
#[tokio::test]
async fn terminal_alert_is_merged_into_the_terminal_reminder() {
    let fixture = terminal_monitor_fixture(TerminalMeta::Hits(2), false).await;
    wait_settled(&fixture.manager).await;
    wait_subscription_exit(&fixture.owner).await;
    let messages = fixture.queue.drain_all_sync();
    assert_eq!(
        messages.len(),
        1,
        "the terminal round must deliver exactly one reminder: {messages:?}"
    );
    assert_eq!(messages[0].kind, MessageKind::Defer);
    assert_eq!(messages[0].source, MessageSource::McpNotification);
    assert!(messages[0].delivery_id.is_some());
    assert_ne!(
        messages[0].delivery_id,
        Some(monitor_alert_delivery_id("mon-1", 2)),
        "the merged reminder keeps the terminal delivery id, not the alert id"
    );
    let body = reminder_body(&messages[0]);
    assert!(body.contains("mon-1"), "{body}");
    assert!(body.contains("2 time(s)"), "{body}");
    assert!(body.contains("2 hit(s) were not reported"), "{body}");
    assert!(body.contains("finished with status Completed"), "{body}");
    shutdown_fixture(fixture).await;
}

/// 结算失败重试：复用终态缓存并只投递一条终态提醒。
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
        "a retried settlement must not deliver the reminder twice: {messages:?}"
    );
    assert!(messages[0].delivery_id.is_some());
    assert_ne!(
        messages[0].delivery_id,
        Some(monitor_alert_delivery_id("mon-1", 1)),
        "the retried terminal reminder keeps its own delivery id"
    );
    let body = reminder_body(&messages[0]);
    assert!(body.contains("1 time(s)"), "{body}");
    assert!(body.contains("1 hit(s) were not reported"), "{body}");
    shutdown_fixture(fixture).await;
}
