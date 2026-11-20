use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use url::Url;

pub const API_KEY_ENV: &str = "PERI_SWARM_API_KEY";
pub const BIND_ENV: &str = "PERI_SWARM_BIND";
pub const WORKSPACE_ROOT_ENV: &str = "PERI_SWARM_WORKSPACE_ROOT";
pub const STORE_LOCATOR_ENV: &str = "PERI_SWARM_STORE_LOCATOR";
pub const STORE_CREDENTIAL_ENV: &str = "PERI_SWARM_STORE_CREDENTIAL_ENV";
pub const BUN_BASE_URL_ENV: &str = "PERI_SWARM_BUN_BASE_URL";
pub const BUN_BEARER_ENV: &str = "PERI_SWARM_BUN_BEARER_ENV";
pub const MODEL_ENV: &str = "PERI_SWARM_MODEL";
pub const MODEL_CREDENTIAL_ENV: &str = "PERI_SWARM_MODEL_CREDENTIAL_ENV";
pub const SSE_HEARTBEAT_MS_ENV: &str = "PERI_SWARM_SSE_HEARTBEAT_MS";

pub const DEFAULT_BIND: &str = "127.0.0.1:8787";
pub const DEFAULT_SSE_HEARTBEAT_MS: u64 = 15_000;
pub const MAX_SSE_HEARTBEAT_MS: u64 = 600_000;

#[derive(Clone, Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("missing required environment variable {0}")]
    MissingEnv(&'static str),
    #[error("invalid value for {variable}: {reason}")]
    Invalid {
        variable: &'static str,
        reason: String,
    },
    #[error("cannot read {variable}: {reason}")]
    Io {
        variable: &'static str,
        reason: String,
    },
}

impl ConfigError {
    fn invalid(variable: &'static str, reason: impl Into<String>) -> Self {
        Self::Invalid {
            variable,
            reason: reason.into(),
        }
    }
}

/// Deployment-level API key. The value is never printed, serialized, or logged.
#[derive(Clone)]
pub struct DeploymentApiKey(String);

impl DeploymentApiKey {
    pub fn new(value: impl Into<String>) -> Result<Self, ConfigError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ConfigError::invalid(API_KEY_ENV, "must not be empty"));
        }
        Ok(Self(value))
    }

    /// Constant-time comparison for the `Authorization: Bearer` value.
    #[must_use]
    pub fn verify(&self, candidate: &str) -> bool {
        let expected = self.0.as_bytes();
        let actual = candidate.as_bytes();
        let mut difference = expected.len() ^ actual.len();
        let max = expected.len().max(actual.len());
        for index in 0..max {
            let left = expected.get(index).copied().unwrap_or(0);
            let right = actual.get(index).copied().unwrap_or(0);
            difference |= usize::from(left ^ right);
        }
        difference == 0
    }
}

impl std::fmt::Debug for DeploymentApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DeploymentApiKey(<redacted>)")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreAccess {
    ReadWrite,
}

/// Typed view of the remote Peri store deployment (`design §4.1`).
///
/// S3 does not open a connection; the locator and credential variable name are
/// carried so S6 can build `SessionStoreDeployment` without re-parsing strings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionStoreConfig {
    pub locator: Option<Url>,
    pub engine: String,
    pub credential_env: Option<String>,
    pub access: StoreAccess,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BunConfig {
    pub base_url: Option<Url>,
    pub bearer_env: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelConfig {
    pub default_model: Option<String>,
    pub credential_env: Option<String>,
}

/// Process configuration built from environment variables or explicit CLI
/// overrides in `bootstrap` (S6). No secret value is stored more than once and
/// none is ever rendered.
#[derive(Clone, Debug)]
pub struct SwarmConfig {
    pub bind_addr: SocketAddr,
    pub api_key: DeploymentApiKey,
    pub workspace_root: PathBuf,
    pub session_store: SessionStoreConfig,
    pub bun: BunConfig,
    pub model: ModelConfig,
    pub sse_heartbeat: Duration,
}

impl SwarmConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_env_map(|name| std::env::var(name).ok())
    }

    /// Parse from an arbitrary lookup so CLI overrides and tests share one path.
    pub fn from_env_map<F>(lookup: F) -> Result<Self, ConfigError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let api_key_value = lookup(API_KEY_ENV).ok_or(ConfigError::MissingEnv(API_KEY_ENV))?;
        let api_key = DeploymentApiKey::new(api_key_value)?;

        let bind_value = lookup(BIND_ENV).unwrap_or_else(|| DEFAULT_BIND.to_owned());
        let bind_addr: SocketAddr = bind_value
            .parse()
            .map_err(|_| ConfigError::invalid(BIND_ENV, "expected host:port"))?;
        if !bind_addr.ip().is_loopback() {
            return Err(ConfigError::invalid(BIND_ENV, "must bind a loopback IP"));
        }

        let workspace_value = match lookup(WORKSPACE_ROOT_ENV) {
            Some(value) => PathBuf::from(value),
            None => std::env::current_dir().map_err(|error| ConfigError::Io {
                variable: WORKSPACE_ROOT_ENV,
                reason: error.to_string(),
            })?,
        };
        let workspace_root = canonical_directory(&workspace_value)?;

        let session_store = SessionStoreConfig {
            locator: match lookup(STORE_LOCATOR_ENV) {
                Some(value) => Some(validate_store_locator(&value, STORE_LOCATOR_ENV)?),
                None => None,
            },
            engine: "turso".to_owned(),
            credential_env: optional_env_name(lookup(STORE_CREDENTIAL_ENV), STORE_CREDENTIAL_ENV)?,
            access: StoreAccess::ReadWrite,
        };

        let bun = BunConfig {
            base_url: match lookup(BUN_BASE_URL_ENV) {
                Some(value) => Some(validate_bun_url(&value)?),
                None => None,
            },
            bearer_env: optional_env_name(lookup(BUN_BEARER_ENV), BUN_BEARER_ENV)?,
        };

        let model = ModelConfig {
            default_model: optional_non_empty(lookup(MODEL_ENV), MODEL_ENV)?,
            credential_env: optional_env_name(lookup(MODEL_CREDENTIAL_ENV), MODEL_CREDENTIAL_ENV)?,
        };

        let heartbeat_ms = match lookup(SSE_HEARTBEAT_MS_ENV) {
            Some(value) => value
                .parse::<u64>()
                .ok()
                .filter(|value| *value > 0 && *value <= MAX_SSE_HEARTBEAT_MS)
                .ok_or_else(|| {
                    ConfigError::invalid(
                        SSE_HEARTBEAT_MS_ENV,
                        format!("expected 1..={MAX_SSE_HEARTBEAT_MS}"),
                    )
                })?,
            None => DEFAULT_SSE_HEARTBEAT_MS,
        };

        Ok(Self {
            bind_addr,
            api_key,
            workspace_root,
            session_store,
            bun,
            model,
            sse_heartbeat: Duration::from_millis(heartbeat_ms),
        })
    }

    #[must_use]
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }
}

fn canonical_directory(path: &Path) -> Result<PathBuf, ConfigError> {
    let canonical = std::fs::canonicalize(path).map_err(|error| ConfigError::Io {
        variable: WORKSPACE_ROOT_ENV,
        reason: error.to_string(),
    })?;
    if !canonical.is_dir() {
        return Err(ConfigError::invalid(
            WORKSPACE_ROOT_ENV,
            "must be a directory",
        ));
    }
    Ok(canonical)
}

/// Remote session-store locator: HTTP(S), no embedded credentials.
///
/// `variable` names the source in errors so the process entry can reuse the
/// rule for its own environment names without forking the check.
pub(crate) fn validate_store_locator(
    value: &str,
    variable: &'static str,
) -> Result<Url, ConfigError> {
    let url = Url::parse(value).map_err(|_| ConfigError::invalid(variable, "expected a URL"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(ConfigError::invalid(
            variable,
            "scheme must be http or https",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ConfigError::invalid(
            variable,
            "embedded credentials are not allowed; use the credential env var",
        ));
    }
    Ok(url)
}

fn validate_bun_url(value: &str) -> Result<Url, ConfigError> {
    let url =
        Url::parse(value).map_err(|_| ConfigError::invalid(BUN_BASE_URL_ENV, "expected a URL"))?;
    if url.scheme() != "http" {
        return Err(ConfigError::invalid(
            BUN_BASE_URL_ENV,
            "Bun bridge must be loopback HTTP",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ConfigError::invalid(
            BUN_BASE_URL_ENV,
            "embedded credentials are not allowed",
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(ConfigError::invalid(
            BUN_BASE_URL_ENV,
            "query and fragment are not allowed",
        ));
    }
    let loopback = matches!(url.host_str(), Some("127.0.0.1" | "::1" | "localhost"));
    if !loopback {
        return Err(ConfigError::invalid(
            BUN_BASE_URL_ENV,
            "host must be loopback (127.0.0.1, ::1 or localhost)",
        ));
    }
    if url.port().is_none() {
        return Err(ConfigError::invalid(
            BUN_BASE_URL_ENV,
            "explicit port is required",
        ));
    }
    Ok(url)
}

pub(crate) fn optional_non_empty(
    value: Option<String>,
    variable: &'static str,
) -> Result<Option<String>, ConfigError> {
    match value {
        Some(value) if value.trim().is_empty() => {
            Err(ConfigError::invalid(variable, "must not be empty"))
        }
        other => Ok(other),
    }
}

pub(crate) fn optional_env_name(
    value: Option<String>,
    variable: &'static str,
) -> Result<Option<String>, ConfigError> {
    match value {
        Some(value) => {
            if !is_env_var_name(&value) {
                return Err(ConfigError::invalid(
                    variable,
                    "expected an environment variable name, not a value",
                ));
            }
            Ok(Some(value))
        }
        None => Ok(None),
    }
}

pub(crate) fn is_env_var_name(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
