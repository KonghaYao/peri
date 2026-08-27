use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

use parking_lot::Mutex;

use peri_acp_types::{command_registry::CommandRegistry, mcp_skills::McpSkillRegistry};
use peri_acp_types::{
    dynamic_mcp::{
        CanonicalDynamicMcpAction, CanonicalDynamicMcpUnloadRequest, DynamicMcpAction,
        DynamicMcpCatalogTool, DynamicMcpConfig, DynamicMcpIncarnationId, DynamicMcpLoadRequest,
        DynamicMcpNotification, DynamicMcpResponse, DynamicMcpStatusRequest,
    },
    ports::{DynamicMcpDeploymentPort, DynamicMcpNotificationSinkPort},
};
use rmcp::model::Tool;

use super::*;
use crate::mcp::dynamic::admission::DynamicMcpAdmissionGate;
use crate::mcp::{client::McpClientHandle, McpTaskOwner};

struct RecordingSink {
    session_id: String,
    notifications: Mutex<Vec<DynamicMcpNotification>>,
    authorization_urls: Mutex<Vec<(DynamicMcpInstanceKey, String, String)>>,
    accept: AtomicBool,
}

impl RecordingSink {
    fn new(session_id: &str) -> Arc<Self> {
        Arc::new(Self {
            session_id: session_id.to_string(),
            notifications: Mutex::new(Vec::new()),
            authorization_urls: Mutex::new(Vec::new()),
            accept: AtomicBool::new(true),
        })
    }
}

impl DynamicMcpNotificationSinkPort for RecordingSink {
    fn notify(&self, notification: DynamicMcpNotification) -> bool {
        if !self.accepts(&notification.instance_key) {
            return false;
        }
        self.notifications.lock().push(notification);
        true
    }

    fn notify_authorization_needed(
        &self,
        instance: &DynamicMcpInstanceKey,
        flow_id: &str,
        authorization_url: &str,
    ) -> bool {
        if !self.accepts(instance) {
            return false;
        }
        self.authorization_urls.lock().push((
            instance.clone(),
            flow_id.to_string(),
            authorization_url.to_string(),
        ));
        true
    }

    fn accepts(&self, instance: &DynamicMcpInstanceKey) -> bool {
        self.accept.load(Ordering::SeqCst) && instance.logical.session_id == self.session_id
    }
}

struct FakeConnector {
    calls: AtomicUsize,
}

struct RetryConnector {
    calls: AtomicUsize,
    fail_first: AtomicBool,
}

#[async_trait]
impl DynamicMcpConnector for RetryConnector {
    async fn prepare(
        &self,
        instance: DynamicMcpInstanceKey,
        _flow_id: DynamicMcpOperationId,
        _config: CanonicalDynamicMcpConfig,
        _progress: Arc<dyn Fn(DynamicMcpOperationState) + Send + Sync>,
    ) -> Result<StagedMcpConnection, DynamicMcpFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_first.swap(false, Ordering::SeqCst) {
            return Err(DynamicMcpFailure::new(
                DynamicMcpErrorCode::InitializeFailed,
                DynamicMcpOperationState::Connecting,
                "Dynamic MCP initialization failed",
            ));
        }
        Ok(StagedMcpConnection::without_service(
            instance.clone(),
            handle(&instance.logical.server_name, "lookup"),
        ))
    }
}

struct GateConnector {
    gate: Mutex<Option<DynamicMcpAdmissionGate>>,
}

#[async_trait]
impl DynamicMcpConnector for GateConnector {
    async fn prepare(
        &self,
        instance: DynamicMcpInstanceKey,
        _flow_id: DynamicMcpOperationId,
        _config: CanonicalDynamicMcpConfig,
        _progress: Arc<dyn Fn(DynamicMcpOperationState) + Send + Sync>,
    ) -> Result<StagedMcpConnection, DynamicMcpFailure> {
        let staged = StagedMcpConnection::without_service(
            instance.clone(),
            handle(&instance.logical.server_name, "lookup"),
        );
        *self.gate.lock() = Some(staged.gate.clone());
        Ok(staged)
    }
}

impl FakeConnector {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl DynamicMcpConnector for FakeConnector {
    async fn prepare(
        &self,
        instance: DynamicMcpInstanceKey,
        _flow_id: DynamicMcpOperationId,
        _config: CanonicalDynamicMcpConfig,
        _progress: Arc<dyn Fn(DynamicMcpOperationState) + Send + Sync>,
    ) -> Result<StagedMcpConnection, DynamicMcpFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let tool: Tool = serde_json::from_value(serde_json::json!({
            "name": "lookup",
            "description": "lookup",
            "inputSchema": {"type": "object"}
        }))
        .unwrap();
        let handle = Arc::new(McpClientHandle {
            name: instance.logical.server_name.clone(),
            version: None,
            cache_version: None,
            peer: None,
            tools: vec![tool],
            resources: vec![rmcp::model::Resource::new(
                "test://example/resource",
                "example resource",
            )],
            status: crate::mcp::ClientStatus::Connected,
            oauth_status: Default::default(),
            source: None,
            url: None,
            channel_capable: false,
            skills_capable: false,
        });
        Ok(StagedMcpConnection::without_service(instance, handle))
    }
}

fn handle(name: &str, tool_name: &str) -> Arc<McpClientHandle> {
    let tool: Tool = serde_json::from_value(serde_json::json!({
        "name": tool_name,
        "description": tool_name,
        "inputSchema": {"type": "object"}
    }))
    .unwrap();
    Arc::new(McpClientHandle {
        name: name.to_string(),
        version: None,
        cache_version: None,
        peer: None,
        tools: vec![tool],
        resources: vec![],
        status: crate::mcp::ClientStatus::Connected,
        oauth_status: Default::default(),
        source: None,
        url: None,
        channel_capable: false,
        skills_capable: false,
    })
}

fn load(name: &str, command: &str) -> CanonicalDynamicMcpAction {
    DynamicMcpAction::Load(DynamicMcpLoadRequest {
        name: name.to_string(),
        config: DynamicMcpConfig {
            command: Some(command.to_string()),
            ..Default::default()
        },
    })
    .canonicalize()
    .unwrap()
}

async fn wait_ready(registry: &DynamicMcpRegistry, session_id: &str, name: &str) {
    for _ in 0..100 {
        if registry
            .capability(session_id)
            .snapshot()
            .servers
            .contains_key(name)
        {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("dynamic MCP did not become ready");
}

#[tokio::test]
async fn same_session_same_config_is_idempotent_and_connects_once() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let connector = FakeConnector::new();
    let registry = DynamicMcpRegistry::new(spawner, connector.clone());
    let first = registry
        .execute("session-a", load("example", "one"))
        .await
        .unwrap();
    let second = registry
        .execute("session-a", load("example", "one"))
        .await
        .unwrap();
    assert!(matches!(
        (first, second),
        (
            DynamicMcpResponse::Accepted(first),
            DynamicMcpResponse::Accepted(second)
        ) if first.operation_id == second.operation_id && second.idempotent
    ));
    wait_ready(&registry, "session-a", "example").await;
    assert_eq!(connector.calls.load(Ordering::SeqCst), 1);
    owner.shutdown().await;
}

#[tokio::test]
async fn sessions_with_same_name_are_isolated() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let connector = FakeConnector::new();
    let registry = DynamicMcpRegistry::new(spawner, connector);
    registry
        .execute("session-a", load("example", "one"))
        .await
        .unwrap();
    registry
        .execute("session-b", load("example", "two"))
        .await
        .unwrap();
    wait_ready(&registry, "session-a", "example").await;
    wait_ready(&registry, "session-b", "example").await;
    assert_eq!(registry.capability("session-a").snapshot().servers.len(), 1);
    assert_eq!(registry.capability("session-b").snapshot().servers.len(), 1);
    let other = registry
        .execute(
            "session-c",
            CanonicalDynamicMcpAction::Status(DynamicMcpStatusRequest {
                name: Some("example".to_string()),
                ..Default::default()
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(other.code, DynamicMcpErrorCode::NotFound);
    owner.shutdown().await;
}

#[tokio::test]
async fn different_config_conflicts_without_second_connector() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let connector = FakeConnector::new();
    let registry = DynamicMcpRegistry::new(spawner, connector.clone());
    registry
        .execute("session-a", load("example", "one"))
        .await
        .unwrap();
    let error = registry
        .execute("session-a", load("example", "two"))
        .await
        .unwrap_err();
    assert_eq!(error.code, DynamicMcpErrorCode::ConfigConflict);
    wait_ready(&registry, "session-a", "example").await;
    assert_eq!(connector.calls.load(Ordering::SeqCst), 1);
    owner.shutdown().await;
}

#[tokio::test]
async fn checked_notification_sinks_isolate_sessions_and_status_survives_loss() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let registry = DynamicMcpRegistry::new(spawner, FakeConnector::new());
    let sink_a = RecordingSink::new("session-a");
    let sink_b = RecordingSink::new("session-b");
    let erased_a: Arc<dyn DynamicMcpNotificationSinkPort> = sink_a.clone();
    let erased_b: Arc<dyn DynamicMcpNotificationSinkPort> = sink_b.clone();
    assert!(registry.bind_notification_sink("session-a", Arc::downgrade(&erased_a)));
    assert!(registry.bind_notification_sink("session-b", Arc::downgrade(&erased_b)));

    registry
        .execute("session-a", load("example", "one"))
        .await
        .unwrap();
    registry
        .execute("session-b", load("example", "two"))
        .await
        .unwrap();
    wait_ready(&registry, "session-a", "example").await;
    wait_ready(&registry, "session-b", "example").await;
    let instance_a = registry.capability("session-a").snapshot().servers["example"]
        .instance_key
        .clone();
    assert!(registry.notify_authorization_needed(
        &instance_a,
        "flow-a",
        "https://auth.example.test/authorize"
    ));
    assert_eq!(sink_a.authorization_urls.lock().len(), 1);
    assert!(sink_b.authorization_urls.lock().is_empty());
    assert!(sink_a
        .notifications
        .lock()
        .iter()
        .all(|notification| notification.session_id == "session-a"));
    assert!(sink_b
        .notifications
        .lock()
        .iter()
        .all(|notification| notification.session_id == "session-b"));

    let count = sink_a.notifications.lock().len();
    drop(erased_a);
    drop(sink_a);
    let status = registry
        .execute(
            "session-a",
            CanonicalDynamicMcpAction::Status(DynamicMcpStatusRequest::default()),
        )
        .await
        .unwrap();
    let DynamicMcpResponse::Status(status) = status else {
        panic!("expected status response");
    };
    assert_eq!(status.operations[0].state, DynamicMcpOperationState::Ready);
    assert!(status.capability_generation > 0);
    assert!(count > 0);
    owner.shutdown().await;
}

#[tokio::test]
async fn checked_notification_rejects_stale_incarnation_close_and_secret_canary() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let registry = DynamicMcpRegistry::new(spawner, FakeConnector::new());
    let sink = RecordingSink::new("session-a");
    let erased: Arc<dyn DynamicMcpNotificationSinkPort> = sink.clone();
    assert!(registry.bind_notification_sink("session-a", Arc::downgrade(&erased)));
    registry
        .execute("session-a", load("example", "CANARY_SECRET_VALUE"))
        .await
        .unwrap();
    wait_ready(&registry, "session-a", "example").await;
    let first = registry.capability("session-a").snapshot().servers["example"]
        .instance_key
        .clone();
    registry
        .execute(
            "session-a",
            CanonicalDynamicMcpAction::Unload(CanonicalDynamicMcpUnloadRequest {
                name: "example".to_string(),
                expected_instance: Some(first.clone()),
            }),
        )
        .await
        .unwrap();
    for _ in 0..100 {
        if registry
            .capability("session-a")
            .snapshot()
            .servers
            .is_empty()
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    for _ in 0..100 {
        if registry
            .execute("session-a", load("example", "next"))
            .await
            .is_ok()
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    wait_ready(&registry, "session-a", "example").await;
    let before = sink.notifications.lock().len();
    let stale_operation = registry
        .state
        .lock()
        .operations
        .values()
        .find(|operation| operation.instance == first)
        .unwrap()
        .operation_id
        .clone();
    assert!(!registry.notify_operation(&stale_operation));
    assert!(!registry.notify_authorization_needed(
        &first,
        "stale-flow",
        "https://auth.example.test/stale"
    ));
    assert_eq!(sink.notifications.lock().len(), before);
    assert!(!serde_json::to_string(&*sink.notifications.lock())
        .unwrap()
        .contains("CANARY_SECRET_VALUE"));
    registry.close_session("session-a").await;
    assert!(!registry.notify_authorization_needed(
        &registry
            .capability("session-a")
            .snapshot()
            .servers
            .get("example")
            .map_or_else(|| first.clone(), |server| server.instance_key.clone(),),
        "late-flow",
        "https://auth.example.test/late"
    ));
    assert!(!registry.bind_notification_sink("session-a", Arc::downgrade(&erased)));
    owner.shutdown().await;
}

#[tokio::test]
async fn unload_revokes_capability_and_reload_uses_new_incarnation() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let registry = DynamicMcpRegistry::new(spawner, FakeConnector::new());
    registry
        .execute("session-a", load("example", "one"))
        .await
        .unwrap();
    wait_ready(&registry, "session-a", "example").await;
    let first = registry.capability("session-a").snapshot();
    let first_instance = first.servers["example"].instance_key.clone();
    let first_generation = first.generation;
    registry
        .execute(
            "session-a",
            CanonicalDynamicMcpAction::Unload(CanonicalDynamicMcpUnloadRequest {
                name: "example".to_string(),
                expected_instance: None,
            }),
        )
        .await
        .unwrap();
    for _ in 0..100 {
        if registry
            .capability("session-a")
            .snapshot()
            .servers
            .is_empty()
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    let revoked = registry.capability("session-a").snapshot();
    assert!(revoked.servers.is_empty());
    assert!(revoked.generation > first_generation);
    let mut reloaded = false;
    for _ in 0..100 {
        match registry.execute("session-a", load("example", "one")).await {
            Ok(_) => {
                reloaded = true;
                break;
            }
            Err(error) if error.code == DynamicMcpErrorCode::ServerBusy => {
                tokio::task::yield_now().await;
            }
            Err(error) => panic!("unexpected reload error: {error:?}"),
        }
    }
    assert!(reloaded, "reload must be admitted after unload completes");
    wait_ready(&registry, "session-a", "example").await;
    let second_instance = registry.capability("session-a").snapshot().servers["example"]
        .instance_key
        .clone();
    assert_ne!(first_instance, second_instance);
    owner.shutdown().await;
}

#[tokio::test]
async fn checked_projection_ready_shadow_unload_aba_and_close() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let registry = DynamicMcpRegistry::new(spawner, FakeConnector::new());
    let static_handle = handle("example", "static_lookup");
    let static_token: peri_acp_types::mcp_skills::HandleToken = static_handle.clone();
    let skills = Arc::new(McpSkillRegistry::new());
    let commands = Arc::new(CommandRegistry::new());
    let lease = registry.capability("session-a").bind_projection(
        vec![("example".to_string(), static_token)],
        Arc::clone(&skills),
        Arc::clone(&commands),
    );
    let projection = lease
        .as_any()
        .downcast_ref::<CheckedSessionMcpProjection>()
        .unwrap();
    assert_eq!(
        projection.pool().get_client("example").unwrap().tools[0].name,
        "static_lookup"
    );

    registry
        .execute("session-a", load("example", "one"))
        .await
        .unwrap();
    wait_ready(&registry, "session-a", "example").await;
    assert!(lease.refresh());
    let first = registry.capability("session-a").snapshot().servers["example"]
        .instance_key
        .clone();
    assert_eq!(
        projection.pool().get_client("example").unwrap().tools[0].name,
        "lookup"
    );

    registry
        .execute(
            "session-a",
            CanonicalDynamicMcpAction::Unload(CanonicalDynamicMcpUnloadRequest {
                name: "example".to_string(),
                expected_instance: Some(first.clone()),
            }),
        )
        .await
        .unwrap();
    for _ in 0..100 {
        if registry
            .capability("session-a")
            .snapshot()
            .servers
            .is_empty()
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(
        projection.pool().get_client("example").unwrap().tools[0].name,
        "static_lookup"
    );

    for _ in 0..100 {
        match registry.execute("session-a", load("example", "one")).await {
            Ok(_) => break,
            Err(error) if error.code == DynamicMcpErrorCode::ServerBusy => {
                tokio::task::yield_now().await
            }
            Err(error) => panic!("reload failed: {error:?}"),
        }
    }
    wait_ready(&registry, "session-a", "example").await;
    let second = registry.capability("session-a").snapshot().servers["example"]
        .instance_key
        .clone();
    assert_ne!(
        first, second,
        "L1 logical-name ABA must use a new incarnation"
    );
    let stale = registry
        .execute(
            "session-a",
            CanonicalDynamicMcpAction::Unload(CanonicalDynamicMcpUnloadRequest {
                name: "example".to_string(),
                expected_instance: Some(first),
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(
        stale.code,
        DynamicMcpErrorCode::ServerBusy,
        "L2 stale incarnation CAS must fail"
    );
    assert!(lease.refresh());
    assert_eq!(
        projection.pool().get_client("example").unwrap().tools[0].name,
        "lookup"
    );

    registry.close_session("session-a").await;
    assert!(
        !lease.refresh(),
        "closed session rejects late projection writes"
    );
    assert_eq!(
        projection.pool().get_client("example").unwrap().tools[0].name,
        "static_lookup"
    );
    owner.shutdown().await;
}

#[tokio::test]
async fn session_owned_projection_keeps_existing_discover_instance_live_until_close() {
    use peri_agent::tools::{BaseTool, ToolContext};

    let (mut owner, spawner) = McpTaskOwner::new();
    let registry = DynamicMcpRegistry::new(spawner, FakeConnector::new());
    let skills = Arc::new(McpSkillRegistry::new());
    let commands = Arc::new(CommandRegistry::new());
    let holder = Arc::new(parking_lot::Mutex::new(Some(
        registry.capability("session-a").bind_projection(
            Vec::new(),
            Arc::clone(&skills),
            Arc::clone(&commands),
        ),
    )));
    let pool = holder
        .lock()
        .as_ref()
        .unwrap()
        .as_any()
        .downcast_ref::<CheckedSessionMcpProjection>()
        .unwrap()
        .pool();
    let discover = crate::mcp::discover_tool::DiscoverMCPTool::new(Arc::clone(&pool), Some(skills));

    registry
        .execute("session-a", load("example", "one"))
        .await
        .unwrap();
    wait_ready(&registry, "session-a", "example").await;

    let listed = discover
        .invoke(
            serde_json::json!({"method": "list", "params": {"server": "example"}}),
            ToolContext::new(&[], "/tmp"),
        )
        .await
        .unwrap();
    let listed: serde_json::Value = serde_json::from_str(&listed).unwrap();
    assert_eq!(listed["server"], "example");
    assert_eq!(listed["tools"], serde_json::json!(["lookup"]));
    assert_eq!(
        listed["resources"],
        serde_json::json!(["test://example/resource"])
    );

    let lease = holder.lock().take().unwrap();
    lease.close();
    assert!(!lease.refresh());
    assert!(pool.get_client("example").is_none());
    drop(lease);
    registry.close_session("session-a").await;
    owner.shutdown().await;
}

#[test]
fn repeated_catalog_registration_requires_identical_canonical_view() {
    let (_owner, spawner) = McpTaskOwner::new();
    let registry = DynamicMcpRegistry::new(spawner, FakeConnector::new());
    let initial = vec![DynamicMcpCatalogTool {
        name: "Builtin".to_string(),
        aliases: vec!["builtin_alias".to_string()],
        static_mcp_server: None,
    }];

    registry
        .register_catalog("session-a", initial.clone())
        .unwrap();
    registry.register_catalog("session-a", initial).unwrap();
    let error = registry
        .register_catalog(
            "session-a",
            vec![DynamicMcpCatalogTool {
                name: "SubagentOnly".to_string(),
                aliases: Vec::new(),
                static_mcp_server: None,
            }],
        )
        .unwrap_err();

    assert_eq!(error.code, DynamicMcpErrorCode::ToolNameConflict);
}

#[tokio::test]
async fn catalog_collision_rejects_load_without_publishing_capability() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let registry = DynamicMcpRegistry::new(spawner, FakeConnector::new());
    registry
        .register_catalog(
            "session-a",
            vec![DynamicMcpCatalogTool {
                name: "mcp__example__lookup".to_string(),
                aliases: Vec::new(),
                static_mcp_server: None,
            }],
        )
        .unwrap();
    registry
        .execute("session-a", load("example", "one"))
        .await
        .unwrap();

    for _ in 0..100 {
        let response = registry
            .execute(
                "session-a",
                CanonicalDynamicMcpAction::Status(DynamicMcpStatusRequest {
                    name: Some("example".to_string()),
                    ..Default::default()
                }),
            )
            .await
            .unwrap();
        if matches!(
            response,
            DynamicMcpResponse::Status(ref status)
                if status.operations.iter().any(|operation| {
                    operation.error.as_ref().is_some_and(|failure| {
                        failure.code == DynamicMcpErrorCode::ToolNameConflict
                    })
                })
        ) {
            break;
        }
        tokio::task::yield_now().await;
    }

    assert!(registry.capability("session-a").snapshot().tools.is_empty());
    owner.shutdown().await;
}

#[tokio::test]
async fn same_named_static_server_is_shadowable_not_a_collision() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let registry = DynamicMcpRegistry::new(spawner, FakeConnector::new());
    registry
        .register_catalog(
            "session-a",
            vec![DynamicMcpCatalogTool {
                name: "mcp__example__lookup".to_string(),
                aliases: Vec::new(),
                static_mcp_server: Some("example".to_string()),
            }],
        )
        .unwrap();

    registry
        .execute("session-a", load("example", "one"))
        .await
        .unwrap();
    wait_ready(&registry, "session-a", "example").await;

    assert!(registry
        .capability("session-a")
        .snapshot()
        .tools
        .contains_key("mcp__example__lookup"));
    owner.shutdown().await;
}

#[tokio::test]
async fn failed_load_is_retained_and_same_config_can_retry() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let connector = Arc::new(RetryConnector {
        calls: AtomicUsize::new(0),
        fail_first: AtomicBool::new(true),
    });
    let registry = DynamicMcpRegistry::new(spawner, connector.clone());

    let first = registry
        .execute("session-a", load("example", "one"))
        .await
        .unwrap();
    let first_operation = match first {
        DynamicMcpResponse::Accepted(accepted) => accepted.operation_id,
        response => panic!("unexpected response: {response:?}"),
    };
    for _ in 0..100 {
        let status = registry
            .execute(
                "session-a",
                CanonicalDynamicMcpAction::Status(DynamicMcpStatusRequest {
                    operation_id: Some(first_operation.clone()),
                    ..Default::default()
                }),
            )
            .await
            .unwrap();
        if matches!(status, DynamicMcpResponse::Status(ref value) if value.operations[0].state == DynamicMcpOperationState::Failed)
        {
            break;
        }
        tokio::task::yield_now().await;
    }

    registry
        .execute("session-a", load("example", "one"))
        .await
        .expect("failed load must remain observable without preventing retry");
    wait_ready(&registry, "session-a", "example").await;
    assert_eq!(connector.calls.load(Ordering::SeqCst), 2);
    owner.shutdown().await;
}

#[tokio::test]
async fn drain_timeout_never_reports_unloaded_and_can_be_retried() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let connector = Arc::new(GateConnector {
        gate: Mutex::new(None),
    });
    let registry = DynamicMcpRegistry::with_drain_timeout(
        spawner,
        connector.clone(),
        std::time::Duration::from_millis(1),
    );
    registry
        .execute("session-a", load("example", "one"))
        .await
        .unwrap();
    wait_ready(&registry, "session-a", "example").await;
    let permit = connector
        .gate
        .lock()
        .as_ref()
        .unwrap()
        .try_acquire()
        .unwrap();

    let unload = registry
        .execute(
            "session-a",
            CanonicalDynamicMcpAction::Unload(CanonicalDynamicMcpUnloadRequest {
                name: "example".to_string(),
                expected_instance: None,
            }),
        )
        .await
        .unwrap();
    let operation_id = match unload {
        DynamicMcpResponse::Accepted(accepted) => accepted.operation_id,
        response => panic!("unexpected response: {response:?}"),
    };
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    let status = registry
        .execute(
            "session-a",
            CanonicalDynamicMcpAction::Status(DynamicMcpStatusRequest {
                operation_id: Some(operation_id),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
    assert!(matches!(status, DynamicMcpResponse::Status(ref value)
        if value.operations[0].state == DynamicMcpOperationState::Failed
            && value.operations[0].error.as_ref().is_some_and(|failure| failure.code == DynamicMcpErrorCode::ShutdownIncomplete)));

    drop(permit);
    registry
        .execute(
            "session-a",
            CanonicalDynamicMcpAction::Unload(CanonicalDynamicMcpUnloadRequest {
                name: "example".to_string(),
                expected_instance: None,
            }),
        )
        .await
        .expect("failed drain must remain retryable");
    owner.shutdown().await;
}

#[tokio::test]
async fn owner_closed_rejects_load_without_starting_connector() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let connector = FakeConnector::new();
    let registry = DynamicMcpRegistry::new(spawner, connector.clone());
    owner.begin_shutdown();

    let error = registry
        .execute("session-a", load("example", "one"))
        .await
        .unwrap_err();

    assert_eq!(error.code, DynamicMcpErrorCode::TaskOwnerClosed);
    assert_eq!(connector.calls.load(Ordering::SeqCst), 0);
    owner.shutdown().await;
}

#[tokio::test]
async fn stale_unload_instance_cannot_target_current_incarnation() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let registry = DynamicMcpRegistry::new(spawner, FakeConnector::new());
    registry
        .execute("session-a", load("example", "one"))
        .await
        .unwrap();
    wait_ready(&registry, "session-a", "example").await;
    let current = registry.capability("session-a").snapshot().servers["example"]
        .instance_key
        .clone();
    let mut stale = current.clone();
    stale.incarnation_id = DynamicMcpIncarnationId::from_string("mcpinc_stale");

    let error = registry
        .execute(
            "session-a",
            CanonicalDynamicMcpAction::Unload(CanonicalDynamicMcpUnloadRequest {
                name: "example".to_string(),
                expected_instance: Some(stale),
            }),
        )
        .await
        .unwrap_err();

    assert_eq!(error.code, DynamicMcpErrorCode::ServerBusy);
    assert_eq!(
        registry.capability("session-a").snapshot().servers["example"].instance_key,
        current
    );
    owner.shutdown().await;
}
