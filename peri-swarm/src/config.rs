use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use url::Url;

pub const API_KEY_ENV: &str = "PERI_SWARM_API_KEY";
pub const BIND_ENV: &str = "PERI_SWARM_BIND";
/// Explicit opt-in for a non-loopback gateway bind (M1). Bearer is still the
/// only authentication, so the flag is only meaningful behind a trusted
/// ingress that terminates TLS.
pub const BIND_ALLOW_NON_LOOPBACK_ENV: &str = "PERI_SWARM_BIND_ALLOW_NON_LOOPBACK";
pub const WORKSPACE_ROOT_ENV: &str = "PERI_SWARM_WORKSPACE_ROOT";
pub const STORE_LOCATOR_ENV: &str = "PERI_SWARM_STORE_LOCATOR";
pub const STORE_CREDENTIAL_ENV: &str = "PERI_SWARM_STORE_CREDENTIAL_ENV";
pub const BUN_BASE_URL_ENV: &str = "PERI_SWARM_BUN_BASE_URL";
/// Alias accepted for the Bun base URL (S8a3a); the canonical name wins when
/// both are set.
pub const BUN_URL_ALIAS_ENV: &str = "PERI_SWARM_BUN_URL";
pub const BUN_BEARER_ENV: &str = "PERI_SWARM_BUN_BEARER_ENV";
/// Direct Bun bridge token (S8a3a). Mutually exclusive with
/// [`BUN_BEARER_ENV`], which names another variable instead.
pub const BUN_TOKEN_ENV: &str = "PERI_SWARM_BUN_TOKEN";
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

/// The PostgreSQL DSN may contain credentials in userinfo or query parameters.
///
/// `Debug` is redacted and [`Self::target`] renders only
/// `postgres://host:port/database`; neither ever carries a credential.
#[derive(Clone, Eq, PartialEq)]
pub struct PostgresLocator(Url);

impl PostgresLocator {
    /// Parse a deployment DSN. `variable` names the source for error text; the
    /// value is never echoed.
    pub fn parse(value: &str, variable: &'static str) -> Result<Self, ConfigError> {
        // One message for both "not a URL" and "wrong scheme": a local path or a
        // Loom locator both need the same next step.
        let invalid =
            || ConfigError::invalid(variable, "expected a postgres:// or postgresql:// DSN");
        let url = Url::parse(value).map_err(|_| invalid())?;
        if !matches!(url.scheme(), "postgres" | "postgresql")
            || !url.has_host()
            || url.fragment().is_some()
        {
            return Err(invalid());
        }
        Ok(Self(url))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Acceptable for logs and error messages: scheme, host, port and database
    /// only. Userinfo, query parameters and the fragment are dropped.
    #[must_use]
    pub fn target(&self) -> String {
        let host = self.0.host_str().unwrap_or_default();
        let port = self.0.port().unwrap_or(5432);
        let database = self.0.path().trim_start_matches('/');
        format!("postgres://{host}:{port}/{database}")
    }

    /// Whether both DSNs address the same database on the same server.
    ///
    /// The key is `(lowercase host, port with the 5432 default, path)`;
    /// credentials and query parameters are ignored on purpose. Two DSNs that
    /// only differ in `options` (for example a `search_path`) still name one
    /// database, and the Swarm state store needs its own.
    #[must_use]
    pub fn same_database(&self, other: &Self) -> bool {
        self.database_key() == other.database_key()
    }

    fn database_key(&self) -> (String, u16, String) {
        (
            self.0.host_str().unwrap_or_default().to_ascii_lowercase(),
            self.0.port().unwrap_or(5432),
            self.0.path().to_owned(),
        )
    }
}

impl std::fmt::Debug for PostgresLocator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PostgresLocator(<redacted>)")
    }
}

/// Typed view of the remote Peri store deployment (`design §4.1`).
///
/// S3 does not open a connection; the locator and credential variable name are
/// carried so S6 can build `SessionStoreDeployment` without re-parsing strings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionStoreConfig {
    pub locator: Option<PostgresLocator>,
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

        let allow_non_loopback = parse_bind_allow_non_loopback(&lookup)?;
        let bind_value = lookup(BIND_ENV).unwrap_or_else(|| DEFAULT_BIND.to_owned());
        let bind_addr = parse_bind(&bind_value, BIND_ENV, allow_non_loopback)?;

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
                Some(value) => Some(PostgresLocator::parse(&value, STORE_LOCATOR_ENV)?),
                None => None,
            },
            engine: "postgres".to_owned(),
            credential_env: optional_env_name(lookup(STORE_CREDENTIAL_ENV), STORE_CREDENTIAL_ENV)?,
            access: StoreAccess::ReadWrite,
        };

        let bun = BunConfig {
            base_url: match lookup(BUN_BASE_URL_ENV) {
                Some(value) => Some(validate_bun_url(&value, BUN_BASE_URL_ENV)?),
                None => None,
            },
            bearer_env: optional_env_name(lookup(BUN_BEARER_ENV), BUN_BEARER_ENV)?,
        };

        let model = ModelConfig {
            default_model: optional_non_empty(lookup(MODEL_ENV), MODEL_ENV)?,
            credential_env: optional_env_name(lookup(MODEL_CREDENTIAL_ENV), MODEL_CREDENTIAL_ENV)?,
        };

        Ok(Self {
            bind_addr,
            api_key,
            workspace_root,
            session_store,
            bun,
            model,
            sse_heartbeat: parse_sse_heartbeat(&lookup)?,
        })
    }

    #[must_use]
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }
}

/// Parse the SSE heartbeat from the same lookup as the rest of the process
/// configuration.
///
/// The bootstrap entry and the standalone [`SwarmConfig`] both call this, so
/// the environment name, the default and the accepted bound have one owner.
pub(crate) fn parse_sse_heartbeat<F>(lookup: &F) -> Result<Duration, ConfigError>
where
    F: Fn(&str) -> Option<String>,
{
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
    Ok(Duration::from_millis(heartbeat_ms))
}

/// The one gateway-bind decision. `BootstrapConfig` and [`SwarmConfig`] both
/// call this so the loopback rule (and the exact error) cannot drift apart.
///
/// A non-loopback address is only accepted when the operator opted in:
/// Bearer remains the only authentication and TLS termination is the
/// responsibility of the trusted ingress in front of this process.
pub(crate) fn parse_bind(
    value: &str,
    variable: &'static str,
    allow_non_loopback: bool,
) -> Result<SocketAddr, ConfigError> {
    let bind_addr: SocketAddr = value
        .parse()
        .map_err(|_| ConfigError::invalid(variable, "expected host:port"))?;
    if !allow_non_loopback && !bind_addr.ip().is_loopback() {
        return Err(ConfigError::invalid(
            variable,
            format!(
                "must bind a loopback IP; set {BIND_ALLOW_NON_LOOPBACK_ENV}=1 only behind a \
                 trusted ingress, because the deployment Bearer token is the only \
                 authentication and that ingress must terminate TLS"
            ),
        ));
    }
    Ok(bind_addr)
}

/// `PERI_SWARM_BIND_ALLOW_NON_LOOPBACK`: `1/true/yes/on` or `0/false/no/off`.
/// Any other value fails the start instead of guessing.
pub(crate) fn parse_bind_allow_non_loopback<F>(lookup: &F) -> Result<bool, ConfigError>
where
    F: Fn(&str) -> Option<String>,
{
    let Some(value) = lookup(BIND_ALLOW_NON_LOOPBACK_ENV) else {
        return Ok(false);
    };
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(ConfigError::invalid(
            BIND_ALLOW_NON_LOOPBACK_ENV,
            "expected one of 1/true/yes/on or 0/false/no/off",
        )),
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

/// Validate the Swarm state store DSN (S6c1a, moved to PostgreSQL).
///
/// The value must be a PostgreSQL DSN naming a database **different** from the
/// session store: sharing one would put the Swarm schema back into the session
/// database, which is exactly what the split avoids. Same instance, different
/// database is the supported deployment. `variable` names the source in errors
/// so the process entry can reuse the rule for its own environment names.
pub(crate) fn validate_state_store_locator(
    value: &str,
    variable: &'static str,
    session: &PostgresLocator,
) -> Result<PostgresLocator, ConfigError> {
    if let Some((scheme, _)) = value.split_once("://") {
        let scheme = scheme.trim().to_ascii_lowercase();
        if matches!(scheme.as_str(), "http" | "https" | "ws" | "wss" | "libsql") {
            return Err(ConfigError::invalid(
                variable,
                "expected a postgres:// or postgresql:// DSN; this build reads a PostgreSQL state \
                 database and a Loom/Hrana locator is no longer supported (see \
                 peri-swarm/README.md 的「升级与回退」)",
            ));
        }
    }
    let locator = PostgresLocator::parse(value, variable)?;
    if locator.same_database(session) {
        return Err(ConfigError::invalid(
            variable,
            format!(
                "must not resolve to the same database as the session store ({}); provision a \
                 dedicated PostgreSQL database for the Swarm state store",
                locator.target()
            ),
        ));
    }
    Ok(locator)
}

/// Rivet engine endpoint (M1): plaintext `http` only, because the pinned
/// client exposes no TLS/CA configuration. `https` is rejected explicitly
/// rather than silently downgraded; TLS termination for the engine needs its
/// own design.
///
/// The token travels to this endpoint, so a literal address must be private:
/// loopback (`127.0.0.0/8`, `::1`), RFC1918 (`10/8`, `172.16/12`,
/// `192.168/16`), ULA (`fc00::/7`) or link-local (`169.254/16`, `fe80::/10`).
/// Public, unspecified (`0.0.0.0`, `::`), multicast and broadcast literals are
/// rejected here. A domain is accepted **without DNS resolution**: the
/// deployment is responsible for pointing it at a private or loopback
/// address.
///
/// The endpoint must carry an explicit port and no userinfo, query, fragment
/// or path (empty or `/`). Callers use the trailing-slash-free spelling so the
/// default stays byte-identical to the former constant.
pub(crate) fn validate_rivet_endpoint(
    value: &str,
    variable: &'static str,
) -> Result<Url, ConfigError> {
    let url = Url::parse(value).map_err(|_| ConfigError::invalid(variable, "expected a URL"))?;
    if url.scheme() == "https" {
        return Err(ConfigError::invalid(
            variable,
            "M1 supports only private plaintext engine endpoints; TLS termination for the \
             engine needs a separate design",
        ));
    }
    if url.scheme() != "http" {
        return Err(ConfigError::invalid(variable, "scheme must be http"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ConfigError::invalid(
            variable,
            "embedded credentials are not allowed; name the token variable instead",
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(ConfigError::invalid(
            variable,
            "query and fragment are not allowed",
        ));
    }
    ensure_private_engine_host(&url, variable)?;
    // `Url::port()` is not enough here: it reports `None` for the scheme's
    // default port, so a written-out `http://host:80` would be mistaken for a
    // missing port. Ask the raw authority instead.
    if !has_explicit_port(value) {
        return Err(ConfigError::invalid(variable, "explicit port is required"));
    }
    if !matches!(url.path(), "" | "/") {
        return Err(ConfigError::invalid(variable, "path must be empty or /"));
    }
    Ok(url)
}

/// Accept a literal engine address only when it cannot be a public host.
///
/// A domain is deliberately accepted without resolution: refusing it would
/// either require DNS at parse time or reject every deployment using an
/// internal name. The documented contract puts that guarantee on the
/// deployment.
fn ensure_private_engine_host(url: &Url, variable: &'static str) -> Result<(), ConfigError> {
    let Some(host) = url.host() else {
        return Err(ConfigError::invalid(variable, "host is required"));
    };
    let rejection = match host {
        url::Host::Domain(_) => return Ok(()),
        url::Host::Ipv4(addr) => {
            if addr.is_loopback() || addr.is_private() || addr.is_link_local() {
                return Ok(());
            }
            if addr.is_unspecified() {
                "the unspecified address"
            } else if addr.is_multicast() {
                "a multicast address"
            } else if addr.is_broadcast() {
                "the broadcast address"
            } else {
                "a public address"
            }
        }
        url::Host::Ipv6(addr) => {
            if addr.is_loopback() || addr.is_unique_local() || addr.is_unicast_link_local() {
                return Ok(());
            }
            if addr.is_unspecified() {
                "the unspecified address"
            } else if addr.is_multicast() {
                "a multicast address"
            } else {
                "a public address"
            }
        }
    };
    Err(ConfigError::invalid(
        variable,
        format!(
            "engine endpoint {url} host {host} is {rejection}; use a loopback, RFC1918, ULA or \
             link-local literal, or a domain that the deployment points at a private network"
        ),
    ))
}

/// Whether the raw URL spells a port.
///
/// `Url::port()` returns `None` when the authority carries the scheme's default
/// port (`http://host:80`), so it cannot distinguish "wrote 80" from "wrote
/// nothing". Read the raw authority instead, after trimming the whitespace the
/// URL parser itself tolerates. Userinfo is rejected before this check; the
/// `@` handling keeps the helper total anyway. Bracketed IPv6 uses `]:` as the
/// port delimiter, so `[::1]:80` is recognized.
fn has_explicit_port(value: &str) -> bool {
    let Some((_, rest)) = value.trim().split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, tail)| tail);
    let port = if let Some(rest) = host_port.strip_prefix('[') {
        match rest.split_once(']') {
            Some((_, after)) => after.strip_prefix(':'),
            None => None,
        }
    } else {
        host_port.rsplit_once(':').map(|(_, port)| port)
    };
    port.is_some_and(|port| !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()))
}

/// Deployment identity for the engine (namespace and pool): non-empty, no
/// whitespace, at most 128 characters. Values are kept verbatim.
pub(crate) fn validate_rivet_identity(
    value: String,
    variable: &'static str,
) -> Result<String, ConfigError> {
    if value.trim().is_empty() {
        return Err(ConfigError::invalid(variable, "must not be empty"));
    }
    if value.chars().any(char::is_whitespace) {
        return Err(ConfigError::invalid(
            variable,
            "must not contain whitespace",
        ));
    }
    if value.chars().count() > 128 {
        return Err(ConfigError::invalid(
            variable,
            "must be at most 128 characters",
        ));
    }
    Ok(value)
}

/// `127.0.0.0/8`, `::1` and `localhost` are loopback. The check is on the
/// parsed host, so `127.0.0.1.example.test` is not mistaken for an address.
pub(crate) fn is_loopback_endpoint(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(addr)) => addr.is_loopback(),
        Some(url::Host::Ipv6(addr)) => addr.is_loopback(),
        Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

pub(crate) fn validate_bun_url(value: &str, variable: &'static str) -> Result<Url, ConfigError> {
    let url = Url::parse(value).map_err(|_| ConfigError::invalid(variable, "expected a URL"))?;
    if url.scheme() != "http" {
        return Err(ConfigError::invalid(
            variable,
            "Bun bridge must be loopback HTTP",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ConfigError::invalid(
            variable,
            "embedded credentials are not allowed",
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(ConfigError::invalid(
            variable,
            "query and fragment are not allowed",
        ));
    }
    let loopback = matches!(url.host_str(), Some("127.0.0.1" | "::1" | "localhost"));
    if !loopback {
        return Err(ConfigError::invalid(
            variable,
            "host must be loopback (127.0.0.1, ::1 or localhost)",
        ));
    }
    if url.port().is_none() {
        return Err(ConfigError::invalid(variable, "explicit port is required"));
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
