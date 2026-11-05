//! Recovery watcher regressions through the real Workspace server and MCP wire.
//!
//! A fresh `TaskManager` + pool with the same owner session models a process
//! restart: the Workspace service still holds the shell, while the client only
//! has `shell_task_list` + `tasks/get`. Completion must settle through the
//! normal external-task path and reach the registered session inbox once.

use std::sync::Arc;
use std::time::Duration;

use peri_acp_types::session::{MessageQueue, MessageSource, QueuedPayload, SessionInbox};
use peri_acp_types::tasks::{BgTaskKind, TaskManager};
use peri_mcp_workspace::WorkspaceMcpServer;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CancelTaskParams, MetaObject, RequestMetaObject,
};
use rmcp::service::{Peer, RoleClient};
use serde_json::json;

use crate::mcp::apps::McpCapabilityProfile;
use crate::mcp::builtin::runtime::{
    spawn_builtin_transport_with_handler, BuiltinInstanceSupervisor, BUILTIN_CONVERGE_TIMEOUT,
};
use crate::mcp::client::{
    serve_client_auto, ClientStatus, McpClientHandle, McpClientPool, McpServiceWrapper,
};
use crate::mcp::config::ConfigSource;
use crate::mcp::McpTaskOwner;
use peri_acp_types::mcp::McpSubscriptionPort;

const SESSION: &str = "session-a";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const OBSERVE_TIMEOUT: Duration = Duration::from_secs(10);

/// A watcher runs every 50 ms in test builds (see `watch_interval`), so a
/// short quiet window is enough to prove "no adoption / no second delivery".
const QUIET_WINDOW: Duration = Duration::from_millis(300);

struct WatchFixture {
    pool: Arc<McpClientPool>,
    owner: McpTaskOwner,
    manager: Arc<dyn TaskManager>,
    queue: Arc<MessageQueue>,
    service: McpServiceWrapper,
    supervisor: BuiltinInstanceSupervisor,
    server: WorkspaceMcpServer,
    peer: Peer<RoleClient>,
    _dir: tempfile::TempDir,
}

async fn watch_fixture() -> WatchFixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = WorkspaceMcpServer::standalone(dir.path().to_string_lossy().into_owned(), false);
    let transport = spawn_builtin_transport_with_handler("workspace", server.clone());
    let (io, supervisor) = transport.into_parts();
    let service = serve_client_auto(io, &McpCapabilityProfile::disabled(), HANDSHAKE_TIMEOUT)
        .await
        .expect("workspace 握手不得超时")
        .expect("workspace 握手不得失败");
    let peer = service.peer().clone();

    let (owner, spawner) = McpTaskOwner::new();
    let mut pool = McpClientPool::new_pending_with_spawner(spawner);
    pool.resource_cache = crate::mcp::resource_cache::McpResourceCache::isolated_for_test();
    let pool = Arc::new(pool);
    pool.clients.write().insert(
        "workspace".to_string(),
        Arc::new(McpClientHandle {
            call_gate: Default::default(),
            name: "workspace".to_string(),
            version: None,
            connected_at: None,
            protocol_version: None,
            cache_version: None,
            peer: Some(peer.clone()),
            tools: Vec::new(),
            resources: Vec::new(),
            status: ClientStatus::Connected,
            oauth_status: Default::default(),
            source: Some(ConfigSource::Builtin {
                instance: "workspace".to_string(),
            }),
            url: None,
            skills_capable: false,
        }),
    );
    let queue = Arc::new(MessageQueue::new());
    pool.register_inbox(SESSION, SessionInbox::new(Arc::clone(&queue)).handle());
    let manager: Arc<dyn TaskManager> =
        Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    pool.bind_session_task_manager(SESSION, &manager);
    WatchFixture {
        pool,
        owner,
        manager,
        queue,
        service,
        supervisor,
        server,
        peer,
        _dir: dir,
    }
}

fn owner_meta(owner: &str) -> RequestMetaObject {
    let mut fields = serde_json::Map::new();
    fields.insert(
        peri_mcp_common::task_owner::TASK_OWNER_META_KEY.into(),
        owner.into(),
    );
    RequestMetaObject(MetaObject(fields))
}

/// Start a shell directly on the Workspace service, exactly as a previous
/// client process would have. This bypasses the host tool bridge.
async fn start_shell(peer: &Peer<RoleClient>, command: &str) -> String {
    let mut spawn = CallToolRequestParams::new("Bash").with_arguments(
        json!({"command": command, "run_in_background": true})
            .as_object()
            .unwrap()
            .clone(),
    );
    spawn.meta = Some(owner_meta(SESSION));
    match peer
        .call_tool_once(spawn)
        .await
        .expect("background shell starts")
    {
        CallToolResponse::Task(created) => created.task.task_id,
        CallToolResponse::Complete(result) => result
            .content
            .iter()
            .filter_map(|block| block.as_text().map(|text| text.text.clone()))
            .collect::<Vec<_>>()
            .join("\n")
            .lines()
            .find_map(|line| line.strip_prefix("task_id: ").map(str::to_owned))
            .expect("receipt carries the task id"),
        other => panic!("unexpected background start response: {other:?}"),
    }
}

async fn cancel_shell(peer: &Peer<RoleClient>, task_id: &str) {
    let mut cancel = CancelTaskParams::new(task_id);
    cancel.meta = Some(owner_meta(SESSION));
    peer.cancel_task(cancel).await.expect("cancel live shell");
}

fn start_watch(pool: &Arc<McpClientPool>) -> tokio_util::sync::CancellationToken {
    let cancel = tokio_util::sync::CancellationToken::new();
    let pool = Arc::clone(pool);
    let token = cancel.clone();
    tokio::spawn(async move {
        pool.watch_workspace_tasks(SESSION, token).await;
    });
    cancel
}

async fn wait_snapshot_tasks(manager: &Arc<dyn TaskManager>, expected: usize) {
    tokio::time::timeout(OBSERVE_TIMEOUT, async {
        while manager.snapshot().tasks.len() != expected {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "task projection did not reach {expected} entries; current: {:?}",
            manager.snapshot().tasks
        )
    });
}

async fn wait_settled(manager: &Arc<dyn TaskManager>) {
    tokio::time::timeout(OBSERVE_TIMEOUT, async {
        while TaskManager::has_unsettled_external(manager.as_ref()) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("adopted task must settle");
}

async fn wait_task_scope_idle(owner: &McpTaskOwner) {
    tokio::time::timeout(OBSERVE_TIMEOUT, async {
        while owner.active_count() != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("task subscriptions must exit after settlement");
}

async fn wait_live_tasks(pool: &Arc<McpClientPool>, present: bool) {
    tokio::time::timeout(OBSERVE_TIMEOUT, async {
        loop {
            let listed = pool.workspace_live_tasks(SESSION).await;
            let peri_acp_types::ports::PeripheralTasks::Known(tasks) = listed else {
                panic!("workspace must answer the owner query: {listed:?}");
            };
            if tasks.is_empty() != present {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("workspace live list must reach the expected state");
}

fn reminders(queue: &Arc<MessageQueue>) -> Vec<(MessageSource, String)> {
    queue
        .drain_all_sync()
        .into_iter()
        .map(|message| {
            let QueuedPayload::SystemReminder(reminder) = &message.payload else {
                panic!("expected a canonical reminder, got {:?}", message.payload);
            };
            (message.source.clone(), reminder.as_reminder().body.clone())
        })
        .collect()
}

async fn shutdown(mut fixture: WatchFixture, cancel: tokio_util::sync::CancellationToken) {
    cancel.cancel();
    fixture.pool.clients.write().clear();
    let _ = fixture
        .service
        .close_with_timeout(Duration::from_millis(500))
        .await;
    let outcome = fixture.supervisor.close(BUILTIN_CONVERGE_TIMEOUT).await;
    assert!(
        !matches!(
            outcome.server,
            crate::mcp::builtin::runtime::BuiltinServerExit::AbortedAfterTimeout
        ),
        "server task 必须收敛，不得 abort: {outcome:?}"
    );
    let _ = fixture.server.shutdown_shell_tasks().await;
    drop(fixture.pool);
    fixture.owner.begin_shutdown();
    let _ = fixture.owner.shutdown().await;
}

/// Downtime live shell is adopted into the current process and its terminal
/// result wakes the owner inbox exactly once through the normal reminder path.
#[tokio::test]
async fn recovered_live_shell_is_adopted_and_delivers_once() {
    let fixture = watch_fixture().await;
    let task_id = start_shell(&fixture.peer, "sleep 30").await;
    wait_live_tasks(&fixture.pool, true).await;

    let cancel = start_watch(&fixture.pool);
    wait_snapshot_tasks(&fixture.manager, 1).await;
    let snapshot = fixture.manager.snapshot();
    assert_eq!(snapshot.tasks[0].kind, BgTaskKind::Shell);
    assert!(
        snapshot.tasks[0].task_id.starts_with("mcp-"),
        "adoption must allocate a host public id: {:?}",
        snapshot.tasks
    );

    cancel_shell(&fixture.peer, &task_id).await;
    wait_settled(&fixture.manager).await;
    wait_task_scope_idle(&fixture.owner).await;

    let delivered = reminders(&fixture.queue);
    assert_eq!(
        delivered.len(),
        1,
        "exactly one terminal reminder: {delivered:?}"
    );
    assert_eq!(delivered[0].0, MessageSource::ShellComplete);
    assert!(
        delivered[0].1.contains(snapshot.tasks[0].task_id.as_str()),
        "reminder must name the host public id: {}",
        delivered[0].1
    );

    tokio::time::sleep(QUIET_WINDOW).await;
    assert!(
        reminders(&fixture.queue).is_empty(),
        "a settled task must not be delivered twice"
    );
    shutdown(fixture, cancel).await;
}

/// A shell that already finished during downtime is absent from
/// `shell_task_list`; recovery must not replay its terminal event.
#[tokio::test]
async fn terminal_shell_from_downtime_is_not_replayed() {
    let fixture = watch_fixture().await;
    start_shell(&fixture.peer, "echo done").await;
    wait_live_tasks(&fixture.pool, false).await;

    let cancel = start_watch(&fixture.pool);
    tokio::time::sleep(QUIET_WINDOW).await;
    assert!(
        fixture.manager.snapshot().tasks.is_empty(),
        "terminal downtime tasks must not be registered: {:?}",
        fixture.manager.snapshot().tasks
    );
    assert!(
        reminders(&fixture.queue).is_empty(),
        "terminal downtime tasks must not be replayed"
    );
    shutdown(fixture, cancel).await;
}

/// Query failure is `Unknown`, not an empty list: no registration and no
/// delivery, so recovery cannot claim the task finished.
#[tokio::test]
async fn workspace_query_failure_delivers_nothing() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
    // A registered but unusable Workspace forces the watcher through the
    // `Unknown` path instead of "no instance" (which skips the query).
    pool.clients.write().insert(
        "workspace".to_string(),
        Arc::new(McpClientHandle {
            call_gate: Default::default(),
            name: "workspace".to_string(),
            version: None,
            connected_at: None,
            protocol_version: None,
            cache_version: None,
            peer: None,
            tools: Vec::new(),
            resources: Vec::new(),
            status: ClientStatus::Disconnected,
            oauth_status: Default::default(),
            source: Some(ConfigSource::Builtin {
                instance: "workspace".to_string(),
            }),
            url: None,
            skills_capable: false,
        }),
    );
    let queue = Arc::new(MessageQueue::new());
    pool.register_inbox(SESSION, SessionInbox::new(Arc::clone(&queue)).handle());
    let manager: Arc<dyn TaskManager> =
        Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    pool.bind_session_task_manager(SESSION, &manager);

    assert!(matches!(
        pool.workspace_live_tasks(SESSION).await,
        peri_acp_types::ports::PeripheralTasks::Unknown { .. }
    ));
    let cancel = start_watch(&pool);
    tokio::time::sleep(QUIET_WINDOW).await;
    assert!(manager.snapshot().tasks.is_empty());
    assert!(reminders(&queue).is_empty());
    cancel.cancel();
    drop(pool);
    owner.begin_shutdown();
    let _ = owner.shutdown().await;
}

/// Two watchers over the same live remote id register/monitor it once and
/// deliver one reminder.
#[tokio::test]
async fn duplicate_watch_adopts_and_delivers_once() {
    let fixture = watch_fixture().await;
    let task_id = start_shell(&fixture.peer, "sleep 30").await;
    wait_live_tasks(&fixture.pool, true).await;

    let first = start_watch(&fixture.pool);
    let second = start_watch(&fixture.pool);
    wait_snapshot_tasks(&fixture.manager, 1).await;
    tokio::time::sleep(QUIET_WINDOW).await;
    assert_eq!(
        fixture.manager.snapshot().tasks.len(),
        1,
        "the same remote id must not create a second registration"
    );

    cancel_shell(&fixture.peer, &task_id).await;
    wait_settled(&fixture.manager).await;
    wait_task_scope_idle(&fixture.owner).await;
    assert_eq!(reminders(&fixture.queue).len(), 1);
    tokio::time::sleep(QUIET_WINDOW).await;
    assert!(
        reminders(&fixture.queue).is_empty(),
        "duplicate watchers must not duplicate the terminal reminder"
    );
    second.cancel();
    shutdown(fixture, first).await;
}

/// Stopping the watcher only stops discovery: it must not cancel the
/// peripheral shell. A task started after the watcher belongs to the normal
/// tool-call path and is intentionally not adopted.
#[tokio::test]
async fn watch_cancel_does_not_cancel_peripheral_shell() {
    let fixture = watch_fixture().await;
    let cancel = start_watch(&fixture.pool);
    tokio::time::sleep(Duration::from_millis(120)).await;

    let task_id = start_shell(&fixture.peer, "sleep 30").await;
    tokio::time::sleep(QUIET_WINDOW).await;
    assert!(
        fixture.manager.snapshot().tasks.is_empty(),
        "tasks started after the watcher are registered by their tool bridge: {:?}",
        fixture.manager.snapshot().tasks
    );

    cancel.cancel();
    tokio::time::sleep(Duration::from_millis(120)).await;
    wait_live_tasks(&fixture.pool, true).await;
    assert!(
        fixture.manager.snapshot().tasks.is_empty(),
        "a stopped watcher must not adopt later work"
    );
    cancel_shell(&fixture.peer, &task_id).await;
    shutdown(fixture, cancel).await;
}

/// The watcher must keep observing a task through its subscription even after
/// it stops scanning; the subscription is the delivery owner, not the scan.
#[tokio::test]
async fn adopted_subscription_survives_watcher_stop() {
    let fixture = watch_fixture().await;
    let task_id = start_shell(&fixture.peer, "sleep 30").await;
    wait_live_tasks(&fixture.pool, true).await;

    let cancel = start_watch(&fixture.pool);
    wait_snapshot_tasks(&fixture.manager, 1).await;
    cancel.cancel();
    tokio::time::sleep(Duration::from_millis(120)).await;

    cancel_shell(&fixture.peer, &task_id).await;
    wait_settled(&fixture.manager).await;
    wait_task_scope_idle(&fixture.owner).await;
    assert_eq!(reminders(&fixture.queue).len(), 1);
    shutdown(fixture, cancel).await;
}
