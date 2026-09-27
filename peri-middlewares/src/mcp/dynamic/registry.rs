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

    /// 注册（或替换）本 session 的动态碰撞目录。
    ///
    /// 目录会在启动闸门提交后随晚到的静态 MCP 工具重注册，因此同一 session 的
    /// 重复注册不是 no-op：先在同一把锁内用候选目录重验已发布动态工具，冲突则
    /// 拒绝且**保留旧目录**（无部分替换），否则整体替换。这样发现期借旧目录
    /// 放行的 load 不会在静态目录更新后继续以过期基线存在。
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
        if let Some(conflict) = candidate_catalog_conflict(&state, session_id, &tools) {
            // safe_summary 固定为冲突工具名，调用方（stage_builder/tools.rs 的
            // 注册回调）据此映射 `StartupRegistrationRejected`。
            return Err(DynamicMcpFailure::new(
                DynamicMcpErrorCode::ToolNameConflict,
                DynamicMcpOperationState::Failed,
                conflict,
            ));
        }
        state.catalogs.insert(session_id.to_string(), tools);
        Ok(())
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

/// 候选静态目录与已发布动态工具的重名/别名冲突；返回首个冲突的候选条目名。
///
/// 判定与 `registry/capability.rs::tools_collide` 的过滤语义对称：大小写不敏感，
/// 且**同名静态 server** 的条目允许被该动态实例遮蔽，不参与冲突判定。只在
/// 本 session 内比较：动态工具的碰撞基线不跨 session 共享。
fn candidate_catalog_conflict(
    state: &RegistryState,
    session_id: &str,
    tools: &[DynamicMcpCatalogTool],
) -> Option<String> {
    let snapshot = state.capabilities.get(session_id)?;
    if snapshot.tools.is_empty() {
        return None;
    }
    let mut candidates: BTreeMap<String, Vec<&DynamicMcpCatalogTool>> = BTreeMap::new();
    for tool in tools {
        for name in std::iter::once(&tool.name).chain(tool.aliases.iter()) {
            candidates
                .entry(name.to_ascii_lowercase())
                .or_default()
                .push(tool);
        }
    }
    for capability in snapshot.tools.values() {
        let server = capability.instance.logical.server_name.as_str();
        for name in
            std::iter::once(capability.tool.name()).chain(capability.tool.aliases().iter().copied())
        {
            let conflict = candidates
                .get(&name.to_ascii_lowercase())
                .and_then(|entries| {
                    entries
                        .iter()
                        .find(|entry| entry.static_mcp_server.as_deref() != Some(server))
                });
            if let Some(entry) = conflict {
                return Some(entry.name.clone());
            }
        }
    }
    None
}

#[cfg(test)]
#[path = "registry_test.rs"]
mod tests;
