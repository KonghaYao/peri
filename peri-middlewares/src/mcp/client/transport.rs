use std::{collections::HashMap, sync::Arc};

use peri_acp_types::plugin::McpProtocolVersion;
use rmcp::{
    model::ProtocolVersion,
    service::{ClientInitializeError, ClientLifecycleMode, RoleClient},
    transport::IntoTransport,
};

use super::super::channel_handler::ChannelHandler;
use super::McpServiceWrapper;

/// 使用官方 lifecycle 协商并限制总连接时间（initialize / reconnect 共用）。
///
/// 缺省使用 Auto；显式版本使用 Discover，不回退 legacy。
// ClientInitializeError 来自 rmcp crate，无法修改其定义
#[allow(clippy::result_large_err)]
pub(crate) async fn serve_client_auto<T, E, A>(
    transport: T,
    channel_handler: Option<&Arc<ChannelHandler>>,
    protocol_version: Option<&McpProtocolVersion>,
    capability_profile: &crate::mcp::apps::McpCapabilityProfile,
    timeout: std::time::Duration,
) -> Result<Result<McpServiceWrapper, ClientInitializeError>, tokio::time::error::Elapsed>
where
    T: IntoTransport<RoleClient, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    let preferred_versions = vec![ProtocolVersion::V_2026_07_28];
    let lifecycle = match protocol_version {
        None => ClientLifecycleMode::Auto {
            preferred_versions,
            legacy_version: None,
        },
        Some(McpProtocolVersion::V2026_07_28) => {
            ClientLifecycleMode::Discover { preferred_versions }
        }
    };
    tokio::time::timeout(timeout, async {
        match channel_handler {
            Some(handler) => {
                let handler = Arc::new(handler.with_capability_profile(capability_profile.clone()));
                rmcp::service::serve_client_with_lifecycle(handler, transport, lifecycle)
                    .await
                    .map(McpServiceWrapper::Channel)
            }
            None => rmcp::service::serve_client_with_lifecycle(
                super::mcpp_client_info_for_profile(capability_profile),
                transport,
                lifecycle,
            )
            .await
            .map(McpServiceWrapper::Default),
        }
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
