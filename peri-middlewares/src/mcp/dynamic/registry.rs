//! Session-scoped Dynamic MCP registry 的状态所有权、装配入口与 deployment port。
//! 连接、load/unload、capability 发布和关闭分别由私有子模块实现。

mod capability;
mod connector;
mod lifecycle;
mod load;
mod operations;
mod unload;

use super::staged_connection::ActiveMcpConnection;
#[cfg(test)]
use super::staged_connection::StagedMcpConnection;
use crate::mcp::task_scope::McpTaskSpawner;
use async_trait::async_trait;
use parking_lot::Mutex;
use peri_acp_types::{
    dynamic_mcp::{
        CanonicalDynamicMcpAction, CanonicalDynamicMcpConfig, DynamicMcpCatalogTool,
        DynamicMcpErrorCode, DynamicMcpFailure, DynamicMcpInstanceKey, DynamicMcpLogicalKey,
        DynamicMcpOperationId, DynamicMcpOperationState, DynamicMcpResponse,
        DynamicMcpShutdownReport, SessionMcpCapabilitySnapshot,
    },
    ports::{
        DynamicMcpDeploymentPort, DynamicMcpNotificationSinkPort, SessionCloseRegistration,
        SessionMcpCapabilityPort,
    },
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Weak},
    time::Duration,
};

pub(crate) use capability::CheckedSessionMcpProjection;
pub use connector::{DynamicMcpConnector, ProductionDynamicMcpConnector};

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

#[cfg(test)]
#[path = "registry_test.rs"]
mod tests;
