use std::sync::Arc;

use rmcp::{
    model::{CustomRequest, CustomResult, Implementation, ServerCapabilities, ServerConfig},
    service::{RequestContext, RoleServer},
    ErrorData, ServerHandler,
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use super::{
    callback::LoginAttempt,
    oauth::OAuth,
    store::{Lease, Store},
    types::{
        AuthError, AuthResult, CredentialReply, ExpectedIdentity, LogoutOutcome, Request, Response,
        StoredAccount,
    },
    METHOD,
};

pub(super) struct AuthServer {
    pub store: Store,
    pub account_ref: String,
    pub oauth: OAuth,
    pub pending: Mutex<Option<LoginAttempt>>,
    pub session_cancellation: CancellationToken,
    pub revocation_cancellation: CancellationToken,
    pub clock: Arc<dyn Fn() -> u64 + Send + Sync>,
}

impl AuthServer {
    async fn execute(
        &self,
        request: Request,
        cancellation: CancellationToken,
    ) -> AuthResult<Response> {
        match request {
            Request::BeginLogin => {
                let host_id = self.store.host_id(&cancellation).await?;
                let lease = self.store.lease(&cancellation).await?;
                let account = match lease.read().await? {
                    Some(account) => account,
                    None => {
                        let account = StoredAccount::new(host_id);
                        lease.save(account.clone()).await?;
                        account
                    }
                };
                drop(lease);
                let (attempt, pending) = LoginAttempt::begin(&self.oauth, account).await?;
                *self.pending.lock().await = Some(attempt);
                Ok(Response::Pending(pending))
            }
            Request::FinishLogin { attempt_id } => {
                self.finish_login(&attempt_id, &cancellation).await
            }
            Request::Status => {
                let lease = self.store.lease(&cancellation).await?;
                Ok(Response::Status(
                    lease
                        .read()
                        .await?
                        .and_then(|account| account.status(&self.account_ref)),
                ))
            }
            Request::Logout => self.logout(&cancellation).await.map(Response::Logout),
            Request::CancelRevocation => {
                self.revocation_cancellation.cancel();
                Ok(Response::RevocationCancelled)
            }
            Request::Credential { identity, binding } => {
                self.ensure_session()?;
                let lease = self.store.lease(&cancellation).await?;
                let mut account = lease.read().await?.ok_or(AuthError::LoginRequired)?;
                validate_identity(&account, &identity, binding.as_deref())?;
                let expires_at = account
                    .tokens
                    .as_ref()
                    .ok_or(AuthError::LoginRequired)?
                    .expires_at;
                if expires_at <= (self.clock)().saturating_add(60) {
                    self.refresh_locked(&lease, &mut account, &identity, &cancellation)
                        .await?;
                }
                self.ensure_session()?;
                let tokens = account.tokens.as_ref().ok_or(AuthError::LoginRequired)?;
                if !super::types::has_plan_scope(&tokens.scope) {
                    return Err(AuthError::MissingScope);
                }
                Ok(Response::Credential(CredentialReply {
                    bearer: tokens.access_token.clone(),
                    version: tokens.version.clone(),
                    binding: account.binding.ok_or(AuthError::LoginRequired)?,
                }))
            }
            Request::Refresh {
                identity,
                binding,
                rejected_version,
            } => {
                self.ensure_session()?;
                let lease = self.store.lease(&cancellation).await?;
                let mut account = lease.read().await?.ok_or(AuthError::LoginRequired)?;
                validate_identity(&account, &identity, Some(&binding))?;
                if account
                    .tokens
                    .as_ref()
                    .ok_or(AuthError::LoginRequired)?
                    .version
                    == rejected_version
                {
                    self.refresh_locked(&lease, &mut account, &identity, &cancellation)
                        .await?;
                }
                self.ensure_session()?;
                Ok(Response::Refreshed(true))
            }
        }
    }

    fn ensure_session(&self) -> AuthResult<()> {
        if self.session_cancellation.is_cancelled() {
            Err(AuthError::LoginRequired)
        } else {
            Ok(())
        }
    }

    async fn finish_login(
        &self,
        attempt_id: &str,
        cancellation: &CancellationToken,
    ) -> AuthResult<Response> {
        let attempt = {
            let mut pending = self.pending.lock().await;
            if pending
                .as_ref()
                .is_none_or(|attempt| attempt.id != attempt_id)
            {
                return Err(AuthError::InvalidCallback);
            }
            pending.take().ok_or(AuthError::InvalidCallback)?
        };
        let code = attempt.receive(cancellation).await?;
        let response = self
            .oauth
            .exchange(
                &code.client_id,
                &code.code,
                &attempt.verifier,
                &attempt.redirect,
                cancellation,
            )
            .await?;
        let tokens = response.into_tokens(None, (self.clock)())?;
        let claims = self
            .oauth
            .claims(
                &tokens.id_token,
                &code.client_id,
                Some(&attempt.nonce),
                (self.clock)(),
                cancellation,
            )
            .await?;
        let lease = self.store.lease(cancellation).await?;
        let mut current = lease
            .read()
            .await?
            .unwrap_or_else(|| StoredAccount::new(attempt.account.host_id.clone()));
        // A login cannot overwrite a newer login/logout or select a different
        // account under an existing reference, even across processes.
        if current.revision != attempt.account.revision
            || current.binding != attempt.account.binding
            || current.client_id != attempt.account.client_id
            || current
                .subject
                .as_deref()
                .is_some_and(|subject| subject != claims.sub)
        {
            return Err(AuthError::AccountChanged);
        }
        current.client_id = Some(code.client_id);
        current.subject = Some(claims.sub);
        current.email = claims.email;
        current.binding = Some(uuid::Uuid::new_v4().to_string());
        current.revision = uuid::Uuid::new_v4().to_string();
        current.tokens = Some(tokens);
        let status = current
            .status(&self.account_ref)
            .ok_or(AuthError::InvalidResponse)?;
        lease.save(current).await?;
        Ok(Response::Status(Some(status)))
    }

    async fn refresh_locked(
        &self,
        lease: &Lease,
        account: &mut StoredAccount,
        identity: &ExpectedIdentity,
        cancellation: &CancellationToken,
    ) -> AuthResult<()> {
        let previous = account.tokens.clone().ok_or(AuthError::LoginRequired)?;
        let response = self
            .oauth
            .refresh(&identity.client_id, &previous.refresh_token, cancellation)
            .await;
        let response = match response {
            Ok(value) => value,
            Err(error) => {
                if refresh_is_terminal(&error) {
                    account.tokens = None;
                    account.binding = None;
                    lease.save(account.clone()).await?;
                }
                return Err(error);
            }
        };
        // If a successful rotation contains unusable credentials, never leave
        // the now-spent old refresh token available to another process.
        let checked = async {
            if let Some(id_token) = response.id_token() {
                let claims = self
                    .oauth
                    .claims(
                        id_token,
                        &identity.client_id,
                        None,
                        (self.clock)(),
                        cancellation,
                    )
                    .await?;
                if claims.sub != identity.subject {
                    return Err(AuthError::AccountChanged);
                }
            }
            response.into_tokens(Some(&previous), (self.clock)())
        }
        .await;
        match checked {
            Ok(tokens) => {
                account.tokens = Some(tokens);
            }
            Err(error) => {
                account.tokens = None;
                account.binding = None;
                lease.save(account.clone()).await?;
                return Err(error);
            }
        }
        validate_identity(account, identity, None)?;
        lease.save(account.clone()).await
    }

    async fn logout(&self, cancellation: &CancellationToken) -> AuthResult<LogoutOutcome> {
        self.session_cancellation.cancel();
        self.pending.lock().await.take();
        // Local removal is completed even when the caller cancels the optional
        // remote revocation. A bounded OAuth request makes lock ownership finite.
        let cleanup = CancellationToken::new();
        let host_id = self.store.host_id(&cleanup).await?;
        let lease = self.store.lease(&cleanup).await?;
        let mut account = lease
            .read()
            .await?
            .unwrap_or_else(|| StoredAccount::new(host_id));
        let tokens = account.tokens.take();
        account.binding = None;
        account.revision = uuid::Uuid::new_v4().to_string();
        lease.save(account.clone()).await?;
        let remote_revoked = match (account.client_id.as_deref(), tokens) {
            (Some(client_id), Some(tokens)) => tokio::select! {
                biased;
                _ = self.revocation_cancellation.cancelled() => false,
                result = self.oauth.revoke(client_id, &tokens.refresh_token, cancellation) => result.is_ok(),
            },
            _ => true,
        };
        Ok(LogoutOutcome {
            local_cleared: true,
            remote_revoked,
        })
    }
}

fn validate_identity(
    account: &StoredAccount,
    expected: &ExpectedIdentity,
    binding: Option<&str>,
) -> AuthResult<()> {
    if account.schema_version != 1 {
        return Err(AuthError::Storage);
    }
    if account.subject.as_deref() != Some(&expected.subject)
        || account.client_id.as_deref() != Some(&expected.client_id)
        || binding.is_some_and(|value| account.binding.as_deref() != Some(value))
    {
        return Err(AuthError::AccountChanged);
    }
    Ok(())
}

fn refresh_is_terminal(error: &AuthError) -> bool {
    matches!(error, AuthError::OAuth { code, .. } if matches!(code.as_str(),
        "invalid_grant" | "invalid_refresh_token" | "token_expired" | "refresh_token_expired" |
        "refresh_token_invalidated" | "refresh_token_reused"))
}

pub(super) fn now() -> u64 {
    peri_time::now_wall()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[derive(Clone)]
pub(super) struct Capability(pub Arc<AuthServer>);

impl ServerHandler for Capability {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::default()).with_server_info(Implementation::new(
            "peri-chatgpt-auth",
            env!("CARGO_PKG_VERSION"),
        ))
    }

    async fn on_custom_request(
        &self,
        request: CustomRequest,
        context: RequestContext<RoleServer>,
    ) -> Result<CustomResult, ErrorData> {
        if request.method != METHOD {
            return Err(ErrorData::new(
                rmcp::model::ErrorCode::METHOD_NOT_FOUND,
                "Unknown authentication method",
                None,
            ));
        }
        let request: Request = serde_json::from_value(
            request
                .params
                .ok_or_else(|| ErrorData::invalid_params("Missing auth operation", None))?,
        )
        .map_err(|_| ErrorData::invalid_params("Invalid auth operation", None))?;
        let result = if matches!(
            request,
            Request::Logout | Request::Status | Request::CancelRevocation
        ) {
            self.0.execute(request, context.ct).await
        } else {
            tokio::select! {
                biased;
                _ = self.0.session_cancellation.cancelled() => Err(AuthError::LoginRequired),
                result = self.0.execute(request, context.ct) => result,
            }
        };
        if let Err(error) = &result {
            tracing::warn!(%error, account_ref = %self.0.account_ref, "ChatGPT authentication operation failed");
        }
        Ok(CustomResult(serde_json::to_value(result).map_err(
            |_| ErrorData::internal_error("Invalid auth result", None),
        )?))
    }
}

#[cfg(test)]
#[path = "server_test.rs"]
mod tests;
