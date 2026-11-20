//! Configuration and failure-path tests for the S6b process entry.
//!
//! No Loom is contacted: these tests only prove that the entry parses one
//! environment shape and fails visibly when the remote deployment is missing,
//! instead of degrading to a local store.

use std::collections::HashMap;
use std::time::Duration;

use peri_acp_types::session_store::SessionStoreLocator;

use super::{
    BootstrapConfig, DEFAULT_BOOTSTRAP_BIND, DEFAULT_STORE_ENGINE, MACHINE_ID_ENV,
    SESSION_STORE_ENV, SETTINGS_JSON_ENV, STATE_STORE_ENV, STORE_ENGINE_ENV, STORE_TOKEN_ENV,
};
use crate::config::{
    ConfigError, API_KEY_ENV, BIND_ENV, BUN_BASE_URL_ENV, BUN_BEARER_ENV, BUN_TOKEN_ENV,
    BUN_URL_ALIAS_ENV, DEFAULT_SSE_HEARTBEAT_MS, MODEL_ENV, SSE_HEARTBEAT_MS_ENV,
    WORKSPACE_ROOT_ENV,
};

const TOKEN_ENV_NAME: &str = "PERI_TEST_LOOM_TOKEN";
const TOKEN_VALUE: &str = "loom-secret-value";
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
    expect_invalid(parse(&[(BIND_ENV, Some("0.0.0.0:8080"))]), BIND_ENV);
    expect_invalid(parse(&[(BIND_ENV, Some("not-an-address"))]), BIND_ENV);
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
