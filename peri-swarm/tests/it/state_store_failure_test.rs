//! Failure paths of the two-database deployment: the state DSN is mandatory,
//! must name a different database, must be reachable, and no path may fall back
//! to a local session store.

use std::collections::BTreeMap;
use std::sync::Arc;

use tokio::sync::broadcast;

use peri_resources::Resources;
use peri_swarm::actor::runtime::{AcpChild, ChildConfig, ChildError, SessionStoreDeployment};
use peri_swarm::actor::ItemLog;
use peri_swarm::bootstrap::{BootstrapConfig, MACHINE_ID_ENV, SESSION_STORE_ENV, STATE_STORE_ENV};
use peri_swarm::config::{ConfigError, PostgresLocator, API_KEY_ENV, WORKSPACE_ROOT_ENV};
use peri_swarm::domain::LogicalId;
use peri_swarm::store::{SwarmStateError, SwarmStateStore, SwarmStateStoreConfig};

use crate::state_store_support::{
    clear_telemetry_env, environment, machine_id, required_env, settings_json, workspace,
    MACHINE_ID, STATE_LOCATOR_ENV,
};

// ── 2. the state locator is mandatory, distinct and actionable ──────────────

/// A missing, shared or unreachable state database must fail closed: the
/// process never substitutes the session database for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires two PostgreSQL databases"]
async fn missing_shared_or_unreachable_state_store_fails_closed() {
    clear_telemetry_env();
    let Some((session_store_dsn, state_store_dsn)) = required_env() else {
        return;
    };
    let (_workspace_temp, root, _cwd) = workspace();

    let mut configuration: BTreeMap<&str, String> = BTreeMap::new();
    configuration.insert(
        API_KEY_ENV,
        "test-deployment-key-not-a-credential".to_owned(),
    );
    configuration.insert(SESSION_STORE_ENV, session_store_dsn.clone());
    configuration.insert(STATE_STORE_ENV, state_store_dsn.clone());
    configuration.insert(MACHINE_ID_ENV, MACHINE_ID.to_owned());
    configuration.insert(WORKSPACE_ROOT_ENV, root.to_string_lossy().into_owned());

    // Missing: a deployment with only the session DSN must not parse.
    let missing = BootstrapConfig::from_env_map(|name| {
        if name == STATE_STORE_ENV {
            None
        } else {
            configuration.get(name).cloned()
        }
    })
    .expect_err("a deployment without a state store must not parse");
    assert!(
        matches!(missing, ConfigError::MissingEnv(STATE_STORE_ENV)),
        "missing state DSN: {missing:?}"
    );
    eprintln!("state store: missing {STATE_STORE_ENV} -> {missing}");

    // Shared: the same database for both would put the Swarm schema back into
    // the session database, so it is rejected as a configuration error.
    let shared = BootstrapConfig::from_env_map(|name| {
        if name == STATE_STORE_ENV {
            Some(session_store_dsn.clone())
        } else {
            configuration.get(name).cloned()
        }
    })
    .expect_err("a shared database must not parse");
    assert!(
        matches!(&shared, ConfigError::Invalid { variable, .. } if *variable == STATE_STORE_ENV),
        "shared DSN: {shared:?}"
    );
    assert!(
        shared.to_string().contains("dedicated PostgreSQL database"),
        "the error must be actionable: {shared}"
    );
    eprintln!("state store: shared DSN -> {shared}");

    // A Loom-era locator is rejected with a migration hint.
    let legacy = BootstrapConfig::from_env_map(|name| {
        if name == STATE_STORE_ENV {
            Some("http://127.0.0.1:9497/db/local_default/swarm/main/".to_owned())
        } else {
            configuration.get(name).cloned()
        }
    })
    .expect_err("a Loom locator must not parse");
    assert!(
        legacy.to_string().contains("no longer supported"),
        "legacy locator: {legacy}"
    );
    eprintln!("state store: legacy locator -> {legacy}");

    // Unreachable: a closed loopback port fails at open with the address in the
    // message and the DSN password nowhere in it.
    let unreachable = SwarmStateStoreConfig {
        locator: PostgresLocator::parse(
            "postgresql://peri:super-secret-password@127.0.0.1:9/swarm_state",
            STATE_LOCATOR_ENV,
        )
        .expect("unreachable state DSN"),
    };
    let error = SwarmStateStore::open(&unreachable)
        .await
        .expect_err("an unreachable state database must fail");
    let message = error.to_string();
    assert!(
        matches!(
            error,
            SwarmStateError::Connect(_) | SwarmStateError::Schema(_)
        ),
        "state store failure class: {error:?}"
    );
    assert!(
        message.contains("127.0.0.1:9/swarm_state"),
        "the failure names the state store target: {message}"
    );
    assert!(
        !message.contains("super-secret-password"),
        "the failure must not echo the DSN password: {message}"
    );
    eprintln!("state store: unreachable state store -> {message}");
}

// ── 3. connection failure is visible and never falls back locally ──────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the remote store integration environment"]
async fn unreachable_session_store_fails_without_creating_a_local_store() {
    clear_telemetry_env();
    let Some((_session_store_dsn, _state_store_dsn)) = required_env() else {
        return;
    };

    let (_workspace_temp, root, cwd) = workspace();
    let machine = machine_id();
    let env = environment(&machine, &root);

    // Closed loopback port: a connection failure, not a locator shape rejection
    // (that path never reaches I/O).
    let unreachable = SessionStoreDeployment::from_locator("postgresql://localhost:9/peri");

    let error = Resources::open_deployment_in_remote_environment(&unreachable, env.clone())
        .await
        .expect_err("an unreachable PostgreSQL server must fail the deployment open");
    let message = format!("{error:#}");
    assert!(
        message.contains("无法打开远程会话存储") || message.contains("remote"),
        "the failure names the remote store: {message}"
    );
    eprintln!("state store failure probe: {message}");

    // The child path must refuse as well: no silent downgrade to SQLite.
    let logical = LogicalId::parse("agent-state-failure").expect("logical id");
    let log = Arc::new(ItemLog::new(
        logical.as_str(),
        Arc::new(peri_swarm::store::MemorySwarmStore::new()),
        broadcast::channel(16).0,
        Arc::new(std::sync::atomic::AtomicU64::new(0)),
    ));
    let child_error = AcpChild::spawn(
        ChildConfig {
            cwd: cwd.clone(),
            session_store: unreachable,
            machine_id: Some(machine),
            workspace_root: Some(root.clone()),
            settings_json: Some(settings_json("http://127.0.0.1:9/v1")),
            model: None,
            instructions: None,
            mcp_tools: Vec::new(),
            workspace: None,
            root_session_id: uuid::Uuid::now_v7().to_string(),
            session_resources: None,
        },
        log,
    )
    .await
    .expect_err("the child must not open a local store when the remote one is unreachable");
    assert!(
        matches!(child_error, ChildError::Local(_)),
        "child failure class: {child_error:?}"
    );
    eprintln!("state store child failure: {child_error}");
}
