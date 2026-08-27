use std::{collections::HashMap, sync::Arc};

use peri_acp_types::plugin::McpProtocolVersion;
use rmcp::{
    model::ProtocolVersion,
    service::{ClientInitializeError, ClientLifecycleMode, RoleClient},
    transport::IntoTransport,
};

use super::super::channel_handler::ChannelHandler;
use super::McpServiceWrapper;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpLifecycle {
    Legacy,
    Discover2026_07_28,
}

fn lifecycle_for(protocol_version: Option<&McpProtocolVersion>) -> McpLifecycle {
    match protocol_version {
        Some(McpProtocolVersion::V2026_07_28) => McpLifecycle::Discover2026_07_28,
        None => McpLifecycle::Legacy,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionMode {
    LegacyDefault,
    LegacyChannel,
    DiscoverDefault,
    DiscoverChannel,
}

fn connection_mode(
    protocol_version: Option<&McpProtocolVersion>,
    has_channel_handler: bool,
) -> ConnectionMode {
    match (lifecycle_for(protocol_version), has_channel_handler) {
        (McpLifecycle::Legacy, false) => ConnectionMode::LegacyDefault,
        (McpLifecycle::Legacy, true) => ConnectionMode::LegacyChannel,
        (McpLifecycle::Discover2026_07_28, false) => ConnectionMode::DiscoverDefault,
        (McpLifecycle::Discover2026_07_28, true) => ConnectionMode::DiscoverChannel,
    }
}

/// 按显式协议版本选择握手方式并带超时连接（initialize / reconnect 共用）。
///
/// 仅 `protocolVersion: "2026-07-28"` 使用 `server/discover`；未配置或其他版本
/// 严格保持 legacy `initialize`。subscriptions 不参与 lifecycle 选择。
// ClientInitializeError 来自 rmcp crate，无法修改其定义
#[allow(clippy::result_large_err)]
pub(crate) async fn serve_client_auto<T, E, A>(
    transport: T,
    channel_handler: Option<&Arc<ChannelHandler>>,
    protocol_version: Option<&McpProtocolVersion>,
    timeout: std::time::Duration,
) -> Result<Result<McpServiceWrapper, ClientInitializeError>, tokio::time::error::Elapsed>
where
    T: IntoTransport<RoleClient, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    match connection_mode(protocol_version, channel_handler.is_some()) {
        ConnectionMode::DiscoverChannel => {
            let handler = channel_handler.expect("channel mode requires handler");
            tokio::time::timeout(
                timeout,
                rmcp::service::serve_client_with_lifecycle(
                    handler.clone(),
                    transport,
                    ClientLifecycleMode::Discover {
                        preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                    },
                ),
            )
            .await
            .map(|inner| inner.map(McpServiceWrapper::Channel))
        }
        ConnectionMode::DiscoverDefault => tokio::time::timeout(
            timeout,
            rmcp::service::serve_client_with_lifecycle(
                super::mcpp_client_info(),
                transport,
                ClientLifecycleMode::Discover {
                    preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                },
            ),
        )
        .await
        .map(|inner| inner.map(McpServiceWrapper::Default)),
        ConnectionMode::LegacyChannel => {
            let handler = channel_handler.expect("channel mode requires handler");
            tokio::time::timeout(
                timeout,
                rmcp::service::serve_client(handler.clone(), transport),
            )
            .await
            .map(|inner| inner.map(McpServiceWrapper::Channel))
        }
        ConnectionMode::LegacyDefault => tokio::time::timeout(
            timeout,
            rmcp::service::serve_client(super::mcpp_client_info(), transport),
        )
        .await
        .map(|inner| inner.map(McpServiceWrapper::Default)),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    async fn observe_first_request(protocol_version: Option<&McpProtocolVersion>) -> String {
        let (client_io, server_io) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            let (read, mut write) = tokio::io::split(server_io);
            let mut lines = BufReader::new(read).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            let method = request["method"].as_str().unwrap().to_string();
            let id = request["id"].clone();
            let result = if method == "server/discover" {
                serde_json::json!({
                    "resultType": "complete",
                    "supportedVersions": ["2026-07-28"],
                    "capabilities": {},
                    "serverInfo": { "name": "test-server", "version": "1.0.0" },
                    "ttlMs": 0,
                    "cacheScope": "private"
                })
            } else {
                serde_json::json!({
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "serverInfo": { "name": "test-server", "version": "1.0.0" }
                })
            };
            let response = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
            write
                .write_all(format!("{response}\n").as_bytes())
                .await
                .unwrap();
            if method == "initialize" {
                let initialized = lines.next_line().await.unwrap().unwrap();
                let notification: serde_json::Value = serde_json::from_str(&initialized).unwrap();
                assert_eq!(notification["method"], "notifications/initialized");
            }
            method
        });

        let _service = serve_client_auto(
            client_io,
            None,
            protocol_version,
            std::time::Duration::from_secs(2),
        )
        .await
        .expect("握手不应超时")
        .expect("握手应成功");
        let method = server.await.unwrap();
        method
    }

    #[tokio::test]
    async fn none_starts_with_initialize_and_accepts_2025_11_25_response() {
        assert_eq!(observe_first_request(None).await, "initialize");
    }

    #[tokio::test]
    async fn explicit_2026_07_28_transport_starts_with_discover() {
        assert_eq!(
            observe_first_request(Some(&McpProtocolVersion::V2026_07_28)).await,
            "server/discover"
        );
    }

    #[test]
    fn lifecycle_requires_explicit_2026_07_28() {
        assert_eq!(lifecycle_for(None), McpLifecycle::Legacy);
        assert_eq!(
            lifecycle_for(Some(&McpProtocolVersion::V2026_07_28)),
            McpLifecycle::Discover2026_07_28
        );
    }

    #[test]
    fn connection_mode_preserves_channel_handler_for_both_lifecycles() {
        assert_eq!(connection_mode(None, false), ConnectionMode::LegacyDefault);
        assert_eq!(connection_mode(None, true), ConnectionMode::LegacyChannel);
        assert_eq!(
            connection_mode(Some(&McpProtocolVersion::V2026_07_28), false),
            ConnectionMode::DiscoverDefault
        );
        assert_eq!(
            connection_mode(Some(&McpProtocolVersion::V2026_07_28), true),
            ConnectionMode::DiscoverChannel
        );
    }
}
