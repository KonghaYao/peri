//! Configuration and failure-path tests for the S6b process entry.
//!
//! No Loom is contacted: these tests only prove that the entry parses one
//! environment shape and fails visibly when the remote deployment is missing,
//! instead of degrading to a local store.

use std::collections::HashMap;

use peri_acp_types::session_store::SessionStoreLocator;

use super::{
    BootstrapConfig, DEFAULT_BOOTSTRAP_BIND, DEFAULT_STORE_ENGINE, MACHINE_ID_ENV,
    SESSION_STORE_ENV, SETTINGS_JSON_ENV, STORE_ENGINE_ENV, STORE_TOKEN_ENV,
};
use crate::config::{ConfigError, API_KEY_ENV, BIND_ENV, MODEL_ENV, WORKSPACE_ROOT_ENV};

const TOKEN_ENV_NAME: &str = "PERI_TEST_LOOM_TOKEN";
const TOKEN_VALUE: &str = "loom-secret-value";
const MACHINE_ID: &str = "0192f0c4-0000-7000-8000-0000000000aa";
const ROOT: &str = "/tmp/peri-swarm-bootstrap-test";

fn base_env() -> Vec<(&'static str, String)> {
    vec![
        (API_KEY_ENV, "deployment-key".to_owned()),
        (
            SESSION_STORE_ENV,
            "http://127.0.0.1:8342/db/main/main/dev/".to_owned(),
        ),
        (STORE_TOKEN_ENV, TOKEN_ENV_NAME.to_owned()),
        (TOKEN_ENV_NAME, TOKEN_VALUE.to_owned()),
        (MACHINE_ID_ENV, MACHINE_ID.to_owned()),
        (WORKSPACE_ROOT_ENV, ROOT.to_owned()),
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
    assert_eq!(config.workspace_root.to_str(), Some(ROOT));
    assert!(config.model.is_none());
    assert!(config.settings_json.is_none());
    assert_eq!(
        config.session_store_locator.as_str(),
        "http://127.0.0.1:8342/db/main/main/dev/"
    );
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
    assert_eq!(
        actor.workspace_root.as_deref().and_then(|p| p.to_str()),
        Some(ROOT)
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
    expect_invalid(
        parse(&[(WORKSPACE_ROOT_ENV, Some("/tmp/../tmp/peri"))]),
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
