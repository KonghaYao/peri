//! HTTP client for the local Bun swarm sandbox service (S8a3a).
//!
//! The service contract consumed here is deliberately small:
//!
//! - `POST /v1/sandboxes` with `{"logicalEnvId": "...", "lazy": true}` returns
//!   `{"sandbox": {sandboxId, workspacePath, state, runId, ...}}` and never
//!   starts SRT. `state` is `reserved` for a fresh reservation and `ready` when
//!   the environment was already running.
//! - the outer bearer token (when configured) is the only credential this
//!   client holds; the inner sandbox token stays inside the Bun process.
//!
//! The descriptor returned by [`BunSandboxClient::reserve`] is what the actor
//! turns into `session/new.mcpServers["workspace"]`; the public `mcpUrl` of the
//! sandbox is derived from the configured base URL so the injected entry can
//! only address the Bun bridge this deployment was configured with.

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;
use url::Url;

const RESERVE_TIMEOUT: Duration = Duration::from_secs(10);
const SANDBOX_ID_HEX_LEN: usize = 32;

/// Deployment-level Bun bridge configuration.
///
/// The token is the **outer** bearer token shared with the Bun service
/// (`PERI_SWARM_BUN_TOKEN` on the Bun side). It is never written to the Swarm
/// store and never rendered by `Debug`.
#[derive(Clone, Eq, PartialEq)]
pub struct SandboxClientConfig {
    pub base_url: Url,
    pub token: Option<String>,
}

impl std::fmt::Debug for SandboxClientConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SandboxClientConfig")
            .field("base_url", &self.base_url)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// One reserved environment as seen by the actor.
#[derive(Clone)]
pub struct SandboxMcpBinding {
    pub logical_env_id: String,
    pub sandbox_id: String,
    pub workspace_path: PathBuf,
    pub mcp_url: String,
    /// Outer Bun token used in the injected `Authorization` header.
    pub token: Option<String>,
}

impl std::fmt::Debug for SandboxMcpBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SandboxMcpBinding")
            .field("logical_env_id", &self.logical_env_id)
            .field("sandbox_id", &self.sandbox_id)
            .field("workspace_path", &self.workspace_path)
            .field("mcp_url", &self.mcp_url)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// Persisted identity plus the routing metadata of one reservation.
#[derive(Debug)]
pub struct SandboxReservation {
    pub binding: SandboxMcpBinding,
    pub run_id: Option<String>,
    pub last_used_at: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SandboxClientError {
    #[error("sandbox bridge request failed: {0}")]
    Unavailable(String),
    #[error("sandbox bridge returned HTTP {status}: {code}: {message}")]
    Rejected {
        status: u16,
        code: String,
        message: String,
    },
    #[error("sandbox bridge returned an invalid reservation: {0}")]
    Invalid(String),
}

/// Logical environment ids are exactly the Bun service's path/segment pattern.
#[must_use]
pub fn is_valid_logical_env_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    let Some(first) = bytes.first() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() || bytes.len() > 128 {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn is_sandbox_id(value: &str) -> bool {
    value.strip_prefix("sbx_").is_some_and(|hex| {
        hex.len() == SANDBOX_ID_HEX_LEN
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SandboxView {
    logical_env_id: String,
    sandbox_id: String,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    run_id: Option<String>,
    workspace_path: String,
    #[serde(default)]
    last_used_at: Option<String>,
}

#[derive(Deserialize)]
struct ReserveResponse {
    sandbox: SandboxView,
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Deserialize)]
struct ErrorBody {
    #[serde(default)]
    code: String,
    #[serde(default)]
    message: String,
}

/// Client for the local Bun sandbox service.
#[derive(Clone)]
pub struct BunSandboxClient {
    config: SandboxClientConfig,
    http: reqwest::Client,
}

impl std::fmt::Debug for BunSandboxClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BunSandboxClient")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl BunSandboxClient {
    #[must_use]
    pub fn new(config: SandboxClientConfig) -> Self {
        let http = reqwest::Client::builder()
            .timeout(RESERVE_TIMEOUT)
            .build()
            .unwrap_or_default();
        Self { config, http }
    }

    #[must_use]
    pub fn config(&self) -> &SandboxClientConfig {
        &self.config
    }

    /// Reserve (or reuse) one logical environment without starting SRT.
    pub async fn reserve(
        &self,
        logical_env_id: &str,
    ) -> Result<SandboxReservation, SandboxClientError> {
        if !is_valid_logical_env_id(logical_env_id) {
            return Err(SandboxClientError::Invalid(format!(
                "logicalEnvId does not match [A-Za-z0-9][A-Za-z0-9._-]{{0,127}}: {logical_env_id}"
            )));
        }
        let url = format!(
            "{}/v1/sandboxes",
            self.config.base_url.as_str().trim_end_matches('/')
        );
        let mut request = self
            .http
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(json!({ "logicalEnvId": logical_env_id, "lazy": true }).to_string());
        if let Some(token) = self.config.token.as_deref() {
            request = request.bearer_auth(token);
        }
        let response = request
            .send()
            .await
            .map_err(|error| SandboxClientError::Unavailable(error.to_string()))?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|error| SandboxClientError::Unavailable(error.to_string()))?;
        if !status.is_success() {
            let (code, message) = serde_json::from_slice::<ErrorEnvelope>(&bytes).map_or_else(
                |_| {
                    (
                        "unknown".to_owned(),
                        "sandbox bridge rejected the request".to_owned(),
                    )
                },
                |envelope| (envelope.error.code, envelope.error.message),
            );
            return Err(SandboxClientError::Rejected {
                status: status.as_u16(),
                code,
                message,
            });
        }
        let parsed: ReserveResponse = serde_json::from_slice(&bytes)
            .map_err(|error| SandboxClientError::Invalid(error.to_string()))?;
        self.validate(logical_env_id, parsed.sandbox)
    }

    fn validate(
        &self,
        logical_env_id: &str,
        view: SandboxView,
    ) -> Result<SandboxReservation, SandboxClientError> {
        if view.logical_env_id != logical_env_id {
            return Err(SandboxClientError::Invalid(format!(
                "reservation returned logicalEnvId {} instead of {logical_env_id}",
                view.logical_env_id
            )));
        }
        if !is_sandbox_id(&view.sandbox_id) {
            return Err(SandboxClientError::Invalid(format!(
                "reservation returned an invalid sandboxId: {}",
                view.sandbox_id
            )));
        }
        if matches!(view.state.as_deref(), Some("failed")) {
            return Err(SandboxClientError::Rejected {
                status: 502,
                code: "sandbox_start_failed".to_owned(),
                message: "sandbox bridge reports a failed environment".to_owned(),
            });
        }
        let mcp_url = format!(
            "{}/sandboxes/{logical_env_id}/mcp",
            self.config.base_url.as_str().trim_end_matches('/')
        );
        Url::parse(&mcp_url).map_err(|error| SandboxClientError::Invalid(error.to_string()))?;
        Ok(SandboxReservation {
            binding: SandboxMcpBinding {
                logical_env_id: logical_env_id.to_owned(),
                sandbox_id: view.sandbox_id,
                workspace_path: PathBuf::from(view.workspace_path),
                mcp_url,
                token: self.config.token.clone(),
            },
            run_id: view.run_id,
            last_used_at: view.last_used_at,
        })
    }
}

#[cfg(test)]
#[path = "client_test.rs"]
mod tests;
