use std::{collections::HashMap, sync::Arc};

use rmcp::{
    model::{Resource, Tool},
    service::{Peer, QuitReason, RoleClient, RunningService, ServiceError},
};
use thiserror::Error;

use super::{
    channel_handler::ChannelHandler,
    config::{ConfigSource, McpServerConfig},
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
}

/// MCP 客户端连接池
pub struct McpClientPool {
    pub(crate) clients: parking_lot::RwLock<HashMap<String, Arc<McpClientHandle>>>,
    pub(crate) services: tokio::sync::Mutex<HashMap<String, McpServiceWrapper>>,
    pub(crate) configs: parking_lot::RwLock<HashMap<String, McpServerConfig>>,
    /// 插件来源旁路表：key 为 server name（如 `"plugin:p1:srv1"`），value 为 `"name@marketplace"`
    pub(crate) plugin_sources: parking_lot::RwLock<HashMap<String, String>>,
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
        }
    }

    #[cfg(test)]
    pub fn new_empty() -> Self {
        Self::new_pending()
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
                channel_capable: false,
            }),
        );
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
                channel_capable: false,
            }),
        );
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
        self.configs.write().remove(server_name);
    }

    /// 将服务器标记为 Disabled：关闭连接但保留 config 和 handle（用于面板展示）
    pub async fn set_disabled(self: &Arc<Self>, server_name: &str) {
        // 关闭实际连接
        if let Some(mut svc) = self.services.lock().await.remove(server_name) {
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
                peer: None,
                tools: vec![],
                resources: vec![],
                status: ClientStatus::Disabled,
                oauth_status: OAuthStatus::default(),
                source,
                url,
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
}

pub(crate) fn spawn_stdio_transport(
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
) -> std::io::Result<rmcp::transport::child_process::TokioChildProcess> {
    use std::process::Stdio;

    let arg_strs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let mut cmd = crate::process::shell_command(command, &arg_strs);
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

#[cfg(test)]
#[path = "client_test.rs"]
mod tests;
