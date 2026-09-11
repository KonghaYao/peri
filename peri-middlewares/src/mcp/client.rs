//! MCP pool 的状态所有权、构造、基础查询与宿主端口。
//! 缓存、OAuth、生命周期及状态投影分别由私有子模块实现。

mod cache;
mod lifecycle;
mod oauth;
mod service;
mod status;
mod subscription;
mod transport;
mod types;

use super::{config::McpServerConfig, oauth_flow::OAuthFlowEvent};
use lifecycle::ServiceShutdownState;
use oauth::{OAuthFlowKey, PendingOAuthCallback};
use peri_acp_types::{
    mcp::McpSubscriptionPort, ports::McpPoolShutdownReport, session::InboxHandle,
};
use rmcp::model::{Resource, Tool};
use std::{any::Any, collections::HashMap, sync::Arc};

pub(crate) use cache::cache_scope_allows_persistence;
pub use oauth::OAuthStartDisposition;
#[cfg(test)]
pub(crate) use service::ControlledMcpService;
pub(crate) use service::{mcpp_client_info_for_profile, peer_declares_skills, McpServiceWrapper};
pub use status::redact_mcp_error;
#[cfg(test)]
pub(crate) use status::status_change_text;
#[cfg(test)]
use status::{mcp_error_summary, mcp_status_label};
#[cfg(test)]
pub(crate) use subscription::build_subscription_filter;
pub(crate) use subscription::setup_subscription;
pub(crate) use transport::{
    build_authed_transport, build_http_transport, serve_client_auto, spawn_stdio_transport,
};
pub(crate) use types::McpConnectionKey;
pub use types::{
    ClientStatus, McpClientHandle, McpInitStatus, McpPoolError, OAuthStatus, ServerInfo,
};

/// MCP 客户端连接池
pub struct McpClientPool {
    /// Pool-wide admission gate. 0=open, 1=closing, 2=closed.
    lifecycle: std::sync::atomic::AtomicU8,
    pub(crate) lifecycle_registration: parking_lot::Mutex<()>,
    /// Pool-owned terminal service-close transaction. Awaiting a borrowed
    /// handle is cancellation-safe: a dropped waiter cannot detach the worker
    /// or the drained services it owns.
    service_shutdown: tokio::sync::Mutex<ServiceShutdownState>,
    pub(crate) task_spawner: super::task_scope::McpTaskSpawner,
    pub(crate) clients: parking_lot::RwLock<HashMap<String, Arc<McpClientHandle>>>,
    handle_generations:
        parking_lot::Mutex<HashMap<String, Vec<(std::sync::Weak<McpClientHandle>, u64)>>>,
    next_handle_generation: std::sync::atomic::AtomicU64,
    pub(crate) services: parking_lot::Mutex<HashMap<String, McpServiceWrapper>>,
    pub(crate) configs: parking_lot::RwLock<HashMap<String, McpServerConfig>>,
    pub(crate) cache_versions: parking_lot::RwLock<HashMap<String, String>>,
    /// 插件来源旁路表：key 为 server name（如 `"plugin:p1:srv1"`），value 为 `"name@marketplace"`
    pub(crate) plugin_sources: parking_lot::RwLock<HashMap<String, String>>,
    /// 初始化阶段内部存储（M-TUI 收口：TUI 不再持有 watch channel，`mcp/list`
    /// 命令面经 `McpPoolPort::snapshot` 读取；`run_initialize` 与外部
    /// `status_tx` 同步更新）。
    pub(crate) init_status: parking_lot::RwLock<McpInitStatus>,
    /// 初始化是否已完成。完成前发生的状态写入**不**产生上下线通知——
    /// 会话首 turn 的 `first_turn_reminder` 概览已覆盖初始连接结果，避免与
    /// 逐台上线事件重复（初始化未完成时，迟到的连接成功自然成为运行中变化）。
    pub(crate) initialized: std::sync::atomic::AtomicBool,
    /// 运行中状态变化的待注入文本缓冲（McpMiddleware::before_model drain 后
    /// 以 Info 消息推送进模型上下文；全局缓冲，任一会话消费一次即清空）。
    pub(crate) pending_changes: parking_lot::Mutex<Vec<String>>,
    /// 状态变化通知回调（装配时注入；发布 system-notification 给 TUI 通知面）。
    notifier: parking_lot::RwLock<Option<Arc<dyn Fn(&str) + Send + Sync>>>,
    /// OAuth 流程事件回调（装配时注入；`AuthorizationNeeded` 需把
    /// `callback_tx` 注册进 `pending_oauth_callbacks` 供授权码回传 RPC 投递，
    /// 其余事件转发为 ACP `oauth-needed` / `oauth-completed` / `oauth-failed`）。
    oauth_event_callback: parking_lot::RwLock<Option<Arc<dyn Fn(OAuthFlowEvent) + Send + Sync>>>,
    /// 待完成 OAuth 授权的回调通道。物理连接与 flow identity 共同定位，
    /// dynamic 路径不得降维为裸 server name。
    pending_oauth_callbacks: parking_lot::Mutex<HashMap<OAuthFlowKey, PendingOAuthCallback>>,
    /// 每个 scoped connection 最多一个活跃 OAuth flow。
    active_oauth_flows: parking_lot::Mutex<HashMap<McpConnectionKey, String>>,
    /// subscriptions/listen 会话 inbox 注册表（session_id → InboxHandle）。
    /// SessionManager（peri-acp）经 `McpSubscriptionPort` 注册；订阅通知到达
    /// 时向全部注册 inbox 推送 Defer 消息并唤醒 idle agent。
    pub(crate) session_inboxes: parking_lot::RwLock<HashMap<String, InboxHandle>>,
    /// 跨进程的 MCP Resource Cache；是否写入由响应 scope 与安全上下文共同决定。
    pub(crate) resource_cache: super::resource_cache::McpResourceCache,
    /// 进程启动时冻结的 deployment capability profile；初始连接和重连复用。
    pub(crate) capability_profile: super::apps::McpCapabilityProfile,
    /// 初始模型 MCP tool invocation 签发、`peri/mcp/open` 单次消费的租约。
    pub(crate) app_binding_leases: Arc<super::apps::McpAppBindingLeaseRegistry>,
}

pub(crate) const STDIO_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
pub(crate) const HTTP_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
pub(crate) const SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

impl McpClientPool {
    pub fn new_pending() -> Self {
        Self::new_pending_with_spawner(super::task_scope::McpTaskSpawner::closed())
    }

    pub fn new_pending_with_spawner(spawner: super::task_scope::McpTaskSpawner) -> Self {
        Self::new_pending_with_spawner_and_profile(
            spawner,
            super::apps::McpCapabilityProfile::disabled(),
        )
    }

    pub fn new_pending_with_spawner_and_profile(
        spawner: super::task_scope::McpTaskSpawner,
        capability_profile: super::apps::McpCapabilityProfile,
    ) -> Self {
        Self {
            lifecycle: std::sync::atomic::AtomicU8::new(0),
            lifecycle_registration: parking_lot::Mutex::new(()),
            service_shutdown: tokio::sync::Mutex::new(ServiceShutdownState::Idle),
            task_spawner: spawner,
            clients: parking_lot::RwLock::new(HashMap::new()),
            handle_generations: parking_lot::Mutex::new(HashMap::new()),
            next_handle_generation: std::sync::atomic::AtomicU64::new(1),
            services: parking_lot::Mutex::new(HashMap::new()),
            configs: parking_lot::RwLock::new(HashMap::new()),
            cache_versions: parking_lot::RwLock::new(HashMap::new()),
            plugin_sources: parking_lot::RwLock::new(HashMap::new()),
            init_status: parking_lot::RwLock::new(McpInitStatus::Pending),
            initialized: std::sync::atomic::AtomicBool::new(false),
            pending_changes: parking_lot::Mutex::new(Vec::new()),
            notifier: parking_lot::RwLock::new(None),
            oauth_event_callback: parking_lot::RwLock::new(None),
            pending_oauth_callbacks: parking_lot::Mutex::new(HashMap::new()),
            active_oauth_flows: parking_lot::Mutex::new(HashMap::new()),
            session_inboxes: parking_lot::RwLock::new(HashMap::new()),
            resource_cache: super::resource_cache::McpResourceCache::new(),
            capability_profile,
            app_binding_leases: Arc::new(super::apps::McpAppBindingLeaseRegistry::default()),
        }
    }

    #[cfg(test)]
    pub fn new_empty() -> Self {
        let mut pool = Self::new_pending();
        pool.resource_cache = super::resource_cache::McpResourceCache::isolated_for_test();
        pool
    }

    /// 查询指定 server 的插件来源标识，非插件 server 返回 None
    /// key 格式为 `"plugin_name__server_name"`，返回 `"name@marketplace"`
    pub fn plugin_source_of(&self, name: &str) -> Option<String> {
        self.plugin_sources.read().get(name).cloned()
    }

    pub fn get_tools(&self, name: &str) -> Vec<Tool> {
        self.clients
            .read()
            .get(name)
            .map(|h| h.tools.clone())
            .unwrap_or_default()
    }
    pub fn get_resources(&self, name: &str) -> Vec<Resource> {
        self.clients
            .read()
            .get(name)
            .map(|h| h.resources.clone())
            .unwrap_or_default()
    }
    pub fn get_client(&self, name: &str) -> Option<Arc<McpClientHandle>> {
        self.clients.read().get(name).cloned()
    }
    pub fn get_all_clients(&self) -> Vec<Arc<McpClientHandle>> {
        self.clients
            .read()
            .values()
            .filter(|c| matches!(c.status, ClientStatus::Connected))
            .cloned()
            .collect()
    }
    pub fn has_resources(&self) -> bool {
        self.clients
            .read()
            .values()
            .any(|c| matches!(c.status, ClientStatus::Connected) && !c.resources.is_empty())
    }
    pub fn resource_summary(&self) -> String {
        self.clients
            .read()
            .values()
            .filter(|c| matches!(c.status, ClientStatus::Connected) && !c.resources.is_empty())
            .map(|c| {
                format!(
                    "- server \"{}\": {} ({} resources)",
                    c.name,
                    c.resources
                        .iter()
                        .map(|r| r.uri.clone())
                        .collect::<Vec<_>>()
                        .join(", "),
                    c.resources.len()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

// 3.0 批 2 波 2：装配注入端口实现（ACP 侧只持 `Arc<dyn McpPoolPort>`）。
// M-TUI 收口：`shutdown`（host/shutdown 命令面）与 `snapshot`（mcp/list
// 命令面）为新增数据端口；TUI 不再直持池句柄与 watch channel。
#[async_trait::async_trait]
impl peri_acp_types::ports::McpPoolPort for McpClientPool {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn begin_shutdown(&self) {
        McpClientPool::begin_shutdown(self);
    }

    async fn shutdown(&self) -> McpPoolShutdownReport {
        McpClientPool::shutdown(self).await
    }

    fn snapshot(&self) -> serde_json::Value {
        let init_phase = match &*self.init_status.read() {
            McpInitStatus::Pending => "pending",
            McpInitStatus::Initializing { .. } => "initializing",
            McpInitStatus::Ready { .. } => "ready",
            McpInitStatus::Failed(_) => "failed",
        };
        let infos = self.all_server_infos();
        serde_json::json!({
            "initPhase": init_phase,
            "servers": infos.iter().map(|info| serde_json::json!({
                "name": info.name.clone(),
                "status": format!("{:?}", info.status).to_lowercase(),
                "transport": info.transport_type.clone(),
                "toolsCount": info.tool_count,
            })).collect::<Vec<_>>(),
        })
    }
}

/// `McpSubscriptionPort` 实现：SessionManager（peri-acp）在 session 创建 /
/// 销毁时注册 / 注销 inbox；订阅通知到达时经 inbox 唤醒 agent。
impl McpSubscriptionPort for McpClientPool {
    fn register_inbox(&self, session_id: &str, handle: InboxHandle) {
        self.session_inboxes
            .write()
            .insert(session_id.to_string(), handle);
    }

    fn unregister_inbox(&self, session_id: &str) {
        self.session_inboxes.write().remove(session_id);
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
#[path = "client_test.rs"]
mod tests;
