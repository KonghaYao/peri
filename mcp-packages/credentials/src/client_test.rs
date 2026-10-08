use super::*;
use async_trait::async_trait;
use peri_acp_types::oauth_credentials::OAuthCredentialResult;
use std::collections::HashMap;

#[derive(Default)]
struct MemoryCredentials(tokio::sync::Mutex<HashMap<String, String>>);

#[async_trait]
impl OAuthCredentialPort for MemoryCredentials {
    async fn load(&self, key: &str) -> OAuthCredentialResult<Option<String>> {
        Ok(self.0.lock().await.get(key).cloned())
    }

    async fn save(&self, key: &str, credentials: &str) -> OAuthCredentialResult<()> {
        self.0.lock().await.insert(key.into(), credentials.into());
        Ok(())
    }

    async fn clear(&self, key: &str) -> OAuthCredentialResult<()> {
        self.0.lock().await.remove(key);
        Ok(())
    }

    async fn clear_all(&self) -> OAuthCredentialResult<()> {
        self.0.lock().await.clear();
        Ok(())
    }

    async fn list(&self) -> OAuthCredentialResult<Vec<String>> {
        Ok(self.0.lock().await.keys().cloned().collect())
    }
}

#[test]
fn event_loop_worker_without_runtime_returns_unavailable_instead_of_panicking() {
    let (_, receiver) = tokio::sync::mpsc::unbounded_channel();
    let error = spawn_worker(Arc::new(MemoryCredentials::default()), receiver).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert!(error
        .to_string()
        .starts_with("OAuth credential runtime is unavailable:"));
    assert!(error.get_ref().unwrap().source().is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn event_loop_worker_serves_requests_and_joins_after_last_client_drops() {
    let provider = Arc::new(MemoryCredentials::default());
    let weak_provider = Arc::downgrade(&provider);
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let worker = spawn_worker(provider, receiver).unwrap();
    let client = OAuthCredentialClient { sender };
    let clone = client.clone();
    assert!(client.load_server("server").await.unwrap().is_none());
    client
        .save_server(
            "server",
            StoredCredentials::new("fixture-client".into(), None, vec![], None),
        )
        .await
        .unwrap();
    drop(client);
    assert_eq!(
        clone
            .load_server("server")
            .await
            .unwrap()
            .unwrap()
            .client_id,
        "fixture-client"
    );
    let error = clone.clear_server(" ").await.unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    clone.clear_server("server").await.unwrap();
    assert!(clone.list_servers().await.unwrap().is_empty());
    drop(clone);
    peri_time::timeout(REQUEST_TIMEOUT, worker)
        .await
        .unwrap()
        .unwrap();
    assert!(weak_provider.upgrade().is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn abandoned_startup_joins_aborted_server_and_releases_provider() {
    let provider = Arc::new(MemoryCredentials::default());
    let weak_provider = Arc::downgrade(&provider);
    let (_sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let (ready, startup) = mpsc::channel();
    drop(startup);
    peri_time::timeout(REQUEST_TIMEOUT, run_worker(provider, receiver, Some(ready)))
        .await
        .unwrap();
    assert!(weak_provider.upgrade().is_none());
}
