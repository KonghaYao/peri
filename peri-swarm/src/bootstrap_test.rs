//! Configuration and failure-path tests for the S6b process entry.
//!
//! No Loom is contacted: these tests only prove that the entry parses one
//! environment shape and fails visibly when the remote deployment is missing,
//! instead of degrading to a local store.

use std::collections::HashMap;
use std::time::Duration;

use peri_acp_types::session_store::SessionStoreLocator;

use super::{
    preflight_external_engine, BootstrapConfig, BootstrapError, RivetConfig, RivetEngineSpawn,
    DEFAULT_BOOTSTRAP_BIND, DEFAULT_RIVET_ENDPOINT, DEFAULT_RIVET_NAMESPACE, DEFAULT_RIVET_POOL,
    DEFAULT_STORE_ENGINE, MACHINE_ID_ENV, RIVET_ENDPOINT_ENV, RIVET_ENGINE_SPAWN_ENV,
    RIVET_NAMESPACE_ENV, RIVET_POOL_ENV, RIVET_TOKEN_ENV, SESSION_STORE_ENV, SETTINGS_JSON_ENV,
    STATE_STORE_ENV, STORE_ENGINE_ENV, STORE_TOKEN_ENV,
};
use crate::config::{
    ConfigError, API_KEY_ENV, BIND_ALLOW_NON_LOOPBACK_ENV, BIND_ENV, BUN_BASE_URL_ENV,
    BUN_BEARER_ENV, BUN_TOKEN_ENV, BUN_URL_ALIAS_ENV, DEFAULT_SSE_HEARTBEAT_MS, MODEL_ENV,
    SSE_HEARTBEAT_MS_ENV, WORKSPACE_ROOT_ENV,
};

const TOKEN_ENV_NAME: &str = "PERI_TEST_LOOM_TOKEN";
const TOKEN_VALUE: &str = "loom-secret-value";
const RIVET_TOKEN_ENV_NAME: &str = "PERI_TEST_ENGINE_TOKEN";
const RIVET_TOKEN_VALUE: &str = "engine-secret-value";
/// A private plaintext address: syntactically valid but not loopback.
const EXTERNAL_ENDPOINT: &str = "http://10.20.30.40:6420";
const MACHINE_ID: &str = "0192f0c4-0000-7000-8000-0000000000aa";
const ROOT_NAME: &str = "peri-swarm-bootstrap-test";
/// Platform-absolute workspace root; `/tmp/...` is not absolute on Windows.
fn workspace_root() -> String {
    let dir = std::env::temp_dir().join(ROOT_NAME);
    let _ = std::fs::create_dir_all(&dir);
    dir.to_string_lossy().into_owned()
}

const SESSION_STORE_VALUE: &str = "http://127.0.0.1:8342/db/main/main/dev/";
const STATE_STORE_VALUE: &str = "http://127.0.0.1:9497/db/local_default/swarm/main/";

fn base_env() -> Vec<(&'static str, String)> {
    vec![
        (API_KEY_ENV, "deployment-key".to_owned()),
        (SESSION_STORE_ENV, SESSION_STORE_VALUE.to_owned()),
        (STATE_STORE_ENV, STATE_STORE_VALUE.to_owned()),
        (STORE_TOKEN_ENV, TOKEN_ENV_NAME.to_owned()),
        (TOKEN_ENV_NAME, TOKEN_VALUE.to_owned()),
        (MACHINE_ID_ENV, MACHINE_ID.to_owned()),
        (WORKSPACE_ROOT_ENV, workspace_root()),
    ]
}

fn parse(overrides: &[(&str, Option<&str>)]) -> Result<BootstrapConfig, ConfigError> {
    let mut env: HashMap<String, String> = base_env()
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value))
        .collect();
    for (name, value) in overrides {
        match value {
            Some(value) => {
                env.insert((*name).to_owned(), (*value).to_owned());
            }
            None => {
                env.remove(*name);
            }
        }
    }
    BootstrapConfig::from_env_map(|name| env.get(name).cloned())
}

fn expect_invalid(result: Result<BootstrapConfig, ConfigError>, variable: &str) -> String {
    match result {
        Err(ConfigError::Invalid {
            variable: found,
            reason,
        }) => {
            assert_eq!(found, variable);
            reason
        }
        other => panic!("expected Invalid({variable}), got {other:?}"),
    }
}

fn expect_missing(result: Result<BootstrapConfig, ConfigError>, variable: &str) {
    match result {
        Err(ConfigError::MissingEnv(found)) => assert_eq!(found, variable),
        other => panic!("expected MissingEnv({variable}), got {other:?}"),
    }
}

// ── complete configuration ──────────────────────────────────────────────────

#[test]
fn complete_environment_parses_with_bootstrap_defaults() {
    let config = parse(&[]).expect("configuration");
    assert_eq!(config.bind_addr.to_string(), DEFAULT_BOOTSTRAP_BIND);
    assert_eq!(config.store_engine, DEFAULT_STORE_ENGINE);
    assert_eq!(config.store_token_env, TOKEN_ENV_NAME);
    assert_eq!(config.machine_id.to_string(), MACHINE_ID);
    let root = workspace_root();
    assert_eq!(config.workspace_root.to_str(), Some(root.as_str()));
    assert!(config.model.is_none());
    assert!(config.settings_json.is_none());
    assert_eq!(config.session_store_locator.as_str(), SESSION_STORE_VALUE);
    assert_eq!(config.state_store_locator.as_str(), STATE_STORE_VALUE);
}

#[test]
fn heartbeat_defaults_to_fifteen_seconds_and_reaches_the_gateway_config() {
    let config = parse(&[]).expect("configuration");
    assert_eq!(
        config.sse_heartbeat,
        Duration::from_millis(DEFAULT_SSE_HEARTBEAT_MS)
    );
    // The SSE handler reads this value from `AppState::config`, so the entry
    // must not reset it while building the gateway configuration.
    assert_eq!(config.swarm_config().sse_heartbeat, config.sse_heartbeat);
}

#[test]
fn a_configured_heartbeat_reaches_the_gateway_config() {
    for value in ["1000", "3000"] {
        let config = parse(&[(SSE_HEARTBEAT_MS_ENV, Some(value))]).expect("configuration");
        let expected = Duration::from_millis(value.parse().expect("milliseconds"));
        assert_eq!(config.sse_heartbeat, expected);
        assert_eq!(config.swarm_config().sse_heartbeat, expected);
    }
}

#[test]
fn invalid_heartbeat_fails_the_start() {
    for value in ["0", "abc", "600001"] {
        let reason = expect_invalid(
            parse(&[(SSE_HEARTBEAT_MS_ENV, Some(value))]),
            SSE_HEARTBEAT_MS_ENV,
        );
        assert!(reason.contains("1..=600000"), "{reason}");
    }
}

#[test]
fn state_store_configuration_carries_the_locator_and_the_shared_credential_name() {
    let config = parse(&[]).expect("configuration");
    let state = config.state_store_config();
    assert_eq!(state.locator.as_str(), STATE_STORE_VALUE);
    assert_eq!(state.credential_env, TOKEN_ENV_NAME);
}

#[test]
fn optional_model_and_settings_are_carried_verbatim() {
    let settings = r#"{"config":{"model":"stub"}}"#;
    let config = parse(&[
        (MODEL_ENV, Some("stub-model")),
        (SETTINGS_JSON_ENV, Some(settings)),
    ])
    .expect("configuration");
    assert_eq!(config.model.as_deref(), Some("stub-model"));
    assert_eq!(config.settings_json.as_deref(), Some(settings));
}

#[test]
fn a_configured_bun_bridge_carries_the_url_and_outer_token() {
    let config = parse(&[
        (BUN_BASE_URL_ENV, Some("http://127.0.0.1:8081")),
        (BUN_TOKEN_ENV, Some("outer-secret")),
    ])
    .expect("configuration");
    let sandbox = config.sandbox.as_ref().expect("sandbox bridge");
    assert_eq!(sandbox.base_url.as_str(), "http://127.0.0.1:8081/");
    assert_eq!(sandbox.token.as_deref(), Some("outer-secret"));
    assert_eq!(
        config.bun.base_url.as_ref().map(url::Url::as_str),
        Some("http://127.0.0.1:8081/")
    );
    let actor = config.actor_config();
    assert!(actor.sandbox.is_some());
    let rendered = format!("{actor:?}");
    assert!(!rendered.contains("outer-secret"), "{rendered}");
    assert!(rendered.contains("<redacted>"), "{rendered}");
}

#[test]
fn the_bun_url_alias_and_named_token_variable_are_accepted() {
    let config = parse(&[
        (BUN_URL_ALIAS_ENV, Some("http://127.0.0.1:8082")),
        (BUN_BEARER_ENV, Some(TOKEN_ENV_NAME)),
    ])
    .expect("configuration");
    let sandbox = config.sandbox.as_ref().expect("sandbox bridge");
    assert_eq!(sandbox.base_url.as_str(), "http://127.0.0.1:8082/");
    assert_eq!(sandbox.token.as_deref(), Some(TOKEN_VALUE));
    assert_eq!(config.bun.bearer_env.as_deref(), Some(TOKEN_ENV_NAME));
}

#[test]
fn an_unconfigured_bridge_leaves_hosted_requests_to_fail_at_session_creation() {
    let config = parse(&[]).expect("configuration");
    assert!(config.sandbox.is_none());
    assert!(config.actor_config().sandbox.is_none());
}

#[test]
fn invalid_bun_token_configuration_fails_the_start() {
    let reason = expect_invalid(
        parse(&[(BUN_TOKEN_ENV, Some("outer-secret"))]),
        BUN_TOKEN_ENV,
    );
    assert!(reason.contains(BUN_BASE_URL_ENV), "{reason}");

    let reason = expect_invalid(
        parse(&[
            (BUN_BASE_URL_ENV, Some("http://127.0.0.1:8081")),
            (BUN_TOKEN_ENV, Some("outer secret")),
        ]),
        BUN_TOKEN_ENV,
    );
    assert!(reason.contains("whitespace"), "{reason}");

    let reason = expect_invalid(
        parse(&[
            (BUN_BASE_URL_ENV, Some("http://127.0.0.1:8081")),
            (BUN_TOKEN_ENV, Some("outer-secret")),
            (BUN_BEARER_ENV, Some(TOKEN_ENV_NAME)),
        ]),
        BUN_TOKEN_ENV,
    );
    assert!(reason.contains(BUN_BEARER_ENV), "{reason}");
}

#[test]
fn actor_configuration_carries_the_remote_deployment() {
    let config = parse(&[]).expect("configuration");
    let actor = config.actor_config();
    assert!(actor.session_store_path.is_none());
    let deployment = actor.session_store.expect("remote deployment");
    assert_eq!(deployment.engine_name(), Some(DEFAULT_STORE_ENGINE));
    assert_eq!(deployment.credential_env_name(), Some(TOKEN_ENV_NAME));
    match deployment.locator() {
        SessionStoreLocator::Locator(raw) => {
            assert_eq!(raw, "http://127.0.0.1:8342/db/main/main/dev/");
        }
        other => panic!("expected a remote locator, got {other:?}"),
    }
    assert_eq!(
        actor.machine_id.map(|id| id.to_string()),
        Some(MACHINE_ID.to_owned())
    );
    let root = workspace_root();
    assert_eq!(
        actor.workspace_root.as_deref().and_then(|p| p.to_str()),
        Some(root.as_str())
    );
}

#[test]
fn debug_output_never_renders_the_api_key_or_store_token() {
    let config = parse(&[]).expect("configuration");
    let rendered = format!("{config:?}");
    assert!(!rendered.contains("deployment-key"), "{rendered}");
    assert!(!rendered.contains(TOKEN_VALUE), "{rendered}");
    assert!(rendered.contains(MACHINE_ID), "{rendered}");
}

// ── missing remote configuration fails the start ───────────────────────────

#[test]
fn missing_api_key_fails_the_start() {
    expect_missing(parse(&[(API_KEY_ENV, None)]), API_KEY_ENV);
}

#[test]
fn missing_session_store_fails_the_start() {
    expect_missing(parse(&[(SESSION_STORE_ENV, None)]), SESSION_STORE_ENV);
}

#[test]
fn missing_state_store_fails_the_start() {
    expect_missing(parse(&[(STATE_STORE_ENV, None)]), STATE_STORE_ENV);
}

#[test]
fn one_remote_store_alone_is_not_enough() {
    // Only the state store: the session store is still required.
    expect_missing(parse(&[(SESSION_STORE_ENV, None)]), SESSION_STORE_ENV);
    // Only the session store: the state store is still required.
    expect_missing(parse(&[(STATE_STORE_ENV, None)]), STATE_STORE_ENV);
}

#[test]
fn missing_token_variable_name_fails_the_start() {
    expect_missing(parse(&[(STORE_TOKEN_ENV, None)]), STORE_TOKEN_ENV);
}

#[test]
fn missing_machine_id_fails_the_start() {
    expect_missing(parse(&[(MACHINE_ID_ENV, None)]), MACHINE_ID_ENV);
}

#[test]
fn missing_workspace_root_fails_the_start() {
    expect_missing(parse(&[(WORKSPACE_ROOT_ENV, None)]), WORKSPACE_ROOT_ENV);
}

#[test]
fn unset_credential_variable_is_named_in_the_error() {
    let reason = expect_invalid(parse(&[(TOKEN_ENV_NAME, None)]), STORE_TOKEN_ENV);
    assert!(reason.contains(TOKEN_ENV_NAME), "{reason}");
}

#[test]
fn empty_credential_variable_is_rejected() {
    let reason = expect_invalid(parse(&[(TOKEN_ENV_NAME, Some("   "))]), STORE_TOKEN_ENV);
    assert!(reason.contains(TOKEN_ENV_NAME), "{reason}");
}

#[test]
fn a_token_value_in_the_name_slot_is_rejected() {
    let reason = expect_invalid(
        parse(&[(STORE_TOKEN_ENV, Some("tok-literal"))]),
        STORE_TOKEN_ENV,
    );
    assert!(reason.contains("variable name"), "{reason}");
}

// ── shape validation ───────────────────────────────────────────────────────

#[test]
fn local_store_locators_are_rejected() {
    expect_invalid(
        parse(&[(SESSION_STORE_ENV, Some("/tmp/threads.db"))]),
        SESSION_STORE_ENV,
    );
    expect_invalid(
        parse(&[(SESSION_STORE_ENV, Some("libsql://example.test/db"))]),
        SESSION_STORE_ENV,
    );
}

#[test]
fn embedded_locator_credentials_are_rejected() {
    expect_invalid(
        parse(&[(
            SESSION_STORE_ENV,
            Some("http://user:pass@127.0.0.1:8342/db/main/main/dev/"),
        )]),
        SESSION_STORE_ENV,
    );
}

#[test]
fn state_store_locators_are_validated_like_the_session_store() {
    expect_invalid(
        parse(&[(STATE_STORE_ENV, Some("/tmp/swarm.db"))]),
        STATE_STORE_ENV,
    );
    expect_invalid(
        parse(&[(STATE_STORE_ENV, Some("libsql://example.test/db"))]),
        STATE_STORE_ENV,
    );
    expect_invalid(
        parse(&[(
            STATE_STORE_ENV,
            Some("http://user:pass@127.0.0.1:9497/db/swarm/main/"),
        )]),
        STATE_STORE_ENV,
    );
}

#[test]
fn state_store_must_not_be_the_session_store() {
    let reason = expect_invalid(
        parse(&[(STATE_STORE_ENV, Some(SESSION_STORE_VALUE))]),
        STATE_STORE_ENV,
    );
    assert!(reason.contains(SESSION_STORE_ENV), "{reason}");

    // The trailing slash is not part of the database identity.
    let without_trailing_slash = SESSION_STORE_VALUE.trim_end_matches('/');
    expect_invalid(
        parse(&[(STATE_STORE_ENV, Some(without_trailing_slash))]),
        STATE_STORE_ENV,
    );
}

#[test]
fn non_uuid_machine_id_is_rejected() {
    expect_invalid(
        parse(&[(MACHINE_ID_ENV, Some("machine-1"))]),
        MACHINE_ID_ENV,
    );
}

#[test]
fn relative_or_unnormalized_workspace_root_is_rejected() {
    expect_invalid(
        parse(&[(WORKSPACE_ROOT_ENV, Some("relative/path"))]),
        WORKSPACE_ROOT_ENV,
    );
    let unnormalized = format!("{}/../peri", workspace_root());
    expect_invalid(
        parse(&[(WORKSPACE_ROOT_ENV, Some(unnormalized.as_str()))]),
        WORKSPACE_ROOT_ENV,
    );
}

#[test]
fn non_loopback_bind_is_rejected() {
    let reason = expect_invalid(parse(&[(BIND_ENV, Some("0.0.0.0:8080"))]), BIND_ENV);
    assert!(reason.contains("loopback"), "{reason}");
    assert!(reason.contains(BIND_ALLOW_NON_LOOPBACK_ENV), "{reason}");
    expect_invalid(parse(&[(BIND_ENV, Some("not-an-address"))]), BIND_ENV);
}

#[test]
fn the_bind_opt_in_allows_a_non_loopback_address_and_rejects_other_values() {
    let config = parse(&[
        (BIND_ENV, Some("0.0.0.0:8080")),
        (BIND_ALLOW_NON_LOOPBACK_ENV, Some("1")),
    ])
    .expect("opted-in bind");
    assert_eq!(config.bind_addr.to_string(), "0.0.0.0:8080");
    // The gateway config receives the same accepted address.
    assert_eq!(config.swarm_config().bind_addr, config.bind_addr);

    // An explicit off value keeps the loopback rule.
    let reason = expect_invalid(
        parse(&[
            (BIND_ENV, Some("0.0.0.0:8080")),
            (BIND_ALLOW_NON_LOOPBACK_ENV, Some("off")),
        ]),
        BIND_ENV,
    );
    assert!(reason.contains("loopback"), "{reason}");

    let reason = expect_invalid(
        parse(&[(BIND_ALLOW_NON_LOOPBACK_ENV, Some("maybe"))]),
        BIND_ALLOW_NON_LOOPBACK_ENV,
    );
    assert!(reason.contains("1/true/yes/on"), "{reason}");
}

// ── rivet engine configuration (M1) ────────────────────────────────────────

#[test]
fn the_default_engine_configuration_matches_the_former_constants() {
    let config = parse(&[]).expect("configuration");
    let rivet = &config.rivet;
    // The client-facing spelling has no trailing slash: byte-identical to the
    // constant the process used before M1.
    assert_eq!(rivet.endpoint_string(), DEFAULT_RIVET_ENDPOINT);
    assert_eq!(rivet.endpoint_string(), "http://127.0.0.1:6420");
    assert_eq!(rivet.endpoint.as_str(), "http://127.0.0.1:6420/");
    assert_eq!(rivet.token_env, None);
    assert_eq!(rivet.namespace, DEFAULT_RIVET_NAMESPACE);
    assert_eq!(rivet.pool, DEFAULT_RIVET_POOL);
    assert_eq!(rivet.engine_spawn, RivetEngineSpawn::Always);
    assert_eq!(rivet.engine_spawn.as_str(), "always");
}

#[test]
fn a_loopback_endpoint_may_be_overridden_and_never_needs_no_token() {
    // Replica B in the M1 topology: attach to an engine someone else started.
    let config = parse(&[
        (RIVET_ENDPOINT_ENV, Some("http://localhost:7420/")),
        (RIVET_ENGINE_SPAWN_ENV, Some("never")),
    ])
    .expect("loopback external engine");
    assert_eq!(config.rivet.endpoint_string(), "http://localhost:7420");
    assert_eq!(config.rivet.engine_spawn, RivetEngineSpawn::Never);
    assert_eq!(config.rivet.token_env, None);

    let auto = parse(&[(RIVET_ENGINE_SPAWN_ENV, Some("AUTO"))]).expect("auto on loopback");
    assert_eq!(auto.rivet.engine_spawn, RivetEngineSpawn::Auto);
    assert_eq!(auto.rivet.engine_spawn.as_str(), "auto");
}

#[test]
fn an_unknown_spawn_mode_is_rejected() {
    let reason = expect_invalid(
        parse(&[(RIVET_ENGINE_SPAWN_ENV, Some("sometimes"))]),
        RIVET_ENGINE_SPAWN_ENV,
    );
    assert!(reason.contains("always, auto or never"), "{reason}");
}

#[test]
fn https_engine_endpoints_are_rejected_until_tls_is_designed() {
    let reason = expect_invalid(
        parse(&[(RIVET_ENDPOINT_ENV, Some("https://engine.internal:6420"))]),
        RIVET_ENDPOINT_ENV,
    );
    assert!(reason.contains("plaintext"), "{reason}");
    assert!(reason.contains("TLS"), "{reason}");
}

#[test]
fn engine_endpoint_shape_is_validated() {
    for (value, expected) in [
        ("ws://127.0.0.1:6420", "http"),
        ("http://127.0.0.1:6420/path", "path"),
        ("http://127.0.0.1:6420/?a=1", "query and fragment"),
        ("http://127.0.0.1:6420/#frag", "query and fragment"),
        ("http://user:pass@127.0.0.1:6420", "embedded credentials"),
        ("http://127.0.0.1", "explicit port"),
        ("not-a-url", "URL"),
    ] {
        let reason = expect_invalid(
            parse(&[(RIVET_ENDPOINT_ENV, Some(value))]),
            RIVET_ENDPOINT_ENV,
        );
        assert!(reason.contains(expected), "{value}: {reason}");
    }
}

#[test]
fn a_non_loopback_endpoint_requires_never_and_a_token_variable() {
    // Default spawn (`always`) with a remote address is a contradiction: the
    // local engine would never be the one answering.
    let reason = expect_invalid(
        parse(&[(RIVET_ENDPOINT_ENV, Some(EXTERNAL_ENDPOINT))]),
        RIVET_ENGINE_SPAWN_ENV,
    );
    assert!(reason.contains("never"), "{reason}");
    assert!(reason.contains(EXTERNAL_ENDPOINT), "{reason}");

    expect_invalid(
        parse(&[
            (RIVET_ENDPOINT_ENV, Some(EXTERNAL_ENDPOINT)),
            (RIVET_ENGINE_SPAWN_ENV, Some("auto")),
        ]),
        RIVET_ENGINE_SPAWN_ENV,
    );

    // `never` alone is not enough: the engine token must come from somewhere.
    let reason = expect_invalid(
        parse(&[
            (RIVET_ENDPOINT_ENV, Some(EXTERNAL_ENDPOINT)),
            (RIVET_ENGINE_SPAWN_ENV, Some("never")),
        ]),
        RIVET_TOKEN_ENV,
    );
    assert!(reason.contains(EXTERNAL_ENDPOINT), "{reason}");
}

#[test]
fn an_external_engine_configuration_keeps_only_the_token_variable_name() {
    let config = parse(&[
        (RIVET_ENDPOINT_ENV, Some(EXTERNAL_ENDPOINT)),
        (RIVET_ENGINE_SPAWN_ENV, Some("never")),
        (RIVET_TOKEN_ENV, Some(RIVET_TOKEN_ENV_NAME)),
        (RIVET_TOKEN_ENV_NAME, Some(RIVET_TOKEN_VALUE)),
        (RIVET_NAMESPACE_ENV, Some("prod-eu")),
        (RIVET_POOL_ENV, Some("swarm-prod")),
    ])
    .expect("external engine");
    assert_eq!(config.rivet.endpoint_string(), EXTERNAL_ENDPOINT);
    assert_eq!(
        config.rivet.token_env.as_deref(),
        Some(RIVET_TOKEN_ENV_NAME)
    );
    assert_eq!(config.rivet.namespace, "prod-eu");
    assert_eq!(config.rivet.pool, "swarm-prod");
    assert_eq!(config.rivet.engine_spawn, RivetEngineSpawn::Never);

    let rendered = format!("{config:?}");
    assert!(!rendered.contains(RIVET_TOKEN_VALUE), "{rendered}");
    assert!(!rendered.contains(TOKEN_VALUE), "{rendered}");
    assert!(rendered.contains(RIVET_TOKEN_ENV_NAME), "{rendered}");
    let rivet = format!("{:?}", config.rivet);
    assert!(!rivet.contains(RIVET_TOKEN_VALUE), "{rivet}");
}

#[test]
fn token_variable_name_and_lookup_failures_are_reported() {
    // A token value in the name slot.
    let reason = expect_invalid(
        parse(&[(RIVET_TOKEN_ENV, Some("eng-secret-literal"))]),
        RIVET_TOKEN_ENV,
    );
    assert!(reason.contains("variable name"), "{reason}");

    // Naming the Loom credential would leak it to the engine.
    let reason = expect_invalid(
        parse(&[(RIVET_TOKEN_ENV, Some(TOKEN_ENV_NAME))]),
        RIVET_TOKEN_ENV,
    );
    assert!(reason.contains(STORE_TOKEN_ENV), "{reason}");

    // The named variable must exist and be non-empty.
    let reason = expect_invalid(
        parse(&[(RIVET_TOKEN_ENV, Some("PERI_TEST_MISSING_ENGINE_TOKEN"))]),
        RIVET_TOKEN_ENV,
    );
    assert!(
        reason.contains("PERI_TEST_MISSING_ENGINE_TOKEN"),
        "{reason}"
    );
    let reason = expect_invalid(
        parse(&[
            (RIVET_TOKEN_ENV, Some(RIVET_TOKEN_ENV_NAME)),
            (RIVET_TOKEN_ENV_NAME, Some("  ")),
        ]),
        RIVET_TOKEN_ENV,
    );
    assert!(reason.contains("empty"), "{reason}");
}

#[test]
fn namespace_and_pool_are_validated_as_deployment_identity() {
    for (variable, value) in [
        (RIVET_NAMESPACE_ENV, ""),
        (RIVET_NAMESPACE_ENV, "  "),
        (RIVET_NAMESPACE_ENV, "prod eu"),
    ] {
        let reason = expect_invalid(parse(&[(variable, Some(value))]), variable);
        assert!(
            reason.contains("empty") || reason.contains("whitespace"),
            "{value:?}: {reason}"
        );
    }
    let too_long = "x".repeat(129);
    let reason = expect_invalid(
        parse(&[(RIVET_POOL_ENV, Some(too_long.as_str()))]),
        RIVET_POOL_ENV,
    );
    assert!(reason.contains("128"), "{reason}");

    let config = parse(&[
        (RIVET_NAMESPACE_ENV, Some("prod-eu")),
        (RIVET_POOL_ENV, Some("swarm-prod")),
    ])
    .expect("identity");
    assert_eq!(config.rivet.namespace, "prod-eu");
    assert_eq!(config.rivet.pool, "swarm-prod");
}

fn rivet_for(endpoint: &str, spawn: RivetEngineSpawn) -> RivetConfig {
    RivetConfig {
        endpoint: url::Url::parse(endpoint).expect("endpoint"),
        token_env: None,
        namespace: "default".to_owned(),
        pool: "peri-swarm".to_owned(),
        engine_spawn: spawn,
    }
}

#[tokio::test]
async fn an_unreachable_external_engine_fails_before_any_store_is_opened() {
    // Reserve a loopback port and release it: nothing listens on it afterwards.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("probe port");
    let port = listener.local_addr().expect("probe addr").port();
    drop(listener);

    let rivet = rivet_for(&format!("http://127.0.0.1:{port}"), RivetEngineSpawn::Never);
    match preflight_external_engine(&rivet).await {
        Err(BootstrapError::RivetUnreachable { endpoint, reason }) => {
            assert_eq!(endpoint, format!("http://127.0.0.1:{port}"));
            assert!(reason.contains("local engine"), "{reason}");
            assert!(reason.contains(RIVET_ENGINE_SPAWN_ENV), "{reason}");
        }
        other => panic!("expected RivetUnreachable, got {other:?}"),
    }
}

#[tokio::test]
async fn a_reachable_external_engine_passes_the_probe_and_local_modes_skip_it() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let port = listener.local_addr().expect("addr").port();
    let rivet = rivet_for(&format!("http://127.0.0.1:{port}"), RivetEngineSpawn::Never);
    preflight_external_engine(&rivet)
        .await
        .expect("the listener is reachable");
    drop(listener);

    // `always`/`auto` never probe: the local engine may not exist yet.
    let local = rivet_for(
        &format!("http://127.0.0.1:{port}"),
        RivetEngineSpawn::Always,
    );
    preflight_external_engine(&local)
        .await
        .expect("local modes do not probe");
}

#[test]
fn malformed_settings_json_is_rejected() {
    expect_invalid(
        parse(&[(SETTINGS_JSON_ENV, Some("{oops"))]),
        SETTINGS_JSON_ENV,
    );
}

#[test]
fn empty_engine_name_is_rejected() {
    expect_invalid(parse(&[(STORE_ENGINE_ENV, Some(" "))]), STORE_ENGINE_ENV);
}
