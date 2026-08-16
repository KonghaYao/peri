use std::{any::Any, collections::HashMap, sync::Arc};

use peri_acp_types::mcp::McpSubscriptionPort;
use peri_acp_types::plugin::McpSubscriptionsConfig;
use peri_acp_types::session::InboxHandle;
use rmcp::{
    model::{ProtocolVersion, Resource, ServerNotification, SubscriptionFilter, Tool},
    service::{
        ClientInitializeError, ClientLifecycleMode, Peer, QuitReason, RoleClient, RunningService,
        ServiceError, Subscription, SubscriptionEnd,
    },
    transport::IntoTransport,
};
use thiserror::Error;

use super::{
    channel_handler::ChannelHandler,
    config::{ConfigSource, McpServerConfig},
    oauth_flow::{OAuthCallbackResult, OAuthFlowEvent},
};

/// Wrapper for RunningService that can hold either handler type
pub(crate) enum McpServiceWrapper {
    Default(RunningService<RoleClient, ()>),
    Channel(RunningService<RoleClient, Arc<ChannelHandler>>),
}

impl McpServiceWrapper {
    pub async fn close_with_timeout(
        &mut self,
        timeout: std::time::Duration,
    ) -> Result<Option<QuitReason>, tokio::task::JoinError> {
        match self {
            McpServiceWrapper::Default(svc) => svc.close_with_timeout(timeout).await,
            McpServiceWrapper::Channel(svc) => svc.close_with_timeout(timeout).await,
        }
    }

    pub async fn list_all_tools(&self) -> Result<Vec<Tool>, ServiceError> {
        match self {
            McpServiceWrapper::Default(svc) => svc.list_all_tools().await,
            McpServiceWrapper::Channel(svc) => svc.list_all_tools().await,
        }
    }

    pub async fn list_all_resources(&self) -> Result<Vec<Resource>, ServiceError> {
        match self {
            McpServiceWrapper::Default(svc) => svc.list_all_resources().await,
            McpServiceWrapper::Channel(svc) => svc.list_all_resources().await,
        }
    }

    pub fn peer(&self) -> &Peer<RoleClient> {
        match self {
            McpServiceWrapper::Default(svc) => svc.peer(),
            McpServiceWrapper::Channel(svc) => svc.peer(),
        }
    }
}

/// MCP 客户端连接状态
#[derive(Debug, Clone, PartialEq)]
pub enum ClientStatus {
    Connected,
    Failed(String),
    Disconnected,
    Disabled,
    /// 配置存在但从未尝试连接（不在 clients 表中，仅在 configs 表中）
    Uninitialized,
}

/// MCP 连接池初始化状态
#[derive(Debug, Clone, PartialEq)]
pub enum McpInitStatus {
    Pending,
    Initializing { connected: usize, total: usize },
    Ready { total: usize },
    Failed(String),
}

/// MCP 服务器 OAuth 授权状态
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum OAuthStatus {
    /// 不使用 OAuth（stdio 传输或未配置 OAuth）
    #[default]
    None,
    /// 已授权（token 有效）
    Authorized,
    /// 需要授权（HTTP 传输且配置了 OAuth，但 token 缺失或过期）
    NeedsAuthorization,
}

/// 单个 MCP 服务器的详细信息（用于 TUI 面板展示）
#[derive(Debug, Clone)]
pub struct ServerInfo {
    pub name: String,
    pub transport_type: String,
    pub status: ClientStatus,
    pub tool_count: usize,
    pub resource_count: usize,
    /// OAuth 授权状态
    pub oauth_status: OAuthStatus,
    /// 配置来源
    pub source: Option<ConfigSource>,
    /// 服务器 URL（HTTP 传输）
    pub url: Option<String>,
    /// 插件来源标识（`"name@marketplace"`），非插件 server 为 None
    pub plugin_source: Option<String>,
}

/// 连接池级别错误
#[derive(Debug, Error)]
pub enum McpPoolError {
    #[error("MCP 服务器 \"{server}\" 连接失败: {reason}")]
    ConnectionFailed { server: String, reason: String },
    #[error("MCP 服务器 \"{server}\" 工具发现失败: {reason}")]
    ToolDiscoveryFailed { server: String, reason: String },
    #[error("MCP 服务器 \"{server}\" 未连接 (状态: {status:?})")]
    NotConnected {
        server: String,
        status: ClientStatus,
    },
}

/// 单个 MCP 服务器的客户端句柄
#[derive(Clone)]
pub struct McpClientHandle {
    pub name: String,
    pub peer: Option<Peer<RoleClient>>,
    pub tools: Vec<Tool>,
    pub resources: Vec<Resource>,
    pub status: ClientStatus,
    pub oauth_status: OAuthStatus,
    /// 配置来源
    pub source: Option<ConfigSource>,
    /// 服务器 URL（HTTP 传输）
    pub url: Option<String>,
    /// Whether the MCP server declared experimental.claude/channel capability
    pub channel_capable: bool,
    /// Whether the MCP server declared the `io.modelcontextprotocol/skills`
    /// extension (SEP-2640)：true 时 skill 发现走 `skills/list` + digest 校验，
    /// false 时回退 legacy `skill://` resources 扫描兜底。
    pub skills_capable: bool,
}

/// SEP-2640 Skills 扩展标识（capabilities.extensions 键）。
pub(crate) const SKILLS_EXTENSION_ID: &str = "io.modelcontextprotocol/skills";

/// 检测 peer 的 server capabilities 是否声明 Skills 扩展（SEP-2640）。
///
/// 仅凭 scheme 不得判定资源为技能（规范 MUST NOT），扩展声明是规范路径的
/// 唯一门闩；未声明时调用 `skills/list` 属于对不支持方法的盲调。
pub(crate) fn peer_declares_skills(peer: &Peer<RoleClient>) -> bool {
    peer.peer_info()
        .map(|info| {
            info.capabilities
                .extensions
                .as_ref()
                .map(|ext| ext.contains_key(SKILLS_EXTENSION_ID))
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

/// MCP 客户端连接池
pub struct McpClientPool {
    pub(crate) clients: parking_lot::RwLock<HashMap<String, Arc<McpClientHandle>>>,
    pub(crate) services: tokio::sync::Mutex<HashMap<String, McpServiceWrapper>>,
    pub(crate) configs: parking_lot::RwLock<HashMap<String, McpServerConfig>>,
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
    notifier: parking_lot::RwLock<Option<Box<dyn Fn(&str) + Send + Sync>>>,
    /// OAuth 流程事件回调（装配时注入；`AuthorizationNeeded` 需把
    /// `callback_tx` 注册进 `pending_oauth_callbacks` 供授权码回传 RPC 投递，
    /// 其余事件转发为 ACP `oauth-needed` / `oauth-completed` / `oauth-failed`）。
    oauth_event_callback: parking_lot::RwLock<Option<Arc<dyn Fn(OAuthFlowEvent) + Send + Sync>>>,
    /// 待完成 OAuth 授权的回调通道（key: server_name）。TUI 经
    /// `mcp/oauth_callback` RPC 回传授权码时由 host 装配面查表投递。
    pending_oauth_callbacks: parking_lot::Mutex<HashMap<String, PendingOAuthCallback>>,
    /// 每台 server 最多一个活跃 OAuth flow；值是稳定 flow identity。
    active_oauth_flows: parking_lot::Mutex<HashMap<String, String>>,
    /// subscriptions/listen 会话 inbox 注册表（session_id → InboxHandle）。
    /// SessionManager（peri-acp）经 `McpSubscriptionPort` 注册；订阅通知到达
    /// 时向全部注册 inbox 推送 Defer 消息并唤醒 idle agent。
    pub(crate) session_inboxes: parking_lot::RwLock<HashMap<String, InboxHandle>>,
    /// 活跃订阅循环任务（server_name → JoinHandle；随 transport 关闭自然结束）。
    pub(crate) subscription_tasks:
        tokio::sync::Mutex<HashMap<String, Vec<tokio::task::JoinHandle<()>>>>,
}

/// MCP 状态变化 → 通知文本（每台一行：上线带工具数，失败报名字 + 错误）。
pub(crate) fn status_change_text(name: &str, status: &ClientStatus, tool_count: usize) -> String {
    match status {
        ClientStatus::Connected => {
            format!("MCP: {name} connected ({tool_count} tools)")
        }
        ClientStatus::Failed(reason) => {
            format!("MCP: {name} failed: {reason}")
        }
        ClientStatus::Disconnected => {
            format!("MCP: {name} disconnected")
        }
        ClientStatus::Disabled => {
            format!("MCP: {name} disabled")
        }
        ClientStatus::Uninitialized => {
            format!("MCP: {name} uninitialized")
        }
    }
}

pub(crate) const STDIO_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
pub(crate) const HTTP_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
pub(crate) const SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

impl McpClientPool {
    pub fn new_pending() -> Self {
        Self {
            clients: parking_lot::RwLock::new(HashMap::new()),
            services: tokio::sync::Mutex::new(HashMap::new()),
            configs: parking_lot::RwLock::new(HashMap::new()),
            plugin_sources: parking_lot::RwLock::new(HashMap::new()),
            init_status: parking_lot::RwLock::new(McpInitStatus::Pending),
            initialized: std::sync::atomic::AtomicBool::new(false),
            pending_changes: parking_lot::Mutex::new(Vec::new()),
            notifier: parking_lot::RwLock::new(None),
            oauth_event_callback: parking_lot::RwLock::new(None),
            pending_oauth_callbacks: parking_lot::Mutex::new(HashMap::new()),
            active_oauth_flows: parking_lot::Mutex::new(HashMap::new()),
            session_inboxes: parking_lot::RwLock::new(HashMap::new()),
            subscription_tasks: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    #[cfg(test)]
    pub fn new_empty() -> Self {
        Self::new_pending()
    }

    /// 注入 OAuth 流程事件回调（host 装配面在 `run_initialize` 前调用；
    /// 回调负责把 `AuthorizationNeeded` 的 `callback_tx` 注册进
    /// `pending_oauth_callbacks`，并将事件转发为 ACP 通知）。
    pub fn set_oauth_event_callback<F>(&self, callback: F)
    where
        F: Fn(OAuthFlowEvent) + Send + Sync + 'static,
    {
        *self.oauth_event_callback.write() = Some(Arc::new(callback));
    }

    /// 读取 OAuth 流程事件回调（spawn 授权任务时克隆给 `OAuthFlowManager`）。
    pub(crate) fn oauth_event_callback(&self) -> Option<Arc<dyn Fn(OAuthFlowEvent) + Send + Sync>> {
        self.oauth_event_callback.read().clone()
    }

    /// 注册待完成 OAuth 授权的回调通道（`AuthorizationNeeded` 事件处理时调用）。
    pub fn register_oauth_callback(
        &self,
        server_name: &str,
        flow_id: &str,
        callback_tx: tokio::sync::oneshot::Sender<OAuthCallbackResult>,
    ) -> bool {
        let mut active = self.active_oauth_flows.lock();
        match active.get(server_name) {
            Some(current) if current != flow_id => return false,
            Some(_) => {}
            None => {
                active.insert(server_name.to_string(), flow_id.to_string());
            }
        }
        drop(active);
        self.pending_oauth_callbacks.lock().insert(
            server_name.to_string(),
            PendingOAuthCallback {
                flow_id: flow_id.to_string(),
                tx: callback_tx,
            },
        );
        true
    }

    /// 投递授权码回传（`mcp/oauth_callback` RPC 调用）：查表取 `callback_tx`
    /// 投递 `{code, state}`；无 pending 通道返回错误。
    pub fn deliver_oauth_callback(
        &self,
        server_name: &str,
        code: String,
        state: String,
    ) -> Result<(), String> {
        let pending = self
            .pending_oauth_callbacks
            .lock()
            .remove(server_name)
            .ok_or_else(|| format!("{server_name} 无进行中的 OAuth 授权"))?;
        pending
            .tx
            .send(OAuthCallbackResult { code, state })
            .map_err(|_| format!("{server_name} OAuth 授权流程已结束"))
    }

    /// 以 flow identity 精确投递授权码。Hub 不使用该接口；保留给未来安全
    /// 手动 callback 能力，避免 server-name-only 的错误归属。
    pub fn deliver_oauth_callback_for_flow(
        &self,
        flow_id: &str,
        code: String,
        state: String,
    ) -> Result<(), String> {
        let server_name = self
            .server_for_oauth_flow(flow_id)
            .ok_or_else(|| "OAuth flow 不存在".to_string())?;
        let pending = self
            .pending_oauth_callbacks
            .lock()
            .remove(&server_name)
            .filter(|pending| pending.flow_id == flow_id)
            .ok_or_else(|| "OAuth flow 不再等待 callback".to_string())?;
        pending
            .tx
            .send(OAuthCallbackResult { code, state })
            .map_err(|_| "OAuth flow 已结束".to_string())
    }

    /// 取消进行中的 OAuth 授权（`mcp/oauth_cancel` RPC 调用）：移除 pending
    /// 通道并 drop sender，后台 `run_oauth_flow` 收到 Cancelled 终止。
    pub fn cancel_oauth_callback(&self, server_name: &str) -> bool {
        let flow_id = self.active_oauth_flows.lock().get(server_name).cloned();
        flow_id
            .as_deref()
            .map(|flow_id| self.cancel_oauth_flow(flow_id))
            .unwrap_or(false)
    }

    /// 精确取消一个活跃 flow；不接受由客户端指定的 server 名称。
    pub fn cancel_oauth_flow(&self, flow_id: &str) -> bool {
        let Some(server_name) = self.server_for_oauth_flow(flow_id) else {
            return false;
        };
        let removed = self
            .pending_oauth_callbacks
            .lock()
            .remove(&server_name)
            .is_some_and(|pending| pending.flow_id == flow_id);
        removed
    }

    pub fn active_oauth_flow(&self, server_name: &str) -> Option<String> {
        self.active_oauth_flows.lock().get(server_name).cloned()
    }

    pub(crate) fn reserve_oauth_flow(
        &self,
        server_name: &str,
        flow_id: &str,
    ) -> OAuthStartDisposition {
        let mut active = self.active_oauth_flows.lock();
        match active.get(server_name) {
            Some(current) if current == flow_id => OAuthStartDisposition::AlreadyActive,
            Some(current) => OAuthStartDisposition::Conflict {
                active_flow_id: current.clone(),
            },
            None => {
                active.insert(server_name.to_string(), flow_id.to_string());
                OAuthStartDisposition::Started
            }
        }
    }

    pub fn release_oauth_flow(&self, server_name: &str, flow_id: &str) {
        let mut active = self.active_oauth_flows.lock();
        if active
            .get(server_name)
            .is_some_and(|current| current == flow_id)
        {
            active.remove(server_name);
        }
        drop(active);
        let mut callbacks = self.pending_oauth_callbacks.lock();
        if callbacks
            .get(server_name)
            .is_some_and(|pending| pending.flow_id == flow_id)
        {
            callbacks.remove(server_name);
        }
    }

    fn server_for_oauth_flow(&self, flow_id: &str) -> Option<String> {
        self.active_oauth_flows
            .lock()
            .iter()
            .find_map(|(server, active)| (active == flow_id).then(|| server.clone()))
    }

    /// 查询指定 server 的插件来源标识，非插件 server 返回 None
    /// key 格式为 `"plugin_name__server_name"`，返回 `"name@marketplace"`
    pub fn plugin_source_of(&self, name: &str) -> Option<String> {
        self.plugin_sources.read().get(name).cloned()
    }

    pub(crate) fn insert_failed(pool: &Arc<Self>, name: &str, reason: String) {
        let (source, url) = pool
            .configs
            .read()
            .get(name)
            .map(|c| (c.source.clone(), c.url.clone()))
            .unwrap_or((None, None));
        let old_status = pool.clients.read().get(name).map(|c| c.status.clone());
        pool.clients.write().insert(
            name.to_string(),
            Arc::new(McpClientHandle {
                name: name.to_string(),
                peer: None,
                tools: vec![],
                resources: vec![],
                status: ClientStatus::Failed(reason.clone()),
                oauth_status: OAuthStatus::default(),
                source,
                url,
                skills_capable: false,
                channel_capable: false,
            }),
        );
        pool.record_status_change(name, old_status.as_ref());
        peri_agent::metrics::emit(
            "mcp.error",
            serde_json::json!({
                "server": name,
                "tool": "connect",
                "error": reason,
            }),
            None,
            None,
        );
    }

    /// 插入需要 OAuth 授权的服务器（HTTP 传输收到 401/AuthRequired 时使用）
    pub(crate) fn insert_needs_auth(pool: &Arc<Self>, name: &str, reason: String) {
        tracing::info!(server = %name, "HTTP 服务器需要 OAuth 授权，可在 MCP 面板按 r 键触发");
        let (source, url) = pool
            .configs
            .read()
            .get(name)
            .map(|c| (c.source.clone(), c.url.clone()))
            .unwrap_or((None, None));
        let old_status = pool.clients.read().get(name).map(|c| c.status.clone());
        pool.clients.write().insert(
            name.to_string(),
            Arc::new(McpClientHandle {
                name: name.to_string(),
                peer: None,
                tools: vec![],
                resources: vec![],
                status: ClientStatus::Failed(reason),
                oauth_status: OAuthStatus::NeedsAuthorization,
                source,
                url,
                skills_capable: false,
                channel_capable: false,
            }),
        );
        pool.record_status_change(name, old_status.as_ref());
    }

    /// 检测错误是否为 HTTP 401 认证错误
    pub(crate) fn is_auth_required_error(error: &str, transport_is_http: bool) -> bool {
        transport_is_http && (error.contains("Auth required") || error.contains("AuthRequired"))
    }

    pub async fn remove_server(self: &Arc<Self>, server_name: &str) {
        self.clients.write().remove(server_name);
        if let Some(mut svc) = self.services.lock().await.remove(server_name) {
            let _ = svc.close_with_timeout(SHUTDOWN_TIMEOUT).await;
        }
        // 关闭连接会终止订阅循环（transport 关闭 → 流结束）；清掉句柄引用即可。
        self.subscription_tasks.lock().await.remove(server_name);
        self.configs.write().remove(server_name);
    }

    /// 将服务器标记为 Disabled：关闭连接但保留 config 和 handle（用于面板展示）
    pub async fn set_disabled(self: &Arc<Self>, server_name: &str) {
        // 关闭实际连接
        if let Some(mut svc) = self.services.lock().await.remove(server_name) {
            let _ = svc.close_with_timeout(SHUTDOWN_TIMEOUT).await;
        }
        self.subscription_tasks.lock().await.remove(server_name);
        // 更新 handle 为 Disabled 状态（保留 config 引用）
        let (source, url) = self
            .configs
            .read()
            .get(server_name)
            .map(|c| (c.source.clone(), c.url.clone()))
            .unwrap_or((None, None));
        self.clients.write().insert(
            server_name.to_string(),
            Arc::new(McpClientHandle {
                name: server_name.to_string(),
                peer: None,
                tools: vec![],
                resources: vec![],
                status: ClientStatus::Disabled,
                oauth_status: OAuthStatus::default(),
                source,
                url,
                skills_capable: false,
                channel_capable: false,
            }),
        );
    }

    // ── subscriptions/listen（2026-07-28 协议）──────────────────────────────

    /// 广播一条订阅通知到所有已注册的会话 inbox。
    ///
    /// 通知以 `<system-reminder><mcp-subscription …/>` Defer 消息注入，
    /// 唤醒 idle executor（agent 随即读资源 / 调工具回复外部消息）。
    fn broadcast_subscription_notification(&self, server: &str, uri: &str, subscription_id: &str) {
        let handles: Vec<InboxHandle> = self.session_inboxes.read().values().cloned().collect();
        if handles.is_empty() {
            tracing::debug!(server = %server, uri = %uri, "订阅通知到达但无注册会话 inbox");
            return;
        }
        let text = format!(
            "<system-reminder><mcp-subscription server=\"{}\" uri=\"{}\" subscription-id=\"{}\">资源已更新，请查看并处理。</mcp-subscription></system-reminder>",
            xml_escape(server),
            xml_escape(uri),
            xml_escape(subscription_id)
        );
        for handle in handles {
            handle.push_defer(
                peri_acp_types::session::MessageSource::ChannelMessage,
                peri_acp_types::messages::BaseMessage::human(text.clone()),
            );
        }
        tracing::info!(server = %server, uri = %uri, sessions = %self.session_inboxes.read().len(), "订阅通知已广播到会话 inbox");
    }

    /// 订阅流异常中断后的最大重试次数（每次中断独立计算，收到通知即重置）。
    const SUBSCRIPTION_RETRY_LIMIT: usize = 3;
    /// 订阅流异常中断后的退避基准秒数（指数递增：1s/2s/4s）。
    const SUBSCRIPTION_RETRY_BASE_DELAY_SECS: u64 = 1;

    /// 启动订阅消费循环：读取 `subscriptions/listen` 流上的通知并广播。
    ///
    /// 循环持有 `Subscription`（drop 即取消订阅）；transport 关闭或流结束
    /// 时自然退出。tool/prompt list_changed 由 rmcp peer 内部自动失效缓存。
    ///
    /// 流异常中断（`SubscriptionEnd::Lagged` / `Abrupt` / 瞬时错误）时按
    /// 指数退避（1s/2s/4s）重新 `Peer::listen` 恢复，最多重试
    /// [`Self::SUBSCRIPTION_RETRY_LIMIT`] 次；期间收到正常通知会重置计数。
    /// 连接关闭、配置移除或重试耗尽后退出循环并告警。
    pub(crate) async fn spawn_subscription_loop(
        self: &Arc<Self>,
        server: &str,
        mut subscription: Subscription,
    ) {
        let pool = Arc::clone(self);
        let task_server = server.to_string();
        let handle = tokio::spawn(async move {
            // 剩余重试次数：收到通知即重置，保证每段中断序列都有独立恢复机会
            let mut retries_left = Self::SUBSCRIPTION_RETRY_LIMIT;
            loop {
                match subscription.next().await {
                    Ok(Some(ServerNotification::ResourceUpdatedNotification(notif))) => {
                        retries_left = Self::SUBSCRIPTION_RETRY_LIMIT;
                        let sid = notif
                            .params
                            .meta
                            .as_ref()
                            .and_then(|m| m.subscription_id())
                            .map(|id| id.to_string())
                            .unwrap_or_default();
                        pool.broadcast_subscription_notification(
                            &task_server,
                            &notif.params.uri,
                            &sid,
                        );
                    }
                    Ok(Some(_)) => {
                        // list_changed 系列：rmcp peer 已失效对应缓存
                        retries_left = Self::SUBSCRIPTION_RETRY_LIMIT;
                    }
                    Ok(None) => {
                        // 仅对异常结束（Lagged/Abrupt）重试；Graceful/Cancelled
                        // 为正常终止，不恢复
                        let retriable = matches!(
                            subscription.end(),
                            Some(SubscriptionEnd::Lagged { .. }) | Some(SubscriptionEnd::Abrupt)
                        );
                        if !retriable || retries_left == 0 {
                            tracing::info!(
                                server = %task_server,
                                end = ?subscription.end(),
                                "订阅流结束，停止消费"
                            );
                            break;
                        }
                        // Subscription 为独占对象：重新 listen 前必须 drop 旧
                        // 句柄（drop 自动发送 cancelled 并注销）
                        drop(subscription);
                        match pool
                            .relisten_subscription(&task_server, &mut retries_left)
                            .await
                        {
                            Some(new_subscription) => subscription = new_subscription,
                            None => break,
                        }
                    }
                    Err(e) => {
                        if retries_left == 0 {
                            tracing::warn!(
                                server = %task_server,
                                error = %e,
                                "订阅流错误，重试耗尽，停止消费"
                            );
                            break;
                        }
                        drop(subscription);
                        match pool
                            .relisten_subscription(&task_server, &mut retries_left)
                            .await
                        {
                            Some(new_subscription) => subscription = new_subscription,
                            None => break,
                        }
                    }
                }
            }
        });
        self.subscription_tasks
            .lock()
            .await
            .insert(server.to_string(), vec![handle]);
    }

    /// 订阅流异常中断后的恢复：退避等待后按 server 当前配置重新建立
    /// `subscriptions/listen` 长流。
    ///
    /// 消耗一次重试机会。连接已关闭（services 表无该 server）、订阅配置
    /// 已移除或重新 listen 失败时返回 None 退出循环。
    async fn relisten_subscription(
        self: &Arc<Self>,
        server: &str,
        retries_left: &mut usize,
    ) -> Option<Subscription> {
        *retries_left -= 1;
        let attempt = Self::SUBSCRIPTION_RETRY_LIMIT - *retries_left;
        let delay_secs = Self::SUBSCRIPTION_RETRY_BASE_DELAY_SECS << (attempt - 1);
        tracing::warn!(
            server = %server,
            attempt = %attempt,
            delay_secs = %delay_secs,
            "订阅流异常中断，退避后重新 listen"
        );
        tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
        // 连接可能已被移除/重连：取 services 表中的当前 peer
        let peer = {
            let services = self.services.lock().await;
            services.get(server).map(|s| s.peer().clone())
        };
        let Some(peer) = peer else {
            tracing::info!(server = %server, "连接已关闭，订阅循环退出");
            return None;
        };
        // 配置可能已被移除：按当前配置重建过滤器
        let filter = self
            .configs
            .read()
            .get(server)
            .and_then(|c| c.subscriptions.as_ref())
            .filter(|s| !s.is_empty())
            .map(build_subscription_filter);
        let Some(filter) = filter else {
            tracing::info!(server = %server, "订阅配置已移除，订阅循环退出");
            return None;
        };
        match peer.listen(filter).await {
            Ok(new_subscription) => {
                tracing::info!(server = %server, "订阅流重新建立");
                Some(new_subscription)
            }
            Err(e) => {
                tracing::warn!(
                    server = %server,
                    error = %e,
                    "重新 listen 失败，订阅循环退出"
                );
                None
            }
        }
    }

    pub fn server_infos(&self) -> Vec<ServerInfo> {
        self.clients
            .read()
            .values()
            .map(|h| ServerInfo {
                name: h.name.clone(),
                transport_type: if h.url.is_some() { "http" } else { "stdio" }.to_string(),
                status: h.status.clone(),
                tool_count: h.tools.len(),
                resource_count: h.resources.len(),
                oauth_status: h.oauth_status.clone(),
                source: h.source.clone(),
                url: h.url.clone(),
                plugin_source: self.plugin_source_of(&h.name),
            })
            .collect()
    }

    /// 返回所有 MCP 服务器信息（合并 configs + clients）
    ///
    /// config 中有但 clients 中没有的 server 会被标记为 Uninitialized。
    /// 这覆盖了连接失败后被移除、运行时新增配置、以及 disabled 后被清理等场景。
    pub fn all_server_infos(&self) -> Vec<ServerInfo> {
        let clients = self.clients.read();
        let configs = self.configs.read();

        let mut result: Vec<ServerInfo> = Vec::new();

        // 先遍历 clients 表中的所有条目
        for h in clients.values() {
            result.push(ServerInfo {
                name: h.name.clone(),
                transport_type: if h.url.is_some() { "http" } else { "stdio" }.to_string(),
                status: h.status.clone(),
                tool_count: h.tools.len(),
                resource_count: h.resources.len(),
                oauth_status: h.oauth_status.clone(),
                source: h.source.clone(),
                url: h.url.clone(),
                plugin_source: self.plugin_source_of(&h.name),
            });
        }

        // 遍历 configs，补充 clients 中不存在的条目（标记为 Uninitialized）
        for (name, sc) in configs.iter() {
            if !clients.contains_key(name) {
                result.push(ServerInfo {
                    name: name.clone(),
                    transport_type: if sc.url.is_some() { "http" } else { "stdio" }.to_string(),
                    status: ClientStatus::Uninitialized,
                    tool_count: 0,
                    resource_count: 0,
                    oauth_status: OAuthStatus::default(),
                    source: sc.source.clone(),
                    url: sc.url.clone(),
                    plugin_source: self.plugin_source_of(name),
                });
            }
        }

        result
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

    pub async fn shutdown(&self) {
        let names: Vec<String> = self.clients.read().keys().cloned().collect();
        for name in &names {
            if let Some(c) = self.clients.write().get_mut(name) {
                if matches!(c.status, ClientStatus::Connected) {
                    tracing::info!(server = %name, "关闭连接");
                }
                let h = Arc::make_mut(c);
                h.status = ClientStatus::Disconnected;
                h.peer = None;
            }
        }
        for (_name, mut svc) in self.services.lock().await.drain() {
            let _ = svc.close_with_timeout(SHUTDOWN_TIMEOUT).await;
        }
    }

    // ── 状态变化统一出口（上下线通知） ──────────────────────────────────────

    /// 标记初始化完成。此后发生的状态变化才产生上下线通知（初始化期间的
    /// 连接结果由首 turn 概览覆盖，不逐条通知）。
    pub fn mark_initialized(&self) {
        self.initialized
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// 记录一次状态变化（统一出口）。
    ///
    /// `old` 为调用方在修改客户端表**之前**捕获的旧状态；`None` 表示表内
    /// 此前不存在（首次插入/重建，无"变化"语义，不通知）。调用方完成
    /// 状态写入后调用本方法。
    ///
    /// 仅当：初始化已完成 + 表内存在且状态确实变化时，生成一行通知文本
    /// （`status_change_text`）写入 `pending_changes` 缓冲（McpMiddleware
    /// 经 before_model drain 后以 Info 消息推送进模型上下文），并调用
    /// notifier 回调（发布 system-notification 给 TUI 通知面）。
    pub(crate) fn record_status_change(&self, name: &str, old: Option<&ClientStatus>) {
        if !self.initialized.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let Some(old) = old else { return };
        let clients = self.clients.read();
        let Some(handle) = clients.get(name) else {
            return;
        };
        if &handle.status == old {
            return;
        }
        let text = status_change_text(name, &handle.status, handle.tools.len());
        self.pending_changes.lock().push(text.clone());
        if let Some(notifier) = self.notifier.read().as_ref() {
            notifier(&text);
        }
    }

    /// 注入状态变化通知回调（发布 system-notification 事件；装配时调用）。
    pub fn set_notifier(&self, notifier: Box<dyn Fn(&str) + Send + Sync>) {
        *self.notifier.write() = Some(notifier);
    }

    /// 初始化收口后补发初始连接通知（仅 notifier 回调，不进
    /// `pending_changes`——初始连接概览由首 turn 的 `first_turn_reminder`
    /// 覆盖，不重复注入模型上下文）。
    ///
    /// 背景：`run_initialize` 直接插入 Connected handle（不经过
    /// [`Self::record_status_change`]），且 `mark_initialized` 在全部连接
    /// 之后才置位——初始化期间的连接事件永远不产生 notifier 调用。
    /// 装配面 / session 预热挂载的连接事件 notifier
    /// （`attach_connection_notifier`）因此收不到初始连接，只有重连 /
    /// OAuth 完成（`mark_initialized` 之后的 `record_status_change`）才能
    /// 触发。本方法在初始化收口时补发一次，使「刚进入、未说话」场景下
    /// 连接完成的 server 也能立即驱动 skill 发现。
    ///
    /// 锁序：先持 clients 读锁收集文本（短临界区），再持 notifier 读锁
    /// 逐条回调。回调可能重入 pool 读锁（`run_ensure_discovery`）——
    /// parking_lot 读锁可重入，与 `record_status_change` 锁内调用先例一致。
    pub fn notify_initial_connections(&self) {
        let texts: Vec<String> = {
            let clients = self.clients.read();
            clients
                .values()
                .filter(|h| matches!(h.status, ClientStatus::Connected))
                .map(|h| status_change_text(&h.name, &h.status, h.tools.len()))
                .collect()
        };
        let notifier_guard = self.notifier.read();
        if let Some(notifier) = notifier_guard.as_ref() {
            for text in texts {
                notifier(&text);
            }
        }
    }

    /// 取出待注入的状态变化文本（McpMiddleware::before_model 调用；恰好一次）。
    pub(crate) fn drain_pending_changes(&self) -> Vec<String> {
        std::mem::take(&mut *self.pending_changes.lock())
    }
}

struct PendingOAuthCallback {
    flow_id: String,
    tx: tokio::sync::oneshot::Sender<OAuthCallbackResult>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OAuthStartDisposition {
    Started,
    AlreadyActive,
    Conflict { active_flow_id: String },
}

/// 由 `McpSubscriptionsConfig` 构建 `subscriptions/listen` 过滤器。
pub(crate) fn build_subscription_filter(sub: &McpSubscriptionsConfig) -> SubscriptionFilter {
    let mut b = SubscriptionFilter::builder();
    if !sub.resources.is_empty() {
        b = b.resource_subscriptions(sub.resources.iter().cloned());
    }
    if sub.tools_list_changed {
        b = b.tools_list_changed();
    }
    if sub.prompts_list_changed {
        b = b.prompts_list_changed();
    }
    if sub.resources_list_changed {
        b = b.resources_list_changed();
    }
    b.build()
}

/// 按订阅配置选择握手方式并带超时连接（initialize / reconnect 共用）。
///
/// - 配置了 subscriptions：协商 2026-07-28 协议（`Auto`：先 server/discover，
///   服务器不支持时回退 legacy 握手）；
/// - 否则维持 legacy 握手（优先 channel handler，其次空 handler）。
// ClientInitializeError 来自 rmcp crate，无法修改其定义
#[allow(clippy::result_large_err)]
pub(crate) async fn serve_client_auto<T, E, A>(
    transport: T,
    channel_handler: Option<&Arc<ChannelHandler>>,
    subscriptions: Option<&McpSubscriptionsConfig>,
    timeout: std::time::Duration,
) -> Result<Result<McpServiceWrapper, ClientInitializeError>, tokio::time::error::Elapsed>
where
    T: IntoTransport<RoleClient, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    if subscriptions.is_some() {
        tokio::time::timeout(
            timeout,
            rmcp::service::serve_client_with_lifecycle(
                (),
                transport,
                ClientLifecycleMode::Auto {
                    preferred_versions: vec![
                        ProtocolVersion::V_2026_07_28,
                        ProtocolVersion::V_2025_11_25,
                    ],
                    legacy_version: None,
                },
            ),
        )
        .await
        .map(|inner| inner.map(McpServiceWrapper::Default))
    } else if let Some(handler) = channel_handler {
        tokio::time::timeout(
            timeout,
            rmcp::service::serve_client(handler.clone(), transport),
        )
        .await
        .map(|inner| inner.map(McpServiceWrapper::Channel))
    } else {
        tokio::time::timeout(timeout, rmcp::service::serve_client((), transport))
            .await
            .map(|inner| inner.map(McpServiceWrapper::Default))
    }
}

/// 连接成功后建立 `subscriptions/listen` 长流并启动消费循环（2026-07-28 协议）。
///
/// 失败仅告警——server 可能不支持，连接本身仍可用。initialize / reconnect 共用。
pub(crate) async fn setup_subscription(
    pool: &Arc<McpClientPool>,
    rs: &McpServiceWrapper,
    name: &str,
    sub: &McpSubscriptionsConfig,
) {
    match rs.peer().listen(build_subscription_filter(sub)).await {
        Ok(subscription) => {
            pool.spawn_subscription_loop(name, subscription).await;
            tracing::info!(
                server = %name,
                resources = ?sub.resources,
                "subscriptions/listen 已建立"
            );
        }
        Err(e) => {
            tracing::warn!(
                server = %name,
                error = %e,
                "subscriptions/listen 建立失败（server 可能不支持）"
            );
        }
    }
}

/// XML 转义五个特殊字符（`&` `<` `>` `"` `'`），防止第三方 MCP server
/// 推送的字段值（如资源 URI）注入 `<system-reminder>` 结构。
pub(crate) fn xml_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

pub(crate) fn spawn_stdio_transport(
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
) -> std::io::Result<rmcp::transport::child_process::TokioChildProcess> {
    use std::process::Stdio;

    let arg_strs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let mut cmd = peri_agent::agent::async_tasks::shell_command(command, &arg_strs);
    cmd.envs(env);

    let builder = rmcp::transport::child_process::TokioChildProcess::builder(cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let (child_process, stderr_opt) = builder.spawn()?;

    // 启动后台任务消费 stderr 并记录到 tracing
    if let Some(stderr) = stderr_opt {
        let cmd_name = command.to_string();
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();

            while let Ok(Some(line)) = lines.next_line().await {
                tracing::warn!(
                    command = %cmd_name,
                    stderr = %line,
                    "MCP 子进程 stderr"
                );
            }
        });
    }

    Ok(child_process)
}

pub(crate) fn build_http_transport(
    url: &str,
    headers: &HashMap<String, String>,
) -> rmcp::transport::StreamableHttpClientTransport<reqwest::Client> {
    use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
    let mut config = StreamableHttpClientTransportConfig::with_uri(url);
    let mut custom_headers = std::collections::HashMap::new();
    for (key, value) in headers {
        match reqwest::header::HeaderName::try_from(key.as_str()) {
            Ok(name) => match reqwest::header::HeaderValue::from_str(value) {
                Ok(val) => {
                    custom_headers.insert(name, val);
                }
                Err(e) => {
                    tracing::warn!(header = %key, error = %e, "header 值无效");
                }
            },
            Err(e) => {
                tracing::warn!(header = %key, error = %e, "header 名称无效");
            }
        }
    }
    if !custom_headers.is_empty() {
        config = config.custom_headers(custom_headers);
    }
    rmcp::transport::StreamableHttpClientTransport::with_client(reqwest::Client::new(), config)
}

pub(crate) fn build_authed_transport(
    url: &str,
    headers: &HashMap<String, String>,
    auth_manager: rmcp::transport::auth::AuthorizationManager,
) -> rmcp::transport::StreamableHttpClientTransport<
    rmcp::transport::auth::AuthClient<reqwest::Client>,
> {
    use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
    let mut config = StreamableHttpClientTransportConfig::with_uri(url);
    let mut custom_headers = std::collections::HashMap::new();
    for (key, value) in headers {
        match reqwest::header::HeaderName::try_from(key.as_str()) {
            Ok(name) => match reqwest::header::HeaderValue::from_str(value) {
                Ok(val) => {
                    custom_headers.insert(name, val);
                }
                Err(e) => {
                    tracing::warn!(header = %key, error = %e, "header 值无效");
                }
            },
            Err(e) => {
                tracing::warn!(header = %key, error = %e, "header 名称无效");
            }
        }
    }
    if !custom_headers.is_empty() {
        config = config.custom_headers(custom_headers);
    }
    let auth_client = rmcp::transport::auth::AuthClient::new(reqwest::Client::new(), auth_manager);
    rmcp::transport::StreamableHttpClientTransport::with_client(auth_client, config)
}

// 3.0 批 2 波 2：装配注入端口实现（ACP 侧只持 `Arc<dyn McpPoolPort>`）。
// M-TUI 收口：`shutdown`（host/shutdown 命令面）与 `snapshot`（mcp/list
// 命令面）为新增数据端口；TUI 不再直持池句柄与 watch channel。
#[async_trait::async_trait]
impl peri_acp_types::ports::McpPoolPort for McpClientPool {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    async fn shutdown(&self) {
        McpClientPool::shutdown(self).await;
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
