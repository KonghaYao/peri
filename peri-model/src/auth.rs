use std::fmt;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::ModelResult;

/// A short-lived credential and opaque rotation version, resolved at execution.
/// The version identifies a token set, never an account selector.
#[derive(Clone)]
pub struct RequestCredential {
    bearer: String,
    version: String,
}

impl RequestCredential {
    pub fn new(bearer: String, version: String) -> Self {
        Self { bearer, version }
    }

    pub fn bearer(&self) -> &str {
        &self.bearer
    }

    pub fn version(&self) -> &str {
        &self.version
    }
}

impl fmt::Debug for RequestCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RequestCredential").finish_non_exhaustive()
    }
}

/// Host-injected authentication. Implementations own renewal and storage;
/// model adapters never inspect environment variables or credential files.
/// An implementation must remain bound to one account for its entire lifetime.
#[async_trait]
pub trait RequestAuth: Send + Sync {
    fn credential_ref(&self) -> &str;

    /// Credential implementations can restrict the origins/routes to which
    /// their bearer may be sent, independently of the selected wire profile.
    fn validate_endpoint(&self, _endpoint: &url::Url) -> ModelResult<()> {
        Ok(())
    }

    async fn credential(&self, cancellation: CancellationToken) -> ModelResult<RequestCredential>;

    /// Returns true only if a different credential is available for a retry.
    /// The runtime invokes this at most once per model call.
    async fn refresh_after_unauthorized(
        &self,
        rejected: &RequestCredential,
        cancellation: CancellationToken,
    ) -> ModelResult<bool>;

    /// Cancels requests when the owning authentication session is logged out.
    fn cancellation_token(&self) -> Option<CancellationToken> {
        None
    }
}
