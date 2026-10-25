use std::{collections::HashMap, time::Duration};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::rand::SecureRandom;
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_util::sync::CancellationToken;
use url::Url;

use super::{
    oauth::OAuth,
    types::{AuthError, AuthResult, PendingLogin, StoredAccount},
    RESOURCE, SCOPES,
};

const LOGIN_TIMEOUT: Duration = Duration::from_secs(600);

pub(super) struct LoginAttempt {
    pub id: String,
    pub nonce: String,
    pub verifier: String,
    pub redirect: String,
    pub account: StoredAccount,
    state: String,
    listener: TcpListener,
    deadline: std::time::Instant,
}

pub(super) struct AuthorizationCode {
    pub code: String,
    pub client_id: String,
}

impl LoginAttempt {
    pub async fn begin(oauth: &OAuth, account: StoredAccount) -> AuthResult<(Self, PendingLogin)> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|error| {
                tracing::warn!(%error, "ChatGPT OAuth callback listener failed");
                AuthError::Unavailable
            })?;
        let port = listener
            .local_addr()
            .map_err(|_| AuthError::Unavailable)?
            .port();
        let redirect = format!("http://127.0.0.1:{port}/auth/callback");
        let verifier = random_secret()?;
        let state = random_secret()?;
        let nonce = random_secret()?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut authorization_url = oauth.authorize.clone();
        {
            let mut query = authorization_url.query_pairs_mut();
            query
                .append_pair("response_type", "code")
                .append_pair(
                    "client_id",
                    account
                        .client_id
                        .as_deref()
                        .unwrap_or("dynamic_agent_client"),
                )
                .append_pair("redirect_uri", &redirect)
                .append_pair("scope", SCOPES)
                .append_pair("resource", RESOURCE)
                .append_pair("state", &state)
                .append_pair("nonce", &nonce)
                .append_pair("code_challenge", &challenge)
                .append_pair("code_challenge_method", "S256")
                .append_pair("ext_agent_host_id", &account.host_id);
            if account.client_id.is_none() {
                query.append_pair("agent_name_hint", "Peri");
            }
        }
        let id = uuid::Uuid::new_v4().to_string();
        let pending = PendingLogin {
            attempt_id: id.clone(),
            authorization_url: authorization_url.into(),
        };
        Ok((
            Self {
                id,
                nonce,
                verifier,
                redirect,
                account,
                state,
                listener,
                deadline: peri_time::monotonic_now() + LOGIN_TIMEOUT,
            },
            pending,
        ))
    }

    pub async fn receive(&self, cancellation: &CancellationToken) -> AuthResult<AuthorizationCode> {
        loop {
            let (mut socket, peer) = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(AuthError::Cancelled),
                _ = peri_time::sleep_until(self.deadline) => return Err(AuthError::Cancelled),
                accepted = self.listener.accept() => accepted.map_err(|_| AuthError::Unavailable)?,
            };
            if !peer.ip().is_loopback() {
                continue;
            }
            let read_request = async {
                let mut bytes = Vec::new();
                while !bytes.ends_with(b"\r\n\r\n") && bytes.len() < 16 * 1024 {
                    let byte = socket
                        .read_u8()
                        .await
                        .map_err(|_| AuthError::InvalidCallback)?;
                    bytes.push(byte);
                }
                let request =
                    std::str::from_utf8(&bytes).map_err(|_| AuthError::InvalidCallback)?;
                self.validate_http(request)
            };
            let result = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(AuthError::Cancelled),
                _ = peri_time::sleep_until(self.deadline) => return Err(AuthError::Cancelled),
                result = peri_time::timeout(Duration::from_secs(3), read_request) => result.unwrap_or(Err(AuthError::InvalidCallback)),
            };
            let (status, body) = if result.is_ok() {
                ("200 OK", "ChatGPT sign-in received. Return to Peri.")
            } else {
                ("400 Bad Request", "ChatGPT sign-in callback rejected.")
            };
            let response = format!("HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len());
            let _ = peri_time::timeout(
                Duration::from_secs(1),
                socket.write_all(response.as_bytes()),
            )
            .await;
            match result {
                Ok(value) => return Ok(value),
                Err(error @ (AuthError::AccessDenied | AuthError::OAuth { .. })) => {
                    return Err(error)
                }
                // A stray local request must not terminate the pending login.
                Err(_) => continue,
            }
        }
    }

    fn validate_http(&self, request: &str) -> AuthResult<AuthorizationCode> {
        if !request.ends_with("\r\n\r\n") {
            return Err(AuthError::InvalidCallback);
        }
        let mut lines = request.split("\r\n");
        let mut first = lines
            .next()
            .ok_or(AuthError::InvalidCallback)?
            .split_whitespace();
        if first.next() != Some("GET") {
            return Err(AuthError::InvalidCallback);
        }
        let target = first.next().ok_or(AuthError::InvalidCallback)?;
        if first.next() != Some("HTTP/1.1")
            || first.next().is_some()
            || !target.starts_with('/')
            || target.starts_with("//")
        {
            return Err(AuthError::InvalidCallback);
        }
        let host = Url::parse(&self.redirect).map_err(|_| AuthError::InvalidCallback)?;
        let expected_host = format!(
            "127.0.0.1:{}",
            host.port().ok_or(AuthError::InvalidCallback)?
        );
        let hosts: Vec<_> = lines
            .filter_map(|line| line.split_once(':'))
            .filter(|(name, _)| name.eq_ignore_ascii_case("host"))
            .map(|(_, value)| value.trim())
            .collect();
        if hosts != [expected_host.as_str()] {
            return Err(AuthError::InvalidCallback);
        }
        let url = host.join(target).map_err(|_| AuthError::InvalidCallback)?;
        if url.origin() != host.origin() {
            return Err(AuthError::InvalidCallback);
        }
        validate_callback(&url, &self.state, self.account.client_id.as_deref())
    }
}

fn validate_callback(
    url: &Url,
    state: &str,
    registered_client: Option<&str>,
) -> AuthResult<AuthorizationCode> {
    if url.path() != "/auth/callback" || url.fragment().is_some() {
        return Err(AuthError::InvalidCallback);
    }
    let mut fields = HashMap::new();
    for (key, value) in url.query_pairs() {
        if fields
            .insert(key.into_owned(), value.into_owned())
            .is_some()
        {
            return Err(AuthError::InvalidCallback);
        }
    }
    if fields.get("state").map(String::as_str) != Some(state) {
        return Err(AuthError::InvalidCallback);
    }
    if let Some(error) = fields.get("error") {
        if error.is_empty() || fields.contains_key("code") {
            return Err(AuthError::InvalidCallback);
        }
        if error == "access_denied" {
            return Err(AuthError::AccessDenied);
        }
        return Err(AuthError::OAuth {
            code: error
                .chars()
                .filter(|c| !c.is_control())
                .take(256)
                .collect(),
            description: fields
                .get("error_description")
                .map(String::as_str)
                .unwrap_or_default()
                .chars()
                .filter(|c| !c.is_control())
                .take(2048)
                .collect(),
        });
    }
    let code = fields
        .get("code")
        .filter(|s| !s.is_empty() && s.len() <= 8192)
        .ok_or(AuthError::InvalidCallback)?
        .clone();
    let client_id = match (registered_client, fields.get("client_id")) {
        (Some(expected), Some(received)) if expected != received => {
            return Err(AuthError::InvalidCallback)
        }
        (Some(expected), _) => expected.to_owned(),
        (None, Some(issued))
            if !issued.is_empty()
                && issued != "dynamic_agent_client"
                && issued.len() <= 512
                && !issued.chars().any(char::is_whitespace) =>
        {
            issued.clone()
        }
        _ => return Err(AuthError::InvalidCallback),
    };
    Ok(AuthorizationCode { code, client_id })
}

fn random_secret() -> AuthResult<String> {
    let mut bytes = [0_u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| AuthError::Unavailable)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(test)]
#[path = "callback_test.rs"]
mod tests;
