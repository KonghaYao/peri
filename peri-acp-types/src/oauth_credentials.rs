use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub const OAUTH_CREDENTIAL_METHOD: &str = "peri/oauth_credentials";

/// Credential-store operations keyed by MCP server identity.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum OAuthCredentialRequest {
    Load {
        server_key: String,
    },
    Save {
        server_key: String,
        credentials: String,
    },
    Clear {
        server_key: String,
    },
    ClearAll,
    List,
}

/// Credential-store outcomes; absent credentials remain distinct from storage failures.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum OAuthCredentialValue {
    Credentials(Option<String>),
    Keys(Vec<String>),
    Saved,
}

/// Stable credential-store failure categories carried across the service boundary.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum OAuthCredentialError {
    #[error("OAuth credential input is invalid")]
    InvalidInput,
    #[error("OAuth credential storage is unavailable")]
    Unavailable,
    #[error("OAuth credential storage is read-only")]
    ReadOnly,
    #[error("OAuth credential record is invalid")]
    InvalidData,
}

/// A credential operation outcome preserving the typed storage failure.
pub type OAuthCredentialResult<T> = Result<T, OAuthCredentialError>;
/// The wire result for a credential-store operation.
pub type OAuthCredentialResponse = OAuthCredentialResult<OAuthCredentialValue>;

pub fn validate_server_key(server_key: &str) -> OAuthCredentialResult<()> {
    if server_key.trim().is_empty() || server_key.len() > 4096 {
        return Err(OAuthCredentialError::InvalidInput);
    }
    Ok(())
}

pub fn validate_credentials(credentials: &str) -> OAuthCredentialResult<()> {
    if credentials.len() > 1024 * 1024 {
        return Err(OAuthCredentialError::InvalidInput);
    }
    match serde_json::from_str::<serde_json::Value>(credentials) {
        Ok(serde_json::Value::Object(_)) => Ok(()),
        _ => Err(OAuthCredentialError::InvalidData),
    }
}

/// Scoped access to serialized OAuth credentials and their server keys.
/// Implementations report storage failures rather than replacing them with missing records.
#[async_trait]
pub trait OAuthCredentialPort: Send + Sync {
    async fn load(&self, server_key: &str) -> OAuthCredentialResult<Option<String>>;
    async fn save(&self, server_key: &str, credentials: &str) -> OAuthCredentialResult<()>;
    async fn clear(&self, server_key: &str) -> OAuthCredentialResult<()>;
    async fn clear_all(&self) -> OAuthCredentialResult<()>;
    async fn list(&self) -> OAuthCredentialResult<Vec<String>>;
}
