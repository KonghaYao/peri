use super::*;
use crate::mcp::config::McpConfigFile;
use rmcp::transport::auth::StoredCredentials;

fn http_config(oauth: Option<OAuthConfig>) -> McpConfigFile {
    serde_json::from_value(serde_json::json!({
        "mcpServers": {
            "server": {
                "url": "https://example/mcp",
                "oauth": oauth,
            }
        }
    }))
    .unwrap()
}

use peri_acp_types::oauth_credentials::{OAuthCredentialPort, OAuthCredentialResult};

#[derive(Default)]
struct MemoryOAuthCredentialPort {
    records: parking_lot::Mutex<std::collections::HashMap<String, String>>,
}

#[async_trait::async_trait]
impl OAuthCredentialPort for MemoryOAuthCredentialPort {
    async fn load(&self, key: &str) -> OAuthCredentialResult<Option<String>> {
        Ok(self.records.lock().get(key).cloned())
    }

    async fn save(&self, key: &str, credentials: &str) -> OAuthCredentialResult<()> {
        self.records
            .lock()
            .insert(key.to_string(), credentials.to_string());
        Ok(())
    }

    async fn clear(&self, key: &str) -> OAuthCredentialResult<()> {
        self.records.lock().remove(key);
        Ok(())
    }

    async fn clear_all(&self) -> OAuthCredentialResult<()> {
        self.records.lock().clear();
        Ok(())
    }

    async fn list(&self) -> OAuthCredentialResult<Vec<String>> {
        Ok(self.records.lock().keys().cloned().collect())
    }
}

fn memory_client() -> crate::mcp::auth_store::OAuthCredentialClient {
    crate::mcp::auth_store::OAuthCredentialClient::new(std::sync::Arc::new(
        MemoryOAuthCredentialPort::default(),
    ))
    .unwrap()
}

#[tokio::test]
async fn explicit_oauth_without_credentials_reports_failure_and_seals_injection() {
    let pool = Arc::new(McpClientPool::new_pending());
    let (status_tx, status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);
    McpClientPool::initialize_config(
        pool.clone(),
        Path::new("."),
        http_config(Some(OAuthConfig::default())),
        Default::default(),
        status_tx,
        None,
    )
    .await;
    let handle = pool.get_client("server").unwrap();
    assert!(
        matches!(&handle.status, ClientStatus::Failed(reason) if reason.contains("not injected"))
    );
    assert!(matches!(&*status_rx.borrow(), McpInitStatus::Failed(_)));
    assert!(pool
        .active_oauth_flow_scoped(&super::super::client::McpConnectionKey::static_server(
            "server"
        ))
        .is_none());
    assert!(pool.inject_oauth_credentials(memory_client()).is_err());
}

#[tokio::test]
async fn anonymous_http_without_credentials_attempts_connection() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let observed = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 2048];
        let received = socket.read(&mut request).await.unwrap();
        socket.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
        String::from_utf8_lossy(&request[..received]).contains("/mcp")
    });
    let config: McpConfigFile = serde_json::from_value(serde_json::json!({
        "mcpServers": { "server": { "url": url } }
    }))
    .unwrap();
    let pool = Arc::new(McpClientPool::new_pending());
    let (status_tx, _) = tokio::sync::watch::channel(McpInitStatus::Pending);
    McpClientPool::initialize_config(
        pool.clone(),
        Path::new("."),
        config,
        Default::default(),
        status_tx,
        None,
    )
    .await;
    assert!(observed.await.unwrap());
    assert!(matches!(
        &pool.get_client("server").unwrap().status,
        ClientStatus::Failed(_)
    ));
}

#[tokio::test]
async fn anonymous_http_without_credentials_discovers_live_tools() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let header_end = loop {
                let mut chunk = [0_u8; 4096];
                let size = socket.read(&mut chunk).await.unwrap();
                assert!(size > 0, "HTTP request ended before headers");
                request.extend_from_slice(&chunk[..size]);
                if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    break end + 4;
                }
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            if headers.starts_with("GET ") {
                socket.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nAllow: POST\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                continue;
            }
            let content_length: usize = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|value| value.trim().parse().ok())
                })
                .unwrap_or(0);
            while request.len() - header_end < content_length {
                let mut chunk = [0_u8; 4096];
                let size = socket.read(&mut chunk).await.unwrap();
                assert!(size > 0, "HTTP request ended before body");
                request.extend_from_slice(&chunk[..size]);
            }
            let message: serde_json::Value =
                serde_json::from_slice(&request[header_end..header_end + content_length]).unwrap();
            let method = message
                .get("method")
                .and_then(|method| method.as_str())
                .unwrap_or_default();
            let body = match method {
                "server/discover" => serde_json::json!({
                    "jsonrpc":"2.0", "id":message["id"],
                    "error":{"code":-32601,"message":"Method not found"}
                }).to_string(),
                "initialize" => serde_json::json!({
                    "jsonrpc":"2.0", "id":message["id"],
                    "result": {"protocolVersion":"2025-03-26", "capabilities":{"tools":{}},
                    "serverInfo":{"name":"anonymous-fixture","version":"1.0.0"}}
                }).to_string(),
                "tools/list" => serde_json::json!({
                    "jsonrpc":"2.0", "id":message["id"],
                    "result":{"tools":[{"name":"ping", "description":"Fixture tool", "inputSchema":{"type":"object","properties":{}}}]}
                }).to_string(),
                "notifications/initialized" => String::new(),
                other => panic!("unexpected MCP method: {other}"),
            };
            let response = if body.is_empty() {
                "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_string()
            } else {
                format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
            };
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    let config: McpConfigFile = serde_json::from_value(serde_json::json!({
        "mcpServers": { "server": { "url": url } }
    }))
    .unwrap();
    let pool = Arc::new(McpClientPool::new_pending());
    let (status_tx, _) = tokio::sync::watch::channel(McpInitStatus::Pending);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        McpClientPool::initialize_config(
            pool.clone(),
            Path::new("."),
            config,
            Default::default(),
            status_tx,
            None,
        ),
    )
    .await
    .unwrap();
    let handle = pool.get_client("server").unwrap();
    assert!(matches!(handle.status, ClientStatus::Connected));
    assert_eq!(
        handle
            .tools
            .iter()
            .map(|tool| tool.name.as_ref())
            .collect::<Vec<_>>(),
        vec!["ping"]
    );
    assert!(pool.discovery_evidence("server").unwrap().is_complete());
    server.abort();
    let _ = pool.shutdown().await;
}

#[tokio::test]
async fn stdio_absence_is_reported_without_starting_process() {
    let config: McpConfigFile = serde_json::from_value(serde_json::json!({
        "mcpServers": { "server": { "command": "this-command-must-not-run" } }
    }))
    .unwrap();
    let pool = Arc::new(McpClientPool::new_pending());
    pool.set_stdio_available(false).unwrap();
    let (status_tx, _) = tokio::sync::watch::channel(McpInitStatus::Pending);
    McpClientPool::initialize_config(
        pool.clone(),
        Path::new("."),
        config,
        Default::default(),
        status_tx,
        None,
    )
    .await;
    assert!(
        matches!(&pool.get_client("server").unwrap().status, ClientStatus::Failed(reason) if reason.contains("unavailable"))
    );
    let error = pool.reconnect("server", None).await.unwrap_err();
    assert!(error.to_string().contains("unavailable"));
}

#[tokio::test]
async fn initialization_discovers_stored_oauth_via_injected_port_without_starting_browser() {
    for oauth in [
        None,
        Some(OAuthConfig {
            scopes: Some(vec!["read".into()]),
            ..Default::default()
        }),
    ] {
        let pool = Arc::new(McpClientPool::new_pending());
        let client = memory_client();
        let key = static_credential_key(
            "server",
            "https://example/mcp",
            &oauth.clone().unwrap_or_default(),
        );
        client
            .save_server(
                &key,
                StoredCredentials::new("client".into(), None, vec![], None),
            )
            .await
            .unwrap();
        pool.inject_oauth_credentials(client).unwrap();
        let (status_tx, _) = tokio::sync::watch::channel(McpInitStatus::Pending);
        McpClientPool::initialize_config(
            pool.clone(),
            Path::new("."),
            http_config(oauth),
            Default::default(),
            status_tx,
            Some(Box::new(|_| {
                panic!("initialization must not start interactive OAuth")
            })),
        )
        .await;
        let handle = pool.get_client("server").unwrap();
        assert!(matches!(
            handle.oauth_status,
            OAuthStatus::NeedsAuthorization
        ));
        assert!(pool
            .active_oauth_flow_scoped(&super::super::client::McpConnectionKey::static_server(
                "server"
            ))
            .is_none());
    }
}
