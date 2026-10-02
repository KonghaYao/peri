use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use peri_acp_types::oauth_credentials::{
    OAuthCredentialError, OAuthCredentialPort, OAuthCredentialResult,
};
use rmcp::transport::auth::StoredCredentials;

use crate::OAuthCredentialClient;

#[derive(Default)]
struct MemoryCredentials(tokio::sync::Mutex<HashMap<String, String>>);

#[async_trait]
impl OAuthCredentialPort for MemoryCredentials {
    async fn load(&self, server_key: &str) -> OAuthCredentialResult<Option<String>> {
        Ok(self.0.lock().await.get(server_key).cloned())
    }

    async fn save(&self, server_key: &str, credentials: &str) -> OAuthCredentialResult<()> {
        self.0
            .lock()
            .await
            .insert(server_key.into(), credentials.into());
        Ok(())
    }

    async fn clear(&self, server_key: &str) -> OAuthCredentialResult<()> {
        self.0.lock().await.remove(server_key);
        Ok(())
    }

    async fn clear_all(&self) -> OAuthCredentialResult<()> {
        self.0.lock().await.clear();
        Ok(())
    }

    async fn list(&self) -> OAuthCredentialResult<Vec<String>> {
        let mut keys: Vec<_> = self.0.lock().await.keys().cloned().collect();
        keys.sort();
        Ok(keys)
    }
}

fn credentials(client_id: &str) -> StoredCredentials {
    StoredCredentials::new(client_id.into(), None, vec![], None)
}

#[tokio::test]
async fn mcp_round_trip_and_blocking_cleanup_need_no_tool_pool() {
    let client = OAuthCredentialClient::new(Arc::new(MemoryCredentials::default())).unwrap();
    assert!(client.load_server("first").await.unwrap().is_none());
    client
        .save_server("first", credentials("first-client"))
        .await
        .unwrap();
    client
        .save_server("second", credentials("second-client"))
        .await
        .unwrap();
    assert_eq!(
        client
            .load_server("first")
            .await
            .unwrap()
            .unwrap()
            .client_id,
        "first-client"
    );
    client.clear_server_blocking("first").unwrap();
    assert!(client.load_server("first").await.unwrap().is_none());
    assert_eq!(client.list_servers().await.unwrap(), vec!["second"]);
    client.clear_all().await.unwrap();
    assert!(client.list_servers().await.unwrap().is_empty());
}

#[tokio::test]
async fn invalid_identity_and_payload_errors_do_not_reveal_credentials() {
    let provider = Arc::new(MemoryCredentials::default());
    provider
        .0
        .lock()
        .await
        .insert("corrupt".into(), "{\"secret-marker\":true}".into());
    let client = OAuthCredentialClient::new(provider).unwrap();
    let error = client.load_server("corrupt").await.err().unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(!error.to_string().contains("secret-marker"));
    let error = client
        .save_server(" ", credentials("secret-marker"))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(!error.to_string().contains("secret-marker"));
}

struct ReadOnlyCredentials;

#[async_trait]
impl OAuthCredentialPort for ReadOnlyCredentials {
    async fn load(&self, _key: &str) -> OAuthCredentialResult<Option<String>> {
        Ok(None)
    }
    async fn save(&self, _key: &str, _credentials: &str) -> OAuthCredentialResult<()> {
        Err(OAuthCredentialError::ReadOnly)
    }
    async fn clear(&self, _key: &str) -> OAuthCredentialResult<()> {
        Err(OAuthCredentialError::ReadOnly)
    }
    async fn clear_all(&self) -> OAuthCredentialResult<()> {
        Err(OAuthCredentialError::ReadOnly)
    }
    async fn list(&self) -> OAuthCredentialResult<Vec<String>> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn read_only_failure_is_explicit_without_fallback() {
    let client = OAuthCredentialClient::new(Arc::new(ReadOnlyCredentials)).unwrap();
    let error = client
        .save_server("server", credentials("fixture-client"))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(client.clear_server_blocking("server").is_err());
    assert!(client.load_server("server").await.unwrap().is_none());
}

#[tokio::test]
async fn credentials_reuse_configured_database_and_survive_resource_restart() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("configured.db");
    let resources = peri_resources::Resources::open_with(Some(database.clone()))
        .await
        .unwrap();
    let (sessions, shutdown) = resources.into_parts();
    let workspace_id = sessions
        .resolve_workspace(directory.path())
        .await
        .unwrap()
        .workspace_id;
    let client = OAuthCredentialClient::new(
        sessions
            .oauth_credentials_for_workspace(workspace_id)
            .unwrap(),
    )
    .unwrap();
    client
        .save_server("endpoint-key", credentials("database-client"))
        .await
        .unwrap();
    let names: Vec<_> = std::fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert!(names
        .iter()
        .all(|name| name.to_string_lossy().starts_with("configured.db")));
    peri_acp_types::session_resources::SessionStoreShutdownPort::shutdown(&shutdown)
        .await
        .unwrap();
    assert!(client.load_server("endpoint-key").await.is_err());
    assert!(client
        .save_server("endpoint-key", credentials("closed-client"))
        .await
        .is_err());
    assert!(client.clear_server("endpoint-key").await.is_err());
    drop(client);
    drop(sessions);
    let resources = peri_resources::Resources::open_with(Some(database))
        .await
        .unwrap();
    let (sessions, shutdown) = resources.into_parts();
    let client = OAuthCredentialClient::new(
        sessions
            .oauth_credentials_for_workspace(workspace_id)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        client
            .load_server("endpoint-key")
            .await
            .unwrap()
            .unwrap()
            .client_id,
        "database-client"
    );
    client.clear_server("endpoint-key").await.unwrap();
    assert!(client.load_server("endpoint-key").await.unwrap().is_none());
    peri_acp_types::session_resources::SessionStoreShutdownPort::shutdown(&shutdown)
        .await
        .unwrap();
}
