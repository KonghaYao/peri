use std::collections::HashMap;

use rmcp::{
    model::ProtocolVersion,
    service::{ClientInitializeError, ClientLifecycleMode, RoleClient},
    transport::IntoTransport,
};

use super::McpServiceWrapper;

/// 客户端声明的首选协议版本：只有 2026-07-28。
const PREFERRED_PROTOCOL_VERSIONS: [ProtocolVersion; 1] = [ProtocolVersion::V_2026_07_28];

/// 在给定 lifecycle 下完成握手，并限制总连接时间（initialize / reconnect 共用）。
///
/// ClientInitializeError 来自 rmcp crate，无法修改其定义
#[allow(clippy::result_large_err)]
async fn serve_client_with_mode<T, E, A>(
    transport: T,
    capability_profile: &crate::mcp::apps::McpCapabilityProfile,
    timeout: std::time::Duration,
    lifecycle: ClientLifecycleMode,
) -> Result<Result<McpServiceWrapper, ClientInitializeError>, peri_time::Elapsed>
where
    T: IntoTransport<RoleClient, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    peri_time::timeout(timeout, async {
        rmcp::service::serve_client_with_lifecycle(
            super::mcpp_client_info_for_profile(capability_profile),
            transport,
            lifecycle,
        )
        .await
        .map(McpServiceWrapper::Default)
    })
    .await
}

/// 非 HTTP 链路（builtin 同进程、stdio、ACP 承载）的握手：Auto。
///
/// 先尝试 `server/discover`，对端不支持时回退 legacy initialize——ACP 承载的
/// MCP 由客户端侧提供，版本由对方决定，不在本仓库的部署约束内。
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
    let lifecycle = ClientLifecycleMode::Auto {
        preferred_versions: PREFERRED_PROTOCOL_VERSIONS.to_vec(),
        legacy_version: None,
    };
    serve_client_with_mode(transport, capability_profile, timeout, lifecycle).await
}

/// HTTP 直连链路的握手：部署决定要不要 legacy 回退。
///
/// 浏览器部署（emscripten，peri-cf）只接 2026-07-28：走 Discover 协商，对端不
/// 支持即失败，不回退 legacy initialize。原生部署维持 Auto。
#[allow(clippy::result_large_err)]
pub(crate) async fn serve_client_http<T, E, A>(
    transport: T,
    capability_profile: &crate::mcp::apps::McpCapabilityProfile,
    timeout: std::time::Duration,
) -> Result<Result<McpServiceWrapper, ClientInitializeError>, peri_time::Elapsed>
where
    T: IntoTransport<RoleClient, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    let lifecycle = http_lifecycle();
    serve_client_with_mode(transport, capability_profile, timeout, lifecycle).await
}

#[cfg(target_os = "emscripten")]
fn http_lifecycle() -> ClientLifecycleMode {
    ClientLifecycleMode::Discover {
        preferred_versions: PREFERRED_PROTOCOL_VERSIONS.to_vec(),
    }
}

#[cfg(not(target_os = "emscripten"))]
fn http_lifecycle() -> ClientLifecycleMode {
    ClientLifecycleMode::Auto {
        preferred_versions: PREFERRED_PROTOCOL_VERSIONS.to_vec(),
        legacy_version: None,
    }
}

/// 浏览器部署的 HTTP 通道是宿主 `fetch`；reqwest 在 wasm 目标上没有真实网络。
#[cfg(target_os = "emscripten")]
pub(crate) fn build_http_transport(
    url: &str,
    headers: &HashMap<String, String>,
) -> rmcp::transport::StreamableHttpClientTransport<super::fetch_http::FetchHttpClient> {
    rmcp::transport::StreamableHttpClientTransport::with_client(
        super::fetch_http::FetchHttpClient,
        http_transport_config(url, headers),
    )
}

#[cfg(not(target_os = "emscripten"))]
pub(crate) fn build_http_transport(
    url: &str,
    headers: &HashMap<String, String>,
) -> rmcp::transport::StreamableHttpClientTransport<reqwest::Client> {
    rmcp::transport::StreamableHttpClientTransport::with_client(
        reqwest::Client::new(),
        http_transport_config(url, headers),
    )
}

/// 两个目标共用的 transport 配置：URI 与自定义头（含 worker 注入的协议头）。
fn http_transport_config(
    url: &str,
    headers: &HashMap<String, String>,
) -> rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig {
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
    config
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
