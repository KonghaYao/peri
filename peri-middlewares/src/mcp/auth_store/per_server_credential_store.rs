use super::Digest as _;
use super::{
    async_trait, fmt, AuthError, CredentialStore, Debug, Formatter, OAuthConfig,
    OAuthCredentialClient, Sha256, StoredCredentials,
};

pub(crate) fn static_credential_key(
    server_name: &str,
    endpoint: &str,
    oauth: &OAuthConfig,
) -> String {
    let identity = serde_json::to_vec(&(
        server_name,
        endpoint,
        oauth.enabled,
        &oauth.client_id,
        &oauth.client_secret,
        &oauth.scopes,
    ))
    .expect("OAuth credential identity is serializable");
    let digest = Sha256::digest(identity);
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    format!("static:{hex}")
}

pub struct PerServerCredentialStore {
    pub(super) inner: OAuthCredentialClient,
    pub(super) server_key: String,
}

impl Debug for PerServerCredentialStore {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PerServerCredentialStore")
            .field("server_key", &self.server_key)
            .finish_non_exhaustive()
    }
}

impl PerServerCredentialStore {
    #[must_use]
    pub fn new(inner: OAuthCredentialClient, server_key: String) -> Self {
        Self { inner, server_key }
    }
}

#[async_trait]
impl CredentialStore for PerServerCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        self.inner
            .load_server(&self.server_key)
            .await
            .map_err(|error| AuthError::InternalError(error.to_string()))
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        self.inner
            .save_server(&self.server_key, credentials)
            .await
            .map_err(|error| AuthError::InternalError(error.to_string()))
    }

    async fn clear(&self) -> Result<(), AuthError> {
        self.inner
            .clear_server(&self.server_key)
            .await
            .map_err(|error| AuthError::InternalError(error.to_string()))
    }
}
