//! 工具冲突裁决、capability snapshot 与 session projection lease。
//! 发布、撤销和 projection 刷新复用调用方持有的同一个 RegistryState 锁。

use super::{DynamicMcpRegistry, RegistryState};
use crate::mcp::{middleware::run_ensure_discovery, McpClientHandle, McpClientPool, McpToolBridge};
use peri_acp_types::{
    command_registry::CommandRegistry,
    dynamic_mcp::{
        DynamicMcpErrorCode, DynamicMcpFailure, DynamicMcpInstanceKey, DynamicMcpOperationState,
        DynamicMcpServerProjection, DynamicMcpToolCapability, SessionMcpCapabilitySnapshot,
    },
    mcp_skills::{HandleToken, McpSkillRegistry},
    ports::{SessionMcpCapabilityPort, SessionMcpProjectionLease},
    tools::BaseTool,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Weak},
};

impl DynamicMcpRegistry {
    pub fn capability(&self, session_id: impl Into<String>) -> Arc<dyn SessionMcpCapabilityPort> {
        Arc::new(RegistrySessionCapability {
            registry: self.self_weak.clone(),
            session_id: session_id.into(),
        })
    }

    pub(super) fn build_instance_tools(
        &self,
        instance: &DynamicMcpInstanceKey,
        handle: &Arc<crate::mcp::client::McpClientHandle>,
        gate: &super::super::admission::DynamicMcpAdmissionGate,
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

    pub(super) fn tools_collide(
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

    pub(super) fn publish_capability(
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

    pub(super) fn revoke_capability(
        &self,
        state: &mut RegistryState,
        instance: &DynamicMcpInstanceKey,
    ) {
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
