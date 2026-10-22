//! Pool-owned server control. Operations share the ordinary transport and task owner.
use super::{
    client::{McpConnectionKey, McpServiceOwner, McpServiceWrapper, SHUTDOWN_TIMEOUT},
    task_scope::McpTaskKey,
    ClientStatus, McpClientPool, OAuthStatus,
};
use peri_acp_types::tools::BaseTool;
use peri_acp_types::{
    command_registry::CommandRegistry, mcp_control::*, mcp_skills::McpSkillRegistry,
};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Weak,
    },
};

#[derive(Default)]
pub(crate) struct PoolControl {
    slots: parking_lot::Mutex<HashMap<String, Arc<ServerSlot>>>,
    projections: parking_lot::Mutex<Vec<ProjectionBinding>>,
}
#[derive(Default)]
pub(crate) struct ServerSlot {
    pub(crate) operation: Arc<tokio::sync::Mutex<()>>,
    pub(crate) control_operation: tokio::sync::Mutex<()>,
    pub(crate) stopped: AtomicBool,
    pub(crate) transient: AtomicBool,
    services: parking_lot::Mutex<Vec<Arc<McpServiceOwner>>>,
    #[cfg(not(target_os = "emscripten"))]
    pub(crate) processes: parking_lot::Mutex<Vec<Arc<super::client::process::McpProcessOwner>>>,
}
struct ProjectionBinding {
    registry: Weak<McpSkillRegistry>,
    commands: Option<Weak<CommandRegistry>>,
    session: Option<String>,
}
impl McpClientPool {
    pub(crate) fn server_slot(&self, name: &str) -> Arc<ServerSlot> {
        self.control
            .slots
            .lock()
            .entry(name.into())
            .or_default()
            .clone()
    }
    pub(crate) fn track_server_service(
        &self,
        name: &str,
        service: McpServiceWrapper,
    ) -> McpServiceWrapper {
        let peer = service.peer().clone();
        let owner = self.own_service(service);
        self.server_slot(name).services.lock().push(owner.clone());
        McpServiceWrapper::shared(owner, peer)
    }
    pub(crate) fn bind_discovery_projection(
        &self,
        registry: &Arc<McpSkillRegistry>,
        commands: Option<&Arc<CommandRegistry>>,
        session: Option<&str>,
    ) {
        let mut projections = self.control.projections.lock();
        projections.retain(|p| p.registry.strong_count() > 0);
        if !projections.iter().any(|p| {
            p.registry.ptr_eq(&Arc::downgrade(registry)) && p.session.as_deref() == session
        }) {
            projections.push(ProjectionBinding {
                registry: Arc::downgrade(registry),
                commands: commands.map(Arc::downgrade),
                session: session.map(str::to_owned),
            });
        }
    }
    fn withdraw_discovery(&self) {
        let projections: Vec<_> = self
            .control
            .projections
            .lock()
            .iter()
            .filter_map(|p| {
                p.registry.upgrade().map(|r| {
                    (
                        r,
                        p.commands.as_ref().and_then(Weak::upgrade),
                        p.session.clone(),
                    )
                })
            })
            .collect();
        for (registry, commands, session) in projections {
            let connected: Vec<_> = self
                .get_all_clients_visible_to(session.as_deref())
                .into_iter()
                .map(|h| (h.name.clone(), h as peri_acp_types::mcp_skills::HandleToken))
                .collect();
            registry.project_connected(&connected);
            if let Some(commands) = commands {
                let sources: Vec<_> = connected
                    .iter()
                    .map(|(name, h)| (super::skill_discovery::mcp_source_key(name), h.clone()))
                    .collect();
                commands.project_sources(&sources);
            }
        }
    }
    pub(crate) fn control_descriptor(&self, name: &str) -> McpServerControl {
        let configs = self.configs.read();
        let config = configs.get(name);
        let source = config.and_then(|c| c.source.clone()).or_else(|| {
            self.get_client(name)
                .and_then(|handle| handle.source.clone())
        });
        let reason = match source.as_ref() {
            Some(super::config::ConfigSource::Builtin { .. }) => Some("builtin"),
            Some(super::config::ConfigSource::WorkspaceRemote) => Some("workspace_remote"),
            Some(super::config::ConfigSource::Acp) => Some("acp"),
            _ if config.is_some_and(|c| c.system_mcp == Some(true)) => Some("system_mcp"),
            _ if config.is_none() => Some("unavailable"),
            _ => None,
        };
        reason
            .map(McpServerControl::unavailable)
            .unwrap_or_else(McpServerControl::available)
    }
    pub(crate) async fn close_server_connection(
        self: &Arc<Self>,
        name: &str,
        stop_oauth: bool,
    ) -> Result<(), McpControlError> {
        let slot = self.server_slot(name);
        slot.stopped.store(true, Ordering::Release);
        let handle = self.get_client(name);
        if let Some(handle) = &handle {
            handle.call_gate.revoke();
        }
        // Withdraw before awaiting: late discovery is rejected by registry token checks.
        if let Some(old) = handle.as_ref() {
            let mut stopped = (**old).clone();
            stopped.peer = None;
            stopped.tools.clear();
            stopped.resources.clear();
            stopped.status = ClientStatus::Disconnected;
            self.clients.write().insert(name.into(), Arc::new(stopped));
        }
        self.system_readiness.clear_evidence(name);
        self.bump_catalog_generation();
        self.withdraw_discovery();
        if let Some(flow) = self.active_oauth_flow(name).filter(|_| stop_oauth) {
            self.revoke_oauth_connection(&McpConnectionKey::static_server(name));
            self.stop_background(&McpTaskKey::OAuth(flow)).await;
        }
        self.stop_background(&McpTaskKey::Subscription(name.into()))
            .await;
        self.task_spawner.stop_server_discovery(name).await;
        self.task_spawner
            .stop_instance_resource_subscriptions(name)
            .await;
        if let Some(handle) = handle {
            if peri_time::timeout(SHUTDOWN_TIMEOUT, handle.call_gate.drain())
                .await
                .is_err()
            {
                return Err(incomplete());
            }
        }
        // Move resources into their pool-retained owner before any cancellation point.
        if let Some(owner) = self.take_service_for_close(name) {
            slot.services.lock().push(owner);
        }
        let resources = slot.services.lock().clone();
        let mut failed = false;
        for service in resources {
            match service.close_with_timeout(SHUTDOWN_TIMEOUT).await {
                Ok(Some(_)) => {}
                Ok(None) => failed = true,
                Err(error) => {
                    tracing::error!(server=name,%error,"MCP service cleanup failed");
                    failed = true;
                }
            }
        }
        slot.services.lock().retain(|s| !s.is_stopped());
        self.close_builtin_task(name).await;
        #[cfg(not(target_os = "emscripten"))]
        {
            let processes = slot.processes.lock().clone();
            for process in processes {
                if !matches!(
                    peri_time::timeout(SHUTDOWN_TIMEOUT, process.close()).await,
                    Ok(Ok(()))
                ) {
                    failed = true;
                }
            }
            slot.processes.lock().retain(|p| !p.is_stopped());
        }
        if failed {
            return Err(incomplete());
        }
        Ok(())
    }
    pub(crate) fn bump_catalog_generation(&self) {
        self.catalog_generation.fetch_add(1, Ordering::AcqRel);
    }
    pub async fn control_mcp(
        self: Arc<Self>,
        request: McpControlRequest,
    ) -> Result<McpControlResponse, McpControlError> {
        request.validate()?;
        if let McpControlRequest::Register {
            config: McpRegistrationConfig::Http { headers, .. },
            ..
        } = &request
        {
            for (name, value) in headers {
                reqwest::header::HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| McpControlError::invalid("invalid HTTP header name"))?;
                reqwest::header::HeaderValue::from_str(value)
                    .map_err(|_| McpControlError::invalid("invalid HTTP header value"))?;
            }
        }
        let session_id = request.session_id().to_owned();
        let name = request.server_name().to_owned();
        let action = request.action();
        // Configuration loading must establish the namespace before temporary registration.
        if matches!(*self.init_status.read(), super::McpInitStatus::Pending) {
            return Err(McpControlError::new(
                -32000,
                "server_busy",
                "MCP configuration is initializing",
            ));
        }
        let slot = self.server_slot(&name);
        let _control = slot.control_operation.try_lock().map_err(|_| {
            McpControlError::new(-32000, "server_busy", "MCP server operation is in progress")
        })?;
        let register_operation = if action == McpControlAction::Register {
            Some(slot.operation.clone().try_lock_owned().map_err(|_| {
                McpControlError::new(-32000, "server_busy", "MCP server operation is in progress")
            })?)
        } else {
            None
        };
        {
            let _admission = self.lifecycle_registration.lock();
            if !self.is_open() {
                return Err(McpControlError::new(
                    -32000,
                    "pool_closed",
                    "MCP pool is closing",
                ));
            }
            match &request {
                McpControlRequest::Register { config, .. } => {
                    if matches!(config, McpRegistrationConfig::Stdio { .. })
                        && !self.stdio_available.load(Ordering::Acquire)
                    {
                        return Err(McpControlError::unsupported());
                    }
                    let normalized =
                        super::tool_bridge::sanitize_name_component(&name).to_ascii_lowercase();
                    if self
                        .configs
                        .read()
                        .keys()
                        .chain(self.clients.read().keys())
                        .any(|existing| {
                            super::tool_bridge::sanitize_name_component(existing)
                                .to_ascii_lowercase()
                                == normalized
                        })
                    {
                        return Err(McpControlError::new(
                            -32000,
                            "name_conflict",
                            "MCP server name is already registered",
                        ));
                    }
                    self.configs
                        .write()
                        .insert(name.clone(), config.clone().into_server_config());
                    slot.transient.store(true, Ordering::Release);
                }
                _ => {
                    if !self.configs.read().contains_key(&name) {
                        return Err(McpControlError::new(
                            -32001,
                            "server_not_found",
                            "MCP server was not found",
                        ));
                    }
                    if !self.is_visible_to_session(&name, &session_id) {
                        return Err(McpControlError::new(
                            -32001,
                            "server_not_found",
                            "MCP server was not found",
                        ));
                    }
                    if !self.control_descriptor(&name).restart {
                        return Err(McpControlError::unsupported());
                    }
                }
            }
        }
        // Control has exclusive admission before cancelling the exact owned flows.
        // New reconnect/OAuth admission checks this same gate, so it cannot revive
        // the stopped server between cancellation and acquiring the connection lock.
        if let Some(flow) = self.active_oauth_flow(&name) {
            self.revoke_oauth_connection(&McpConnectionKey::static_server(&name));
            self.stop_background(&McpTaskKey::OAuth(flow)).await;
            self.revoke_oauth_connection(&McpConnectionKey::static_server(&name));
        }
        self.stop_background(&McpTaskKey::Reconnect(name.clone()))
            .await;
        let _operation = match register_operation {
            Some(operation) => operation,
            None => slot.operation.clone().try_lock_owned().map_err(|_| {
                McpControlError::new(-32000, "server_busy", "MCP server operation is in progress")
            })?,
        };
        let status = if action == McpControlAction::Stop {
            self.close_server_connection(&name, true).await?;
            McpControlStatus::Stopped
        } else {
            self.close_server_connection(&name, true).await?;
            slot.stopped.store(false, Ordering::Release);
            let result = self.reconnect_managed(&name, None, false).await;
            match self.get_client(&name) {
                Some(h) if h.oauth_status == OAuthStatus::NeedsAuthorization => {
                    McpControlStatus::NeedsAuthorization
                }
                Some(h) if h.status == ClientStatus::Connected && result.is_ok() => {
                    McpControlStatus::Connected
                }
                _ => {
                    let error = result
                        .err()
                        .map(|e| e.to_string())
                        .unwrap_or_else(|| "MCP connection did not become ready".into());
                    tracing::error!(server=%name,%error,"MCP control failed");
                    if self.is_open() && self.get_client(&name).is_none() {
                        McpClientPool::insert_failed(&self, &name, error.clone());
                    }
                    return Err(McpControlError::new(-32603, "connection_failed", error));
                }
            }
        };
        self.withdraw_discovery();
        Ok(McpControlResponse {
            session_id,
            server_name: name,
            action,
            status,
        })
    }
}
fn incomplete() -> McpControlError {
    McpControlError::new(
        -32603,
        "shutdown_incomplete",
        "MCP connection cleanup is incomplete; retry stop",
    )
}

struct PoolCatalog {
    pool: Arc<McpClientPool>,
    session: String,
}
impl SessionMcpCatalogPort for PoolCatalog {
    fn snapshot(&self) -> Arc<SessionMcpCatalogSnapshot> {
        loop {
            let generation = self.pool.catalog_generation.load(Ordering::Acquire);
            let mut bridges = super::tool_bridge::build_typed_tool_bridges_visible_to(
                &self.pool,
                Some(&self.session),
            );
            let required: std::collections::BTreeMap<_, _> = self
                .pool
                .system_requirements()
                .into_iter()
                .filter(|r| {
                    self.pool
                        .get_client(&r.server)
                        .is_some_and(|h| h.status == ClientStatus::Connected)
                })
                .map(|r| (r.server, r.required_tools))
                .collect();
            // Readiness validated these immutable system declarations before the first Reason.
            for bridge in &mut bridges {
                if required
                    .get(bridge.mcp_server_name().unwrap_or_default())
                    .is_some_and(|names| {
                        names.iter().any(|name| name == bridge.original_tool_name())
                    })
                {
                    *bridge = bridge.clone().with_system_direct();
                }
            }
            if generation != self.pool.catalog_generation.load(Ordering::Acquire) {
                continue;
            }
            return Arc::new(SessionMcpCatalogSnapshot {
                generation,
                tools: bridges
                    .into_iter()
                    .map(|b| Arc::new(b) as Arc<dyn peri_acp_types::tools::BaseTool>)
                    .collect(),
            });
        }
    }
}
pub(crate) fn catalog_source(
    pool: Arc<McpClientPool>,
    session: &str,
) -> Arc<dyn SessionMcpCatalogPort> {
    Arc::new(PoolCatalog {
        pool,
        session: session.into(),
    })
}

#[cfg(test)]
#[path = "control_test.rs"]
mod tests;
