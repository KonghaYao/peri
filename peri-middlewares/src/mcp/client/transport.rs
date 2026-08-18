use std::{collections::HashMap, sync::Arc};

use peri_acp_types::plugin::McpSubscriptionsConfig;
use rmcp::{
    model::ProtocolVersion,
    service::{ClientInitializeError, ClientLifecycleMode, RoleClient},
    transport::IntoTransport,
};

use super::super::channel_handler::ChannelHandler;
use super::McpServiceWrapper;

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
