use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use super::*;

fn config(pairs: &[(&str, &str)], http: Option<SocketAddr>) -> Result<BrowserConfig, ConfigError> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
    BrowserConfig::from_lookup(move |key| map.get(key).cloned(), http)
}

#[test]
fn an_empty_environment_has_no_endpoint_and_safe_defaults() {
    let config = config(&[], None).expect("defaults parse");

    assert_eq!(config.cdp_url, None);
    assert_eq!(config.owner, "peri-browser");
    assert_eq!(config.session_name, "peri-browser");
    let socket = config.gateway_sock.expect("gateway socket default");
    assert_eq!(socket.file_name().unwrap(), "peri-browser-gateway.sock");
    assert!(socket.starts_with(std::env::temp_dir()));
    assert_eq!(
        config.provision_lock,
        std::env::temp_dir().join("kitesurf-provision.lock")
    );
    assert_eq!(config.connect_timeout, Duration::from_secs(5));
    assert_eq!(config.page_wait, Duration::from_secs(30));
    assert_eq!(config.idle_yield, Duration::from_secs(15));
    assert_eq!(config.reclaim_wait, Duration::from_secs(2));
    assert_eq!(config.yield_timeout, Duration::from_secs(60));
    assert_eq!(config.request_human_timeout, Duration::from_secs(300));
    assert_eq!(config.provision_lock_wait, Duration::from_secs(10));
    assert_eq!(config.lock_stale_after, Duration::from_secs(30));
    assert_eq!(config.http, None);
}

#[test]
fn the_shared_endpoint_is_read_from_kitesurf_and_overridable() {
    let shared = config(&[("KITESURF_CDP_URL", " ws://127.0.0.1:8827 ")], None).unwrap();
    assert_eq!(shared.cdp_url.as_deref(), Some("ws://127.0.0.1:8827"));

    let overridden = config(
        &[
            ("KITESURF_CDP_URL", "ws://127.0.0.1:8827"),
            ("PERI_BROWSER_CDP_URL", "ws://127.0.0.1:8831"),
        ],
        None,
    )
    .unwrap();
    assert_eq!(overridden.cdp_url.as_deref(), Some("ws://127.0.0.1:8831"));

    let blank_override = config(
        &[
            ("KITESURF_CDP_URL", "ws://127.0.0.1:8827"),
            ("PERI_BROWSER_CDP_URL", "   "),
        ],
        None,
    )
    .unwrap();
    assert_eq!(
        blank_override.cdp_url.as_deref(),
        Some("ws://127.0.0.1:8827"),
        "a blank override falls back instead of clearing the endpoint"
    );
}

#[test]
fn session_name_defaults_to_the_owner_and_can_differ() {
    let settings = config(&[("PERI_BROWSER_OWNER", "coder-1")], None).unwrap();
    assert_eq!(settings.owner, "coder-1");
    assert_eq!(settings.session_name, "coder-1");

    let named = config(
        &[
            ("PERI_BROWSER_OWNER", "coder-1"),
            ("PERI_BROWSER_SESSION_NAME", "Feature work"),
        ],
        None,
    )
    .unwrap();
    assert_eq!(named.session_name, "Feature work");
}

#[test]
fn explicit_paths_override_the_runtime_defaults() {
    let settings = config(
        &[
            ("PERI_BROWSER_GATEWAY_SOCK", "/tmp/gw.sock"),
            ("PERI_BROWSER_PROVISION_LOCK", "/tmp/provision.lock"),
        ],
        None,
    )
    .unwrap();
    assert_eq!(settings.gateway_sock, Some(PathBuf::from("/tmp/gw.sock")));
    assert_eq!(
        settings.provision_lock,
        PathBuf::from("/tmp/provision.lock")
    );
}

#[test]
fn millisecond_bounds_are_parsed() {
    let settings = config(
        &[
            ("PERI_BROWSER_CONNECT_TIMEOUT_MS", "250"),
            ("PERI_BROWSER_PAGE_WAIT_MS", "0"),
            ("PERI_BROWSER_IDLE_YIELD_MS", "1"),
            ("PERI_BROWSER_RECLAIM_WAIT_MS", "2"),
            ("PERI_BROWSER_YIELD_TIMEOUT_MS", "3"),
            ("PERI_BROWSER_REQUEST_HUMAN_TIMEOUT_MS", "4"),
            ("PERI_BROWSER_PROVISION_LOCK_WAIT_MS", "5"),
            ("PERI_BROWSER_LOCK_STALE_MS", "6"),
        ],
        None,
    )
    .unwrap();
    assert_eq!(settings.connect_timeout, Duration::from_millis(250));
    assert_eq!(settings.page_wait, Duration::ZERO);
    assert_eq!(settings.idle_yield, Duration::from_millis(1));
    assert_eq!(settings.reclaim_wait, Duration::from_millis(2));
    assert_eq!(settings.yield_timeout, Duration::from_millis(3));
    assert_eq!(settings.request_human_timeout, Duration::from_millis(4));
    assert_eq!(settings.provision_lock_wait, Duration::from_millis(5));
    assert_eq!(settings.lock_stale_after, Duration::from_millis(6));
}

#[test]
fn a_non_numeric_bound_is_rejected_by_name() {
    let error = config(&[("PERI_BROWSER_PAGE_WAIT_MS", "soon")], None).unwrap_err();
    assert_eq!(
        error,
        ConfigError::InvalidNumber {
            key: "PERI_BROWSER_PAGE_WAIT_MS",
            value: "soon".to_owned(),
        }
    );
    assert!(error.to_string().contains("soon"));
}

#[test]
fn an_empty_owner_is_rejected_instead_of_silently_defaulting() {
    let error = config(&[("PERI_BROWSER_OWNER", "  ")], None).unwrap_err();
    assert_eq!(
        error,
        ConfigError::EmptyValue {
            key: "PERI_BROWSER_OWNER"
        }
    );
}

#[test]
fn the_http_flag_wins_over_the_environment() {
    let flag: SocketAddr = "127.0.0.1:8832".parse().unwrap();
    let settings = config(&[("PERI_BROWSER_HTTP_ADDR", "127.0.0.1:8833")], Some(flag)).unwrap();
    assert_eq!(settings.http, Some(flag));

    let env = config(&[("PERI_BROWSER_HTTP_ADDR", "127.0.0.1:8833")], None).unwrap();
    assert_eq!(env.http, Some("127.0.0.1:8833".parse().unwrap()));

    let invalid = config(&[("PERI_BROWSER_HTTP_ADDR", "not-an-address")], None).unwrap_err();
    assert_eq!(
        invalid,
        ConfigError::InvalidHttpAddr("not-an-address".to_owned())
    );
}
