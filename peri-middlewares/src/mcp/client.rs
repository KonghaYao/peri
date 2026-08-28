mod subscription;
mod transport;

use std::{any::Any, collections::HashMap, sync::Arc};

use peri_acp_types::dynamic_mcp::DynamicMcpInstanceKey;
use peri_acp_types::mcp::McpSubscriptionPort;
use peri_acp_types::ports::McpPoolShutdownReport;
use peri_acp_types::session::InboxHandle;
use rmcp::{
    model::{
        CacheScope, ClientCapabilities, Implementation, InitializeRequestParams,
        PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResult, Resource, Tool,
    },
    service::{Peer, QuitReason, RoleClient, RunningService},
};
use thiserror::Error;

use super::{
    channel_handler::ChannelHandler,
    config::{ConfigSource, McpServerConfig},
    oauth_flow::{OAuthCallbackResult, OAuthFlowEvent},
};

#[cfg(test)]
pub(crate) use subscription::build_subscription_filter;
pub(crate) use subscription::setup_subscription;
pub(crate) use transport::{
    build_authed_transport, build_http_transport, serve_client_auto, spawn_stdio_transport,
};

/// Wrapper for RunningService that can hold either handler type
pub(crate) enum McpServiceWrapper {
    Default(RunningService<RoleClient, InitializeRequestParams>),
    Channel(RunningService<RoleClient, Arc<ChannelHandler>>),
    #[cfg(test)]
    Controlled(ControlledMcpService),
}

#[cfg(test)]
pub(crate) struct ControlledMcpService {
    entered: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: Arc<tokio::sync::Notify>,
    close_count: Arc<std::sync::atomic::AtomicUsize>,
    completes: bool,
}

#[cfg(test)]
impl ControlledMcpService {
    pub(crate) fn new(
        entered: tokio::sync::oneshot::Sender<()>,
        release: Arc<tokio::sync::Notify>,
        close_count: Arc<std::sync::atomic::AtomicUsize>,
    ) -> Self {
        Self {
            entered: std::sync::Mutex::new(Some(entered)),
            release,
            close_count,
            completes: true,
        }
    }

    pub(crate) fn timing_out(close_count: Arc<std::sync::atomic::AtomicUsize>) -> Self {
        let (entered, _entered_rx) = tokio::sync::oneshot::channel();
        Self {
            entered: std::sync::Mutex::new(Some(entered)),
            release: Arc::new(tokio::sync::Notify::new()),
            close_count,
            completes: false,
        }
    }

    async fn close(&mut self) -> Result<Option<QuitReason>, tokio::task::JoinError> {
        self.close_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(entered) = self
            .entered
            .lock()
            .expect("controlled service poisoned")
            .take()
        {
            let _ = entered.send(());
        }
        if self.completes {
            self.release.notified().await;
            Ok(Some(QuitReason::Closed))
        } else {
            Ok(None)
        }
    }
}

impl McpServiceWrapper {
    pub async fn close_with_timeout(
        &mut self,
        timeout: std::time::Duration,
    ) -> Result<Option<QuitReason>, tokio::task::JoinError> {
        match self {
            McpServiceWrapper::Default(svc) => svc.close_with_timeout(timeout).await,
            McpServiceWrapper::Channel(svc) => svc.close_with_timeout(timeout).await,
            #[cfg(test)]
            McpServiceWrapper::Controlled(svc) => svc.close().await,
        }
    }

    pub fn peer(&self) -> &Peer<RoleClient> {
        match self {
            McpServiceWrapper::Default(svc) => svc.peer(),
            McpServiceWrapper::Channel(svc) => svc.peer(),
            #[cfg(test)]
            McpServiceWrapper::Controlled(_) => {
                panic!("controlled test service has no protocol peer")
            }
        }
    }
}

/// 供状态与日志使用的 MCP 错误文本清洗：移除 URL query，遮蔽常见凭据键值。
/// 不应将原始底层错误链直接投影到 UI 或日志。
pub fn redact_mcp_error(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for token in input.split_whitespace() {
        let token = if let Some((prefix, _)) = token.split_once('?') {
            if prefix.starts_with("http://") || prefix.starts_with("https://") {
                format!("{prefix}?…")
            } else {
                token.to_string()
            }
        } else {
            token.to_string()
        };
        let lower = token.to_ascii_lowercase();
        if ["token=", "password=", "secret=", "api_key=", "apikey="]
            .iter()
            .any(|key| lower.contains(key))
        {
            output.push_str("[redacted]");
        } else {
            output.push_str(&token);
        }
        output.push(' ');
    }
    output.trim_end().to_string()
}

pub(crate) const SERVER_CACHE_VERSION_EXTENSION: &str = "io.mcpp/server-cache-version";

pub(crate) fn mcpp_client_info_for_profile(
    profile: &super::apps::McpCapabilityProfile,
) -> InitializeRequestParams {
    let mut extensions = std::collections::BTreeMap::from([(
        SERVER_CACHE_VERSION_EXTENSION.to_string(),
        serde_json::Map::new(),
    )]);
    if let Some(extension) = profile.ui_extension() {
        extensions.insert(super::apps::MCP_UI_EXTENSION.to_string(), extension);
    }
    let mut capabilities = ClientCapabilities::default();
    capabilities.extensions = Some(extensions);
    InitializeRequestParams::new(capabilities, Implementation::from_build_env())
}

pub(crate) fn peer_cache_version(peer: &Peer<RoleClient>) -> Option<String> {
    peer.peer_info()?
        .capabilities
        .extensions
        .as_ref()?
        .get(SERVER_CACHE_VERSION_EXTENSION)?
        .get("cacheVersion")?
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
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
    pub version: Option<String>,
    pub cache_version: Option<String>,
    pub transport_type: String,
    pub status: ClientStatus,
    /// 供 UI 显示的稳定状态标签，不暴露 `ClientStatus::Failed` 的完整错误链。
    pub status_label: String,
    /// 供 UI 显示的一行安全错误摘要；完整诊断仅写入 tracing 日志。
    pub error_summary: Option<String>,
    /// 供 UI 显示最近一次持久化 cache 结果；None 表示尚无缓存请求。
    pub cache_status: Option<String>,
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
    pub version: Option<String>,
    pub cache_version: Option<String>,
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

fn mcp_status_label(status: &ClientStatus) -> &'static str {
    match status {
        ClientStatus::Connected => "connected",
        ClientStatus::Failed(_) => "failed",
        ClientStatus::Disconnected => "disconnected",
        ClientStatus::Disabled => "disabled",
        ClientStatus::Uninitialized => "uninitialized",
    }
}

fn mcp_error_summary(status: &ClientStatus) -> Option<String> {
    let ClientStatus::Failed(reason) = status else {
        return None;
    };
    let summary = redact_mcp_error(reason.lines().next().unwrap_or_default().trim());
    let summary: String = summary.chars().take(160).collect();
    (!summary.is_empty()).then_some(summary)
}

pub(crate) fn cache_scope_allows_persistence(scope: Option<CacheScope>) -> bool {
    match scope {
        Some(CacheScope::Public) | Some(CacheScope::Private) => true,
        Some(_) | None => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum McpConnectionKey {
    Static { server_name: String },
    Dynamic { instance: DynamicMcpInstanceKey },
}

impl McpConnectionKey {
    pub(crate) fn static_server(server_name: impl Into<String>) -> Self {
        Self::Static {
            server_name: server_name.into(),
        }
    }

    pub(crate) fn dynamic(instance: DynamicMcpInstanceKey) -> Self {
        Self::Dynamic { instance }
    }

    pub(crate) fn server_name(&self) -> &str {
        match self {
            Self::Static { server_name } => server_name,
            Self::Dynamic { instance } => &instance.logical.server_name,
        }
    }

    pub(crate) fn is_dynamic(&self) -> bool {
        matches!(self, Self::Dynamic { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct OAuthFlowKey {
    connection: McpConnectionKey,
    flow_id: String,
}

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

enum ServiceShutdownState {
    Idle,
    Running {
        handle: tokio::task::JoinHandle<McpPoolShutdownReport>,
        total_services: usize,
    },
    Terminal(McpPoolShutdownReport),
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

async fn close_services(services: Vec<(String, McpServiceWrapper)>) -> McpPoolShutdownReport {
    let mut settled_services = 0;
    let mut unfinished_services = 0;
    let mut failed_services = 0;
    for (server_name, mut service) in services {
        match service.close_with_timeout(SHUTDOWN_TIMEOUT).await {
            Ok(Some(_reason)) => settled_services += 1,
            Ok(None) => {
                unfinished_services += 1;
                tracing::warn!(server = %server_name, "MCP service cleanup remained unfinished");
            }
            Err(error) => {
                settled_services += 1;
                failed_services += 1;
                tracing::warn!(server = %server_name, %error, "MCP service cleanup task failed");
            }
        }
    }
    if unfinished_services == 0 {
        McpPoolShutdownReport::Complete {
            settled_services,
            failed_services,
        }
    } else {
        McpPoolShutdownReport::Incomplete {
            settled_services,
            unfinished_services,
            failed_services,
        }
    }
}

impl McpClientPool {
    pub(crate) fn handle_generation(&self, handle: &Arc<McpClientHandle>) -> u64 {
        self.handle_generations
            .lock()
            .get(&handle.name)
            .and_then(|entries| {
                entries.iter().find_map(|(candidate, generation)| {
                    candidate
                        .upgrade()
                        .filter(|candidate| Arc::ptr_eq(candidate, handle))
                        .map(|_| *generation)
                })
            })
            .unwrap_or(0)
    }

    fn advance_handle_generation(&self, handle: &Arc<McpClientHandle>) -> u64 {
        let generation = self
            .next_handle_generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut generations = self.handle_generations.lock();
        let entries = generations.entry(handle.name.clone()).or_default();
        entries.retain(|(candidate, _)| candidate.strong_count() > 0);
        entries.push((Arc::downgrade(handle), generation));
        generation
    }

    fn config_allows_persistent_cache(config: &McpServerConfig) -> bool {
        // `private` 只可在匿名上下文复用。任意静态 header、HTTP query 与
        // stdio env 都可能携带 Cookie、API key 或服务自定义凭据，保守禁用。
        config.oauth.is_none()
            && config
                .headers
                .as_ref()
                .is_none_or(std::collections::HashMap::is_empty)
            && config.url.as_deref().is_none_or(|url| !url.contains('?'))
            && config
                .env
                .as_ref()
                .is_none_or(std::collections::HashMap::is_empty)
    }

    pub(crate) fn persistent_cache_allowed(&self, server_name: &str) -> bool {
        self.persistent_cache_allowed_for(&McpConnectionKey::static_server(server_name))
    }

    pub(crate) fn persistent_cache_allowed_for(&self, connection: &McpConnectionKey) -> bool {
        if connection.is_dynamic() {
            // Dynamic MCP 首版 fail-closed：在 scoped cache ticket/current-instance
            // fencing 完成前，不读取或写入持久化 resource cache。
            return false;
        }
        self.configs
            .read()
            .get(connection.server_name())
            .is_none_or(Self::config_allows_persistent_cache)
    }

    pub(crate) fn install_peer_cache_version(
        &self,
        server_name: &str,
        peer: &Peer<RoleClient>,
    ) -> Option<String> {
        let cache_version = peer_cache_version(peer);
        let origin = self.cache_origin(server_name);
        self.resource_cache
            .set_cache_version(&origin, cache_version.as_deref());
        if let Some(version) = cache_version.as_ref() {
            self.cache_versions
                .write()
                .insert(server_name.to_string(), version.clone());
        } else {
            self.cache_versions.write().remove(server_name);
        }
        cache_version
    }

    pub(crate) async fn read_resource_cached(
        &self,
        server_name: &str,
        uri: &str,
        peer: &Peer<RoleClient>,
    ) -> Result<
        (
            ReadResourceResult,
            Option<super::resource_cache::CacheTicket>,
        ),
        rmcp::service::ServiceError,
    > {
        if !self.persistent_cache_allowed(server_name) {
            return Ok((
                peer.read_resource(ReadResourceRequestParams::new(uri))
                    .await?,
                None,
            ));
        }
        let origin = self.cache_origin(server_name);
        let cache_version = self.cache_versions.read().get(server_name).cloned();
        if let Some(result) = self
            .resource_cache
            .get_versioned(&origin, "resources/read", uri, cache_version.as_deref())
            .await
        {
            return Ok((result, None));
        }
        self.resource_cache
            .mark_live_fetch(&origin, "resources/read");
        let Some(ticket) = self
            .resource_cache
            .ticket(&origin, "resources/read", uri)
            .await
        else {
            return Ok((
                peer.read_resource(ReadResourceRequestParams::new(uri))
                    .await?,
                None,
            ));
        };
        let result = peer
            .read_resource(ReadResourceRequestParams::new(uri))
            .await?;
        Ok((result, Some(ticket)))
    }

    /// 仅由资源使用方在内容验证成功后调用。这样受 SEP-2640 内容绑定保护的
    /// `skill://` 响应不会在 digest 校验失败时落入跨进程缓存。
    pub(crate) async fn cache_verified_resource(
        &self,
        server_name: &str,
        ticket: Option<super::resource_cache::CacheTicket>,
        result: &ReadResourceResult,
    ) {
        let Some(ticket) = ticket else { return };
        self.persist_cacheable_response(
            server_name,
            &ticket,
            result,
            result.ttl_ms,
            result.cache_scope,
        )
        .await;
    }

    pub(crate) async fn list_resources_cached(
        &self,
        server_name: &str,
        params: Option<PaginatedRequestParams>,
        peer: &Peer<RoleClient>,
    ) -> Result<rmcp::model::ListResourcesResult, rmcp::service::ServiceError> {
        if !self.persistent_cache_allowed(server_name) {
            return peer.list_resources(params).await;
        }
        let origin = self.cache_origin(server_name);
        let params_key = serde_json::to_string(&params).unwrap_or_default();
        let cache_version = self.cache_versions.read().get(server_name).cloned();
        if let Some(result) = self
            .resource_cache
            .get_versioned(
                &origin,
                "resources/list",
                &params_key,
                cache_version.as_deref(),
            )
            .await
        {
            return Ok(result);
        }
        self.resource_cache
            .mark_live_fetch(&origin, "resources/list");
        let ticket = self
            .resource_cache
            .ticket(&origin, "resources/list", &params_key)
            .await;
        let result = peer.list_resources(params).await?;
        if let Some(ticket) = ticket {
            self.persist_cacheable_response(
                server_name,
                &ticket,
                &result,
                result.ttl_ms,
                result.cache_scope,
            )
            .await;
        }
        Ok(result)
    }

    pub(crate) async fn list_all_resources_cached(
        &self,
        server_name: &str,
        peer: &Peer<RoleClient>,
    ) -> Result<Vec<Resource>, rmcp::service::ServiceError> {
        let mut resources = Vec::new();
        let mut cursor = None;
        loop {
            let result = self
                .list_resources_cached(
                    server_name,
                    Some(PaginatedRequestParams::default().with_cursor(cursor)),
                    peer,
                )
                .await?;
            resources.extend(result.resources);
            cursor = result.next_cursor;
            if cursor.is_none() {
                return Ok(resources);
            }
        }
    }

    /// 缓存包装器供后续 Resource Template 消费者使用；当前 Agent 尚未暴露
    /// templates/list 的目录工具，因此不在初始化阶段进行无目的预取。
    pub async fn list_resource_templates_cached(
        &self,
        server_name: &str,
        params: Option<PaginatedRequestParams>,
        peer: &Peer<RoleClient>,
    ) -> Result<rmcp::model::ListResourceTemplatesResult, rmcp::service::ServiceError> {
        if !self.persistent_cache_allowed(server_name) {
            return peer.list_resource_templates(params).await;
        }
        let origin = self.cache_origin(server_name);
        let params_key = serde_json::to_string(&params).unwrap_or_default();
        let cache_version = self.cache_versions.read().get(server_name).cloned();
        if let Some(result) = self
            .resource_cache
            .get_versioned(
                &origin,
                "resources/templates/list",
                &params_key,
                cache_version.as_deref(),
            )
            .await
        {
            return Ok(result);
        }
        let ticket = self
            .resource_cache
            .ticket(&origin, "resources/templates/list", &params_key)
            .await;
        let result = peer.list_resource_templates(params).await?;
        if let Some(ticket) = ticket {
            self.persist_cacheable_response(
                server_name,
                &ticket,
                &result,
                result.ttl_ms,
                result.cache_scope,
            )
            .await;
        }
        Ok(result)
    }

    pub(crate) async fn invalidate_resource_cache(&self, server_name: &str, uri: Option<&str>) {
        let origin = self.cache_origin(server_name);
        self.invalidate_resource_cache_origin(&origin, uri).await;
    }

    /// 仅当 server 在 initialize 声明 `io.mcpp/server-cache-version` 且当前安全
    /// 策略允许持久化时，跨进程复用磁盘上的 `tools/list` schema；否则保持原始
    /// 网络行为（每次回源）。命中以协商的 cache_version 为准：同版本命中跳过
    /// 网络，版本缺失/变化必定回源。
    pub(crate) async fn list_all_tools_cached(
        &self,
        server_name: &str,
        peer: &Peer<RoleClient>,
    ) -> Result<Vec<Tool>, rmcp::service::ServiceError> {
        if !self.tools_cache_eligible(server_name) {
            return peer.list_all_tools().await;
        }
        let origin = self.cache_origin(server_name);
        let cache_version = self.cache_versions.read().get(server_name).cloned();
        if let Some(version) = cache_version.as_deref() {
            if let Some(tools) = self
                .resource_cache
                .get_versioned::<Vec<Tool>>(&origin, "tools/list", "", Some(version))
                .await
            {
                return Ok(tools);
            }
        }
        self.resource_cache.mark_live_fetch(&origin, "tools/list");
        let ticket = self.resource_cache.ticket(&origin, "tools/list", "").await;
        let tools = peer.list_all_tools().await?;
        if let (Some(ticket), Some(version)) = (ticket, cache_version.as_deref()) {
            self.resource_cache
                .put_ticket_versioned(&ticket, std::time::Duration::ZERO, Some(version), &tools)
                .await;
        }
        Ok(tools)
    }

    /// 跨进程复用 `tools/list` 缓存的准入：安全策略允许持久化且 server 已声明
    /// cache_version。二者任一不满足则只回源、不读盘（对应「无版本不命中」与
    /// 「安全策略回退」）。
    pub(crate) fn tools_cache_eligible(&self, server_name: &str) -> bool {
        self.persistent_cache_allowed(server_name)
            && self.cache_versions.read().contains_key(server_name)
    }

    /// `notifications/tools/list_changed` 到达时失效该 origin 的磁盘 `tools/list`
    /// 缓存。订阅未启用时由版本比对安全兜底（下次回源用新版本失效旧条目）。
    pub(crate) async fn invalidate_tools_cache(&self, server_name: &str) {
        let origin = self.cache_origin(server_name);
        self.resource_cache
            .invalidate(&origin, "tools/list", None)
            .await;
    }

    pub(crate) async fn invalidate_resource_cache_origin(&self, origin: &str, uri: Option<&str>) {
        match uri {
            Some(_uri) => {
                // 一个 resources/read 响应可包含多个 contents[] URI；当前 cache
                // 未维护反向索引，无法确认通知 URI 对应哪个聚合请求。按 MCPP
                // 7.3.2 保守失效该 origin 的 read domain，避免聚合响应继续命中。
                self.resource_cache
                    .invalidate(origin, "resources/read", None)
                    .await;
            }
            None => {
                self.resource_cache
                    .invalidate(origin, "resources/list", None)
                    .await;
                self.resource_cache
                    .invalidate(origin, "resources/templates/list", None)
                    .await;
            }
        }
    }

    pub(crate) fn cache_origin(&self, server_name: &str) -> String {
        let config = self.configs.read().get(server_name).cloned();
        super::resource_cache::cache_origin(server_name, config.as_ref())
    }

    pub(crate) fn resource_cache(&self) -> super::resource_cache::McpResourceCache {
        self.resource_cache.clone()
    }

    fn cache_status_for(&self, server_name: &str) -> Option<String> {
        if !self.persistent_cache_allowed(server_name) {
            return Some("cache_disabled".to_string());
        }
        let origin = self.cache_origin(server_name);
        if let Some(status) = self.resource_cache.recent_status(&origin) {
            return Some(match status {
                super::resource_cache::CacheLoadStatus::VersionHit => "version_cached".to_string(),
                super::resource_cache::CacheLoadStatus::McppHit => "mcpp_cached".to_string(),
                super::resource_cache::CacheLoadStatus::ResourceHit => "cached".to_string(),
                super::resource_cache::CacheLoadStatus::LiveFetch => "live_fetch".to_string(),
                super::resource_cache::CacheLoadStatus::StoredAfterFetch => {
                    "stored_after_fetch".to_string()
                }
            });
        }
        Some(if self.persistent_cache_allowed(server_name) {
            "cache_ready".to_string()
        } else {
            "cache_disabled".to_string()
        })
    }

    async fn persist_cacheable_response<T: serde::Serialize>(
        &self,
        server_name: &str,
        ticket: &super::resource_cache::CacheTicket,
        result: &T,
        ttl_ms: Option<u64>,
        cache_scope: Option<CacheScope>,
    ) {
        if !self.persistent_cache_allowed(server_name) {
            return;
        }
        let cache_version = self.cache_versions.read().get(server_name).cloned();
        let can_reuse = cache_scope_allows_persistence(cache_scope)
            || (cache_scope.is_none() && ttl_ms.is_some());
        if !can_reuse {
            return;
        }
        let ttl = std::time::Duration::from_millis(ttl_ms.unwrap_or_default());
        if !ttl.is_zero() || cache_version.is_some() {
            self.resource_cache
                .put_ticket_versioned(ticket, ttl, cache_version.as_deref(), result)
                .await;
        }
    }

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

    /// 注入 OAuth 流程事件回调（host 装配面在 `run_initialize` 前调用；
    /// 回调负责把 `AuthorizationNeeded` 的 `callback_tx` 注册进
    /// `pending_oauth_callbacks`，并将事件转发为 ACP 通知）。
    pub fn set_oauth_event_callback<F>(&self, callback: F)
    where
        F: Fn(OAuthFlowEvent) + Send + Sync + 'static,
    {
        let _admission = self.lifecycle_registration.lock();
        if self.is_open() {
            *self.oauth_event_callback.write() = Some(Arc::new(callback));
        }
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
        self.register_oauth_callback_scoped(
            McpConnectionKey::static_server(server_name),
            flow_id,
            callback_tx,
        )
    }

    pub fn register_dynamic_oauth_callback(
        &self,
        instance: DynamicMcpInstanceKey,
        flow_id: &str,
        callback_tx: tokio::sync::oneshot::Sender<OAuthCallbackResult>,
    ) -> bool {
        self.register_oauth_callback_scoped(
            McpConnectionKey::dynamic(instance),
            flow_id,
            callback_tx,
        )
    }

    pub(crate) fn register_oauth_callback_scoped(
        &self,
        connection: McpConnectionKey,
        flow_id: &str,
        callback_tx: tokio::sync::oneshot::Sender<OAuthCallbackResult>,
    ) -> bool {
        let _admission = self.lifecycle_registration.lock();
        if !self.is_open() {
            return false;
        }
        let mut active = self.active_oauth_flows.lock();
        match active.get(&connection) {
            Some(current) if current != flow_id => return false,
            Some(_) => {}
            None => {
                active.insert(connection.clone(), flow_id.to_string());
            }
        }
        drop(active);
        self.pending_oauth_callbacks.lock().insert(
            OAuthFlowKey {
                connection,
                flow_id: flow_id.to_string(),
            },
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
        let connection = McpConnectionKey::static_server(server_name);
        let flow_id = self
            .active_oauth_flow_scoped(&connection)
            .ok_or_else(|| format!("{server_name} 无进行中的 OAuth 授权"))?;
        let pending = self
            .pending_oauth_callbacks
            .lock()
            .remove(&OAuthFlowKey {
                connection,
                flow_id,
            })
            .ok_or_else(|| format!("{server_name} 无进行中的 OAuth 授权"))?;
        pending
            .tx
            .send(OAuthCallbackResult { code, state })
            .map_err(|_| format!("{server_name} OAuth 授权流程已结束"))
    }

    pub fn deliver_dynamic_oauth_callback(
        &self,
        instance: DynamicMcpInstanceKey,
        flow_id: &str,
        code: String,
        state: String,
    ) -> Result<(), String> {
        self.deliver_oauth_callback_scoped(
            &McpConnectionKey::dynamic(instance),
            flow_id,
            code,
            state,
        )
    }

    /// 以完整 dynamic identity 精确投递授权码。调用方必须持有当前
    /// `(session_id, incarnation_id, flow_id)`，禁止退化为裸 server name。
    fn deliver_oauth_callback_scoped(
        &self,
        connection: &McpConnectionKey,
        flow_id: &str,
        code: String,
        state: String,
    ) -> Result<(), String> {
        if !connection.is_dynamic() {
            return Err("scoped OAuth callback requires dynamic identity".to_string());
        }
        let key = OAuthFlowKey {
            connection: connection.clone(),
            flow_id: flow_id.to_string(),
        };
        let pending = self
            .pending_oauth_callbacks
            .lock()
            .remove(&key)
            .filter(|pending| pending.flow_id == flow_id)
            .ok_or_else(|| "OAuth flow 不再等待 callback".to_string())?;
        pending
            .tx
            .send(OAuthCallbackResult { code, state })
            .map_err(|_| "OAuth flow 已结束".to_string())
    }

    /// 以 flow identity 精确投递授权码。Hub 不使用该接口；保留给未来安全
    /// 手动 callback 能力，避免 server-name-only 的错误归属。
    pub fn deliver_oauth_callback_for_flow(
        &self,
        flow_id: &str,
        code: String,
        state: String,
    ) -> Result<(), String> {
        let connection = self
            .connection_for_oauth_flow(flow_id)
            .ok_or_else(|| "OAuth flow 不存在".to_string())?;
        let key = OAuthFlowKey {
            connection,
            flow_id: flow_id.to_string(),
        };
        let pending = self
            .pending_oauth_callbacks
            .lock()
            .remove(&key)
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
        let connection = McpConnectionKey::static_server(server_name);
        let flow_id = self.active_oauth_flows.lock().get(&connection).cloned();
        flow_id
            .as_deref()
            .map(|flow_id| self.cancel_oauth_flow(flow_id))
            .unwrap_or(false)
    }

    pub fn cancel_dynamic_oauth_flow(
        &self,
        instance: DynamicMcpInstanceKey,
        flow_id: &str,
    ) -> bool {
        self.cancel_oauth_flow_scoped(&McpConnectionKey::dynamic(instance), flow_id)
    }

    fn cancel_oauth_flow_scoped(&self, connection: &McpConnectionKey, flow_id: &str) -> bool {
        if !connection.is_dynamic() {
            return false;
        }
        self.pending_oauth_callbacks
            .lock()
            .remove(&OAuthFlowKey {
                connection: connection.clone(),
                flow_id: flow_id.to_string(),
            })
            .is_some_and(|pending| pending.flow_id == flow_id)
    }

    /// 精确取消一个活跃 flow；不接受由客户端指定的 server 名称。
    pub fn cancel_oauth_flow(&self, flow_id: &str) -> bool {
        let Some(connection) = self.connection_for_oauth_flow(flow_id) else {
            return false;
        };
        self.pending_oauth_callbacks
            .lock()
            .remove(&OAuthFlowKey {
                connection,
                flow_id: flow_id.to_string(),
            })
            .is_some_and(|pending| pending.flow_id == flow_id)
    }

    pub fn active_oauth_flow(&self, server_name: &str) -> Option<String> {
        self.active_oauth_flow_scoped(&McpConnectionKey::static_server(server_name))
    }

    pub(crate) fn active_oauth_flow_scoped(&self, connection: &McpConnectionKey) -> Option<String> {
        self.active_oauth_flows.lock().get(connection).cloned()
    }

    pub(crate) fn reserve_oauth_flow(
        &self,
        server_name: &str,
        flow_id: &str,
    ) -> OAuthStartDisposition {
        self.reserve_oauth_flow_scoped(McpConnectionKey::static_server(server_name), flow_id)
    }

    pub(crate) fn reserve_oauth_flow_scoped(
        &self,
        connection: McpConnectionKey,
        flow_id: &str,
    ) -> OAuthStartDisposition {
        let _admission = self.lifecycle_registration.lock();
        if !self.is_open() {
            return OAuthStartDisposition::Conflict {
                active_flow_id: "pool-closing".to_string(),
            };
        }
        let mut active = self.active_oauth_flows.lock();
        match active.get(&connection) {
            Some(current) if current == flow_id => OAuthStartDisposition::AlreadyActive,
            Some(current) => OAuthStartDisposition::Conflict {
                active_flow_id: current.clone(),
            },
            None => {
                active.insert(connection, flow_id.to_string());
                OAuthStartDisposition::Started
            }
        }
    }

    pub fn release_oauth_flow(&self, server_name: &str, flow_id: &str) {
        self.release_oauth_flow_scoped(&McpConnectionKey::static_server(server_name), flow_id);
    }

    pub(crate) fn release_oauth_flow_scoped(&self, connection: &McpConnectionKey, flow_id: &str) {
        let mut active = self.active_oauth_flows.lock();
        if active
            .get(connection)
            .is_some_and(|current| current == flow_id)
        {
            active.remove(connection);
        }
        drop(active);
        self.pending_oauth_callbacks.lock().remove(&OAuthFlowKey {
            connection: connection.clone(),
            flow_id: flow_id.to_string(),
        });
    }

    pub(crate) fn revoke_oauth_connection(&self, connection: &McpConnectionKey) {
        let flow_id = self.active_oauth_flows.lock().remove(connection);
        if let Some(flow_id) = flow_id {
            self.pending_oauth_callbacks.lock().remove(&OAuthFlowKey {
                connection: connection.clone(),
                flow_id,
            });
        }
    }

    fn connection_for_oauth_flow(&self, flow_id: &str) -> Option<McpConnectionKey> {
        self.active_oauth_flows
            .lock()
            .iter()
            .find_map(|(connection, active)| (active == flow_id).then(|| connection.clone()))
    }

    /// 查询指定 server 的插件来源标识，非插件 server 返回 None
    /// key 格式为 `"plugin_name__server_name"`，返回 `"name@marketplace"`
    pub fn plugin_source_of(&self, name: &str) -> Option<String> {
        self.plugin_sources.read().get(name).cloned()
    }

    pub(crate) fn insert_failed(pool: &Arc<Self>, name: &str, reason: String) {
        let old_status = {
            let _admission = pool.lifecycle_registration.lock();
            if !pool.is_open() {
                return;
            }
            let (source, url) = pool
                .configs
                .read()
                .get(name)
                .map(|c| (c.source.clone(), c.url.clone()))
                .unwrap_or((None, None));
            let old_status = pool.clients.read().get(name).map(|c| c.status.clone());
            let handle = Arc::new(McpClientHandle {
                name: name.to_string(),
                version: None,
                cache_version: None,
                peer: None,
                tools: vec![],
                resources: vec![],
                status: ClientStatus::Failed(reason.clone()),
                oauth_status: OAuthStatus::default(),
                source,
                url,
                skills_capable: false,
                channel_capable: false,
            });
            pool.advance_handle_generation(&handle);
            pool.clients.write().insert(name.to_string(), handle);
            old_status
        };
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
        let old_status = {
            let _admission = pool.lifecycle_registration.lock();
            if !pool.is_open() {
                return;
            }
            let (source, url) = pool
                .configs
                .read()
                .get(name)
                .map(|c| (c.source.clone(), c.url.clone()))
                .unwrap_or((None, None));
            let old_status = pool.clients.read().get(name).map(|c| c.status.clone());
            let handle = Arc::new(McpClientHandle {
                name: name.to_string(),
                version: None,
                cache_version: None,
                peer: None,
                tools: vec![],
                resources: vec![],
                status: ClientStatus::Failed(reason),
                oauth_status: OAuthStatus::NeedsAuthorization,
                source,
                url,
                skills_capable: false,
                channel_capable: false,
            });
            pool.advance_handle_generation(&handle);
            pool.clients.write().insert(name.to_string(), handle);
            old_status
        };
        pool.record_status_change(name, old_status.as_ref());
    }

    /// 检测错误是否为 HTTP 401 认证错误
    pub(crate) fn is_auth_required_error(error: &str, transport_is_http: bool) -> bool {
        transport_is_http && (error.contains("Auth required") || error.contains("AuthRequired"))
    }

    pub async fn remove_server(self: &Arc<Self>, server_name: &str) {
        self.stop_background(&super::task_scope::McpTaskKey::Subscription(
            server_name.to_string(),
        ))
        .await;
        self.clients.write().remove(server_name);
        let service = { self.services.lock().remove(server_name) };
        if let Some(mut svc) = service {
            let _ = svc.close_with_timeout(SHUTDOWN_TIMEOUT).await;
        }
        self.configs.write().remove(server_name);
    }

    /// 将服务器标记为 Disabled：关闭连接但保留 config 和 handle（用于面板展示）
    pub async fn set_disabled(self: &Arc<Self>, server_name: &str) {
        self.stop_background(&super::task_scope::McpTaskKey::Subscription(
            server_name.to_string(),
        ))
        .await;
        // 关闭实际连接
        let service = { self.services.lock().remove(server_name) };
        if let Some(mut svc) = service {
            let _ = svc.close_with_timeout(SHUTDOWN_TIMEOUT).await;
        }
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
                version: None,
                cache_version: None,
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

    pub fn server_infos(&self) -> Vec<ServerInfo> {
        self.clients
            .read()
            .values()
            .map(|h| ServerInfo {
                name: h.name.clone(),
                version: h.version.clone(),
                cache_version: h.cache_version.clone(),
                transport_type: if h.url.is_some() { "http" } else { "stdio" }.to_string(),
                status: h.status.clone(),
                status_label: mcp_status_label(&h.status).to_string(),
                error_summary: mcp_error_summary(&h.status),
                cache_status: self.cache_status_for(&h.name),
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
                version: h.version.clone(),
                cache_version: h.cache_version.clone(),
                transport_type: if h.url.is_some() { "http" } else { "stdio" }.to_string(),
                status: h.status.clone(),
                status_label: mcp_status_label(&h.status).to_string(),
                error_summary: mcp_error_summary(&h.status),
                cache_status: self.cache_status_for(&h.name),
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
                    version: None,
                    cache_version: None,
                    name: name.clone(),
                    transport_type: if sc.url.is_some() { "http" } else { "stdio" }.to_string(),
                    status: ClientStatus::Uninitialized,
                    status_label: "uninitialized".to_string(),
                    error_summary: None,
                    cache_status: self.cache_status_for(name),
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

    pub(crate) fn is_open(&self) -> bool {
        self.lifecycle.load(std::sync::atomic::Ordering::Acquire) == 0
    }

    pub fn begin_shutdown(&self) {
        let _admission = self.lifecycle_registration.lock();
        if !self.is_open() {
            return;
        }
        self.lifecycle
            .store(1, std::sync::atomic::Ordering::Release);
        self.notifier.write().take();
        self.oauth_event_callback.write().take();
        self.pending_oauth_callbacks.lock().clear();
        self.active_oauth_flows.lock().clear();
    }

    pub fn spawn_background<F>(
        &self,
        key: super::task_scope::McpTaskKey,
        future: F,
    ) -> Result<(), super::task_scope::McpTaskScopeClosed>
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let _admission = self.lifecycle_registration.lock();
        if !self.is_open() {
            return Err(super::task_scope::TaskAdmissionError::OwnerClosed);
        }
        self.task_spawner.spawn(key, future)
    }

    pub async fn stop_background(&self, key: &super::task_scope::McpTaskKey) {
        self.task_spawner.stop_key(key).await;
    }

    pub(crate) fn try_commit_connection(
        &self,
        name: String,
        handle: Arc<McpClientHandle>,
        service: McpServiceWrapper,
    ) -> Result<(), McpServiceWrapper> {
        let _admission = self.lifecycle_registration.lock();
        if !self.is_open() {
            return Err(service);
        }
        self.advance_handle_generation(&handle);
        self.services.lock().insert(name.clone(), service);
        self.clients.write().insert(name, handle);
        Ok(())
    }

    pub async fn shutdown(&self) -> McpPoolShutdownReport {
        let mut transaction = self.service_shutdown.lock().await;
        if matches!(*transaction, ServiceShutdownState::Idle) {
            self.begin_shutdown();
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
            let mut services: Vec<_> = self.services.lock().drain().collect();
            services.sort_unstable_by(|left, right| left.0.cmp(&right.0));
            let total_services = services.len();
            let handle = tokio::spawn(close_services(services));
            *transaction = ServiceShutdownState::Running {
                handle,
                total_services,
            };
        }

        let report = match &mut *transaction {
            ServiceShutdownState::Idle => unreachable!("shutdown transaction must be installed"),
            ServiceShutdownState::Terminal(report) => return *report,
            ServiceShutdownState::Running {
                handle,
                total_services,
            } => match handle.await {
                Ok(report) => report,
                Err(error) => {
                    tracing::error!(%error, "MCP service shutdown transaction failed");
                    McpPoolShutdownReport::Incomplete {
                        settled_services: 0,
                        unfinished_services: *total_services,
                        failed_services: *total_services,
                    }
                }
            },
        };
        *transaction = ServiceShutdownState::Terminal(report);
        if report.is_complete() {
            self.lifecycle
                .store(2, std::sync::atomic::Ordering::Release);
        }
        report
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
        let notifier = self.notifier.read().clone();
        if let Some(notifier) = notifier {
            notifier(&text);
        }
    }

    /// 注入状态变化通知回调（发布 system-notification 事件；装配时调用）。
    pub fn set_notifier(&self, notifier: Box<dyn Fn(&str) + Send + Sync>) {
        let _admission = self.lifecycle_registration.lock();
        if self.is_open() {
            *self.notifier.write() = Some(Arc::from(notifier));
        }
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
        let notifier = self.notifier.read().clone();
        if let Some(notifier) = notifier {
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
