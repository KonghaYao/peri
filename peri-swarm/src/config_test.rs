use std::collections::HashMap;
use std::time::Duration;

use tempfile::TempDir;

use super::*;

const TEST_KEY: &str = "test-deployment-key-value";

fn base_env(workspace: &TempDir) -> HashMap<String, String> {
    HashMap::from([
        (API_KEY_ENV.to_owned(), TEST_KEY.to_owned()),
        (
            WORKSPACE_ROOT_ENV.to_owned(),
            workspace.path().to_string_lossy().into_owned(),
        ),
    ])
}

fn parse(env: HashMap<String, String>) -> Result<SwarmConfig, ConfigError> {
    SwarmConfig::from_env_map(|name| env.get(name).cloned())
}

#[test]
fn parses_defaults() {
    let workspace = tempfile::tempdir().expect("workspace");
    let config = parse(base_env(&workspace)).expect("config");
    assert_eq!(config.bind_addr, DEFAULT_BIND.parse().expect("bind"));
    assert_eq!(config.sse_heartbeat, Duration::from_millis(15_000));
    assert_eq!(config.session_store.engine, "turso");
    assert_eq!(config.session_store.access, StoreAccess::ReadWrite);
    assert!(config.session_store.locator.is_none());
    assert_eq!(
        config.workspace_root,
        std::fs::canonicalize(workspace.path()).expect("canonical")
    );
    assert!(config.api_key.verify(TEST_KEY));
}

#[test]
fn missing_api_key_is_reported_by_name_only() {
    let workspace = tempfile::tempdir().expect("workspace");
    let mut env = base_env(&workspace);
    env.remove(API_KEY_ENV);
    let error = parse(env).expect_err("missing key");
    assert_eq!(
        error.to_string(),
        "missing required environment variable PERI_SWARM_API_KEY"
    );
}

#[test]
fn debug_never_renders_the_api_key() {
    let workspace = tempfile::tempdir().expect("workspace");
    let config = parse(base_env(&workspace)).expect("config");
    let rendered = format!("{config:?}");
    assert!(!rendered.contains(TEST_KEY));
    assert!(rendered.contains("<redacted>"));
}

#[test]
fn api_key_comparison_rejects_different_values_and_lengths() {
    let workspace = tempfile::tempdir().expect("workspace");
    let config = parse(base_env(&workspace)).expect("config");
    assert!(config.api_key.verify(TEST_KEY));
    assert!(!config.api_key.verify("test-deployment-key-valu"));
    assert!(!config.api_key.verify("test-deployment-key-value-x"));
    assert!(!config.api_key.verify(""));
}

#[test]
fn rejects_non_loopback_bind() {
    let workspace = tempfile::tempdir().expect("workspace");
    let mut env = base_env(&workspace);
    env.insert(BIND_ENV.to_owned(), "0.0.0.0:8787".to_owned());
    let error = parse(env).expect_err("non-loopback bind");
    assert!(error.to_string().contains("loopback"));
    // The error must name the opt-in that is missing.
    assert!(error.to_string().contains(BIND_ALLOW_NON_LOOPBACK_ENV));
}

#[test]
fn bind_opt_in_is_the_only_way_to_a_non_loopback_address() {
    let workspace = tempfile::tempdir().expect("workspace");

    for enabled in ["1", "true", "yes", "on", " TRUE "] {
        let mut env = base_env(&workspace);
        env.insert(BIND_ENV.to_owned(), "0.0.0.0:8787".to_owned());
        env.insert(BIND_ALLOW_NON_LOOPBACK_ENV.to_owned(), enabled.to_owned());
        let config = parse(env).expect("opted-in bind");
        assert_eq!(config.bind_addr.to_string(), "0.0.0.0:8787");
    }

    for disabled in ["0", "false", "no", "off"] {
        let mut env = base_env(&workspace);
        env.insert(BIND_ENV.to_owned(), "0.0.0.0:8787".to_owned());
        env.insert(BIND_ALLOW_NON_LOOPBACK_ENV.to_owned(), disabled.to_owned());
        let error = parse(env).expect_err("switch off");
        assert!(error.to_string().contains("loopback"));
    }

    let mut env = base_env(&workspace);
    env.insert(BIND_ALLOW_NON_LOOPBACK_ENV.to_owned(), "maybe".to_owned());
    let error = parse(env).expect_err("unknown switch value");
    assert!(error.to_string().contains(BIND_ALLOW_NON_LOOPBACK_ENV));
    assert!(error.to_string().contains("1/true/yes/on"));

    // A malformed address is still a parse error, not a policy error.
    let mut env = base_env(&workspace);
    env.insert(BIND_ENV.to_owned(), "not-an-address".to_owned());
    env.insert(BIND_ALLOW_NON_LOOPBACK_ENV.to_owned(), "1".to_owned());
    let error = parse(env).expect_err("malformed bind");
    assert!(error.to_string().contains("host:port"));
}

#[test]
fn rejects_non_loopback_or_non_http_bun_base() {
    let workspace = tempfile::tempdir().expect("workspace");

    let mut env = base_env(&workspace);
    env.insert(
        BUN_BASE_URL_ENV.to_owned(),
        "http://10.0.0.5:9000".to_owned(),
    );
    let error = parse(env).expect_err("remote bun host");
    assert!(error.to_string().contains("loopback"));

    let mut env = base_env(&workspace);
    env.insert(
        BUN_BASE_URL_ENV.to_owned(),
        "https://127.0.0.1:9000".to_owned(),
    );
    let error = parse(env).expect_err("https bun host");
    assert!(error.to_string().contains("HTTP"));

    let mut env = base_env(&workspace);
    env.insert(BUN_BASE_URL_ENV.to_owned(), "http://127.0.0.1".to_owned());
    let error = parse(env).expect_err("missing port");
    assert!(error.to_string().contains("port"));

    let mut env = base_env(&workspace);
    env.insert(
        BUN_BASE_URL_ENV.to_owned(),
        "http://127.0.0.1:9000".to_owned(),
    );
    let config = parse(env).expect("loopback bun");
    assert_eq!(
        config.bun.base_url.expect("url").as_str(),
        "http://127.0.0.1:9000/"
    );
}

#[test]
fn credential_fields_accept_names_not_values() {
    let workspace = tempfile::tempdir().expect("workspace");

    let mut env = base_env(&workspace);
    env.insert(
        STORE_CREDENTIAL_ENV.to_owned(),
        "PERI_SWARM_STORE_TOKEN".to_owned(),
    );
    env.insert(
        MODEL_CREDENTIAL_ENV.to_owned(),
        "ANTHROPIC_API_KEY".to_owned(),
    );
    let config = parse(env).expect("names");
    assert_eq!(
        config.session_store.credential_env.as_deref(),
        Some("PERI_SWARM_STORE_TOKEN")
    );
    assert_eq!(
        config.model.credential_env.as_deref(),
        Some("ANTHROPIC_API_KEY")
    );

    let mut env = base_env(&workspace);
    env.insert(STORE_CREDENTIAL_ENV.to_owned(), "sk-live-value".to_owned());
    let error = parse(env).expect_err("a secret value is not a variable name");
    assert!(error.to_string().contains("environment variable name"));

    let mut env = base_env(&workspace);
    env.insert(MODEL_ENV.to_owned(), "   ".to_owned());
    let error = parse(env).expect_err("empty model");
    assert!(error.to_string().contains("must not be empty"));
}

#[test]
fn store_locator_rejects_embedded_credentials() {
    let workspace = tempfile::tempdir().expect("workspace");
    let mut env = base_env(&workspace);
    env.insert(
        STORE_LOCATOR_ENV.to_owned(),
        "http://user:pass@127.0.0.1:8080/db/t/d/b/".to_owned(),
    );
    let error = parse(env).expect_err("embedded credentials");
    assert!(error.to_string().contains("embedded credentials"));

    let mut env = base_env(&workspace);
    env.insert(
        STORE_LOCATOR_ENV.to_owned(),
        "http://127.0.0.1:8080/db/t/d/b/".to_owned(),
    );
    let config = parse(env).expect("locator");
    assert_eq!(
        config.session_store.locator.expect("url").as_str(),
        "http://127.0.0.1:8080/db/t/d/b/"
    );
}

#[test]
fn heartbeat_bounds_are_enforced() {
    let workspace = tempfile::tempdir().expect("workspace");

    for value in ["0", "abc", "600001"] {
        let mut env = base_env(&workspace);
        env.insert(SSE_HEARTBEAT_MS_ENV.to_owned(), value.to_owned());
        let error = parse(env).expect_err("invalid heartbeat");
        assert!(error.to_string().contains("expected 1..=600000"));
    }

    let mut env = base_env(&workspace);
    env.insert(SSE_HEARTBEAT_MS_ENV.to_owned(), "25".to_owned());
    let config = parse(env).expect("heartbeat");
    assert_eq!(config.sse_heartbeat, Duration::from_millis(25));
}

#[test]
fn workspace_root_must_exist() {
    let missing = std::env::temp_dir().join("peri-swarm-missing-workspace-root");
    let env = HashMap::from([
        (API_KEY_ENV.to_owned(), TEST_KEY.to_owned()),
        (
            WORKSPACE_ROOT_ENV.to_owned(),
            missing.to_string_lossy().into_owned(),
        ),
    ]);
    let error = parse(env).expect_err("missing workspace root");
    assert!(error.to_string().contains(WORKSPACE_ROOT_ENV));
}
