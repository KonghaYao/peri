use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Weak},
    time::Duration,
};

use async_trait::async_trait;
use parking_lot::Mutex;
use peri_acp_types::{
    command_registry::CommandRegistry,
    dynamic_mcp::{
        CanonicalDynamicMcpAction, CanonicalDynamicMcpConfig, CanonicalDynamicMcpLoadRequest,
        CanonicalDynamicMcpUnloadRequest, DynamicMcpAccepted, DynamicMcpCatalogTool,
        DynamicMcpErrorCode, DynamicMcpFailure, DynamicMcpInstanceKey, DynamicMcpLogicalKey,
        DynamicMcpNotification, DynamicMcpOperationId, DynamicMcpOperationState,
        DynamicMcpOperationStatus, DynamicMcpResponse, DynamicMcpServerProjection,
        DynamicMcpShutdownReport, DynamicMcpStatusRequest, DynamicMcpStatusResponse,
        DynamicMcpToolCapability, SessionMcpCapabilitySnapshot,
    },
    mcp_skills::{HandleToken, McpSkillRegistry},
    ports::{
        DynamicMcpDeploymentPort, DynamicMcpNotificationSinkPort, SecretResolverPort,
        SessionCloseRegistration, SessionMcpCapabilityPort, SessionMcpProjectionLease,
    },
    tools::BaseTool,
};

use super::staged_connection::{
    prepare_single_server, ActiveMcpConnection, EnvironmentSecretResolver, RejectingSecretResolver,
    StagedMcpConnection,
};
use crate::mcp::{
    middleware::run_ensure_discovery,
    task_scope::{DynamicMcpTaskKind, McpTaskKey, McpTaskSpawner, TaskAdmissionError},
    McpClientHandle, McpClientPool, McpToolBridge,
};

#[async_trait]
pub trait DynamicMcpConnector: Send + Sync {
    async fn prepare(
        &self,
        instance: DynamicMcpInstanceKey,
        flow_id: DynamicMcpOperationId,
        config: CanonicalDynamicMcpConfig,
        progress: Arc<dyn Fn(DynamicMcpOperationState) + Send + Sync>,
    ) -> Result<StagedMcpConnection, DynamicMcpFailure>;
}

pub struct ProductionDynamicMcpConnector {
    secret_resolver: Arc<dyn SecretResolverPort>,
    cleanup_spawner: McpTaskSpawner,
    oauth_pool: Arc<McpClientPool>,
}

impl ProductionDynamicMcpConnector {
    pub fn new(
        secret_resolver: Arc<dyn SecretResolverPort>,
        cleanup_spawner: McpTaskSpawner,
        oauth_pool: Arc<McpClientPool>,
    ) -> Self {
        Self {
            secret_resolver,
            cleanup_spawner,
            oauth_pool,
        }
    }

    pub fn from_environment(
        cleanup_spawner: McpTaskSpawner,
        oauth_pool: Arc<McpClientPool>,
    ) -> Self {
        Self::new(
            Arc::new(EnvironmentSecretResolver),
            cleanup_spawner,
            oauth_pool,
        )
    }

    pub fn fail_closed(cleanup_spawner: McpTaskSpawner, oauth_pool: Arc<McpClientPool>) -> Self {
        Self::new(
            Arc::new(RejectingSecretResolver),
            cleanup_spawner,
            oauth_pool,
        )
    }
}

#[async_trait]
impl DynamicMcpConnector for ProductionDynamicMcpConnector {
    async fn prepare(
        &self,
        instance: DynamicMcpInstanceKey,
        flow_id: DynamicMcpOperationId,
        config: CanonicalDynamicMcpConfig,
        progress: Arc<dyn Fn(DynamicMcpOperationState) + Send + Sync>,
    ) -> Result<StagedMcpConnection, DynamicMcpFailure> {
        prepare_single_server(
            instance,
            flow_id,
            &config,
            self.secret_resolver.as_ref(),
            self.cleanup_spawner.clone(),
            Arc::clone(&self.oauth_pool),
            progress,
        )
        .await
    }
}

struct DynamicEntry {
    instance: DynamicMcpInstanceKey,
    config: CanonicalDynamicMcpConfig,
    load_operation: DynamicMcpOperationId,
    unload_operation: Option<DynamicMcpOperationId>,
    state: DynamicMcpOperationState,
    active: Option<Arc<ActiveMcpConnection>>,
}

#[derive(Clone)]
struct OperationRecord {
    operation_id: DynamicMcpOperationId,
    instance: DynamicMcpInstanceKey,
    config: CanonicalDynamicMcpConfig,
    state: DynamicMcpOperationState,
    error: Option<DynamicMcpFailure>,
    tool_count: usize,
    resource_count: usize,
}

fn notification_text(operation: &OperationRecord) -> (String, &'static str) {
    let server = &operation.instance.logical.server_name;
    match operation.state {
        DynamicMcpOperationState::Starting => (format!("Dynamic MCP {server} is starting"), "info"),
        DynamicMcpOperationState::Authorizing => (
            format!("Dynamic MCP {server} is awaiting authorization"),
            "info",
        ),
        DynamicMcpOperationState::Connecting => {
            (format!("Dynamic MCP {server} is connecting"), "info")
        }
        DynamicMcpOperationState::Discovering => (
            format!("Dynamic MCP {server} is discovering capabilities"),
            "info",
        ),
        DynamicMcpOperationState::Ready => (format!("Dynamic MCP {server} is ready"), "info"),
        DynamicMcpOperationState::Revoking | DynamicMcpOperationState::Draining => {
            (format!("Dynamic MCP {server} is draining"), "info")
        }
        DynamicMcpOperationState::Unloaded => {
            (format!("Dynamic MCP {server} was unloaded"), "info")
        }
        DynamicMcpOperationState::Failed => {
            let code = operation
                .error
                .as_ref()
                .map_or("INTERNAL", |failure| failure.code.as_str());
            (format!("Dynamic MCP {server} failed ({code})"), "error")
        }
    }
}

#[derive(Default)]
struct RegistryState {
    closing: bool,
    closed_sessions: BTreeSet<String>,
    entries: BTreeMap<DynamicMcpLogicalKey, DynamicEntry>,
    operations: BTreeMap<DynamicMcpOperationId, OperationRecord>,
    capabilities: BTreeMap<String, Arc<SessionMcpCapabilitySnapshot>>,
    catalogs: BTreeMap<String, Vec<DynamicMcpCatalogTool>>,
    projections: BTreeMap<String, Weak<CheckedSessionMcpProjection>>,
    notification_sinks: BTreeMap<String, Weak<dyn DynamicMcpNotificationSinkPort>>,
}

pub struct DynamicMcpRegistry {
    state: Mutex<RegistryState>,
    task_spawner: McpTaskSpawner,
    connector: Arc<dyn DynamicMcpConnector>,
    self_weak: Weak<DynamicMcpRegistry>,
    drain_timeout: Duration,
}

impl DynamicMcpRegistry {
    pub fn new(task_spawner: McpTaskSpawner, connector: Arc<dyn DynamicMcpConnector>) -> Arc<Self> {
        Self::with_drain_timeout(task_spawner, connector, Duration::from_secs(30))
    }

    fn with_drain_timeout(
        task_spawner: McpTaskSpawner,
        connector: Arc<dyn DynamicMcpConnector>,
        drain_timeout: Duration,
    ) -> Arc<Self> {
        Arc::new_cyclic(|weak| Self {
            state: Mutex::new(RegistryState::default()),
            task_spawner,
            connector,
            self_weak: weak.clone(),
            drain_timeout,
        })
    }

    pub fn capability(&self, session_id: impl Into<String>) -> Arc<dyn SessionMcpCapabilityPort> {
        Arc::new(RegistrySessionCapability {
            registry: self.self_weak.clone(),
            session_id: session_id.into(),
        })
    }

    pub fn close_registration(
        &self,
        session_id: impl Into<String>,
    ) -> Arc<dyn SessionCloseRegistration> {
        Arc::new(RegistrySessionClose {
            registry: self.self_weak.clone(),
            session_id: session_id.into(),
            closed: std::sync::atomic::AtomicBool::new(false),
        })
    }

    fn failure(
        code: DynamicMcpErrorCode,
        phase: DynamicMcpOperationState,
        summary: &'static str,
    ) -> DynamicMcpFailure {
        DynamicMcpFailure::new(code, phase, summary)
    }

    fn operation_status(
        state: &RegistryState,
        operation: &OperationRecord,
    ) -> DynamicMcpOperationStatus {
        let generation = state
            .capabilities
            .get(&operation.instance.logical.session_id)
            .map_or(0, |snapshot| snapshot.generation);
        DynamicMcpOperationStatus {
            operation_id: operation.operation_id.clone(),
            server: operation.instance.logical.server_name.clone(),
            state: operation.state,
            instance_key: operation.instance.clone(),
            config: operation.config.safe_summary(),
            error: operation.error.clone(),
            tool_count: operation.tool_count,
            resource_count: operation.resource_count,
            capability_generation: generation,
        }
    }

    fn accepted(operation: &OperationRecord, idempotent: bool) -> DynamicMcpResponse {
        DynamicMcpResponse::Accepted(DynamicMcpAccepted {
            operation_id: operation.operation_id.clone(),
            server: operation.instance.logical.server_name.clone(),
            state: operation.state,
            scope: "session".to_string(),
            idempotent,
        })
    }

    fn notify_operation(&self, operation_id: &DynamicMcpOperationId) -> bool {
        let (sink, notification) = {
            let state = self.state.lock();
            let Some(operation) = state.operations.get(operation_id) else {
                return false;
            };
            let session_id = &operation.instance.logical.session_id;
            if state.closing || state.closed_sessions.contains(session_id) {
                return false;
            }
            let current_matches = state
                .entries
                .get(&operation.instance.logical)
                .is_some_and(|entry| entry.instance == operation.instance);
            let completed_unload = operation.state == DynamicMcpOperationState::Unloaded
                && !state.entries.contains_key(&operation.instance.logical);
            if !current_matches && !completed_unload {
                return false;
            }
            let Some(sink) = state
                .notification_sinks
                .get(session_id)
                .and_then(Weak::upgrade)
            else {
                return false;
            };
            let (text, _) = notification_text(operation);
            (
                sink,
                DynamicMcpNotification {
                    session_id: operation.instance.logical.session_id.clone(),
                    operation_id: operation.operation_id.clone(),
                    instance_key: operation.instance.clone(),
                    state: operation.state,
                    safe_summary: text,
                },
            )
        };
        sink.accepts(&notification.instance_key) && sink.notify(notification)
    }

    async fn load(
        &self,
        session_id: &str,
        request: CanonicalDynamicMcpLoadRequest,
    ) -> Result<DynamicMcpResponse, DynamicMcpFailure> {
        let logical = DynamicMcpLogicalKey {
            session_id: session_id.to_string(),
            server_name: request.name.clone(),
        };
        let (instance, operation_id) = {
            let mut state = self.state.lock();
            if state.closing || state.closed_sessions.contains(session_id) {
                return Err(Self::failure(
                    DynamicMcpErrorCode::TaskOwnerClosed,
                    DynamicMcpOperationState::Failed,
                    "Dynamic MCP task admission is closed",
                ));
            }
            if state
                .entries
                .get(&logical)
                .is_some_and(|entry| entry.state == DynamicMcpOperationState::Failed)
            {
                state.entries.remove(&logical);
            }
            if let Some(entry) = state.entries.get(&logical) {
                if matches!(
                    entry.state,
                    DynamicMcpOperationState::Revoking | DynamicMcpOperationState::Draining
                ) {
                    return Err(Self::failure(
                        DynamicMcpErrorCode::ServerBusy,
                        entry.state,
                        "Dynamic MCP server is draining",
                    ));
                }
                if entry.config != request.config {
                    return Err(Self::failure(
                        DynamicMcpErrorCode::ConfigConflict,
                        entry.state,
                        "A different configuration already uses this server name",
                    ));
                }
                let operation = state
                    .operations
                    .get(&entry.load_operation)
                    .expect("load operation exists");
                return Ok(Self::accepted(operation, true));
            }
            let instance = DynamicMcpInstanceKey {
                logical: logical.clone(),
                incarnation_id: Default::default(),
            };
            let operation_id = DynamicMcpOperationId::new();
            state.operations.insert(
                operation_id.clone(),
                OperationRecord {
                    operation_id: operation_id.clone(),
                    instance: instance.clone(),
                    config: request.config.clone(),
                    state: DynamicMcpOperationState::Starting,
                    error: None,
                    tool_count: 0,
                    resource_count: 0,
                },
            );
            state.entries.insert(
                logical,
                DynamicEntry {
                    instance: instance.clone(),
                    config: request.config,
                    load_operation: operation_id.clone(),
                    unload_operation: None,
                    state: DynamicMcpOperationState::Starting,
                    active: None,
                },
            );
            (instance, operation_id)
        };
        let _ = self.notify_operation(&operation_id);

        let (start_tx, start_rx) = tokio::sync::oneshot::channel();
        let weak = self.self_weak.clone();
        let key = McpTaskKey::dynamic(DynamicMcpTaskKind::Connect, &instance);
        let task_instance = instance.clone();
        let task_operation = operation_id.clone();
        let admission = self.task_spawner.spawn(key, async move {
            if start_rx.await.is_ok() {
                if let Some(registry) = weak.upgrade() {
                    registry.run_load(task_instance, task_operation).await;
                }
            }
        });
        match admission {
            Ok(()) => {
                let state = self.state.lock();
                let operation = state
                    .operations
                    .get(&operation_id)
                    .expect("reserved operation exists");
                let response = Self::accepted(operation, false);
                drop(state);
                let _ = start_tx.send(());
                Ok(response)
            }
            Err(error) => {
                let failure = Self::failure(
                    match error {
                        TaskAdmissionError::OwnerClosed => DynamicMcpErrorCode::TaskOwnerClosed,
                        TaskAdmissionError::DuplicateKey => DynamicMcpErrorCode::Internal,
                    },
                    DynamicMcpOperationState::Failed,
                    "Dynamic MCP task could not be admitted",
                );
                let mut state = self.state.lock();
                if let Some(operation) = state.operations.get_mut(&operation_id) {
                    operation.state = DynamicMcpOperationState::Failed;
                    operation.error = Some(failure.clone());
                }
                if let Some(entry) = state.entries.get_mut(&instance.logical) {
                    entry.state = DynamicMcpOperationState::Failed;
                }
                Err(failure)
            }
        }
    }

    async fn run_load(
        self: Arc<Self>,
        instance: DynamicMcpInstanceKey,
        operation_id: DynamicMcpOperationId,
    ) {
        let config = {
            let mut state = self.state.lock();
            if !self.load_commit_allowed(&state, &instance, &operation_id) {
                return;
            }
            let config = state
                .entries
                .get(&instance.logical)
                .expect("checked entry exists")
                .config
                .clone();
            state
                .entries
                .get_mut(&instance.logical)
                .expect("checked entry exists")
                .state = DynamicMcpOperationState::Connecting;
            state
                .operations
                .get_mut(&operation_id)
                .expect("checked operation exists")
                .state = DynamicMcpOperationState::Connecting;
            config
        };
        let _ = self.notify_operation(&operation_id);
        let connection_key = crate::mcp::client::McpConnectionKey::dynamic(instance.clone());
        debug_assert!(connection_key.is_dynamic());
        let weak = self.self_weak.clone();
        let progress_instance = instance.clone();
        let progress_operation = operation_id.clone();
        let progress: Arc<dyn Fn(DynamicMcpOperationState) + Send + Sync> = Arc::new(move |next| {
            let Some(registry) = weak.upgrade() else {
                return;
            };
            let mut state = registry.state.lock();
            if !registry.load_commit_allowed(&state, &progress_instance, &progress_operation) {
                return;
            }
            state
                .entries
                .get_mut(&progress_instance.logical)
                .expect("checked entry exists")
                .state = next;
            state
                .operations
                .get_mut(&progress_operation)
                .expect("checked operation exists")
                .state = next;
            drop(state);
            let _ = registry.notify_operation(&progress_operation);
        });
        let staged = match self
            .connector
            .prepare(instance.clone(), operation_id.clone(), config, progress)
            .await
        {
            Ok(staged) => staged,
            Err(failure) => {
                self.fail_operation(&instance, &operation_id, failure);
                return;
            }
        };
        let allowed = {
            let state = self.state.lock();
            self.load_commit_allowed(&state, &instance, &operation_id)
        };
        if !allowed {
            if let Err(failure) = staged.cleanup().await {
                self.fail_operation(&instance, &operation_id, failure);
            }
            return;
        }
        {
            let mut state = self.state.lock();
            state
                .entries
                .get_mut(&instance.logical)
                .expect("checked entry exists")
                .state = DynamicMcpOperationState::Discovering;
            state
                .operations
                .get_mut(&operation_id)
                .expect("checked operation exists")
                .state = DynamicMcpOperationState::Discovering;
        }
        let _ = self.notify_operation(&operation_id);
        let dynamic_tools =
            match self.build_instance_tools(&staged.instance_key, &staged.handle, &staged.gate) {
                Ok(tools) => tools,
                Err(failure) => {
                    let cleanup_failure = staged.cleanup().await.err();
                    self.fail_operation(
                        &instance,
                        &operation_id,
                        cleanup_failure.unwrap_or(failure),
                    );
                    return;
                }
            };
        let active = Arc::new(staged.commit());
        let commit_result = {
            let mut state = self.state.lock();
            if !self.load_commit_allowed(&state, &instance, &operation_id) {
                Err(())
            } else if self.tools_collide(&state, &instance, &dynamic_tools) {
                let failure = Self::failure(
                    DynamicMcpErrorCode::ToolNameConflict,
                    DynamicMcpOperationState::Discovering,
                    "Dynamic MCP tool names conflict with the current catalog",
                );
                if let Some(operation) = state.operations.get_mut(&operation_id) {
                    operation.state = DynamicMcpOperationState::Failed;
                    operation.error = Some(failure.clone());
                }
                if let Some(entry) = state.entries.get_mut(&instance.logical) {
                    entry.state = DynamicMcpOperationState::Failed;
                }
                Err(())
            } else {
                let tool_count = active.handle.tools.len();
                let resource_count = active.handle.resources.len();
                let entry = state
                    .entries
                    .get_mut(&instance.logical)
                    .expect("checked entry exists");
                entry.state = DynamicMcpOperationState::Ready;
                entry.active = Some(Arc::clone(&active));
                let operation = state
                    .operations
                    .get_mut(&operation_id)
                    .expect("checked operation exists");
                operation.state = DynamicMcpOperationState::Ready;
                operation.tool_count = tool_count;
                operation.resource_count = resource_count;
                self.publish_capability(&mut state, &instance, dynamic_tools);
                Ok(())
            }
        };
        let _ = self.notify_operation(&operation_id);
        if commit_result.is_err() {
            let _ = active.close().await;
        }
    }

    fn load_commit_allowed(
        &self,
        state: &RegistryState,
        instance: &DynamicMcpInstanceKey,
        operation_id: &DynamicMcpOperationId,
    ) -> bool {
        !state.closing
            && !state.closed_sessions.contains(&instance.logical.session_id)
            && state.entries.get(&instance.logical).is_some_and(|entry| {
                entry.instance == *instance && entry.load_operation == *operation_id
            })
            && state.operations.contains_key(operation_id)
    }

    fn fail_operation(
        &self,
        instance: &DynamicMcpInstanceKey,
        operation_id: &DynamicMcpOperationId,
        failure: DynamicMcpFailure,
    ) {
        let mut state = self.state.lock();
        if !self.load_commit_allowed(&state, instance, operation_id) {
            return;
        }
        if let Some(operation) = state.operations.get_mut(operation_id) {
            operation.state = DynamicMcpOperationState::Failed;
            operation.error = Some(failure);
        }
        if let Some(entry) = state.entries.get_mut(&instance.logical) {
            entry.state = DynamicMcpOperationState::Failed;
        }
        drop(state);
        let _ = self.notify_operation(operation_id);
    }

    fn build_instance_tools(
        &self,
        instance: &DynamicMcpInstanceKey,
        handle: &Arc<crate::mcp::client::McpClientHandle>,
        gate: &super::admission::DynamicMcpAdmissionGate,
    ) -> Result<BTreeMap<String, Arc<dyn BaseTool>>, DynamicMcpFailure> {
        let mut tools = BTreeMap::<String, Arc<dyn BaseTool>>::new();
        let mut folded = BTreeSet::new();
        for tool in &handle.tools {
            let bridge = McpToolBridge::new_dynamic(
                &instance.logical.server_name,
                tool,
                Arc::clone(handle),
                gate.clone(),
            )
            .map_err(|_| {
                Self::failure(
                    DynamicMcpErrorCode::ToolNameConflict,
                    DynamicMcpOperationState::Discovering,
                    "Dynamic MCP server or tool name is invalid",
                )
            })?;
            let name = bridge.name().to_string();
            if !folded.insert(name.to_ascii_lowercase()) || tools.contains_key(&name) {
                return Err(Self::failure(
                    DynamicMcpErrorCode::ToolNameConflict,
                    DynamicMcpOperationState::Discovering,
                    "Dynamic MCP tool names are not unique",
                ));
            }
            tools.insert(name, Arc::new(bridge));
        }
        Ok(tools)
    }

    fn tools_collide(
        &self,
        state: &RegistryState,
        instance: &DynamicMcpInstanceKey,
        tools: &BTreeMap<String, Arc<dyn BaseTool>>,
    ) -> bool {
        let mut existing = state
            .catalogs
            .get(&instance.logical.session_id)
            .into_iter()
            .flatten()
            .filter(|tool| {
                tool.static_mcp_server.as_deref() != Some(instance.logical.server_name.as_str())
            })
            .flat_map(|tool| {
                std::iter::once(tool.name.as_str()).chain(tool.aliases.iter().map(String::as_str))
            })
            .map(str::to_ascii_lowercase)
            .collect::<BTreeSet<_>>();
        if let Some(snapshot) = state.capabilities.get(&instance.logical.session_id) {
            for tool in snapshot.tools.values() {
                existing.insert(tool.tool.name().to_ascii_lowercase());
                existing.extend(
                    tool.tool
                        .aliases()
                        .iter()
                        .map(|alias| alias.to_ascii_lowercase()),
                );
            }
        }
        tools.values().any(|tool| {
            std::iter::once(tool.name())
                .chain(tool.aliases().iter().copied())
                .map(str::to_ascii_lowercase)
                .any(|name| existing.contains(&name))
        })
    }

    fn publish_capability(
        &self,
        state: &mut RegistryState,
        instance: &DynamicMcpInstanceKey,
        newly_committed: BTreeMap<String, Arc<dyn BaseTool>>,
    ) {
        let session_id = &instance.logical.session_id;
        let previous = state
            .capabilities
            .get(session_id)
            .cloned()
            .unwrap_or_default();
        let mut servers = previous.servers.clone();
        let mut tools = previous.tools.clone();
        tools.retain(|_, capability| capability.instance.logical != instance.logical);
        tools.extend(newly_committed.into_iter().map(|(name, tool)| {
            (
                name,
                DynamicMcpToolCapability {
                    instance: instance.clone(),
                    tool,
                },
            )
        }));
        for entry in state.entries.values().filter(|entry| {
            entry.instance.logical.session_id == *session_id
                && entry.state == DynamicMcpOperationState::Ready
        }) {
            if let Some(active) = &entry.active {
                servers.insert(
                    entry.instance.logical.server_name.clone(),
                    DynamicMcpServerProjection {
                        instance_key: entry.instance.clone(),
                        name: entry.instance.logical.server_name.clone(),
                        config: entry.config.clone(),
                        tool_count: active.handle.tools.len(),
                        resource_count: active.handle.resources.len(),
                    },
                );
            }
        }
        state.capabilities.insert(
            session_id.to_string(),
            Arc::new(SessionMcpCapabilitySnapshot {
                generation: previous.generation.saturating_add(1),
                servers,
                tools,
            }),
        );
        if let Some(projection) = state.projections.get(session_id).and_then(Weak::upgrade) {
            projection.refresh_locked(state);
        }
    }

    fn revoke_capability(&self, state: &mut RegistryState, instance: &DynamicMcpInstanceKey) {
        let previous = state
            .capabilities
            .get(&instance.logical.session_id)
            .cloned()
            .unwrap_or_default();
        let current_matches = previous
            .servers
            .get(&instance.logical.server_name)
            .is_some_and(|projection| projection.instance_key == *instance);
        if !current_matches {
            return;
        }
        let mut servers = previous.servers.clone();
        servers.remove(&instance.logical.server_name);
        let mut tools = previous.tools.clone();
        tools.retain(|_, capability| capability.instance != *instance);
        state.capabilities.insert(
            instance.logical.session_id.clone(),
            Arc::new(SessionMcpCapabilitySnapshot {
                generation: previous.generation.saturating_add(1),
                servers,
                tools,
            }),
        );
        if let Some(projection) = state
            .projections
            .get(&instance.logical.session_id)
            .and_then(Weak::upgrade)
        {
            projection.refresh_locked(state);
        }
    }

    async fn unload(
        &self,
        session_id: &str,
        request: CanonicalDynamicMcpUnloadRequest,
    ) -> Result<DynamicMcpResponse, DynamicMcpFailure> {
        let logical = DynamicMcpLogicalKey {
            session_id: session_id.to_string(),
            server_name: request.name,
        };
        let (instance, operation_id) = {
            let mut state = self.state.lock();
            if state.closing || state.closed_sessions.contains(session_id) {
                return Err(Self::failure(
                    DynamicMcpErrorCode::TaskOwnerClosed,
                    DynamicMcpOperationState::Failed,
                    "Dynamic MCP task admission is closed",
                ));
            }
            let Some(entry) = state.entries.get(&logical) else {
                return Err(Self::failure(
                    DynamicMcpErrorCode::NotFound,
                    DynamicMcpOperationState::Failed,
                    "Dynamic MCP server was not found",
                ));
            };
            if request
                .expected_instance
                .as_ref()
                .is_some_and(|expected| expected != &entry.instance)
            {
                return Err(Self::failure(
                    DynamicMcpErrorCode::ServerBusy,
                    entry.state,
                    "Dynamic MCP server incarnation changed before unload",
                ));
            }
            if let Some(operation_id) = &entry.unload_operation {
                let operation = state
                    .operations
                    .get(operation_id)
                    .expect("unload operation exists");
                if operation.state != DynamicMcpOperationState::Failed {
                    return Ok(Self::accepted(operation, true));
                }
            }
            let instance = entry.instance.clone();
            let config = entry.config.clone();
            let operation_id = DynamicMcpOperationId::new();
            state.operations.insert(
                operation_id.clone(),
                OperationRecord {
                    operation_id: operation_id.clone(),
                    instance: instance.clone(),
                    config,
                    state: DynamicMcpOperationState::Revoking,
                    error: None,
                    tool_count: 0,
                    resource_count: 0,
                },
            );
            state
                .entries
                .get_mut(&logical)
                .expect("checked entry exists")
                .unload_operation = Some(operation_id.clone());
            (instance, operation_id)
        };
        let (start_tx, start_rx) = tokio::sync::oneshot::channel();
        let weak = self.self_weak.clone();
        let task_instance = instance.clone();
        let task_operation = operation_id.clone();
        let admission = self.task_spawner.spawn(
            McpTaskKey::dynamic(DynamicMcpTaskKind::Unload, &instance),
            async move {
                if start_rx.await.is_ok() {
                    if let Some(registry) = weak.upgrade() {
                        registry.run_unload(task_instance, task_operation).await;
                    }
                }
            },
        );
        match admission {
            Ok(()) => {
                let response = {
                    let mut state = self.state.lock();
                    let active = state
                        .entries
                        .get(&instance.logical)
                        .and_then(|entry| entry.active.clone());
                    if let Some(active) = active {
                        active.gate.begin_draining();
                    }
                    if let Some(entry) = state.entries.get_mut(&instance.logical) {
                        entry.state = DynamicMcpOperationState::Draining;
                    }
                    if let Some(operation) = state.operations.get_mut(&operation_id) {
                        operation.state = DynamicMcpOperationState::Draining;
                    }
                    self.revoke_capability(&mut state, &instance);
                    Self::accepted(
                        state
                            .operations
                            .get(&operation_id)
                            .expect("reserved operation exists"),
                        false,
                    )
                };
                let _ = self.notify_operation(&operation_id);
                let _ = start_tx.send(());
                Ok(response)
            }
            Err(_) => {
                let failure = Self::failure(
                    DynamicMcpErrorCode::TaskOwnerClosed,
                    DynamicMcpOperationState::Failed,
                    "Dynamic MCP unload task could not be admitted",
                );
                let mut state = self.state.lock();
                if let Some(operation) = state.operations.get_mut(&operation_id) {
                    operation.state = DynamicMcpOperationState::Failed;
                    operation.error = Some(failure.clone());
                }
                if let Some(entry) = state.entries.get_mut(&instance.logical) {
                    entry.unload_operation = None;
                }
                Err(failure)
            }
        }
    }

    async fn run_unload(
        self: Arc<Self>,
        instance: DynamicMcpInstanceKey,
        operation_id: DynamicMcpOperationId,
    ) {
        self.task_spawner
            .stop_instance_except(&instance, DynamicMcpTaskKind::Unload)
            .await;
        let active = self
            .state
            .lock()
            .entries
            .get(&instance.logical)
            .filter(|entry| entry.instance == instance)
            .and_then(|entry| entry.active.clone());
        let result = if let Some(active) = &active {
            match tokio::time::timeout(self.drain_timeout, active.gate.drain()).await {
                Ok(()) => active.close().await,
                Err(_) => Err(Self::failure(
                    DynamicMcpErrorCode::ShutdownIncomplete,
                    DynamicMcpOperationState::Draining,
                    "Dynamic MCP in-flight calls did not drain in time",
                )),
            }
        } else {
            Ok(())
        };
        let mut state = self.state.lock();
        let entry_matches = state
            .entries
            .get(&instance.logical)
            .is_some_and(|entry| entry.instance == instance);
        if !entry_matches {
            return;
        }
        match result {
            Ok(()) => {
                if let Some(active) = active {
                    active.gate.close();
                }
                if let Some(operation) = state.operations.get_mut(&operation_id) {
                    operation.state = DynamicMcpOperationState::Unloaded;
                }
                state.entries.remove(&instance.logical);
            }
            Err(failure) => {
                if let Some(operation) = state.operations.get_mut(&operation_id) {
                    operation.state = DynamicMcpOperationState::Failed;
                    operation.error = Some(failure);
                }
                if let Some(entry) = state.entries.get_mut(&instance.logical) {
                    entry.state = DynamicMcpOperationState::Failed;
                }
            }
        }
        drop(state);
        let _ = self.notify_operation(&operation_id);
    }

    fn status(
        &self,
        session_id: &str,
        request: DynamicMcpStatusRequest,
    ) -> Result<DynamicMcpResponse, DynamicMcpFailure> {
        let state = self.state.lock();
        if state.closed_sessions.contains(session_id) {
            return Err(Self::failure(
                DynamicMcpErrorCode::NotFound,
                DynamicMcpOperationState::Failed,
                "Dynamic MCP state was not found",
            ));
        }
        let operations = state
            .operations
            .values()
            .filter(|operation| operation.instance.logical.session_id == session_id)
            .filter(|operation| {
                request
                    .operation_id
                    .as_ref()
                    .is_none_or(|id| id == &operation.operation_id)
            })
            .filter(|operation| {
                request
                    .name
                    .as_ref()
                    .is_none_or(|name| name == &operation.instance.logical.server_name)
            })
            .map(|operation| Self::operation_status(&state, operation))
            .collect::<Vec<_>>();
        if (request.operation_id.is_some() || request.name.is_some()) && operations.is_empty() {
            return Err(Self::failure(
                DynamicMcpErrorCode::NotFound,
                DynamicMcpOperationState::Failed,
                "Dynamic MCP state was not found",
            ));
        }
        let generation = state
            .capabilities
            .get(session_id)
            .map_or(0, |snapshot| snapshot.generation);
        Ok(DynamicMcpResponse::Status(DynamicMcpStatusResponse {
            operations,
            capability_generation: generation,
        }))
    }

    async fn close_session_impl(&self, session_id: &str) -> DynamicMcpShutdownReport {
        let (instances, active) = {
            let mut state = self.state.lock();
            state.closed_sessions.insert(session_id.to_string());
            state.notification_sinks.remove(session_id);
            if let Some(projection) = state
                .projections
                .remove(session_id)
                .and_then(|p| p.upgrade())
            {
                projection.close();
            }
            let entries = state
                .entries
                .values_mut()
                .filter(|entry| entry.instance.logical.session_id == session_id)
                .collect::<Vec<_>>();
            let instances = entries
                .iter()
                .map(|entry| entry.instance.clone())
                .collect::<Vec<_>>();
            let active = entries
                .iter()
                .filter_map(|entry| {
                    entry
                        .active
                        .clone()
                        .map(|connection| (entry.instance.clone(), connection))
                })
                .collect::<Vec<_>>();
            for entry in entries {
                entry.state = DynamicMcpOperationState::Draining;
                if let Some(active) = &entry.active {
                    active.gate.begin_draining();
                }
            }
            let previous = state.capabilities.remove(session_id).unwrap_or_default();
            state.capabilities.insert(
                session_id.to_string(),
                Arc::new(SessionMcpCapabilitySnapshot {
                    generation: previous.generation.saturating_add(1),
                    ..Default::default()
                }),
            );
            (instances, active)
        };
        let mut unfinished = 0;
        let mut retained = BTreeSet::new();
        for (instance, connection) in &active {
            let drain_incomplete =
                tokio::time::timeout(self.drain_timeout, connection.gate.drain())
                    .await
                    .is_err();
            let close_incomplete = connection.close().await.is_err();
            if drain_incomplete || close_incomplete {
                unfinished += 1;
                retained.insert(instance.logical.clone());
            } else {
                connection.gate.close();
            }
            self.task_spawner.stop_instance(instance).await;
        }
        let active_instances = active
            .iter()
            .map(|(instance, _)| instance)
            .collect::<BTreeSet<_>>();
        for instance in instances
            .iter()
            .filter(|instance| !active_instances.contains(instance))
        {
            self.task_spawner.stop_instance(instance).await;
        }
        let mut state = self.state.lock();
        state
            .entries
            .retain(|key, _| key.session_id != session_id || retained.contains(key));
        state.operations.retain(|_, operation| {
            operation.instance.logical.session_id != session_id
                || retained.contains(&operation.instance.logical)
        });
        if retained.is_empty() {
            state.catalogs.remove(session_id);
        }
        if unfinished == 0 {
            DynamicMcpShutdownReport::Complete
        } else {
            DynamicMcpShutdownReport::Incomplete {
                unfinished_instances: unfinished,
            }
        }
    }
}

#[async_trait]
impl DynamicMcpDeploymentPort for DynamicMcpRegistry {
    async fn execute(
        &self,
        session_id: &str,
        action: CanonicalDynamicMcpAction,
    ) -> Result<DynamicMcpResponse, DynamicMcpFailure> {
        match action {
            CanonicalDynamicMcpAction::Load(request) => self.load(session_id, request).await,
            CanonicalDynamicMcpAction::Status(request) => self.status(session_id, request),
            CanonicalDynamicMcpAction::Unload(request) => self.unload(session_id, request).await,
        }
    }

    fn register_catalog(
        &self,
        session_id: &str,
        tools: Vec<DynamicMcpCatalogTool>,
    ) -> Result<(), DynamicMcpFailure> {
        let mut state = self.state.lock();
        if state.closing || state.closed_sessions.contains(session_id) {
            return Err(Self::failure(
                DynamicMcpErrorCode::TaskOwnerClosed,
                DynamicMcpOperationState::Failed,
                "Dynamic MCP task admission is closed",
            ));
        }
        match state.catalogs.entry(session_id.to_string()) {
            std::collections::btree_map::Entry::Occupied(_) => Ok(()),
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(tools);
                Ok(())
            }
        }
    }

    fn capability(&self, session_id: &str) -> Arc<dyn SessionMcpCapabilityPort> {
        DynamicMcpRegistry::capability(self, session_id.to_string())
    }

    fn close_registration(&self, session_id: &str) -> Arc<dyn SessionCloseRegistration> {
        DynamicMcpRegistry::close_registration(self, session_id.to_string())
    }

    fn accepts_instance(&self, instance: &DynamicMcpInstanceKey) -> bool {
        let state = self.state.lock();
        !state.closing
            && !state.closed_sessions.contains(&instance.logical.session_id)
            && state
                .entries
                .get(&instance.logical)
                .is_some_and(|entry| entry.instance == *instance)
    }

    fn bind_notification_sink(
        &self,
        session_id: &str,
        sink: Weak<dyn DynamicMcpNotificationSinkPort>,
    ) -> bool {
        let mut state = self.state.lock();
        if state.closing || state.closed_sessions.contains(session_id) || sink.upgrade().is_none() {
            return false;
        }
        state
            .notification_sinks
            .insert(session_id.to_string(), sink);
        true
    }

    fn notify_authorization_needed(
        &self,
        instance: &DynamicMcpInstanceKey,
        flow_id: &str,
        authorization_url: &str,
    ) -> bool {
        let sink = {
            let state = self.state.lock();
            if state.closing
                || state.closed_sessions.contains(&instance.logical.session_id)
                || state
                    .entries
                    .get(&instance.logical)
                    .is_none_or(|entry| entry.instance != *instance)
            {
                return false;
            }
            let Some(sink) = state
                .notification_sinks
                .get(&instance.logical.session_id)
                .and_then(Weak::upgrade)
            else {
                return false;
            };
            sink
        };
        sink.accepts(instance)
            && sink.notify_authorization_needed(instance, flow_id, authorization_url)
    }

    fn begin_shutdown(&self) {
        self.state.lock().closing = true;
    }

    async fn close_session(&self, session_id: &str) -> DynamicMcpShutdownReport {
        self.close_session_impl(session_id).await
    }

    async fn shutdown(&self) -> DynamicMcpShutdownReport {
        self.begin_shutdown();
        let sessions = self
            .state
            .lock()
            .entries
            .keys()
            .map(|key| key.session_id.clone())
            .collect::<BTreeSet<_>>();
        let mut unfinished = 0;
        for session_id in sessions {
            if let DynamicMcpShutdownReport::Incomplete {
                unfinished_instances,
            } = self.close_session_impl(&session_id).await
            {
                unfinished += unfinished_instances;
            }
        }
        if unfinished == 0 {
            DynamicMcpShutdownReport::Complete
        } else {
            DynamicMcpShutdownReport::Incomplete {
                unfinished_instances: unfinished,
            }
        }
    }
}

pub(crate) struct CheckedSessionMcpProjection {
    registry: Weak<DynamicMcpRegistry>,
    session_id: String,
    pool: Arc<McpClientPool>,
    static_handles: BTreeMap<String, Arc<McpClientHandle>>,
    skill_registry: Arc<McpSkillRegistry>,
    command_registry: Arc<CommandRegistry>,
    cancel: peri_agent::agent::AgentCancellationToken,
    closed: std::sync::atomic::AtomicBool,
}

impl CheckedSessionMcpProjection {
    pub(crate) fn pool(&self) -> Arc<McpClientPool> {
        Arc::clone(&self.pool)
    }

    fn refresh_locked(&self, state: &RegistryState) -> bool {
        if self.closed.load(std::sync::atomic::Ordering::Acquire)
            || state.closing
            || state.closed_sessions.contains(&self.session_id)
        {
            return false;
        }
        let snapshot = state
            .capabilities
            .get(&self.session_id)
            .cloned()
            .unwrap_or_default();
        let mut effective = self.static_handles.clone();
        for (name, server) in &snapshot.servers {
            let Some(entry) = state.entries.get(&server.instance_key.logical) else {
                continue;
            };
            if entry.instance != server.instance_key
                || entry.state != DynamicMcpOperationState::Ready
            {
                continue;
            }
            let Some(active) = &entry.active else {
                continue;
            };
            effective.insert(name.clone(), Arc::clone(&active.handle));
        }
        *self.pool.clients.write() = effective.into_iter().collect();
        run_ensure_discovery(
            &self.pool,
            Some(&self.skill_registry),
            Some(&self.command_registry),
            &self.cancel,
        );
        true
    }
}

impl SessionMcpProjectionLease for CheckedSessionMcpProjection {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn refresh(&self) -> bool {
        let Some(registry) = self.registry.upgrade() else {
            return false;
        };
        let refreshed = self.refresh_locked(&registry.state.lock());
        refreshed
    }

    fn close(&self) {
        if self.closed.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return;
        }
        self.cancel.cancel();
        *self.pool.clients.write() = self.static_handles.clone().into_iter().collect();
        let connected = self
            .static_handles
            .iter()
            .map(|(name, handle)| {
                let token: HandleToken = handle.clone();
                (name.clone(), token)
            })
            .collect::<Vec<_>>();
        self.skill_registry.project_connected(&connected);
        let command_connected = connected
            .into_iter()
            .filter(|(name, _)| !crate::mcp::skill_discovery::mcp_namespace_reserved(name))
            .map(|(name, token)| (crate::mcp::skill_discovery::mcp_source_key(&name), token))
            .collect::<Vec<_>>();
        self.command_registry.project_sources(&command_connected);
    }
}

struct RegistrySessionCapability {
    registry: Weak<DynamicMcpRegistry>,
    session_id: String,
}

impl SessionMcpCapabilityPort for RegistrySessionCapability {
    fn snapshot(&self) -> Arc<SessionMcpCapabilitySnapshot> {
        self.registry
            .upgrade()
            .and_then(|registry| {
                registry
                    .state
                    .lock()
                    .capabilities
                    .get(&self.session_id)
                    .cloned()
            })
            .unwrap_or_default()
    }

    fn bind_projection(
        &self,
        static_handles: Vec<(String, HandleToken)>,
        skill_registry: Arc<McpSkillRegistry>,
        command_registry: Arc<CommandRegistry>,
    ) -> Arc<dyn SessionMcpProjectionLease> {
        let Some(registry) = self.registry.upgrade() else {
            return Arc::new(CheckedSessionMcpProjection {
                registry: Weak::new(),
                session_id: self.session_id.clone(),
                pool: Arc::new(McpClientPool::new_pending()),
                static_handles: BTreeMap::new(),
                skill_registry,
                command_registry,
                cancel: peri_agent::agent::AgentCancellationToken::new(),
                closed: std::sync::atomic::AtomicBool::new(true),
            });
        };
        let static_handles = static_handles
            .into_iter()
            .filter_map(|(name, token)| {
                token
                    .downcast::<McpClientHandle>()
                    .ok()
                    .map(|handle| (name, handle))
            })
            .collect::<BTreeMap<_, _>>();
        let projection = Arc::new(CheckedSessionMcpProjection {
            registry: Arc::downgrade(&registry),
            session_id: self.session_id.clone(),
            pool: Arc::new(McpClientPool::new_pending()),
            static_handles,
            skill_registry,
            command_registry,
            cancel: peri_agent::agent::AgentCancellationToken::new(),
            closed: std::sync::atomic::AtomicBool::new(false),
        });
        {
            let mut state = registry.state.lock();
            if state.closing || state.closed_sessions.contains(&self.session_id) {
                projection
                    .closed
                    .store(true, std::sync::atomic::Ordering::Release);
            } else {
                state
                    .projections
                    .insert(self.session_id.clone(), Arc::downgrade(&projection));
                projection.refresh_locked(&state);
            }
        }
        projection
    }
}

struct RegistrySessionClose {
    registry: Weak<DynamicMcpRegistry>,
    session_id: String,
    closed: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl SessionCloseRegistration for RegistrySessionClose {
    async fn revoke_and_cleanup(&self) -> DynamicMcpShutdownReport {
        if self.closed.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return DynamicMcpShutdownReport::Complete;
        }
        match self.registry.upgrade() {
            Some(registry) => registry.close_session_impl(&self.session_id).await,
            None => DynamicMcpShutdownReport::Complete,
        }
    }
}

#[cfg(test)]
#[path = "registry_test.rs"]
mod tests;
