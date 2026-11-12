//! End-to-end behavior of monitor tasks on the host task subscription.
//!
//! These tests use the real builtin monitor instance and the production tool
//! bridge: starting a monitor task registers the external task and spawns the
//! subscription loop, which owns `tasks/get` polling and alert delivery.

use std::sync::Arc;
use std::time::Duration;

use peri_acp_types::mcp::McpSubscriptionPort;
use peri_acp_types::monitor::monitor_alert_delivery_id;
use peri_acp_types::plugin::ConfigSource;
use peri_acp_types::ports::McpPoolPort;
use peri_acp_types::session::{
    MessageKind, MessageQueue, MessageSource, QueuedMessage, QueuedPayload, SessionInbox,
};
use peri_acp_types::tasks::TaskManager;
use peri_agent::tools::BaseTool;
use peri_mcp_monitor::MonitorMcpServer;
use rmcp::model::{CancelTaskParams, GetTaskParams, RequestMetaObject, TaskStatus};
use rmcp::service::{Peer, RoleClient};
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
const OBSERVE_TIMEOUT: Duration = Duration::from_secs(10);

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

fn monitor_tool(name: &str) -> rmcp::model::Tool {
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
) -> (Arc<MessageQueue>, Arc<dyn TaskManager>) {
    let queue = Arc::new(MessageQueue::new());
    pool.register_inbox(session_id, SessionInbox::new(Arc::clone(&queue)).handle());
    let manager: Arc<dyn TaskManager> =
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

/// Wait until the host task subscription settled the terminal monitor task.
async fn wait_settled_external(manager: &Arc<dyn TaskManager>) {
    let deadline = tokio::time::Instant::now() + OBSERVE_TIMEOUT;
    while manager.has_unsettled_external() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the task subscription must settle the terminal task"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Wait until the owner no longer registers the finished subscription.
async fn wait_host_registration_release(owner: &McpTaskOwner) {
    let deadline = tokio::time::Instant::now() + OBSERVE_TIMEOUT;
    while owner.active_count() > 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the finished task subscription must release its owner registration"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_reminder(queue: &MessageQueue) -> Vec<QueuedMessage> {
    let deadline = tokio::time::Instant::now() + OBSERVE_TIMEOUT;
    loop {
        let messages = queue.drain_all_sync();
        if !messages.is_empty() {
            return messages;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the task subscription did not deliver an alert reminder"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn drain(queue: &MessageQueue) -> Vec<QueuedMessage> {
    queue.drain_all_sync()
}

fn reminder_body(message: &QueuedMessage) -> String {
    match &message.payload {
        QueuedPayload::SystemReminder(reminder) => reminder.as_reminder().body.clone(),
        other => panic!("expected a canonical reminder, got {other:?}"),
    }
}

#[tokio::test]
async fn receipt_ids_drive_monitor_tools_and_host_id_is_unknown() {
    let link = connect_monitor().await;
    let (owner, spawner) = McpTaskOwner::new();
    let pool = pool_with_monitor(&link, spawner);
    let (queue, manager) = register_bridge_session(&pool, "session-a");

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
    wait_settled_external(&manager).await;
    let messages = drain(&queue);
    assert_eq!(
        messages.len(),
        1,
        "a stopped monitor task wakes its initiator once: {messages:?}"
    );
    assert!(
        reminder_body(&messages[0]).contains(&monitor_id),
        "the terminal reminder identifies the monitor id"
    );

    shutdown_link(link, pool, owner).await;
}

#[tokio::test]
async fn running_monitor_delivers_the_first_hit_without_waiting_for_terminal() {
    let link = connect_monitor().await;
    let (owner, spawner) = McpTaskOwner::new();
    let pool = pool_with_monitor(&link, spawner);
    let (queue, manager) = register_bridge_session(&pool, "session-a");

    let receipt = start_monitor_via_bridge(
        &pool,
        &link,
        "session-a",
        json!({
            "command": "echo ERROR boom; sleep 30",
            "alert": "ERROR",
            "label": "build"
        }),
    )
    .await;
    let monitor_id = receipt_field(&receipt, "monitor_id");

    let messages = wait_for_reminder(&queue).await;
    assert_eq!(messages.len(), 1, "one alert observation: {messages:?}");
    let message = &messages[0];
    assert_eq!(message.kind, MessageKind::Defer);
    assert_eq!(message.source, MessageSource::McpNotification);
    let alert_delivery_id = message.delivery_id;
    assert!(alert_delivery_id.is_some(), "canonical delivery id");
    let body = reminder_body(message);
    assert!(body.contains(&monitor_id), "{body}");
    assert!(body.contains("\"build\""), "{body}");
    assert!(body.contains("1 time(s)"), "{body}");
    assert!(!body.contains("ERROR boom"), "no event text: {body}");

    let mut cancel = CancelTaskParams::new(&monitor_id);
    cancel.meta = Some(owner_meta("session-a"));
    link.peer()
        .cancel_task(cancel)
        .await
        .expect("tasks/cancel ack");
    assert_eq!(
        wait_monitor_terminal(&link.peer(), "session-a", &monitor_id).await,
        TaskStatus::Cancelled
    );
    wait_settled_external(&manager).await;
    let terminal = drain(&queue);
    assert_eq!(
        terminal.len(),
        1,
        "the cancellation wakes the initiator exactly once: {terminal:?}"
    );
    let body = reminder_body(&terminal[0]);
    assert!(body.contains(&monitor_id), "{body}");
    assert!(body.contains("Cancelled"), "{body}");
    assert!(
        !body.contains("time(s)"),
        "the reported hit must not repeat in the terminal reminder: {body}"
    );
    assert_ne!(
        terminal[0].delivery_id, alert_delivery_id,
        "the terminal reminder must not reuse the alert delivery id"
    );

    shutdown_link(link, pool, owner).await;
}

#[tokio::test]
async fn completed_monitor_merges_its_alert_into_the_terminal_reminder() {
    let link = connect_monitor().await;
    let (owner, spawner) = McpTaskOwner::new();
    let pool = pool_with_monitor(&link, spawner);
    let (queue, manager) = register_bridge_session(&pool, "session-a");

    // The hit lands after the first poll (which runs immediately) and the
    // terminal state is observed on the next poll, so the round has no
    // separate running alert to deliver.
    let receipt = start_monitor_via_bridge(
        &pool,
        &link,
        "session-a",
        json!({
            "command": "sleep 1; echo ERROR boom",
            "alert": "ERROR",
            "label": "build"
        }),
    )
    .await;
    let monitor_id = receipt_field(&receipt, "monitor_id");

    wait_settled_external(&manager).await;
    let messages = drain(&queue);
    assert_eq!(
        messages.len(),
        1,
        "a terminal round delivers exactly one reminder: {messages:?}"
    );
    assert_eq!(messages[0].kind, MessageKind::Defer);
    assert_eq!(messages[0].source, MessageSource::McpNotification);
    assert!(messages[0].delivery_id.is_some());
    assert_ne!(
        messages[0].delivery_id,
        Some(monitor_alert_delivery_id(&monitor_id, 1)),
        "the merged reminder keeps the terminal delivery id, not the alert id"
    );
    let body = reminder_body(&messages[0]);
    assert!(body.contains(&monitor_id), "{body}");
    assert!(body.contains("\"build\""), "{body}");
    assert!(body.contains("1 time(s)"), "{body}");
    assert!(body.contains("1 hit(s) were not reported"), "{body}");
    assert!(body.contains("finished with status Completed"), "{body}");
    assert!(!body.contains("ERROR boom"), "no event text: {body}");
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

#[tokio::test]
async fn completed_monitor_without_alert_wakes_the_owner() {
    let link = connect_monitor().await;
    let (owner, spawner) = McpTaskOwner::new();
    let pool = pool_with_monitor(&link, spawner);
    let (queue, manager) = register_bridge_session(&pool, "session-a");

    let receipt = start_monitor_via_bridge(
        &pool,
        &link,
        "session-a",
        json!({ "command": "echo done", "label": "plain" }),
    )
    .await;
    let monitor_id = receipt_field(&receipt, "monitor_id");

    wait_settled_external(&manager).await;
    let messages = drain(&queue);
    assert_eq!(
        messages.len(),
        1,
        "a completed task without alerts still wakes the initiator: {messages:?}"
    );
    let body = reminder_body(&messages[0]);
    assert!(body.contains(&monitor_id), "{body}");
    assert!(body.contains("finished with status Completed"), "{body}");
    assert!(!body.contains("time(s)"), "{body}");

    shutdown_link(link, pool, owner).await;
}

#[tokio::test]
async fn alert_goes_only_to_the_initiating_session() {
    let link = connect_monitor().await;
    let (owner, spawner) = McpTaskOwner::new();
    let pool = pool_with_monitor(&link, spawner);
    let (queue_a, manager_a) = register_bridge_session(&pool, "session-a");
    let (queue_b, _manager_b) = register_bridge_session(&pool, "session-b");

    let receipt = start_monitor_via_bridge(
        &pool,
        &link,
        "session-a",
        json!({ "command": "sleep 1; echo ERROR boom", "alert": "ERROR" }),
    )
    .await;
    let monitor_id = receipt_field(&receipt, "monitor_id");

    wait_settled_external(&manager_a).await;
    let messages = drain(&queue_a);
    assert_eq!(
        messages.len(),
        1,
        "the initiator receives exactly one merged terminal reminder: {messages:?}"
    );
    let body = reminder_body(&messages[0]);
    assert!(body.contains(&monitor_id), "{body}");
    assert!(body.contains("1 time(s)"), "{body}");
    assert!(
        drain(&queue_b).is_empty(),
        "a reminder must never be broadcast to another session"
    );

    shutdown_link(link, pool, owner).await;
}

/// 离线发起会话：运行中告警与终态提醒都丢弃，任务仍能结算。
#[tokio::test]
async fn offline_initiating_session_drops_the_alert_and_still_settles() {
    let link = connect_monitor().await;
    let (owner, spawner) = McpTaskOwner::new();
    let pool = pool_with_monitor(&link, spawner);
    let queue = Arc::new(MessageQueue::new());
    let inbox = SessionInbox::new(Arc::clone(&queue)).handle();
    let manager: Arc<dyn TaskManager> =
        Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    pool.bind_agent_session("child", inbox, queue.as_ref().clone(), Arc::clone(&manager));
    assert_eq!(
        pool.session_bindings.read().kind("child"),
        Some(crate::mcp::client::session_bindings::BindingKind::Agent)
    );

    let receipt = start_monitor_via_bridge(
        &pool,
        &link,
        "child",
        json!({ "command": "echo ERROR boom", "alert": "ERROR" }),
    )
    .await;
    let monitor_id = receipt_field(&receipt, "monitor_id");
    pool.release_agent_session("child").expect("release");
    assert!(!pool.owner_online("child"));

    assert_eq!(
        wait_monitor_terminal(&link.peer(), "child", &monitor_id).await,
        TaskStatus::Completed
    );
    wait_settled_external(&manager).await;
    assert!(
        drain(&queue).is_empty(),
        "an offline initiator must not receive an alert"
    );
    assert!(
        manager
            .snapshot()
            .tasks
            .iter()
            .all(|task| task.status.as_str() == "completed"),
        "the alert drop must not block settlement"
    );

    shutdown_link(link, pool, owner).await;
}

/// 取消终态向发起会话投递一条 cancelled 终态提醒。
#[cfg(unix)]
#[tokio::test]
async fn cancelled_monitor_terminal_state_wakes_the_owner() {
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
    let messages = drain(&queue);
    assert_eq!(
        messages.len(),
        1,
        "cancelled monitor terminal must deliver exactly one reminder: {messages:?}"
    );
    let body = reminder_body(&messages[0]);
    assert!(body.contains(&monitor_id), "{body}");
    assert!(body.contains("Cancelled"), "{body}");
    assert!(!body.contains("time(s)"), "no alert was configured: {body}");

    shutdown_link(link, pool, owner).await;
}

/// deadline 终态（failed）向发起会话投递一条 failed 提醒。
#[cfg(unix)]
#[tokio::test]
async fn deadline_monitor_terminal_state_wakes_the_owner() {
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

    assert_eq!(
        wait_monitor_terminal(&link.peer(), "session-a", &monitor_id).await,
        TaskStatus::Failed,
        "deadline expiry must end in the failed terminal state"
    );
    wait_settled_external(&manager).await;
    let messages = drain(&queue);
    assert_eq!(
        messages.len(),
        1,
        "deadline expiry must deliver exactly one reminder: {messages:?}"
    );
    let body = reminder_body(&messages[0]);
    assert!(body.contains(&monitor_id), "{body}");
    assert!(body.contains("finished with status Failed"), "{body}");
    assert!(!body.contains("time(s)"), "no alert was configured: {body}");

    shutdown_link(link, pool, owner).await;
}

/// 重复完成 / 取消 monitor 任务后，宿主登记必须回到基线，终态提醒不重复。
#[tokio::test]
async fn completed_and_cancelled_monitor_rounds_release_host_registrations() {
    let link = connect_monitor().await;
    let (owner, spawner) = McpTaskOwner::new();
    let pool = pool_with_monitor(&link, spawner);
    let (queue, manager) = register_bridge_session(&pool, "session-a");

    for round in 0..4 {
        let completed = round % 2 == 0;
        let command = if completed { "echo done" } else { "sleep 30" };
        let receipt = start_monitor_via_bridge(
            &pool,
            &link,
            "session-a",
            json!({ "command": command, "label": format!("retention-{round}") }),
        )
        .await;
        let monitor_id = receipt_field(&receipt, "monitor_id");

        if !completed {
            let stopped: serde_json::Value = serde_json::from_str(
                &invoke_monitor_via_bridge(
                    &pool,
                    &link,
                    "session-a",
                    "monitor_stop",
                    json!({ "task_id": monitor_id, "reason": "retention probe" }),
                )
                .await,
            )
            .expect("stop JSON");
            assert!(stopped["cleanup"].is_object(), "{stopped}");
        }

        wait_monitor_terminal(&link.peer(), "session-a", &monitor_id).await;
        wait_settled_external(&manager).await;
        wait_host_registration_release(&owner).await;
        assert_eq!(
            owner.active_count(),
            0,
            "round {round} must return to the host baseline"
        );

        let messages = drain(&queue);
        assert_eq!(
            messages.len(),
            1,
            "round {round} must deliver exactly one terminal reminder: {messages:?}"
        );
        let body = reminder_body(&messages[0]);
        assert!(body.contains(&monitor_id), "{body}");
    }

    shutdown_link(link, pool, owner).await;
}
