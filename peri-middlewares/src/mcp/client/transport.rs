use std::{collections::HashMap, sync::Arc};

use rmcp::{
    model::ProtocolVersion,
    service::{ClientInitializeError, ClientLifecycleMode, RoleClient},
    transport::IntoTransport,
};

use super::{McpClientPool, McpServiceWrapper, NotificationTap, PeriClientHandler};

/// 使用官方 lifecycle 协商并限制总连接时间（initialize / reconnect 共用）。
///
/// 所有连接使用 Auto：先尝试 `server/discover`，不支持时回退 legacy initialize。
/// 本入口不装配通知 sink（ACP 声明连接 / 测试直连）；生产 server 用
/// [`serve_client_auto_for_server`]。
// ClientInitializeError 来自 rmcp crate，无法修改其定义
#[allow(clippy::result_large_err)]
pub(crate) async fn serve_client_auto<T, E, A>(
    transport: T,
    capability_profile: &crate::mcp::apps::McpCapabilityProfile,
    timeout: std::time::Duration,
) -> Result<Result<McpServiceWrapper, ClientInitializeError>, peri_time::Elapsed>
where
    T: IntoTransport<RoleClient, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    let handler = PeriClientHandler::new(
        super::mcpp_client_info_for_profile(capability_profile),
        None,
    );
    serve_client_auto_with_handler(handler, transport, timeout).await
}

/// 与 [`serve_client_auto`] 同一条 Auto 握手，但把该 server 的通知汇入 pool 缓冲。
#[allow(clippy::result_large_err)]
pub(crate) async fn serve_client_auto_for_server<T, E, A>(
    transport: T,
    pool: &Arc<McpClientPool>,
    server: &str,
    timeout: std::time::Duration,
) -> Result<Result<McpServiceWrapper, ClientInitializeError>, peri_time::Elapsed>
where
    T: IntoTransport<RoleClient, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    let handler = PeriClientHandler::new(
        super::mcpp_client_info_for_profile(&pool.capability_profile),
        Some(Arc::new(NotificationTap::new(pool, server))),
    );
    serve_client_auto_with_handler(handler, transport, timeout).await
}

#[allow(clippy::result_large_err)]
async fn serve_client_auto_with_handler<T, E, A>(
    handler: PeriClientHandler,
    transport: T,
    timeout: std::time::Duration,
) -> Result<Result<McpServiceWrapper, ClientInitializeError>, peri_time::Elapsed>
where
    T: IntoTransport<RoleClient, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    let preferred_versions = vec![ProtocolVersion::V_2026_07_28];
    let lifecycle = ClientLifecycleMode::Auto {
        preferred_versions,
        legacy_version: None,
    };
    peri_time::timeout(timeout, async {
        rmcp::service::serve_client_with_lifecycle(handler, transport, lifecycle)
            .await
            .map(McpServiceWrapper::Default)
    })
    .await
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
#[path = "transport_test.rs"]
mod tests;
