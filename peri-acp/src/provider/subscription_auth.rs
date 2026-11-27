//! Host assembly of the credentials MCP capability. No credential I/O lives here.

use std::path::PathBuf;
use std::sync::Arc;

use peri_model::{ModelError, ModelResult, ProtocolErrorKind, RequestAuth, RequestCredential};
use tokio_util::sync::CancellationToken;

pub use peri_mcp_credentials::model_auth::{
    AccountStatus, AuthError, AuthResult, ChatGptAuthManager, LogoutOutcome, PendingLogin,
};

pub(super) fn request_auth(
    store: PathBuf,
    account: String,
    subject: String,
    client_id: String,
) -> Arc<dyn RequestAuth> {
    let reference = peri_mcp_credentials::model_auth::chatgpt_credential_ref(&subject, &client_id);
    let manager = ChatGptAuthManager::try_new(store, account)
        .map(|manager| manager.with_expected_identity(subject, client_id))
        .map_err(|error| error.to_string());
    Arc::new(SubscriptionAuth { reference, manager })
}

struct SubscriptionAuth {
    reference: String,
    manager: Result<ChatGptAuthManager, String>,
}

impl SubscriptionAuth {
    fn manager(&self) -> ModelResult<&ChatGptAuthManager> {
        self.manager.as_ref().map_err(|error| auth_error(error))
    }
}

fn auth_error(message: &str) -> ModelError {
    ModelError::protocol(ProtocolErrorKind::ProviderTerminal).with_message(message)
}

#[async_trait::async_trait]
impl RequestAuth for SubscriptionAuth {
    fn credential_ref(&self) -> &str {
        &self.reference
    }

    fn validate_endpoint(&self, endpoint: &url::Url) -> ModelResult<()> {
        self.manager()?.validate_endpoint(endpoint)
    }

    async fn credential(&self, cancellation: CancellationToken) -> ModelResult<RequestCredential> {
        self.manager()?.credential(cancellation).await
    }

    async fn refresh_after_unauthorized(
        &self,
        rejected: &RequestCredential,
        cancellation: CancellationToken,
    ) -> ModelResult<bool> {
        self.manager()?
            .refresh_after_unauthorized(rejected, cancellation)
            .await
    }

    fn cancellation_token(&self) -> Option<CancellationToken> {
        self.manager
            .as_ref()
            .ok()
            .and_then(RequestAuth::cancellation_token)
    }
}

#[cfg(test)]
#[path = "auth_test.rs"]
mod tests;
