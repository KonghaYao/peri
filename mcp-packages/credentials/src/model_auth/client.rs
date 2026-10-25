use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::Duration,
};

use peri_model::{ModelError, ModelResult, ProtocolErrorKind, RequestAuth, RequestCredential};
use rmcp::{
    model::{CancelledNotificationParam, ClientRequest, CustomRequest, ServerResult},
    service::PeerRequestOptions,
    ServiceExt,
};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use super::{
    oauth::OAuth,
    server::{AuthServer, Capability},
    store::Store,
    types::{
        AccountStatus, AuthError, AuthResult, ExpectedIdentity, LogoutOutcome, PendingLogin,
        Request, Response,
    },
    ISSUER, METHOD,
};

/// Host facade for the isolated credentials MCP capability. It does no file
/// I/O. A fresh in-process MCP connection owns each operation's request scope.
#[derive(Clone)]
pub struct ChatGptAuthManager {
    server: Arc<AuthServer>,
    credential_ref: String,
    expected: Option<ExpectedIdentity>,
    binding: Arc<OnceLock<String>>,
}

impl ChatGptAuthManager {
    pub fn new(store_dir: PathBuf, account_ref: String) -> AuthResult<Self> {
        let store = Store::new(store_dir, account_ref.clone())?;
        Ok(Self {
            server: Arc::new(AuthServer {
                store,
                account_ref: account_ref.clone(),
                oauth: OAuth::production()?,
                pending: tokio::sync::Mutex::new(None),
                session_cancellation: CancellationToken::new(),
                revocation_cancellation: CancellationToken::new(),
                clock: Arc::new(super::server::now),
            }),
            credential_ref: account_ref,
            expected: None,
            binding: Arc::new(OnceLock::new()),
        })
    }

    /// Bind inference to the verified identity printed by successful login.
    /// Account aliases alone cannot authorize replay of encrypted model state.
    pub fn with_expected_identity(mut self, subject: String, client_id: String) -> Self {
        self.credential_ref = chatgpt_credential_ref(&subject, &client_id);
        self.expected = Some(ExpectedIdentity { subject, client_id });
        self.binding = Arc::new(OnceLock::new());
        self
    }

    pub async fn begin_login(&self) -> AuthResult<PendingLogin> {
        match self
            .request(Request::BeginLogin, CancellationToken::new())
            .await?
        {
            Response::Pending(value) => Ok(value),
            _ => Err(AuthError::InvalidResponse),
        }
    }

    pub async fn finish_login(
        &self,
        pending: PendingLogin,
        cancellation: CancellationToken,
    ) -> AuthResult<AccountStatus> {
        match self
            .request(
                Request::FinishLogin {
                    attempt_id: pending.attempt_id,
                },
                cancellation,
            )
            .await?
        {
            Response::Status(Some(value)) => Ok(value),
            _ => Err(AuthError::InvalidResponse),
        }
    }

    pub async fn status(&self) -> AuthResult<Option<AccountStatus>> {
        match self
            .request(Request::Status, CancellationToken::new())
            .await?
        {
            Response::Status(value) => Ok(value),
            _ => Err(AuthError::InvalidResponse),
        }
    }

    pub async fn logout(&self, cancellation: CancellationToken) -> AuthResult<LogoutOutcome> {
        // Cancel model requests before the MCP logout waits for native refresh
        // ownership. Remote revocation is best-effort; local clear is mandatory.
        self.server.session_cancellation.cancel();
        match self.request(Request::Logout, cancellation).await? {
            Response::Logout(value) => Ok(value),
            _ => Err(AuthError::InvalidResponse),
        }
    }

    async fn request(
        &self,
        request: Request,
        cancellation: CancellationToken,
    ) -> AuthResult<Response> {
        let logout = matches!(request, Request::Logout);
        let server = Capability(self.server.clone());
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let server_task = tokio::spawn(async move {
            if let Ok(service) = server.serve(server_io).await {
                let _ = service.waiting().await;
            }
        });
        let mut server_guard = AbortOnDrop(Some(server_task));
        let mut client = ().serve(client_io).await.map_err(|_| AuthError::Unavailable)?;
        let params = serde_json::to_value(request).map_err(|_| AuthError::InvalidResponse)?;
        let handle = client
            .peer()
            .send_request_with_option(
                ClientRequest::CustomRequest(CustomRequest::new(METHOD, Some(params))),
                PeerRequestOptions::no_options(),
            )
            .await
            .map_err(|_| AuthError::Unavailable)?;
        let request_id = handle.id.clone();
        let response = if logout {
            // Do not discard local deletion when a UI cancellation arrives.
            // MCP notifications/cancelled would also cancel the response handle.
            // A distinct host-only operation cancels revocation while preserving
            // the original request's confirmed local-clear outcome.
            let response = handle.await_response();
            tokio::pin!(response);
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => {
                    let control = serde_json::to_value(Request::CancelRevocation).map_err(|_| AuthError::InvalidResponse)?;
                    if let Ok(control) = client.peer().send_request_with_option(
                        ClientRequest::CustomRequest(CustomRequest::new(METHOD, Some(control))),
                        PeerRequestOptions::with_timeout(Duration::from_secs(5)),
                    ).await {
                        let _ = control.await_response().await;
                    }
                    response.await.map_err(|_| AuthError::Unavailable)
                }
                result = &mut response => result.map_err(|_| AuthError::Unavailable),
            }
        } else {
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => {
                    let _ = client.peer().notify_cancelled(CancelledNotificationParam::new(Some(request_id), Some("authentication cancelled".to_owned()))).await;
                    Err(AuthError::Cancelled)
                }
                response = handle.await_response() => response.map_err(|_| AuthError::Unavailable),
            }
        };
        let _ = client.close_with_timeout(Duration::from_secs(1)).await;
        if let Some(mut task) = server_guard.0.take() {
            if peri_time::timeout(Duration::from_secs(1), &mut task)
                .await
                .is_err()
            {
                task.abort();
                let _ = task.await;
            }
        }
        match response? {
            ServerResult::CustomResult(result) => {
                serde_json::from_value::<AuthResult<Response>>(result.0)
                    .map_err(|_| AuthError::InvalidResponse)?
            }
            _ => Err(AuthError::InvalidResponse),
        }
    }
}

#[async_trait::async_trait]
impl RequestAuth for ChatGptAuthManager {
    fn credential_ref(&self) -> &str {
        &self.credential_ref
    }

    fn validate_endpoint(&self, endpoint: &url::Url) -> ModelResult<()> {
        if endpoint.as_str() != "https://api.openai.com/v1/responses" {
            return Err(ModelError::protocol(ProtocolErrorKind::InvalidEndpoint)
                .with_message("ChatGPT credentials require the official Responses endpoint"));
        }
        Ok(())
    }

    async fn credential(&self, cancellation: CancellationToken) -> ModelResult<RequestCredential> {
        let identity = self
            .expected
            .clone()
            .filter(|v| !v.subject.is_empty() && !v.client_id.is_empty())
            .ok_or_else(|| model_error(AuthError::InvalidReference))?;
        let response = self
            .request(
                Request::Credential {
                    identity,
                    binding: self.binding.get().cloned(),
                },
                cancellation,
            )
            .await
            .map_err(model_error)?;
        match response {
            Response::Credential(value) => {
                if self.binding.set(value.binding.clone()).is_err()
                    && self.binding.get() != Some(&value.binding)
                {
                    return Err(model_error(AuthError::AccountChanged));
                }
                Ok(RequestCredential::new(value.bearer, value.version))
            }
            _ => Err(model_error(AuthError::InvalidResponse)),
        }
    }

    async fn refresh_after_unauthorized(
        &self,
        rejected: &RequestCredential,
        cancellation: CancellationToken,
    ) -> ModelResult<bool> {
        let identity = self
            .expected
            .clone()
            .ok_or_else(|| model_error(AuthError::InvalidReference))?;
        let binding = self
            .binding
            .get()
            .cloned()
            .ok_or_else(|| model_error(AuthError::LoginRequired))?;
        match self
            .request(
                Request::Refresh {
                    identity,
                    binding,
                    rejected_version: rejected.version().to_owned(),
                },
                cancellation,
            )
            .await
            .map_err(model_error)?
        {
            Response::Refreshed(value) => Ok(value),
            _ => Err(model_error(AuthError::InvalidResponse)),
        }
    }

    fn cancellation_token(&self) -> Option<CancellationToken> {
        Some(self.server.session_cancellation.clone())
    }
}

/// Stable identity used by Responses replay binding; contains no credential.
pub fn chatgpt_credential_ref(subject: &str, client_id: &str) -> String {
    let mut identity = Sha256::new();
    for part in [ISSUER, subject, client_id] {
        identity.update((part.len() as u64).to_be_bytes());
        identity.update(part.as_bytes());
    }
    let hash = identity.finalize();
    let hex: String = hash.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("chatgpt:{hex}")
}

fn model_error(error: AuthError) -> ModelError {
    if error == AuthError::Cancelled {
        ModelError::cancelled()
    } else if error == AuthError::Unavailable {
        ModelError::transport(
            peri_model::TransportErrorKind::Connection,
            Some("chatgpt-auth"),
        )
        .with_message(error.to_string())
    } else if matches!(&error, AuthError::OAuth { code, .. } if matches!(code.as_str(), "temporarily_unavailable" | "server_error"))
    {
        ModelError::protocol(ProtocolErrorKind::Provider).with_message(error.to_string())
    } else {
        ModelError::protocol(ProtocolErrorKind::ProviderTerminal).with_message(error.to_string())
    }
}

struct AbortOnDrop(Option<tokio::task::JoinHandle<()>>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(task) = &self.0 {
            task.abort();
        }
    }
}

#[cfg(all(test, unix))]
#[path = "client_test.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "client_process_test.rs"]
mod process_tests;
