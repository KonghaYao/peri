//! Host assembly of the credentials MCP capability. No credential I/O lives here.

use std::{path::PathBuf, sync::Arc};

use peri_model::{ModelError, ModelResult, ProtocolErrorKind, RequestAuth, RequestCredential};
use tokio_util::sync::CancellationToken;

#[cfg(not(target_os = "emscripten"))]
pub use peri_mcp_credentials::model_auth::ChatGptAuthManager;

pub(super) fn request_auth(
    store: PathBuf,
    account: String,
    subject: String,
    client_id: String,
) -> Arc<dyn RequestAuth> {
    #[cfg(not(target_os = "emscripten"))]
    let reference = peri_mcp_credentials::model_auth::chatgpt_credential_ref(&subject, &client_id);
    #[cfg(target_os = "emscripten")]
    let reference = {
        let _ = (store, account, subject, client_id);
        "unsupported-native-auth".to_owned()
    };
    #[cfg(not(target_os = "emscripten"))]
    let manager = ChatGptAuthManager::new(store, account)
        .map(|manager| manager.with_expected_identity(subject, client_id))
        .map_err(|error| error.to_string());
    Arc::new(SubscriptionAuth {
        reference,
        #[cfg(not(target_os = "emscripten"))]
        manager,
    })
}

struct SubscriptionAuth {
    reference: String,
    #[cfg(not(target_os = "emscripten"))]
    manager: Result<ChatGptAuthManager, String>,
}

impl SubscriptionAuth {
    #[cfg(not(target_os = "emscripten"))]
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
        #[cfg(not(target_os = "emscripten"))]
        {
            self.manager()?.validate_endpoint(endpoint)
        }
        #[cfg(target_os = "emscripten")]
        {
            let _ = endpoint;
            Err(auth_error(
                "ChatGPT plan credentials require a native credentials MCP capability",
            ))
        }
    }

    async fn credential(&self, cancellation: CancellationToken) -> ModelResult<RequestCredential> {
        #[cfg(not(target_os = "emscripten"))]
        {
            self.manager()?.credential(cancellation).await
        }
        #[cfg(target_os = "emscripten")]
        {
            let _ = cancellation;
            Err(auth_error(
                "ChatGPT plan credentials require a native credentials MCP capability",
            ))
        }
    }

    async fn refresh_after_unauthorized(
        &self,
        rejected: &RequestCredential,
        cancellation: CancellationToken,
    ) -> ModelResult<bool> {
        #[cfg(not(target_os = "emscripten"))]
        {
            self.manager()?
                .refresh_after_unauthorized(rejected, cancellation)
                .await
        }
        #[cfg(target_os = "emscripten")]
        {
            let _ = (rejected, cancellation);
            Err(auth_error(
                "ChatGPT plan credentials require a native credentials MCP capability",
            ))
        }
    }

    fn cancellation_token(&self) -> Option<CancellationToken> {
        #[cfg(not(target_os = "emscripten"))]
        {
            self.manager
                .as_ref()
                .ok()
                .and_then(RequestAuth::cancellation_token)
        }
        #[cfg(target_os = "emscripten")]
        {
            None
        }
    }
}

#[cfg(test)]
#[path = "auth_test.rs"]
mod tests;
