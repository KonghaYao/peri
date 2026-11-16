use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::oauth_credentials::{
    OAuthCredentialError, OAuthCredentialPort, OAuthCredentialResult,
};
use tokio::sync::{RwLock, RwLockReadGuard};

use super::lifecycle::{Lifecycle, LifecycleState};

pub(super) struct LifecycleCredentials {
    inner: Arc<dyn OAuthCredentialPort>,
    lifecycle: Lifecycle,
    operations: Arc<RwLock<()>>,
}

impl LifecycleCredentials {
    pub(super) fn new(
        inner: Arc<dyn OAuthCredentialPort>,
        lifecycle: Lifecycle,
        operations: Arc<RwLock<()>>,
    ) -> Self {
        Self {
            inner,
            lifecycle,
            operations,
        }
    }

    async fn admit(&self) -> OAuthCredentialResult<RwLockReadGuard<'_, ()>> {
        let admission = self.operations.read().await;
        if self.lifecycle.state() != LifecycleState::Open {
            return Err(OAuthCredentialError::Unavailable);
        }
        Ok(admission)
    }
}

#[async_trait]
impl OAuthCredentialPort for LifecycleCredentials {
    async fn load(&self, server_key: &str) -> OAuthCredentialResult<Option<String>> {
        let _admission = self.admit().await?;
        self.inner.load(server_key).await
    }

    async fn save(&self, server_key: &str, credentials: &str) -> OAuthCredentialResult<()> {
        let _admission = self.admit().await?;
        self.inner.save(server_key, credentials).await
    }

    async fn clear(&self, server_key: &str) -> OAuthCredentialResult<()> {
        let _admission = self.admit().await?;
        self.inner.clear(server_key).await
    }

    async fn clear_all(&self) -> OAuthCredentialResult<()> {
        let _admission = self.admit().await?;
        self.inner.clear_all().await
    }

    async fn list(&self) -> OAuthCredentialResult<Vec<String>> {
        let _admission = self.admit().await?;
        self.inner.list().await
    }
}
