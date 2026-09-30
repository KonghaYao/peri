use super::*;
use std::sync::Arc;

use crate::mcp::{client::McpClientPool, config::McpServerConfig};

fn http_config(endpoint: &str, oauth: Option<OAuthConfig>) -> McpServerConfig {
    serde_json::from_value(serde_json::json!({ "url": endpoint, "oauth": oauth })).unwrap()
}

use peri_acp_types::oauth_credentials::{
    OAuthCredentialError, OAuthCredentialPort, OAuthCredentialResult,
};

#[derive(Default)]
struct MemoryOAuthCredentialPort {
    records: parking_lot::Mutex<std::collections::HashMap<String, String>>,
    reject_clear: bool,
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
        if self.reject_clear {
            return Err(OAuthCredentialError::Unavailable);
        }
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

fn failing_clear_client() -> crate::mcp::auth_store::OAuthCredentialClient {
    crate::mcp::auth_store::OAuthCredentialClient::new(std::sync::Arc::new(
        MemoryOAuthCredentialPort {
            reject_clear: true,
            ..Default::default()
        },
    ))
    .unwrap()
}

#[test]
fn oauth_credential_injection_is_required_and_cannot_be_replaced() {
    let pool = McpClientPool::new_pending();
    assert!(pool.oauth_credentials().is_err());
    let client = memory_client();
    pool.inject_oauth_credentials(client.clone()).unwrap();
    assert!(pool.oauth_credentials().is_ok());
    assert!(pool.inject_oauth_credentials(client).is_err());
    assert!(pool.inject_oauth_credentials(memory_client()).is_err());
}

#[test]
fn oauth_credential_injection_is_rejected_after_initialization_starts() {
    let pool = McpClientPool::new_pending();
    pool.seal_builtin_context();
    assert!(pool.inject_oauth_credentials(memory_client()).is_err());
    assert!(pool.oauth_credentials().is_err());
}

#[test]
fn oauth_credential_injection_is_rejected_when_pool_is_closing() {
    let pool = McpClientPool::new_pending();
    pool.begin_shutdown();
    assert!(pool.inject_oauth_credentials(memory_client()).is_err());
    assert!(pool.oauth_credentials().is_err());
}

#[tokio::test]
async fn per_server_adapter_load_save_and_clear_are_isolated() {
    let client = memory_client();
    let first = PerServerCredentialStore::new(client.clone(), "first".into());
    let second = PerServerCredentialStore::new(client, "second".into());
    first
        .save(StoredCredentials::new(
            "first-client".into(),
            None,
            vec![],
            None,
        ))
        .await
        .unwrap();
    second
        .save(StoredCredentials::new(
            "second-client".into(),
            None,
            vec![],
            None,
        ))
        .await
        .unwrap();
    first.clear().await.unwrap();
    assert!(first.load().await.unwrap().is_none());
    assert_eq!(
        second.load().await.unwrap().unwrap().client_id,
        "second-client"
    );
}

#[test]
fn static_credential_identity_binds_server_endpoint_and_oauth_config() {
    let oauth = OAuthConfig::default();
    let key = static_credential_key("server", "https://first.example/mcp", &oauth);
    assert_eq!(
        key,
        static_credential_key("server", "https://first.example/mcp", &oauth)
    );
    assert_ne!(
        key,
        static_credential_key("other", "https://first.example/mcp", &oauth)
    );
    assert_ne!(
        key,
        static_credential_key("server", "https://second.example/mcp", &oauth)
    );
    for changed in [
        OAuthConfig {
            client_id: Some("changed".into()),
            ..Default::default()
        },
        OAuthConfig {
            client_secret: Some("secret-marker".into()),
            ..Default::default()
        },
        OAuthConfig {
            scopes: Some(vec!["read".into()]),
            ..Default::default()
        },
        OAuthConfig {
            enabled: Some(false),
            ..Default::default()
        },
    ] {
        let changed_key = static_credential_key("server", "https://first.example/mcp", &changed);
        assert_ne!(key, changed_key);
        assert!(!changed_key.contains("secret-marker"));
    }
}

#[tokio::test]
async fn clear_oauth_uses_current_config_key_without_clearing_other_endpoint_or_dynamic() {
    let pool = Arc::new(McpClientPool::new_pending());
    let client = memory_client();
    pool.inject_oauth_credentials(client.clone()).unwrap();
    let config = http_config(
        "https://new.example/mcp",
        Some(OAuthConfig {
            scopes: Some(vec!["read".into()]),
            ..Default::default()
        }),
    );
    let oauth = config.oauth.as_ref().unwrap();
    let current_key = static_credential_key("server", config.url.as_deref().unwrap(), oauth);
    let previous_key = static_credential_key("server", "https://old.example/mcp", oauth);
    let dynamic_key = "dynamic:session:incarnation:server";
    for key in [&current_key, &previous_key, dynamic_key] {
        client
            .save_server(
                key,
                StoredCredentials::new("client".into(), None, vec![], None),
            )
            .await
            .unwrap();
    }
    pool.configs.write().insert("server".into(), config);
    pool.clear_oauth("server").await.unwrap();
    assert!(client.load_server(&current_key).await.unwrap().is_none());
    assert!(client.load_server(&previous_key).await.unwrap().is_some());
    assert!(client.load_server(dynamic_key).await.unwrap().is_some());
}

#[tokio::test]
async fn failed_oauth_clear_is_reported_without_publishing_cleared_status() {
    let pool = Arc::new(McpClientPool::new_pending());
    pool.inject_oauth_credentials(failing_clear_client())
        .unwrap();
    pool.configs
        .write()
        .insert("server".into(), http_config("https://example/mcp", None));
    assert!(pool.clear_oauth("server").await.is_err());
    assert!(!pool.clients.read().contains_key("server"));
}

#[tokio::test]
async fn oauth_start_without_injection_fails_without_fallback() {
    let pool = Arc::new(McpClientPool::new_pending());
    let events = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_events = events.clone();
    pool.set_oauth_event_callback(move |event| {
        match event {
            crate::mcp::oauth_flow::OAuthFlowEvent::AuthorizationFailed {
                flow_id,
                server_name,
                error,
                ..
            } => {
                assert_eq!(flow_id, "flow");
                assert_eq!(server_name, "server");
                assert!(error.contains("not injected"));
            }
            _ => panic!("missing credentials must emit a terminal failure"),
        }
        observed_events.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    });
    pool.configs
        .write()
        .insert("server".into(), http_config("https://example/mcp", None));
    let error = pool
        .start_oauth_flow("flow", "server", false)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not injected"));
    assert_eq!(events.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn reconnect_without_oauth_injection_reports_error() {
    let pool = Arc::new(McpClientPool::new_pending());
    pool.configs
        .write()
        .insert("server".into(), http_config("https://example/mcp", None));
    let error = pool.reconnect("server", None).await.unwrap_err();
    assert!(error.to_string().contains("not injected"));
    assert!(pool.oauth_credentials().is_err());
}

#[tokio::test]
async fn test_load_nonexistent_server_returns_none() {
    let store = memory_client();
    assert!(store.load_server("nonexistent").await.unwrap().is_none());
}

#[tokio::test]
async fn injected_clients_share_only_their_port() {
    let port = Arc::new(MemoryOAuthCredentialPort::default());
    let first = OAuthCredentialClient::new(port.clone()).unwrap();
    let second = OAuthCredentialClient::new(port).unwrap();
    first
        .save_server(
            "srv1",
            StoredCredentials::new("client1".into(), None, vec![], None),
        )
        .await
        .unwrap();
    assert!(second.load_server("srv1").await.unwrap().is_some());
    assert!(memory_client().load_server("srv1").await.unwrap().is_none());
}

#[tokio::test]
async fn test_clear_server() {
    let store = memory_client();
    store
        .save_server(
            "srv",
            StoredCredentials::new("c".into(), None, vec![], None),
        )
        .await
        .unwrap();
    store.clear_server("srv").await.unwrap();
    assert!(store.load_server("srv").await.unwrap().is_none());
}

#[tokio::test]
async fn test_overwrite_server_token() {
    let store = memory_client();
    store
        .save_server(
            "srv",
            StoredCredentials::new("c1".into(), None, vec![], None),
        )
        .await
        .unwrap();
    store
        .save_server(
            "srv",
            StoredCredentials::new("c2".into(), None, vec![], None),
        )
        .await
        .unwrap();
    assert_eq!(
        store.load_server("srv").await.unwrap().unwrap().client_id,
        "c2"
    );
}

#[tokio::test]
async fn test_clear_all() {
    let store = memory_client();
    store
        .save_server("s1", StoredCredentials::new("c".into(), None, vec![], None))
        .await
        .unwrap();
    store
        .save_server("s2", StoredCredentials::new("c".into(), None, vec![], None))
        .await
        .unwrap();
    store.clear_all().await.unwrap();
    assert!(store.load_server("s1").await.unwrap().is_none());
}

#[tokio::test]
async fn test_list_servers() {
    let store = memory_client();
    store
        .save_server("s1", StoredCredentials::new("c".into(), None, vec![], None))
        .await
        .unwrap();
    store
        .save_server("s2", StoredCredentials::new("c".into(), None, vec![], None))
        .await
        .unwrap();
    let servers = store.list_servers().await.unwrap();
    assert_eq!(servers.len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_concurrent_save_across_store_instances_preserves_all_servers() {
    let port = Arc::new(MemoryOAuthCredentialPort::default());
    let mut handles = vec![];
    for i in 0..10 {
        let store = OAuthCredentialClient::new(port.clone()).unwrap();
        handles.push(tokio::spawn(async move {
            store
                .save_server(
                    &format!("srv{i}"),
                    StoredCredentials::new(format!("c{i}"), None, vec![], None),
                )
                .await
                .unwrap();
        }));
    }
    for handle in handles {
        handle.await.unwrap();
    }

    let store = OAuthCredentialClient::new(port).unwrap();
    assert_eq!(store.list_servers().await.unwrap().len(), 10);
}

#[tokio::test]
async fn test_concurrent_save_does_not_corrupt() {
    let store = memory_client();
    let mut handles = vec![];
    for i in 0..10 {
        let s = store.clone();
        handles.push(tokio::spawn(async move {
            s.save_server(
                &format!("srv{}", i),
                StoredCredentials::new(format!("c{}", i), None, vec![], None),
            )
            .await
            .unwrap();
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    assert_eq!(store.list_servers().await.unwrap().len(), 10);
}
