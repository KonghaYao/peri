//! Process entry for the Swarm gateway (S6b/S6c1a).
//!
//! One process owns **two** PostgreSQL databases:
//!
//! 1. the **remote Peri session store** (`design §4.1`): opened once with the
//!    deployment's DSN and a virtual workspace identity. Opening also runs the
//!    Peri schema check/upgrade (`design §4.2`); an unknown or newer store
//!    fails closed. There is no local SQLite fallback on this entry.
//! 2. the **Swarm state store** (S6c1a): `PERI_SWARM_STATE_STORE` is a separate
//!    PostgreSQL database opened by [`SwarmStateStore`] with its own
//!    `swarm_meta` schema version. Both DSNs are required and must name
//!    different databases (the same instance is fine); a failure to open either
//!    one stops the process instead of reusing the other.
//!
//! The installed [`StaticDeployment`] carries the Swarm state store and the
//! deployment's session-store business handle, so the gateway and every actor
//! read and write Swarm facts there and every ACP child shares one session
//! store facade and PostgreSQL pool. The process holds the only close right;
//! children never close the shared store.
//!
//! 3. the **Rivet registry** that serves [`crate::actor::SwarmActor`] and the
//!    **axum gateway** ([`crate::http::build_router`]). Both run on one shared
//!    [`CancellationToken`]; either failing stops the process with a
//!    diagnosable error.
//!
//! This stage does not require a contactable database or a real model: the entry
//! must fail visibly when either remote configuration is missing, and every
//! actor child shares the deployment's remote session-store handle.
//!
//! Environment (secrets are only read by name, never rendered):
//!
//! | variable | meaning |
//! | --- | --- |
//! | `PERI_SWARM_BIND` | gateway address, default `127.0.0.1:8080` |
//! | `PERI_SWARM_BIND_ALLOW_NON_LOOPBACK` | opt-in switch for a non-loopback bind, default off |
//! | `PERI_SWARM_API_KEY` | deployment bearer key (required) |
//! | `PERI_SWARM_SESSION_STORE` | Peri session store PostgreSQL DSN (required) |
//! | `PERI_SWARM_STATE_STORE` | Swarm state store PostgreSQL DSN (required, different database) |
//! | `PERI_SWARM_STORE_ENGINE` | remote engine name, default `postgres` |
//! | `PERI_SWARM_MACHINE_ID` | stable machine UUID (required) |
//! | `PERI_SWARM_WORKSPACE_ROOT` | absolute, normalized virtual workspace root |
//! | `PERI_SWARM_MODEL` | optional deployment model |
//! | `PERI_SWARM_SETTINGS_JSON` | optional trusted settings document (JSON) |
//! | `PERI_SWARM_BUN_BASE_URL` | optional Bun sandbox bridge (`http://127.0.0.1:<port>`) |
//! | `PERI_SWARM_BUN_URL` | alias for the Bun base URL |
//! | `PERI_SWARM_BUN_TOKEN` | optional Bun outer bearer token (value; not rendered) |
//! | `PERI_SWARM_BUN_BEARER_ENV` | alternative: **name** of the variable holding that token |
//! | `PERI_SWARM_SSE_HEARTBEAT_MS` | SSE heartbeat in milliseconds, default `15000` |
//! | `PERI_SWARM_RIVET_ENDPOINT` | engine endpoint, default `http://127.0.0.1:6420` (plaintext `http` only) |
//! | `PERI_SWARM_RIVET_TOKEN_ENV` | **name** of the variable holding the engine token; required for a non-loopback endpoint |
//! | `PERI_SWARM_RIVET_NAMESPACE` | engine namespace (deployment identity), default `default` |
//! | `PERI_SWARM_RIVET_POOL` | engine pool (deployment identity), default `peri-swarm` |
//! | `PERI_SWARM_RIVET_ENGINE_SPAWN` | `always` (default), `auto` or `never`; a non-loopback endpoint requires `never` |

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use peri_acp_types::session_resources::SessionStoreShutdownPort;
use peri_resources::{RemoteWorkspaceEnvironment, Resources, SessionStorePoolPolicy};
use rivetkit::client::{Client, ClientConfig};
use rivetkit::{EngineSpawnMode, ServeConfig};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;

use crate::actor::deployment::{install_deployment, StaticDeployment};
use crate::actor::runtime::SessionStoreDeployment;
use crate::actor::service::ActorServiceConfig;
use crate::actor::swarm;
use crate::config::{
    is_env_var_name, is_loopback_endpoint, optional_non_empty, parse_bind,
    parse_bind_allow_non_loopback, parse_sse_heartbeat, validate_bun_url, validate_rivet_endpoint,
    validate_rivet_identity, validate_state_store_locator, BunConfig, ConfigError,
    DeploymentApiKey, ModelConfig, PostgresLocator, SessionStoreConfig, StoreAccess, SwarmConfig,
    API_KEY_ENV, BIND_ENV, BUN_BASE_URL_ENV, BUN_BEARER_ENV, BUN_TOKEN_ENV, BUN_URL_ALIAS_ENV,
    MODEL_ENV, WORKSPACE_ROOT_ENV,
};
use crate::gateway::{RivetServiceConfig, RivetSwarmService};
use crate::http::{build_router, AppState};
use crate::sandbox::SandboxClientConfig;
use crate::store::{SwarmStateStore, SwarmStateStoreConfig, SwarmStore};

/// PostgreSQL DSN for the Peri session store.
pub const SESSION_STORE_ENV: &str = "PERI_SWARM_SESSION_STORE";
/// PostgreSQL DSN for the **Swarm state** database (S6c1a).
///
/// A dedicated database: it must not name the session store's database, so a
/// Swarm schema change can never touch Peri's session schema. The same
/// PostgreSQL instance is supported.
pub const STATE_STORE_ENV: &str = "PERI_SWARM_STATE_STORE";
/// Remote engine name; only needed when the locator cannot decide it.
pub const STORE_ENGINE_ENV: &str = "PERI_SWARM_STORE_ENGINE";
/// Stable machine UUID persisted for the virtual workspace identity.
pub const MACHINE_ID_ENV: &str = "PERI_SWARM_MACHINE_ID";
/// Optional trusted settings document handed to every child host.
pub const SETTINGS_JSON_ENV: &str = "PERI_SWARM_SETTINGS_JSON";

pub const DEFAULT_BOOTSTRAP_BIND: &str = "127.0.0.1:8080";
pub const DEFAULT_STORE_ENGINE: &str = "postgres";

/// Rivet engine endpoint (M1). A local engine is the default; an external one
/// is reachable through the same configuration surface.
pub const RIVET_ENDPOINT_ENV: &str = "PERI_SWARM_RIVET_ENDPOINT";
/// **Name** of the environment variable that holds the engine token. Never the
/// value: the token is resolved once in [`run`] and never stored in a config.
pub const RIVET_TOKEN_ENV: &str = "PERI_SWARM_RIVET_TOKEN_ENV";
/// Engine namespace (deployment identity).
pub const RIVET_NAMESPACE_ENV: &str = "PERI_SWARM_RIVET_NAMESPACE";
/// Engine pool name (deployment identity).
pub const RIVET_POOL_ENV: &str = "PERI_SWARM_RIVET_POOL";
/// Whether this process may start a local engine (`always`/`auto`) or must
/// attach to an external one (`never`).
pub const RIVET_ENGINE_SPAWN_ENV: &str = "PERI_SWARM_RIVET_ENGINE_SPAWN";

/// Defaults match the constants used before M1 byte for byte, so an unchanged
/// environment keeps the local development chain identical.
pub const DEFAULT_RIVET_ENDPOINT: &str = "http://127.0.0.1:6420";
/// The local engine's fixed development token. Only used for a loopback
/// endpoint; a non-loopback endpoint must name its own variable.
pub const DEFAULT_RIVET_TOKEN: &str = "default";
pub const DEFAULT_RIVET_NAMESPACE: &str = "default";
pub const DEFAULT_RIVET_POOL: &str = "peri-swarm";

/// How this process relates to the engine process (M1).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RivetEngineSpawn {
    /// Start or reuse the local engine; only valid for a loopback endpoint.
    Always,
    /// Delegate start-or-reuse to RivetKit; only valid for a loopback endpoint.
    Auto,
    /// Never start an engine: the configured endpoint must already serve one.
    Never,
}

impl RivetEngineSpawn {
    fn parse(value: &str, variable: &'static str) -> Result<Self, ConfigError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "always" => Ok(Self::Always),
            "auto" => Ok(Self::Auto),
            "never" => Ok(Self::Never),
            _ => Err(ConfigError::Invalid {
                variable,
                reason: "expected always, auto or never".to_owned(),
            }),
        }
    }

    fn is_local(self) -> bool {
        matches!(self, Self::Always | Self::Auto)
    }

    /// Stable spelling for logs and errors (no secret involved).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::Auto => "auto",
            Self::Never => "never",
        }
    }

    #[must_use]
    pub fn rivetkit_mode(self) -> EngineSpawnMode {
        match self {
            Self::Always => EngineSpawnMode::Always,
            Self::Auto => EngineSpawnMode::Auto,
            Self::Never => EngineSpawnMode::Never,
        }
    }
}

/// Engine endpoint and deployment identity (M1).
///
/// The token **value** is deliberately absent: [`Self::token_env`] names the
/// environment variable and [`run`] resolves it once, so neither `Debug` nor a
/// startup log can render a credential.
#[derive(Clone)]
pub struct RivetConfig {
    pub endpoint: Url,
    pub token_env: Option<String>,
    pub namespace: String,
    pub pool: String,
    pub engine_spawn: RivetEngineSpawn,
}

impl RivetConfig {
    /// The endpoint in the spelling the engine client uses. `Url` always adds a
    /// trailing `/`; the previous constants had none, so the default must be
    /// byte-identical after stripping it.
    #[must_use]
    pub fn endpoint_string(&self) -> String {
        let raw = self.endpoint.as_str();
        raw.strip_suffix('/').unwrap_or(raw).to_owned()
    }

    /// Resolve the token value once. A named variable is re-checked at runtime
    /// so a value present at parse time but unset later fails visibly.
    fn resolve_token<F>(&self, lookup: F) -> Result<String, ConfigError>
    where
        F: Fn(&str) -> Option<String>,
    {
        match &self.token_env {
            Some(name) => match lookup(name) {
                Some(value) if !value.trim().is_empty() => Ok(value),
                Some(_) => Err(ConfigError::Invalid {
                    variable: RIVET_TOKEN_ENV,
                    reason: format!("{name} is set but empty"),
                }),
                None => Err(ConfigError::Invalid {
                    variable: RIVET_TOKEN_ENV,
                    reason: format!("{name} is not set"),
                }),
            },
            // Loopback only: the local engine's fixed development token.
            None => Ok(DEFAULT_RIVET_TOKEN.to_owned()),
        }
    }
}

impl std::fmt::Debug for RivetConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RivetConfig")
            .field("endpoint", &self.endpoint)
            .field("token_env", &self.token_env)
            .field("namespace", &self.namespace)
            .field("pool", &self.pool)
            .field("engine_spawn", &self.engine_spawn)
            .finish()
    }
}

/// One model per actor (`design §0.4-3`); the channel is the SSE fan-out bound.
const ACTOR_CHANNEL_CAPACITY: usize = 256;

/// How long shutdown waits for the gateway/registry tasks before aborting.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// Bound on the reachability probe for an engine this process must not spawn.
const RIVET_PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(5);

/// A parse or validation failure of the process environment.
#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    #[error("configuration error: {0}")]
    Config(#[from] ConfigError),
    #[error("invalid remote workspace environment: {0}")]
    Environment(String),
    /// The external engine is unreachable. Raised before either database is
    /// opened, so a failed start leaves no half-initialized process behind.
    #[error("external Rivet engine at {endpoint} is not reachable: {reason}")]
    RivetUnreachable { endpoint: String, reason: String },
    /// Opening the remote session store failed (credential, connection, schema).
    /// The inner text comes from `peri-resources` and contains no credential
    /// value.
    #[error("cannot open the remote Peri session store: {0}")]
    StoreOpen(String),
    /// Opening the independent Swarm state store failed. No fallback exists:
    /// the process does not run against the session store instead.
    #[error("cannot open the swarm state store: {0}")]
    StateStoreOpen(String),
    #[error("cannot bind the gateway on {addr}: {reason}")]
    Bind { addr: SocketAddr, reason: String },
    #[error("cannot wait for a shutdown signal: {0}")]
    Signal(String),
    #[error("swarm gateway failed: {0}")]
    Gateway(String),
    #[error("rivet registry failed: {0}")]
    Registry(String),
    #[error("cannot close the remote Peri session store: {0}")]
    StoreClose(String),
    #[error("cannot close the swarm state store: {0}")]
    StateStoreClose(String),
}

/// Validated process configuration. Every remote field is mandatory: a
/// partially configured entry is a startup error, not a local deployment.
#[derive(Clone, Debug)]
pub struct BootstrapConfig {
    pub bind_addr: SocketAddr,
    pub api_key: DeploymentApiKey,
    pub session_store_locator: PostgresLocator,
    pub state_store_locator: PostgresLocator,
    pub store_engine: String,
    /// Engine endpoint and deployment identity (M1). Holds the token variable
    /// name, never the token value.
    pub rivet: RivetConfig,
    pub machine_id: Uuid,
    pub workspace_root: PathBuf,
    pub model: Option<String>,
    pub settings_json: Option<String>,
    pub sse_heartbeat: Duration,
    /// Bun bridge base URL and outer token name (S8a3a). Not a credential by
    /// itself; the token is resolved into [`Self::sandbox`].
    pub bun: BunConfig,
    /// Configured Bun bridge. `None` means an `openai_hosted` request fails
    /// with a diagnosable "bridge not configured" error; `none` sessions are
    /// unaffected.
    pub sandbox: Option<SandboxClientConfig>,
}

impl BootstrapConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_env_map(|name| std::env::var(name).ok())
    }

    /// Parse from an arbitrary lookup so tests and `from_env` share one path.
    pub fn from_env_map<F>(lookup: F) -> Result<Self, ConfigError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let api_key = DeploymentApiKey::new(
            lookup(API_KEY_ENV).ok_or(ConfigError::MissingEnv(API_KEY_ENV))?,
        )?;

        let allow_non_loopback = parse_bind_allow_non_loopback(&lookup)?;
        let bind_value = lookup(BIND_ENV).unwrap_or_else(|| DEFAULT_BOOTSTRAP_BIND.to_owned());
        let bind_addr = parse_bind(&bind_value, BIND_ENV, allow_non_loopback)?;

        let locator_raw =
            lookup(SESSION_STORE_ENV).ok_or(ConfigError::MissingEnv(SESSION_STORE_ENV))?;
        let session_store_locator = PostgresLocator::parse(&locator_raw, SESSION_STORE_ENV)?;

        // The Swarm facts have their own database (S6c1a). Both DSNs are
        // required: a missing state store is a startup error, and pointing both
        // at one database would put the Swarm schema back into the session
        // store, which is exactly what the split avoids. The same PostgreSQL
        // instance is fine; only the database must differ.
        let state_locator_raw =
            lookup(STATE_STORE_ENV).ok_or(ConfigError::MissingEnv(STATE_STORE_ENV))?;
        let state_store_locator = validate_state_store_locator(
            &state_locator_raw,
            STATE_STORE_ENV,
            &session_store_locator,
        )?;
        let store_engine = optional_non_empty(lookup(STORE_ENGINE_ENV), STORE_ENGINE_ENV)?
            .unwrap_or_else(|| DEFAULT_STORE_ENGINE.to_owned());
        if !matches!(store_engine.as_str(), "postgres" | "postgresql") {
            return Err(ConfigError::Invalid {
                variable: STORE_ENGINE_ENV,
                reason: "expected postgres or postgresql".to_owned(),
            });
        }

        let rivet = rivet_config(&lookup)?;

        let machine_id_raw =
            lookup(MACHINE_ID_ENV).ok_or(ConfigError::MissingEnv(MACHINE_ID_ENV))?;
        let machine_id = Uuid::parse_str(&machine_id_raw).map_err(|_| ConfigError::Invalid {
            variable: MACHINE_ID_ENV,
            reason: "expected a UUID".to_owned(),
        })?;

        // The root rule lives in one place: `RemoteWorkspaceEnvironment` rejects
        // relative and non-normalized roots before any store I/O.
        let workspace_value =
            lookup(WORKSPACE_ROOT_ENV).ok_or(ConfigError::MissingEnv(WORKSPACE_ROOT_ENV))?;
        let workspace_root = PathBuf::from(workspace_value);
        RemoteWorkspaceEnvironment::virtual_workspace(
            &machine_id.to_string(),
            workspace_root.clone(),
        )
        .map_err(|error| ConfigError::Invalid {
            variable: WORKSPACE_ROOT_ENV,
            reason: error.to_string(),
        })?;

        let model = optional_non_empty(lookup(MODEL_ENV), MODEL_ENV)?;

        let settings_json = match lookup(SETTINGS_JSON_ENV) {
            Some(value) => {
                serde_json::from_str::<serde_json::Value>(&value).map_err(|_| {
                    ConfigError::Invalid {
                        variable: SETTINGS_JSON_ENV,
                        reason: "expected a JSON document".to_owned(),
                    }
                })?;
                Some(value)
            }
            None => None,
        };

        // S8a3a: the Bun sandbox bridge is optional. A hosted environment is
        // rejected at session creation when it is missing, so a `none`
        // deployment keeps working unchanged.
        let (bun_base_url_variable, bun_base_url_value) = match lookup(BUN_BASE_URL_ENV) {
            Some(value) => (BUN_BASE_URL_ENV, Some(value)),
            None => (BUN_URL_ALIAS_ENV, lookup(BUN_URL_ALIAS_ENV)),
        };
        let bun_base_url = match bun_base_url_value {
            Some(value) => Some(validate_bun_url(&value, bun_base_url_variable)?),
            None => None,
        };
        let bun_bearer_env =
            crate::config::optional_env_name(lookup(BUN_BEARER_ENV), BUN_BEARER_ENV)?;
        let direct_bun_token = optional_non_empty(lookup(BUN_TOKEN_ENV), BUN_TOKEN_ENV)?;
        if direct_bun_token.is_some() && bun_bearer_env.is_some() {
            return Err(ConfigError::Invalid {
                variable: BUN_TOKEN_ENV,
                reason: format!("must not be combined with {BUN_BEARER_ENV}"),
            });
        }
        let bun_token = match (direct_bun_token, bun_bearer_env.as_deref()) {
            (Some(value), _) => {
                validate_bun_token(&value, BUN_TOKEN_ENV)?;
                Some(value)
            }
            (None, Some(name)) => match lookup(name) {
                Some(value) => {
                    validate_bun_token(&value, BUN_BEARER_ENV)?;
                    Some(value)
                }
                None => {
                    return Err(ConfigError::Invalid {
                        variable: BUN_BEARER_ENV,
                        reason: format!("{name} is not set"),
                    });
                }
            },
            (None, None) => None,
        };
        if bun_token.is_some() && bun_base_url.is_none() {
            return Err(ConfigError::Invalid {
                variable: BUN_TOKEN_ENV,
                reason: format!("requires {BUN_BASE_URL_ENV}"),
            });
        }
        let sandbox = bun_base_url.clone().map(|base_url| SandboxClientConfig {
            base_url,
            token: bun_token,
        });

        Ok(Self {
            bind_addr,
            api_key,
            session_store_locator,
            state_store_locator,
            store_engine,
            rivet,
            machine_id,
            workspace_root,
            model,
            settings_json,
            sse_heartbeat: parse_sse_heartbeat(&lookup)?,
            bun: BunConfig {
                base_url: bun_base_url,
                bearer_env: bun_bearer_env,
            },
            sandbox,
        })
    }

    fn session_store_deployment(&self) -> SessionStoreDeployment {
        SessionStoreDeployment::from_locator(self.session_store_locator.as_str())
            .with_engine(self.store_engine.clone())
    }

    fn state_store_config(&self) -> SwarmStateStoreConfig {
        SwarmStateStoreConfig {
            locator: self.state_store_locator.clone(),
        }
    }

    fn actor_config(&self) -> ActorServiceConfig {
        ActorServiceConfig {
            settings_json: self.settings_json.clone(),
            session_store_path: None,
            session_store: Some(self.session_store_deployment()),
            machine_id: Some(self.machine_id),
            workspace_root: Some(self.workspace_root.clone()),
            model: self.model.clone(),
            channel_capacity: ACTOR_CHANNEL_CAPACITY,
            sandbox: self
                .sandbox
                .clone()
                .map(crate::sandbox::BunSandboxClient::new),
        }
    }

    fn swarm_config(&self) -> SwarmConfig {
        SwarmConfig {
            bind_addr: self.bind_addr,
            api_key: self.api_key.clone(),
            workspace_root: self.workspace_root.clone(),
            session_store: SessionStoreConfig {
                locator: Some(self.session_store_locator.clone()),
                engine: self.store_engine.clone(),
                credential_env: None,
                access: StoreAccess::ReadWrite,
            },
            bun: self.bun.clone(),
            model: ModelConfig {
                default_model: self.model.clone(),
                credential_env: None,
            },
            sse_heartbeat: self.sse_heartbeat,
        }
    }
}

/// Parse the engine surface and cross-check it against the endpoint shape.
///
/// Contradictions fail here, at configuration time, instead of surfacing as a
/// confusing connection error later:
///
/// - a non-loopback endpoint must use `never` (this process cannot spawn a
///   local engine for a remote address) and must name its token variable;
/// - the token variable must be a variable name and hold a non-empty value;
/// - the loopback default reuses the local engine's fixed development token.
fn rivet_config<F>(lookup: &F) -> Result<RivetConfig, ConfigError>
where
    F: Fn(&str) -> Option<String>,
{
    let endpoint_value =
        lookup(RIVET_ENDPOINT_ENV).unwrap_or_else(|| DEFAULT_RIVET_ENDPOINT.to_owned());
    let endpoint = validate_rivet_endpoint(&endpoint_value, RIVET_ENDPOINT_ENV)?;
    let loopback = is_loopback_endpoint(&endpoint);

    let engine_spawn = match lookup(RIVET_ENGINE_SPAWN_ENV) {
        Some(value) => RivetEngineSpawn::parse(&value, RIVET_ENGINE_SPAWN_ENV)?,
        None => RivetEngineSpawn::Always,
    };
    if !loopback && engine_spawn.is_local() {
        return Err(ConfigError::Invalid {
            variable: RIVET_ENGINE_SPAWN_ENV,
            reason: format!(
                "must be `never` for the non-loopback engine endpoint {endpoint}; a local \
                 engine is only spawned for a loopback endpoint"
            ),
        });
    }

    let token_env = match lookup(RIVET_TOKEN_ENV) {
        Some(name) => {
            if !is_env_var_name(&name) {
                return Err(ConfigError::Invalid {
                    variable: RIVET_TOKEN_ENV,
                    reason: "expected an environment variable name, not a value".to_owned(),
                });
            }
            match lookup(&name) {
                Some(value) if !value.trim().is_empty() => {}
                Some(_) => {
                    return Err(ConfigError::Invalid {
                        variable: RIVET_TOKEN_ENV,
                        reason: format!("{name} is set but empty"),
                    });
                }
                None => {
                    return Err(ConfigError::Invalid {
                        variable: RIVET_TOKEN_ENV,
                        reason: format!("{name} is not set"),
                    });
                }
            }
            Some(name)
        }
        None => {
            if !loopback {
                return Err(ConfigError::Invalid {
                    variable: RIVET_TOKEN_ENV,
                    reason: format!(
                        "required for the non-loopback engine endpoint {endpoint}; name the \
                         variable that holds the engine token"
                    ),
                });
            }
            None
        }
    };

    let namespace = validate_rivet_identity(
        lookup(RIVET_NAMESPACE_ENV).unwrap_or_else(|| DEFAULT_RIVET_NAMESPACE.to_owned()),
        RIVET_NAMESPACE_ENV,
    )?;
    let pool = validate_rivet_identity(
        lookup(RIVET_POOL_ENV).unwrap_or_else(|| DEFAULT_RIVET_POOL.to_owned()),
        RIVET_POOL_ENV,
    )?;

    Ok(RivetConfig {
        endpoint,
        token_env,
        namespace,
        pool,
        engine_spawn,
    })
}

/// Run the process until shutdown. Errors are startup/runtime failures of this
/// process; none of them is repaired by opening a local store.
pub async fn run(config: BootstrapConfig) -> Result<(), BootstrapError> {
    run_with_telemetry(config, None).await
}

/// Run the process and, when configured, observe its HTTP gateway.
///
/// The telemetry handle is the one `telemetry::init` produced; `None` keeps the
/// disabled path (no middleware, no exporters). The guard itself is owned by
/// `main`, not here: its flush must happen after this function returns and the
/// stores are closed.
pub async fn run_with_telemetry(
    config: BootstrapConfig,
    telemetry: Option<crate::telemetry::HttpTelemetry>,
) -> Result<(), BootstrapError> {
    // The bind opt-in is a deployment decision, not a per-request one: say it
    // once at startup, with the two facts that constrain it.
    if !config.bind_addr.ip().is_loopback() {
        tracing::warn!(
            bind = %config.bind_addr,
            "gateway binds a non-loopback address: the deployment Bearer token is the only \
             authentication and TLS must be terminated by the trusted ingress in front of \
             this process"
        );
    }

    // Resolve the engine token once, before any database connection exists. Only
    // the endpoint/namespace/pool/spawn are ever logged; the token is not.
    let rivet_token = config
        .rivet
        .resolve_token(|name| std::env::var(name).ok())?;
    let rivet_endpoint = config.rivet.endpoint_string();
    tracing::info!(
        endpoint = %rivet_endpoint,
        namespace = %config.rivet.namespace,
        pool = %config.rivet.pool,
        engine_spawn = config.rivet.engine_spawn.as_str(),
        "rivet engine configuration"
    );
    // For an external engine, fail before opening either database: no local
    // engine is started and a half-initialized process must not remain.
    preflight_external_engine(&config.rivet).await?;

    let environment = RemoteWorkspaceEnvironment::virtual_workspace(
        &config.machine_id.to_string(),
        config.workspace_root.clone(),
    )
    .map_err(|error| BootstrapError::Environment(error.to_string()))?;

    tracing::info!(
        bind = %config.bind_addr,
        engine = %config.store_engine,
        machine_id = %config.machine_id,
        "opening the remote Peri session store"
    );
    // One deployment-wide session-store facade serves every ACP child and the
    // cold-actor `sessions.subagents` read. Connection
    // budget with PostgreSQL's default max_connections=100:
    //   2 replicas × (32 session + 8 state) + 3 reserved = 83 ≤ 100.
    // 32 is a cap, not a reservation (`min_connections(0)`): an idle pool
    // returns its connections. The single-facade default stays 2.
    let resources = Resources::open_deployment_in_remote_environment_with_pool(
        &config.session_store_deployment(),
        environment,
        SessionStorePoolPolicy::new(32),
    )
    .await
    .map_err(|error| BootstrapError::StoreOpen(format!("{error:#}")))?;

    // The process keeps the close right for both stores and hands the session
    // store's business handle to the deployment: every ACP child clones it
    // instead of opening its own facade and PostgreSQL pool.
    let (session_resources, store_shutdown) = resources.into_parts();

    // S6c1a: the Swarm facts live in their own database. Failing to open it
    // closes the session store that was already opened and stops the process;
    // it never reuses the session store's connection. Only the DSN target is
    // logged, never the DSN itself.
    tracing::info!(
        state_store = %config.state_store_locator.target(),
        "opening the independent swarm state store"
    );
    let state_store = match SwarmStateStore::open(&config.state_store_config()).await {
        Ok(store) => Arc::new(store),
        Err(error) => {
            if let Err(close_error) = store_shutdown.shutdown().await {
                tracing::warn!(
                    error = %close_error,
                    "remote Peri session store close failed after the state store failed to open"
                );
            }
            return Err(BootstrapError::StateStoreOpen(error.to_string()));
        }
    };

    let store: Arc<dyn SwarmStore> = state_store.clone();

    if crate::actor::deployment::current_deployment().is_some() {
        tracing::debug!("a swarm deployment was already installed; the first install wins");
    }
    let deployment = install_deployment(StaticDeployment::new_with_session_resources(
        store,
        config.actor_config(),
        Some(Arc::clone(&session_resources)),
    ));
    let store = deployment.store();

    let client = Client::new(
        ClientConfig::new(rivet_endpoint.clone())
            .token(rivet_token.clone())
            .namespace(config.rivet.namespace.clone())
            .pool_name(config.rivet.pool.clone()),
    );
    let service = Arc::new(RivetSwarmService::new(
        client,
        store,
        RivetServiceConfig::default(),
    ));
    let router = build_router(
        AppState::new(service, Arc::new(config.swarm_config())).with_optional_telemetry(telemetry),
    );

    // Bind before the registry starts so an address conflict fails fast and
    // does not leave an engine-backed registry behind.
    let listener =
        TcpListener::bind(config.bind_addr)
            .await
            .map_err(|error| BootstrapError::Bind {
                addr: config.bind_addr,
                reason: error.to_string(),
            })?;
    let bound = listener
        .local_addr()
        .map_err(|error| BootstrapError::Bind {
            addr: config.bind_addr,
            reason: error.to_string(),
        })?;

    let shutdown = CancellationToken::new();
    let mut http_task = {
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async move { shutdown.cancelled().await })
                .await
                .map_err(|error| error.to_string())
        })
    };
    let mut registry_task = {
        let shutdown = shutdown.clone();
        let registry = swarm::registry();
        let serve_config = ServeConfig {
            endpoint: rivet_endpoint.clone(),
            token: Some(rivet_token.clone()),
            namespace: config.rivet.namespace.clone(),
            pool_name: config.rivet.pool.clone(),
            engine_spawn: config.rivet.engine_spawn.rivetkit_mode(),
            ..ServeConfig::default()
        };
        tokio::spawn(async move {
            registry
                .serve_with_config(serve_config, shutdown)
                .await
                .map_err(|error| error.to_string())
        })
    };

    tracing::info!(
        addr = %bound,
        rivet = %rivet_endpoint,
        namespace = %config.rivet.namespace,
        pool = %config.rivet.pool,
        engine_spawn = config.rivet.engine_spawn.as_str(),
        "peri-swarm gateway is listening"
    );

    let mut http_done = false;
    let mut registry_done = false;
    let outcome = tokio::select! {
        signal = tokio::signal::ctrl_c() => match signal {
            Ok(()) => {
                tracing::info!("shutdown signal received");
                Ok(())
            }
            Err(error) => Err(BootstrapError::Signal(error.to_string())),
        },
        result = &mut http_task => {
            http_done = true;
            Err(BootstrapError::Gateway(task_end("swarm gateway", result)))
        }
        result = &mut registry_task => {
            registry_done = true;
            Err(BootstrapError::Registry(task_end("rivet registry", result)))
        }
    };

    shutdown.cancel();
    if !http_done {
        drain("swarm gateway", &mut http_task).await;
    }
    if !registry_done {
        drain("rivet registry", &mut registry_task).await;
    }

    // Close both stores even when the first one fails, and only report a close
    // failure when nothing else already failed.
    let mut close_failure = None;
    match store_shutdown.shutdown().await {
        Ok(()) => tracing::info!("remote Peri session store closed"),
        Err(error) => {
            tracing::error!(error = %error, "remote Peri session store close failed");
            if outcome.is_ok() {
                close_failure = Some(BootstrapError::StoreClose(error.to_string()));
            }
        }
    }
    match state_store.close().await {
        Ok(()) => tracing::info!("swarm state store closed"),
        Err(error) => {
            tracing::error!(error = %error, "swarm state store close failed");
            if outcome.is_ok() && close_failure.is_none() {
                close_failure = Some(BootstrapError::StateStoreClose(error.to_string()));
            }
        }
    }
    match close_failure {
        Some(error) => Err(error),
        None => outcome,
    }
}

/// A bounded TCP probe of an engine this process must not spawn.
///
/// It only proves that a listener exists at the configured address; the real
/// handshake still happens in the registry task and reports its own failure.
/// The probe runs before either database is opened so an unreachable
/// engine stops the process without a half-initialized deployment.
async fn preflight_external_engine(rivet: &RivetConfig) -> Result<(), BootstrapError> {
    if rivet.engine_spawn.is_local() {
        return Ok(());
    }
    let endpoint = rivet.endpoint_string();
    let unreachable = |reason: String| BootstrapError::RivetUnreachable {
        endpoint: endpoint.clone(),
        reason: format!(
            "{reason} (this process does not spawn a local engine; \
             {RIVET_ENGINE_SPAWN_ENV}=never)"
        ),
    };
    let host = rivet
        .endpoint
        .host_str()
        .ok_or_else(|| unreachable("the endpoint has no host".to_owned()))?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned();
    let port = rivet
        .endpoint
        .port_or_known_default()
        .ok_or_else(|| unreachable("the endpoint has no port".to_owned()))?;

    match tokio::time::timeout(
        RIVET_PREFLIGHT_TIMEOUT,
        tokio::net::TcpStream::connect((host.as_str(), port)),
    )
    .await
    {
        Ok(Ok(_stream)) => {
            tracing::info!(endpoint = %endpoint, "external rivet engine accepted a TCP probe");
            Ok(())
        }
        Ok(Err(error)) => Err(unreachable(format!("TCP connect failed: {error}"))),
        Err(_) => Err(unreachable(format!(
            "TCP connect timed out after {} seconds",
            RIVET_PREFLIGHT_TIMEOUT.as_secs()
        ))),
    }
}

/// The Bun service rejects empty or whitespace-bearing tokens; fail before the
/// first session instead of discovering it on the wire.
fn validate_bun_token(value: &str, variable: &'static str) -> Result<(), ConfigError> {
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        return Err(ConfigError::Invalid {
            variable,
            reason: "must be non-empty and contain no whitespace".to_owned(),
        });
    }
    Ok(())
}

/// Flatten one finished server task into a diagnosable reason.
fn task_end(
    component: &'static str,
    result: Result<Result<(), String>, tokio::task::JoinError>,
) -> String {
    match result {
        Ok(Ok(())) => format!("{component} stopped without an error"),
        Ok(Err(error)) => error,
        Err(error) => format!("{component} task failed: {error}"),
    }
}

/// Wait for a cancelled task, aborting it when the grace period expires.
async fn drain(component: &'static str, task: &mut JoinHandle<Result<(), String>>) {
    match tokio::time::timeout(SHUTDOWN_GRACE, &mut *task).await {
        Ok(Ok(Ok(()))) => tracing::debug!(component, "task stopped"),
        Ok(Ok(Err(error))) => tracing::warn!(component, error, "task stopped with an error"),
        Ok(Err(error)) => tracing::warn!(component, error = %error, "task join failed"),
        Err(_) => {
            tracing::warn!(
                component,
                "task did not stop within the grace period; aborting"
            );
            task.abort();
        }
    }
}

#[cfg(test)]
#[path = "bootstrap_test.rs"]
mod tests;
