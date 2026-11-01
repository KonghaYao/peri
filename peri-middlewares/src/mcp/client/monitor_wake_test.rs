//! Host wake-pump behavior against a real in-process monitor instance and a
//! wire-level fake that injects acknowledgement failures.

use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

use peri_acp_types::mcp::McpSubscriptionPort;
use peri_acp_types::plugin::ConfigSource;
use peri_acp_types::ports::McpPoolPort;
use peri_acp_types::session::{
    InboxHandle, MessageKind, MessageQueue, MessageSource, QueuedMessage, SessionInbox,
};
use peri_agent::tools::BaseTool;
use peri_mcp_monitor::MonitorMcpServer;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, CancelTaskParams, ContentBlock,
    GetTaskParams, Implementation, ListToolsResult, PaginatedRequestParams, RequestMetaObject,
    ServerCapabilities, ServerConfig, TaskStatus, Tool,
};
use rmcp::service::{Peer, RequestContext, RoleClient, RoleServer};
use rmcp::{ErrorData, ServerHandler};
use serde_json::json;

use crate::mcp::apps::McpCapabilityProfile;
use crate::mcp::builtin::runtime::{
    spawn_builtin_transport_with_handler, BuiltinInstanceSupervisor, BuiltinServerExit,
    BUILTIN_CONVERGE_TIMEOUT,
};
use crate::mcp::client::{
    serve_client_auto, ClientStatus, McpClientHandle, McpClientPool, McpServiceWrapper,
};
use crate::mcp::current_invocation_fixture::CurrentInvocationFixture;
use crate::mcp::tool_bridge::McpToolBridge;
use crate::mcp::McpTaskOwner;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const CLOSE_TIMEOUT: Duration = Duration::from_millis(500);
const OBSERVE_TIMEOUT: Duration = Duration::from_secs(5);

fn owner_meta(owner: &str) -> RequestMetaObject {
    let mut fields = serde_json::Map::new();
    fields.insert(
        peri_mcp_core::task_owner::TASK_OWNER_META_KEY.into(),
        owner.into(),
    );
    RequestMetaObject(rmcp::model::MetaObject(fields))
}

fn handle(name: &str, peer: Option<Peer<RoleClient>>) -> Arc<McpClientHandle> {
    Arc::new(McpClientHandle {
        call_gate: Default::default(),
        name: name.to_string(),
        version: None,
        connected_at: None,
        protocol_version: None,
        cache_version: None,
        peer,
        tools: Vec::new(),
        resources: Vec::new(),
        status: ClientStatus::Connected,
        oauth_status: Default::default(),
        source: Some(ConfigSource::Builtin {
            instance: name.to_string(),
        }),
        url: None,
        skills_capable: false,
    })
}

fn track_owner(pool: &McpClientPool, owner: &str) {
    pool.monitor_wake.owners.lock().insert(owner.to_string());
}

struct MonitorLink {
    service: McpServiceWrapper,
    supervisor: BuiltinInstanceSupervisor,
    _dir: tempfile::TempDir,
}

impl MonitorLink {
    fn peer(&self) -> Peer<RoleClient> {
        self.service.peer().clone()
    }
}

async fn connect_monitor() -> MonitorLink {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = MonitorMcpServer::new(dir.path().to_string_lossy().into_owned());
    let transport = spawn_builtin_transport_with_handler("monitor", server);
    let (io, supervisor) = transport.into_parts();
    let service = serve_client_auto(io, &McpCapabilityProfile::disabled(), HANDSHAKE_TIMEOUT)
        .await
        .expect("monitor handshake must not time out")
        .expect("monitor handshake must not fail");
    MonitorLink {
        service,
        supervisor,
        _dir: dir,
    }
}

async fn connect_fake(server: FlakyAckServer) -> MonitorLink {
    let dir = tempfile::tempdir().expect("tempdir");
    let transport = spawn_builtin_transport_with_handler("monitor", server);
    let (io, supervisor) = transport.into_parts();
    let service = serve_client_auto(io, &McpCapabilityProfile::disabled(), HANDSHAKE_TIMEOUT)
        .await
        .expect("fake handshake must not time out")
        .expect("fake handshake must not fail");
    MonitorLink {
        service,
        supervisor,
        _dir: dir,
    }
}

fn pool_with_monitor(
    link: &MonitorLink,
    spawner: crate::mcp::McpTaskSpawner,
) -> Arc<McpClientPool> {
    let mut pool = McpClientPool::new_pending_with_spawner(spawner);
    pool.resource_cache = crate::mcp::resource_cache::McpResourceCache::isolated_for_test();
    let pool = Arc::new(pool);
    pool.clients
        .write()
        .insert("monitor".into(), handle("monitor", Some(link.peer())));
    pool
}

async fn shutdown_link(mut link: MonitorLink, pool: Arc<McpClientPool>, mut owner: McpTaskOwner) {
    pool.clients.write().clear();
    let _ = link.service.close_with_timeout(CLOSE_TIMEOUT).await;
    let outcome = link.supervisor.close(BUILTIN_CONVERGE_TIMEOUT).await;
    assert!(
        !matches!(outcome.server, BuiltinServerExit::AbortedAfterTimeout),
        "server task must converge, not abort: {outcome:?}"
    );
    drop(pool);
    owner.begin_shutdown();
    let _ = owner.shutdown().await;
}

fn monitor_tool(name: &str) -> Tool {
    serde_json::from_value(json!({
        "name": name,
        "description": "monitor fixture",
        "inputSchema": {
            "type": "object",
            "properties": {
                "command": { "type": "string" },
                "task_id": { "type": "string" },
                "alert": { "type": "string" },
                "label": { "type": "string" },
                "deadline_ms": { "type": "integer" }
            },
            "required": ["command"]
        }
    }))
    .expect("monitor tool declaration")
}

/// Start a monitor task through the production `McpToolBridge` path, which is
/// also the only production path that wires the wake pump.
async fn start_monitor_via_bridge(
    pool: &Arc<McpClientPool>,
    link: &MonitorLink,
    session_id: &str,
    arguments: serde_json::Value,
) -> String {
    invoke_monitor_via_bridge(pool, link, session_id, "monitor_start", arguments).await
}

async fn invoke_monitor_via_bridge(
    pool: &Arc<McpClientPool>,
    link: &MonitorLink,
    session_id: &str,
    tool_name: &str,
    arguments: serde_json::Value,
) -> String {
    let tool = monitor_tool(tool_name);
    let bridge = McpToolBridge::new("monitor", &tool, handle("monitor", Some(link.peer())))
        .with_output_store(pool, Some(session_id));
    let fixture = CurrentInvocationFixture::new(session_id, tool_name, &[json!({})]).await;
    bridge
        .invoke(arguments, fixture.context(0, "/"))
        .await
        .expect("monitor bridge invocation")
}

/// Read one named id out of the host receipt. Ids are single tokens.
fn receipt_field(receipt: &str, key: &str) -> String {
    let marker = format!("{key}: ");
    let start = receipt
        .find(&marker)
        .unwrap_or_else(|| panic!("receipt must contain {key}: {receipt}"))
        + marker.len();
    let rest = &receipt[start..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c == ',' || c == ')')
        .unwrap_or(rest.len());
    rest[..end].to_string()
}

fn register_bridge_session(
    pool: &Arc<McpClientPool>,
    session_id: &str,
) -> (
    Arc<MessageQueue>,
    Arc<dyn peri_acp_types::tasks::TaskManager>,
) {
    let queue = Arc::new(MessageQueue::new());
    pool.register_inbox(session_id, SessionInbox::new(Arc::clone(&queue)).handle());
    let manager: Arc<dyn peri_acp_types::tasks::TaskManager> =
        Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    pool.bind_session_task_manager(session_id, &manager);
    (queue, manager)
}

async fn monitor_status(peer: &Peer<RoleClient>, owner: &str, task_id: &str) -> TaskStatus {
    let mut params = GetTaskParams::new(task_id);
    params.meta = Some(owner_meta(owner));
    peer.get_task(params)
        .await
        .expect("tasks/get")
        .task
        .status()
}

async fn wait_monitor_terminal(peer: &Peer<RoleClient>, owner: &str, task_id: &str) -> TaskStatus {
    let deadline = tokio::time::Instant::now() + OBSERVE_TIMEOUT;
    loop {
        let status = monitor_status(peer, owner, task_id).await;
        if status.is_terminal() {
            return status;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "monitor task {task_id} did not reach a terminal state"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn assert_no_reminder(pool: &Arc<McpClientPool>, queue: &MessageQueue, context: &str) {
    for _ in 0..5 {
        pool.poll_monitor_wakes_once().await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        drain(queue).is_empty(),
        "{context} must not deliver a Defer reminder"
    );
}

/// Wait until the host task subscription settled the terminal monitor task.
///
/// The terminal projection (`tasks/get`) can become visible before the host
/// subscription poll (2s interval) settles the task; asserting "no reminder"
/// before settlement would pass even if settlement later delivered one.
async fn wait_settled_external(manager: &Arc<dyn peri_acp_types::tasks::TaskManager>) {
    let deadline = tokio::time::Instant::now() + OBSERVE_TIMEOUT;
    while manager.has_unsettled_external() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the task subscription must settle the terminal task before reminders are asserted"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn start_monitor(peer: &Peer<RoleClient>, owner: &str, label: &str, command: &str) -> String {
    let mut request = CallToolRequestParams::new("monitor_start").with_arguments(
        json!({ "command": command, "alert": "ERROR", "label": label })
            .as_object()
            .cloned()
            .expect("object"),
    );
    request.meta = Some(owner_meta(owner));
    let CallToolResponse::Task(created) = peer
        .call_tool_once(request)
        .await
        .expect("monitor_start returns a task")
    else {
        panic!("monitor_start must return a task");
    };
    created.task.task_id
}

async fn read_wakes(peer: &Peer<RoleClient>, owner: &str) -> serde_json::Value {
    let mut request = CallToolRequestParams::new("monitor_wakes")
        .with_arguments(json!({}).as_object().cloned().expect("object"));
    request.meta = Some(owner_meta(owner));
    let CallToolResponse::Complete(result) =
        peer.call_tool_once(request).await.expect("monitor_wakes")
    else {
        panic!("monitor_wakes must return a completion");
    };
    serde_json::from_str(
        result
            .content
            .first()
            .and_then(|block| block.as_text())
            .expect("wakes text")
            .text
            .as_str(),
    )
    .expect("wakes JSON")
}

async fn pump_until(pool: &Arc<McpClientPool>, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + OBSERVE_TIMEOUT;
    loop {
        pool.poll_monitor_wakes_once().await;
        if condition() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn drain(queue: &MessageQueue) -> Vec<QueuedMessage> {
    queue.drain_all()
}

fn reminder_body(message: &QueuedMessage) -> String {
    match &message.payload {
        peri_acp_types::session::QueuedPayload::SystemReminder(reminder) => {
            reminder.as_reminder().body.clone()
        }
        other => panic!("expected a canonical reminder, got {other:?}"),
    }
}

#[tokio::test]
async fn pump_delivers_owner_wake_and_acknowledges_it() {
    let link = connect_monitor().await;
    let (owner, spawner) = McpTaskOwner::new();
    let pool = pool_with_monitor(&link, spawner);
    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(Arc::clone(&queue));
    pool.register_inbox("session-a", inbox.handle());
    track_owner(&pool, "session-a");

    let task_id = start_monitor(
        &link.peer(),
        "session-a",
        "build",
        "echo ok; echo ERROR boom",
    )
    .await;
    assert!(
        pump_until(&pool, || !queue.is_empty()).await,
        "a matching alert must wake the owner inbox"
    );
    let messages = drain(&queue);
    assert_eq!(messages.len(), 1);
    let message = &messages[0];
    assert_eq!(message.kind, MessageKind::Defer);
    assert_eq!(message.source, MessageSource::McpNotification);
    let body = reminder_body(message);
    assert!(
        body.contains(&task_id),
        "wake must carry the raw task id: {body}"
    );
    assert!(
        body.contains("\"build\""),
        "wake must carry the label: {body}"
    );
    assert!(body.contains("seq 2"));
    assert!(body.contains("1 time(s)"));
    assert!(
        !body.contains("ERROR boom"),
        "wake must not carry event text: {body}"
    );
    assert_eq!(
        pool.monitor_wake.metrics.delivered.load(Ordering::Relaxed),
        1
    );

    let page = read_wakes(&link.peer(), "session-a").await;
    assert_eq!(
        page["wakes"],
        json!([]),
        "delivery must acknowledge the wake"
    );

    shutdown_link(link, pool, owner).await;
}

#[tokio::test]
async fn ack_failure_retries_with_the_same_delivery_id() {
    let server = FlakyAckServer::new(1);
    let ack_attempts = Arc::clone(&server.ack_attempts);
    let link = connect_fake(server).await;
    let (owner, spawner) = McpTaskOwner::new();
    let pool = pool_with_monitor(&link, spawner);
    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(Arc::clone(&queue));
    pool.register_inbox("session-a", inbox.handle());
    track_owner(&pool, "session-a");

    assert!(pump_until(&pool, || !queue.is_empty()).await);
    let first = drain(&queue);
    assert_eq!(first.len(), 1, "the reminder is pushed before the ack");
    let first_id = first[0].delivery_id.expect("canonical delivery id");
    assert_eq!(ack_attempts.load(Ordering::Relaxed), 1);

    // The server still lists the wake because the ack failed; the pump pushes
    // the same canonical reminder again and retries the ack.
    assert!(pump_until(&pool, || !queue.is_empty()).await);
    let second = drain(&queue);
    assert_eq!(second.len(), 1);
    assert_eq!(
        second[0].delivery_id.expect("canonical delivery id"),
        first_id,
        "retries must reuse the same delivery id"
    );
    assert_eq!(ack_attempts.load(Ordering::Relaxed), 2);
    assert_eq!(
        pool.monitor_wake
            .metrics
            .ack_failures
            .load(Ordering::Relaxed),
        1
    );

    // Once acknowledged, the wake stops being delivered.
    pool.poll_monitor_wakes_once().await;
    assert!(drain(&queue).is_empty());
    shutdown_link(link, pool, owner).await;
}

#[tokio::test]
async fn offline_owner_is_acked_and_counted_without_delivery() {
    let link = connect_monitor().await;
    let (owner, spawner) = McpTaskOwner::new();
    let pool = pool_with_monitor(&link, spawner);
    track_owner(&pool, "child");
    let task_id = start_monitor(&link.peer(), "child", "offline", "echo ERROR gone").await;

    assert!(
        pump_until(&pool, || pool
            .monitor_wake
            .metrics
            .offline
            .load(Ordering::Relaxed)
            > 0)
        .await,
        "an owner without an inbox must be acknowledged offline"
    );
    assert_eq!(
        pool.monitor_wake.metrics.delivered.load(Ordering::Relaxed),
        0
    );
    let page = read_wakes(&link.peer(), "child").await;
    assert_eq!(page["wakes"], json!([]), "offline wakes are dropped");
    assert_eq!(
        pool.monitor_wake.metrics.offline.load(Ordering::Relaxed),
        1,
        "task {task_id} wake must be counted once"
    );
    shutdown_link(link, pool, owner).await;
}

#[tokio::test]
async fn each_owner_receives_only_its_own_wake() {
    let link = connect_monitor().await;
    let (owner, spawner) = McpTaskOwner::new();
    let pool = pool_with_monitor(&link, spawner);
    let queue_a = Arc::new(MessageQueue::new());
    let queue_b = Arc::new(MessageQueue::new());
    pool.register_inbox(
        "session-a",
        SessionInbox::new(Arc::clone(&queue_a)).handle(),
    );
    pool.register_inbox(
        "session-b",
        SessionInbox::new(Arc::clone(&queue_b)).handle(),
    );
    track_owner(&pool, "session-a");
    track_owner(&pool, "session-b");
    let task_a = start_monitor(&link.peer(), "session-a", "a", "echo ERROR a").await;
    let task_b = start_monitor(&link.peer(), "session-b", "b", "echo ERROR b").await;

    assert!(
        pump_until(&pool, || !queue_a.is_empty() && !queue_b.is_empty()).await,
        "both owners must receive their own wake"
    );
    let body_a = reminder_body(&drain(&queue_a)[0]);
    let body_b = reminder_body(&drain(&queue_b)[0]);
    assert!(body_a.contains(&task_a) && !body_a.contains(&task_b));
    assert!(body_b.contains(&task_b) && !body_b.contains(&task_a));
    shutdown_link(link, pool, owner).await;
}

#[tokio::test]
async fn released_child_binding_is_offline_and_counted() {
    let link = connect_monitor().await;
    let (owner, spawner) = McpTaskOwner::new();
    let pool = pool_with_monitor(&link, spawner);
    let queue = Arc::new(MessageQueue::new());
    let inbox: InboxHandle = SessionInbox::new(Arc::clone(&queue)).handle();
    let manager: Arc<dyn peri_acp_types::tasks::TaskManager> =
        Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    pool.bind_agent_session("child", inbox, manager);
    assert_eq!(
        pool.session_bindings.read().kind("child"),
        Some(crate::mcp::client::session_bindings::BindingKind::Agent)
    );
    assert!(pool.owner_online("child"));

    let _task_id = start_monitor(&link.peer(), "child", "child", "echo ERROR child").await;
    track_owner(&pool, "child");
    pool.release_agent_session("child").expect("release");
    assert!(!pool.owner_online("child"));

    assert!(
        pump_until(&pool, || pool
            .monitor_wake
            .metrics
            .offline
            .load(Ordering::Relaxed)
            > 0)
        .await,
        "a released child must be treated as offline"
    );
    assert!(
        drain(&queue).is_empty(),
        "released child must not receive a wake"
    );
    assert!(
        pump_until(&pool, || !pool.monitor_wake.owners.lock().contains("child")).await,
        "a released binding must be removed from the pump owner set after the grace window"
    );
    shutdown_link(link, pool, owner).await;
}

#[tokio::test]
async fn missing_monitor_instance_never_starts_the_pump() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
    pool.ensure_monitor_wake_pump("session-a");
    assert!(!pool.monitor_wake.started.load(Ordering::Acquire));
    assert!(!pool.poll_monitor_wakes_once().await);
    owner.shutdown().await;
}

#[tokio::test]
async fn pool_shutdown_stops_the_pump() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
    pool.clients
        .write()
        .insert("monitor".into(), handle("monitor", None));
    pool.ensure_monitor_wake_pump("session-a");
    tokio::time::timeout(OBSERVE_TIMEOUT, async {
        while owner.active_count() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("pump must be admitted");
    assert_eq!(owner.active_count(), 1);
    owner.begin_shutdown();
    owner.shutdown().await;
    assert_eq!(owner.active_count(), 0, "pool shutdown must stop the pump");
}

#[tokio::test]
async fn registered_owner_without_a_monitor_task_is_not_polled() {
    let link = connect_monitor().await;
    let (owner, spawner) = McpTaskOwner::new();
    let pool = pool_with_monitor(&link, spawner);
    let queue = Arc::new(MessageQueue::new());
    pool.register_inbox("session-a", SessionInbox::new(queue).handle());

    assert!(pool.poll_monitor_wakes_once().await);
    assert!(
        pool.monitor_wake.owners.lock().is_empty(),
        "the pump must track monitor-task owners, not every registered session"
    );
    shutdown_link(link, pool, owner).await;
}

/// B9：通过 production bridge 启动 monitor task 后，泵自动接线；alert 命中
/// 只产生一条 wake，任务终态结算不再追加提醒。
#[tokio::test]
async fn bridge_started_monitor_settles_without_a_terminal_wake() {
    let link = connect_monitor().await;
    let (owner, spawner) = McpTaskOwner::new();
    let pool = pool_with_monitor(&link, spawner);
    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(Arc::clone(&queue));
    pool.register_inbox("session-a", inbox.handle());
    let concrete = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    let manager: Arc<dyn peri_acp_types::tasks::TaskManager> = concrete;
    pool.bind_session_task_manager("session-a", &manager);

    let text = start_monitor_via_bridge(
        &pool,
        &link,
        "session-a",
        json!({ "command": "echo ERROR boom", "alert": "ERROR", "label": "build" }),
    )
    .await;
    assert!(text.contains("Background task started"), "{text}");
    assert!(
        text.contains("Only alert matches wake"),
        "the monitor receipt must not promise terminal delivery: {text}"
    );
    assert!(
        pool.monitor_wake.started.load(Ordering::Acquire),
        "the bridge path must start the wake pump"
    );
    assert!(
        pool.monitor_wake.owners.lock().contains("session-a"),
        "the bridge path must track the monitor task owner"
    );

    assert!(
        pump_until(&pool, || !queue.is_empty()).await,
        "the alert match must wake the owner"
    );
    let first = drain(&queue);
    assert_eq!(first.len(), 1, "one alert match yields one wake: {first:?}");
    assert_eq!(first[0].kind, MessageKind::Defer);
    assert!(
        reminder_body(&first[0]).contains("matched its alert"),
        "the single wake must be the alert wake: {}",
        reminder_body(&first[0])
    );

    let deadline = tokio::time::Instant::now() + OBSERVE_TIMEOUT;
    while manager.has_unsettled_external() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the task subscription must settle the terminal task"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    for _ in 0..5 {
        pool.poll_monitor_wakes_once().await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        drain(&queue).is_empty(),
        "terminal settlement must not add a second wake"
    );
    assert_eq!(
        manager
            .snapshot()
            .tasks
            .iter()
            .map(|task| task.status.as_str())
            .collect::<Vec<_>>(),
        vec!["completed"],
        "the terminal task must remain observable through the task manager"
    );

    shutdown_link(link, pool, owner).await;
}

/// 宿主回执必须同时给出 host `task_id` 与 monitor `monitor_id`；模型用后者即可
/// read/stop，不需要先调 `monitor_list` 猜 raw id。
#[tokio::test]
async fn bridge_receipt_monitor_id_drives_read_list_and_stop() {
    let link = connect_monitor().await;
    let (owner, spawner) = McpTaskOwner::new();
    let pool = pool_with_monitor(&link, spawner);
    let (_queue, _manager) = register_bridge_session(&pool, "session-a");

    let receipt = start_monitor_via_bridge(
        &pool,
        &link,
        "session-a",
        json!({ "command": "echo ready", "label": "ids" }),
    )
    .await;
    let task_id = receipt_field(&receipt, "task_id");
    let monitor_id = receipt_field(&receipt, "monitor_id");
    assert!(task_id.starts_with("mcp-"), "{receipt}");
    assert!(monitor_id.starts_with("monitor-"), "{receipt}");
    assert_ne!(task_id, monitor_id, "{receipt}");

    let wrong = invoke_monitor_via_bridge(
        &pool,
        &link,
        "session-a",
        "monitor_read",
        json!({ "task_id": task_id }),
    )
    .await;
    assert!(
        wrong.contains("unknown task"),
        "the host task_id must not resolve inside the monitor instance: {wrong}"
    );

    let deadline = tokio::time::Instant::now() + OBSERVE_TIMEOUT;
    let page = loop {
        let text = invoke_monitor_via_bridge(
            &pool,
            &link,
            "session-a",
            "monitor_read",
            json!({ "task_id": monitor_id }),
        )
        .await;
        let page: serde_json::Value = serde_json::from_str(&text).expect("read JSON");
        if page["events"]
            .as_array()
            .is_some_and(|events| !events.is_empty())
        {
            break page;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "monitor_read with the receipt monitor_id must observe output: {text}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(page["events"][0]["text"], json!("ready"));

    let listed: serde_json::Value = serde_json::from_str(
        &invoke_monitor_via_bridge(
            &pool,
            &link,
            "session-a",
            "monitor_list",
            json!({ "include_ended": true }),
        )
        .await,
    )
    .expect("list JSON");
    assert!(
        listed["tasks"]
            .as_array()
            .is_some_and(|tasks| tasks.iter().any(|task| task["taskId"] == json!(monitor_id))),
        "monitor_list must report the same monitor id: {listed}"
    );

    let stopped: serde_json::Value = serde_json::from_str(
        &invoke_monitor_via_bridge(
            &pool,
            &link,
            "session-a",
            "monitor_stop",
            json!({ "task_id": monitor_id, "reason": "bridge id test" }),
        )
        .await,
    )
    .expect("stop JSON");
    assert!(
        matches!(
            stopped["status"].as_str(),
            Some("completed") | Some("cancelled")
        ),
        "monitor_stop with the receipt monitor_id must succeed: {stopped}"
    );
    assert!(stopped["cleanup"].is_object(), "{stopped}");

    shutdown_link(link, pool, owner).await;
}

/// 取消终态只更新任务投影，不产生 Defer/reminder。
#[cfg(unix)]
#[tokio::test]
async fn cancelled_monitor_terminal_state_never_wakes_the_owner() {
    let link = connect_monitor().await;
    let (owner, spawner) = McpTaskOwner::new();
    let pool = pool_with_monitor(&link, spawner);
    let (queue, manager) = register_bridge_session(&pool, "session-a");

    let receipt = start_monitor_via_bridge(
        &pool,
        &link,
        "session-a",
        json!({ "command": "sleep 30", "label": "cancel" }),
    )
    .await;
    let monitor_id = receipt_field(&receipt, "monitor_id");
    assert!(
        pool.monitor_wake.started.load(Ordering::Acquire),
        "the bridge path must arm the wake pump before the terminal assertion"
    );
    assert!(
        pool.monitor_wake.owners.lock().contains("session-a"),
        "the pump must track this owner before the terminal assertion"
    );

    let mut cancel = CancelTaskParams::new(&monitor_id);
    cancel.meta = Some(owner_meta("session-a"));
    link.peer()
        .cancel_task(cancel)
        .await
        .expect("tasks/cancel ack");
    assert_eq!(
        wait_monitor_terminal(&link.peer(), "session-a", &monitor_id).await,
        TaskStatus::Cancelled,
        "tasks/cancel must end in the cancelled terminal state"
    );
    wait_settled_external(&manager).await;
    assert_no_reminder(&pool, &queue, "cancelled monitor terminal").await;

    shutdown_link(link, pool, owner).await;
}

/// deadline 终态（failed）只更新任务投影，不产生 Defer/reminder。
#[cfg(unix)]
#[tokio::test]
async fn deadline_monitor_terminal_state_never_wakes_the_owner() {
    let link = connect_monitor().await;
    let (owner, spawner) = McpTaskOwner::new();
    let pool = pool_with_monitor(&link, spawner);
    let (queue, manager) = register_bridge_session(&pool, "session-a");

    let receipt = start_monitor_via_bridge(
        &pool,
        &link,
        "session-a",
        json!({ "command": "sleep 30", "label": "deadline", "deadline_ms": 200 }),
    )
    .await;
    let monitor_id = receipt_field(&receipt, "monitor_id");
    assert!(
        pool.monitor_wake.started.load(Ordering::Acquire),
        "the bridge path must arm the wake pump before the terminal assertion"
    );
    assert!(
        pool.monitor_wake.owners.lock().contains("session-a"),
        "the pump must track this owner before the terminal assertion"
    );

    assert_eq!(
        wait_monitor_terminal(&link.peer(), "session-a", &monitor_id).await,
        TaskStatus::Failed,
        "deadline expiry must end in the failed terminal state"
    );
    wait_settled_external(&manager).await;
    assert_no_reminder(&pool, &queue, "deadline monitor terminal").await;

    shutdown_link(link, pool, owner).await;
}

// ─── Fake monitor server: injects one failed acknowledgement ─────────────────

#[derive(Clone)]
struct FlakyAckServer {
    ack_failures_left: Arc<AtomicUsize>,
    acked: Arc<AtomicBool>,
    ack_attempts: Arc<AtomicUsize>,
}

impl FlakyAckServer {
    fn new(failures: usize) -> Self {
        Self {
            ack_failures_left: Arc::new(AtomicUsize::new(failures)),
            acked: Arc::new(AtomicBool::new(false)),
            ack_attempts: Arc::new(AtomicUsize::new(0)),
        }
    }
}

fn complete_json(value: serde_json::Value) -> CallToolResponse {
    let mut result = CallToolResult::success(vec![ContentBlock::text(value.to_string())]);
    result.structured_content = Some(value);
    CallToolResponse::Complete(result)
}

impl ServerHandler for FlakyAckServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("flaky-monitor", "test"))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(Vec::new()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        match request.name.as_ref() {
            "monitor_wakes" => {
                let wakes = if self.acked.load(Ordering::Acquire) {
                    json!([])
                } else {
                    json!([{
                        "wakeId": "wake-1",
                        "taskId": "task-1",
                        "label": "build",
                        "seq": 4,
                        "matched": 2,
                        "createdAt": "2026-11-01T00:00:00Z"
                    }])
                };
                Ok(complete_json(json!({ "wakes": wakes })))
            }
            "monitor_ack" => {
                self.ack_attempts.fetch_add(1, Ordering::Relaxed);
                let remaining = self.ack_failures_left.load(Ordering::Acquire);
                if remaining > 0 {
                    self.ack_failures_left
                        .store(remaining - 1, Ordering::Release);
                    return Ok(CallToolResponse::Complete(CallToolResult::error(vec![
                        ContentBlock::text("injected ack failure"),
                    ])));
                }
                self.acked.store(true, Ordering::Release);
                Ok(complete_json(json!({ "acked": 1 })))
            }
            other => Err(ErrorData::invalid_params(
                format!("unknown tool: {other}"),
                None,
            )),
        }
    }
}
