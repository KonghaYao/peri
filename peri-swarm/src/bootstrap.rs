//! Process entry for the Swarm gateway (S6b/S6c1a).
//!
//! One process owns **two** Loom databases:
//!
//! 1. the **remote Peri session store** (`design §4.1`): opened once with the
//!    deployment's locator and a virtual workspace identity. Opening also runs
//!    the Peri schema check/upgrade (`design §4.2`); an unknown or newer store
//!    fails closed. There is no local SQLite fallback on this entry.
//! 2. the **Swarm state store** (S6c1a): `PERI_SWARM_STATE_STORE` is a separate
//!    logical database opened by [`SwarmStateStore`] with its own `swarm_meta`
//!    schema version. Both locators are required and must differ; a failure to
//!    open either one stops the process instead of reusing the other.
//!
//! The installed [`StaticDeployment`] carries the Swarm state store, so the
//! gateway and every actor read and write Swarm facts there. Actor children
//! still open their own Peri session store from
//! [`SessionStoreDeployment`](crate::actor::runtime::SessionStoreDeployment).
//!
//! 3. the **Rivet registry** that serves [`crate::actor::SwarmActor`] and the
//!    **axum gateway** ([`crate::http::build_router`]). Both run on one shared
//!    [`CancellationToken`]; either failing stops the process with a
//!    diagnosable error.
//!
//! This stage does not require a contactable Loom or a real model: the entry
//! must fail visibly when either remote configuration is missing, and every
//! actor child opens its own remote session store from the same deployment.
//!
//! Environment (secrets are only read by name, never rendered):
//!
//! | variable | meaning |
//! | --- | --- |
//! | `PERI_SWARM_BIND` | gateway address, default `127.0.0.1:8080`, loopback only |
//! | `PERI_SWARM_API_KEY` | deployment bearer key (required) |
//! | `PERI_SWARM_SESSION_STORE` | Peri session store locator `http(s)://…` (required) |
//! | `PERI_SWARM_STATE_STORE` | Swarm state store locator `http(s)://…` (required, distinct) |
//! | `PERI_SWARM_STORE_ENGINE` | remote engine name, default `turso` |
//! | `PERI_SWARM_STORE_TOKEN_ENV` | **name** of the variable holding the store token (both databases) |
//! | `PERI_SWARM_MACHINE_ID` | stable machine UUID (required) |
//! | `PERI_SWARM_WORKSPACE_ROOT` | absolute, normalized virtual workspace root |
//! | `PERI_SWARM_MODEL` | optional deployment model |
//! | `PERI_SWARM_SETTINGS_JSON` | optional trusted settings document (JSON) |

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use peri_acp_types::session_resources::SessionStoreShutdownPort;
use peri_resources::{RemoteWorkspaceEnvironment, Resources};
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
    is_env_var_name, optional_non_empty, validate_store_locator, BunConfig, ConfigError,
    DeploymentApiKey, ModelConfig, SessionStoreConfig, StoreAccess, SwarmConfig, API_KEY_ENV,
    BIND_ENV, DEFAULT_SSE_HEARTBEAT_MS, MODEL_ENV, WORKSPACE_ROOT_ENV,
};
use crate::gateway::{RivetServiceConfig, RivetSwarmService};
use crate::http::{build_router, AppState};
use crate::store::{SwarmStateStore, SwarmStateStoreConfig, SwarmStore};

/// Remote Loom locator for the Peri session store
/// (`http://127.0.0.1:<port>/db/<tenant>/<database>/<branch>/`).
pub const SESSION_STORE_ENV: &str = "PERI_SWARM_SESSION_STORE";
/// Remote Loom locator for the **Swarm state** database (S6c1a).
///
/// A separate logical database: it must not be the session-store locator, so a
/// Swarm schema change can never touch Peri's session schema.
pub const STATE_STORE_ENV: &str = "PERI_SWARM_STATE_STORE";
/// Remote engine name; only needed when the locator cannot decide it.
pub const STORE_ENGINE_ENV: &str = "PERI_SWARM_STORE_ENGINE";
/// **Name** of the environment variable that holds the store credential.
pub const STORE_TOKEN_ENV: &str = "PERI_SWARM_STORE_TOKEN_ENV";
/// Stable machine UUID persisted for the virtual workspace identity.
pub const MACHINE_ID_ENV: &str = "PERI_SWARM_MACHINE_ID";
/// Optional trusted settings document handed to every child host.
pub const SETTINGS_JSON_ENV: &str = "PERI_SWARM_SETTINGS_JSON";

pub const DEFAULT_BOOTSTRAP_BIND: &str = "127.0.0.1:8080";
pub const DEFAULT_STORE_ENGINE: &str = "turso";

/// Rivet endpoint/identity verified in `.peri/plans/S5-report.md` §2. A local
/// engine is spawned on demand; the pool name is the deployment identity, so
/// two processes serving it share actor state through the engine.
const RIVET_ENDPOINT: &str = "http://127.0.0.1:6420";
const RIVET_TOKEN: &str = "default";
const RIVET_NAMESPACE: &str = "default";
const RIVET_POOL: &str = "peri-swarm";

/// One model per actor (`design §0.4-3`); the channel is the SSE fan-out bound.
const ACTOR_CHANNEL_CAPACITY: usize = 256;

/// How long shutdown waits for the gateway/registry tasks before aborting.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// A parse or validation failure of the process environment.
#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    #[error("configuration error: {0}")]
    Config(#[from] ConfigError),
    #[error("invalid remote workspace environment: {0}")]
    Environment(String),
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
    pub session_store_locator: Url,
    pub state_store_locator: Url,
    pub store_engine: String,
    pub store_token_env: String,
    pub machine_id: Uuid,
    pub workspace_root: PathBuf,
    pub model: Option<String>,
    pub settings_json: Option<String>,
    pub sse_heartbeat: Duration,
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

        let bind_value = lookup(BIND_ENV).unwrap_or_else(|| DEFAULT_BOOTSTRAP_BIND.to_owned());
        let bind_addr: SocketAddr = bind_value.parse().map_err(|_| ConfigError::Invalid {
            variable: BIND_ENV,
            reason: "expected host:port".to_owned(),
        })?;
        if !bind_addr.ip().is_loopback() {
            return Err(ConfigError::Invalid {
                variable: BIND_ENV,
                reason: "must bind a loopback IP".to_owned(),
            });
        }

        let locator_raw =
            lookup(SESSION_STORE_ENV).ok_or(ConfigError::MissingEnv(SESSION_STORE_ENV))?;
        let session_store_locator = validate_store_locator(&locator_raw, SESSION_STORE_ENV)?;

        // The Swarm facts have their own database (S6c1a). Both locators are
        // required: a missing state store is a startup error, and pointing both
        // at one database would put the Swarm schema back into the session
        // store, which is exactly what the split avoids.
        let state_locator_raw =
            lookup(STATE_STORE_ENV).ok_or(ConfigError::MissingEnv(STATE_STORE_ENV))?;
        let state_store_locator = validate_store_locator(&state_locator_raw, STATE_STORE_ENV)?;
        if same_store_locator(&state_store_locator, &session_store_locator) {
            return Err(ConfigError::Invalid {
                variable: STATE_STORE_ENV,
                reason: format!("must name a database separate from {SESSION_STORE_ENV}"),
            });
        }

        let store_engine = optional_non_empty(lookup(STORE_ENGINE_ENV), STORE_ENGINE_ENV)?
            .unwrap_or_else(|| DEFAULT_STORE_ENGINE.to_owned());

        // The token is a source, never a value: validate the name, then require
        // the named variable to be present and non-empty without keeping it.
        let store_token_env =
            lookup(STORE_TOKEN_ENV).ok_or(ConfigError::MissingEnv(STORE_TOKEN_ENV))?;
        if !is_env_var_name(&store_token_env) {
            return Err(ConfigError::Invalid {
                variable: STORE_TOKEN_ENV,
                reason: "expected an environment variable name, not a value".to_owned(),
            });
        }
        match lookup(&store_token_env) {
            Some(value) if !value.trim().is_empty() => {}
            Some(_) => {
                return Err(ConfigError::Invalid {
                    variable: STORE_TOKEN_ENV,
                    reason: format!("{store_token_env} is set but empty"),
                });
            }
            None => {
                return Err(ConfigError::Invalid {
                    variable: STORE_TOKEN_ENV,
                    reason: format!("{store_token_env} is not set"),
                });
            }
        }

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

        Ok(Self {
            bind_addr,
            api_key,
            session_store_locator,
            state_store_locator,
            store_engine,
            store_token_env,
            machine_id,
            workspace_root,
            model,
            settings_json,
            sse_heartbeat: Duration::from_millis(DEFAULT_SSE_HEARTBEAT_MS),
        })
    }

    fn session_store_deployment(&self) -> SessionStoreDeployment {
        SessionStoreDeployment::from_locator(self.session_store_locator.as_str())
            .with_engine(self.store_engine.clone())
            .with_credential_env(self.store_token_env.clone())
    }

    fn state_store_config(&self) -> SwarmStateStoreConfig {
        SwarmStateStoreConfig {
            locator: self.state_store_locator.clone(),
            credential_env: self.store_token_env.clone(),
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
                credential_env: Some(self.store_token_env.clone()),
                access: StoreAccess::ReadWrite,
            },
            bun: BunConfig {
                base_url: None,
                bearer_env: None,
            },
            model: ModelConfig {
                default_model: self.model.clone(),
                credential_env: None,
            },
            sse_heartbeat: self.sse_heartbeat,
        }
    }
}

/// Run the process until shutdown. Errors are startup/runtime failures of this
/// process; none of them is repaired by opening a local store.
pub async fn run(config: BootstrapConfig) -> Result<(), BootstrapError> {
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
    let resources = Resources::open_deployment_in_remote_environment(
        &config.session_store_deployment(),
        environment,
    )
    .await
    .map_err(|error| BootstrapError::StoreOpen(format!("{error:#}")))?;

    // S6c1a: the Swarm facts live in their own database. Failing to open it
    // closes the session store that was already opened and stops the process;
    // it never reuses the session store's connection.
    tracing::info!(
        state_store = %config.state_store_locator,
        "opening the independent swarm state store"
    );
    let state_store = match SwarmStateStore::open(&config.state_store_config()).await {
        Ok(store) => Arc::new(store),
        Err(error) => {
            let (_session_resources, store_shutdown) = resources.into_parts();
            if let Err(close_error) = store_shutdown.shutdown().await {
                tracing::warn!(
                    error = %close_error,
                    "remote Peri session store close failed after the state store failed to open"
                );
            }
            return Err(BootstrapError::StateStoreOpen(error.to_string()));
        }
    };

    // The process keeps the close right for both stores; actors open their own
    // session store from the deployment.
    let (_session_resources, store_shutdown) = resources.into_parts();
    let store: Arc<dyn SwarmStore> = state_store.clone();

    if crate::actor::deployment::current_deployment().is_some() {
        tracing::debug!("a swarm deployment was already installed; the first install wins");
    }
    let deployment = install_deployment(StaticDeployment::new(store, config.actor_config()));
    let store = deployment.store();

    let client = Client::new(
        ClientConfig::new(RIVET_ENDPOINT)
            .token(RIVET_TOKEN)
            .namespace(RIVET_NAMESPACE)
            .pool_name(RIVET_POOL),
    );
    let service = Arc::new(RivetSwarmService::new(
        client,
        store,
        RivetServiceConfig::default(),
    ));
    let router = build_router(AppState::new(service, Arc::new(config.swarm_config())));

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
            endpoint: RIVET_ENDPOINT.to_owned(),
            token: Some(RIVET_TOKEN.to_owned()),
            namespace: RIVET_NAMESPACE.to_owned(),
            pool_name: RIVET_POOL.to_owned(),
            engine_spawn: EngineSpawnMode::Always,
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
        rivet = RIVET_ENDPOINT,
        pool = RIVET_POOL,
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

/// Two locators name one database when scheme, host, port and path agree.
/// The trailing slash is not part of the identity (`…/main/` == `…/main`).
fn same_store_locator(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
        && left.path().trim_end_matches('/') == right.path().trim_end_matches('/')
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
