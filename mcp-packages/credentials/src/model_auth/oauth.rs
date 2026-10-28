use std::time::Duration;

use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use url::Url;

use super::{
    jwt,
    types::{AuthError, AuthResult, TokenSet},
    ISSUER, RESOURCE,
};

#[derive(Clone)]
pub(super) struct OAuth {
    client: reqwest::Client,
    pub issuer: Url,
    pub authorize: Url,
    token: Url,
    discovery: Url,
    resource: String,
}

#[derive(Deserialize)]
struct Discovery {
    issuer: String,
    jwks_uri: String,
    revocation_endpoint: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct TokenResponse {
    access_token: String,
    token_type: String,
    expires_in: u64,
    refresh_token: Option<String>,
    id_token: Option<String>,
    scope: Option<String>,
}

#[derive(Deserialize)]
struct Failure {
    error: String,
    error_description: Option<String>,
}

impl OAuth {
    pub fn production() -> AuthResult<Self> {
        Self::new(Url::parse(ISSUER).map_err(|_| AuthError::InvalidReference)?)
    }

    fn new(issuer: Url) -> AuthResult<Self> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|error| {
                tracing::warn!(%error, "ChatGPT OAuth HTTP client initialization failed");
                AuthError::Unavailable
            })?;
        Ok(Self {
            authorize: issuer
                .join("/api/accounts/authorize")
                .map_err(|_| AuthError::InvalidReference)?,
            token: issuer
                .join("/api/accounts/oauth/token")
                .map_err(|_| AuthError::InvalidReference)?,
            discovery: issuer
                .join("/.well-known/openid-configuration")
                .map_err(|_| AuthError::InvalidReference)?,
            issuer,
            client,
            resource: RESOURCE.to_owned(),
        })
    }

    #[cfg(test)]
    pub(super) fn for_test(issuer: Url) -> Self {
        Self::new(issuer).unwrap()
    }

    pub async fn exchange(
        &self,
        client_id: &str,
        code: &str,
        verifier: &str,
        redirect: &str,
        cancellation: &CancellationToken,
    ) -> AuthResult<TokenResponse> {
        self.token_request(
            &[
                ("grant_type", "authorization_code"),
                ("client_id", client_id),
                ("code", code),
                ("code_verifier", verifier),
                ("redirect_uri", redirect),
                ("resource", &self.resource),
            ],
            cancellation,
        )
        .await
    }

    pub async fn refresh(
        &self,
        client_id: &str,
        token: &str,
        cancellation: &CancellationToken,
    ) -> AuthResult<TokenResponse> {
        self.token_request(
            &[
                ("grant_type", "refresh_token"),
                ("client_id", client_id),
                ("refresh_token", token),
                ("resource", &self.resource),
            ],
            cancellation,
        )
        .await
    }

    async fn token_request(
        &self,
        form: &[(&str, &str)],
        cancellation: &CancellationToken,
    ) -> AuthResult<TokenResponse> {
        let (status, bytes) = self
            .request(
                self.client.post(self.token.clone()).form(form),
                cancellation,
            )
            .await?;
        if !status.is_success() {
            let failure: Failure =
                serde_json::from_slice(&bytes).map_err(|_| AuthError::InvalidResponse)?;
            // Preserve the structured provider diagnosis; never log successful
            // token responses, request headers or authorization-code payloads.
            let code = bounded_diagnostic(&failure.error, 256);
            let description = bounded_diagnostic(
                failure.error_description.as_deref().unwrap_or_default(),
                2048,
            );
            tracing::warn!(%code, %description, "ChatGPT OAuth request failed");
            return Err(AuthError::OAuth { code, description });
        }
        serde_json::from_slice(&bytes).map_err(|_| AuthError::InvalidResponse)
    }

    pub async fn claims(
        &self,
        token: &str,
        client_id: &str,
        nonce: Option<&str>,
        now: u64,
        cancellation: &CancellationToken,
    ) -> AuthResult<jwt::Claims> {
        let discovery = self.discovery(cancellation).await?;
        let jwks = self.trusted_url(&discovery.jwks_uri)?;
        let (status, bytes) = self.request(self.client.get(jwks), cancellation).await?;
        if !status.is_success() {
            return Err(AuthError::Unavailable);
        }
        let keys = serde_json::from_slice(&bytes).map_err(|_| AuthError::InvalidIdToken)?;
        jwt::verify(
            token,
            &keys,
            self.issuer.as_str().trim_end_matches('/'),
            client_id,
            nonce,
            now,
        )
    }

    pub async fn revoke(
        &self,
        client_id: &str,
        refresh_token: &str,
        cancellation: &CancellationToken,
    ) -> AuthResult<()> {
        let discovery = self.discovery(cancellation).await?;
        let endpoint = self.trusted_url(
            discovery
                .revocation_endpoint
                .as_deref()
                .ok_or(AuthError::InvalidResponse)?,
        )?;
        let (status, _) = self
            .request(
                self.client.post(endpoint).form(&[
                    ("token", refresh_token),
                    ("token_type_hint", "refresh_token"),
                    ("client_id", client_id),
                ]),
                cancellation,
            )
            .await?;
        if status.is_success() {
            Ok(())
        } else {
            Err(AuthError::Unavailable)
        }
    }

    async fn discovery(&self, cancellation: &CancellationToken) -> AuthResult<Discovery> {
        let (status, bytes) = self
            .request(self.client.get(self.discovery.clone()), cancellation)
            .await?;
        if !status.is_success() {
            return Err(AuthError::Unavailable);
        }
        let value: Discovery =
            serde_json::from_slice(&bytes).map_err(|_| AuthError::InvalidResponse)?;
        if value.issuer != self.issuer.as_str().trim_end_matches('/') {
            return Err(AuthError::InvalidIdToken);
        }
        Ok(value)
    }

    fn trusted_url(&self, value: &str) -> AuthResult<Url> {
        let url = Url::parse(value).map_err(|_| AuthError::InvalidResponse)?;
        if url.origin() != self.issuer.origin()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(AuthError::InvalidResponse);
        }
        Ok(url)
    }

    async fn request(
        &self,
        request: reqwest::RequestBuilder,
        cancellation: &CancellationToken,
    ) -> AuthResult<(reqwest::StatusCode, Vec<u8>)> {
        let work = async {
            let mut response = request.send().await.map_err(network_error)?;
            let status = response.status();
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(network_error)? {
                if bytes.len().saturating_add(chunk.len()) > 1024 * 1024 {
                    return Err(AuthError::InvalidResponse);
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok((status, bytes))
        };
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(AuthError::Cancelled),
            result = work => result,
        }
    }
}

impl TokenResponse {
    pub fn into_tokens(self, previous: Option<&TokenSet>, now: u64) -> AuthResult<TokenSet> {
        if !self.token_type.eq_ignore_ascii_case("bearer")
            || self.access_token.is_empty()
            || self.expires_in == 0
        {
            return Err(AuthError::InvalidResponse);
        }
        let scope = self
            .scope
            .or_else(|| previous.map(|p| p.scope.clone()))
            .ok_or(AuthError::MissingScope)?;
        if !super::types::has_plan_scope(&scope) {
            return Err(AuthError::MissingScope);
        }
        let refresh_token = self
            .refresh_token
            .or_else(|| previous.map(|p| p.refresh_token.clone()))
            .filter(|s| !s.is_empty())
            .ok_or(AuthError::InvalidResponse)?;
        let id_token = self
            .id_token
            .or_else(|| previous.map(|p| p.id_token.clone()))
            .filter(|s| !s.is_empty())
            .ok_or(AuthError::InvalidResponse)?;
        Ok(TokenSet {
            access_token: self.access_token,
            refresh_token,
            id_token,
            scope,
            expires_at: now
                .checked_add(self.expires_in)
                .ok_or(AuthError::InvalidResponse)?,
            version: uuid::Uuid::new_v4().to_string(),
        })
    }

    pub fn id_token(&self) -> Option<&str> {
        self.id_token.as_deref()
    }
}

fn network_error(error: reqwest::Error) -> AuthError {
    tracing::warn!(%error, "ChatGPT OAuth transport failed");
    AuthError::Unavailable
}

fn bounded_diagnostic(value: &str, limit: usize) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(limit)
        .collect()
}
