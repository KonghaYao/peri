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
async fn initialization_without_credentials_reports_failure_and_seals_injection() {
    let pool = Arc::new(McpClientPool::new_pending());
    let (status_tx, status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);
    McpClientPool::initialize_config(
        pool.clone(),
        Path::new("."),
        http_config(None),
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
    assert!(pool.inject_oauth_credentials(memory_client()).is_err());
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
