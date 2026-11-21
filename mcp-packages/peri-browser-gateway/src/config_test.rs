use std::collections::HashMap;

use super::*;

fn parse(args: &[&str]) -> Result<Config, ConfigError> {
    parse_with_env(args, &HashMap::new())
}

fn parse_with_env(args: &[&str], env: &HashMap<&str, &str>) -> Result<Config, ConfigError> {
    Config::parse_from(args.iter().map(|arg| (*arg).to_owned()), |key| {
        env.get(key).map(|value| (*value).to_owned())
    })
}

fn cdp_only(args: &[&str]) -> Config {
    parse(args).expect("valid config")
}

fn assert_hex_secret(secret: &Secret) {
    assert_eq!(secret.as_bytes().len(), SECRET_HEX_LEN);
    assert!(
        secret
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)),
        "generated secret must be lowercase hex"
    );
}

#[test]
fn cli_minimal_uses_defaults() {
    let config = cdp_only(&["--cdp-url", "ws://127.0.0.1:8827"]);
    assert_eq!(config.cdp_url, "ws://127.0.0.1:8827");
    assert_eq!(config.bind, DEFAULT_BIND.parse().unwrap());
    assert_eq!(config.registry_sock, default_registry_sock());
    assert!(config.secret.is_none());
    assert_eq!(
        config.secret_path(),
        default_registry_sock().with_extension("secret")
    );
}

#[test]
fn environment_supplies_every_setting() {
    let env = HashMap::from([
        (CDP_URL_ENV, "ws://127.0.0.1:9000"),
        (BIND_ENV, "127.0.0.1:9400"),
        (REGISTRY_SOCK_ENV, "/tmp/peri-gateway-test.sock"),
        (SECRET_ENV, "env-secret"),
    ]);
    let config = parse_with_env(&[], &env).expect("valid config");
    assert_eq!(config.cdp_url, "ws://127.0.0.1:9000");
    assert_eq!(config.bind, "127.0.0.1:9400".parse().unwrap());
    assert_eq!(
        config.registry_sock,
        PathBuf::from("/tmp/peri-gateway-test.sock")
    );
    assert_eq!(
        config.secret.as_ref().map(Secret::as_bytes),
        Some(&b"env-secret"[..])
    );
}

#[test]
fn cli_overrides_environment() {
    let env = HashMap::from([
        (CDP_URL_ENV, "ws://127.0.0.1:9000"),
        (BIND_ENV, "127.0.0.1:9400"),
        (SECRET_ENV, "env-secret"),
    ]);
    let config = parse_with_env(
        &[
            "--cdp-url",
            "ws://127.0.0.1:8827",
            "--bind",
            "127.0.0.1:7500",
            "--secret",
            "cli-secret",
        ],
        &env,
    )
    .expect("valid config");
    assert_eq!(config.cdp_url, "ws://127.0.0.1:8827");
    assert_eq!(config.bind, "127.0.0.1:7500".parse().unwrap());
    assert_eq!(
        config.secret.as_ref().map(Secret::as_bytes),
        Some(&b"cli-secret"[..])
    );
}

#[test]
fn inline_values_are_accepted() {
    let config = parse(&["--cdp-url=wss://example.test:9222"]).expect("valid config");
    assert_eq!(config.cdp_url, "wss://example.test:9222");
}

#[test]
fn missing_cdp_url_is_rejected() {
    let error = parse(&[]).expect_err("cdp url is required");
    assert!(matches!(error, ConfigError::MissingCdpUrl(CDP_URL_ENV)));
}

#[test]
fn invalid_cdp_url_is_rejected() {
    for value in [
        "http://127.0.0.1:8827",
        "ws://",
        "ws:///devtools",
        "127.0.0.1",
    ] {
        let error = parse(&["--cdp-url", value]).expect_err("cdp url must be a websocket url");
        assert!(
            matches!(error, ConfigError::InvalidCdpUrl { .. }),
            "{value} must be rejected: {error}"
        );
    }
}

#[test]
fn unknown_flag_is_rejected() {
    let error = parse(&["--cdp-url", "ws://127.0.0.1:8827", "--wat"]).expect_err("unknown flag");
    assert!(matches!(error, ConfigError::UnknownFlag(flag) if flag == "--wat"));
}

#[test]
fn positional_argument_is_rejected() {
    let error = parse(&["serve"]).expect_err("positional arguments are not accepted");
    assert!(matches!(error, ConfigError::UnknownFlag(flag) if flag == "serve"));
}

#[test]
fn missing_value_is_rejected() {
    let error = parse(&["--cdp-url"]).expect_err("value is required");
    assert!(matches!(error, ConfigError::MissingValue { flag } if flag == "--cdp-url"));
}

#[test]
fn invalid_bind_is_rejected() {
    let error = parse(&["--cdp-url", "ws://127.0.0.1:8827", "--bind", "nope"])
        .expect_err("bind must be a socket address");
    assert!(matches!(error, ConfigError::InvalidBind { .. }));
}

#[test]
fn empty_secret_is_rejected() {
    let error = parse(&["--cdp-url", "ws://127.0.0.1:8827", "--secret="])
        .expect_err("empty secret must not be accepted");
    assert!(matches!(error, ConfigError::EmptySecret { .. }));

    let env = HashMap::from([(CDP_URL_ENV, "ws://127.0.0.1:8827"), (SECRET_ENV, "")]);
    let error = parse_with_env(&[], &env).expect_err("empty env secret must not be accepted");
    assert!(matches!(error, ConfigError::EmptySecret { .. }));
}

#[test]
fn empty_registry_socket_is_rejected() {
    let error = parse(&["--cdp-url", "ws://127.0.0.1:8827", "--registry-sock="])
        .expect_err("empty socket path must not be accepted");
    assert!(matches!(error, ConfigError::EmptySocketPath));
}

#[test]
fn generated_secrets_are_hex_and_unique() {
    let first = generate_secret();
    let second = generate_secret();
    assert_eq!(first.len(), SECRET_HEX_LEN);
    assert_ne!(first, second);
    assert_hex_secret(&Secret::from_value(first, "test").unwrap());
}

#[test]
fn debug_does_not_print_the_secret() {
    let config = cdp_only(&[
        "--cdp-url",
        "ws://127.0.0.1:8827",
        "--secret",
        "super-secret",
    ]);
    let rendered = format!("{config:?}");
    assert!(rendered.contains("[redacted]"));
    assert!(!rendered.contains("super-secret"));
}

#[test]
fn persisted_secret_survives_restarts_and_is_private() {
    let dir = std::env::temp_dir().join(format!("peri-bgw-secret-{}", Uuid::new_v4()));
    let sock = dir.join("gateway.sock");
    let sock_text = sock.to_str().expect("temp path is UTF-8").to_owned();

    let first = parse(&[
        "--cdp-url",
        "ws://127.0.0.1:8827",
        "--registry-sock",
        &sock_text,
    ])
    .expect("valid config");
    let secret = first.resolve_secret().expect("generate the secret");
    assert_hex_secret(&secret);
    assert_eq!(first.secret_path(), dir.join("gateway.secret"));
    assert!(first.secret_path().exists());

    // A fresh process reusing the same socket reads the same secret, so every
    // derived token (and therefore every monitor URL) stays valid.
    let second = parse(&[
        "--cdp-url",
        "ws://127.0.0.1:8827",
        "--registry-sock",
        &sock_text,
    ])
    .expect("valid config");
    let reloaded = second.resolve_secret().expect("reload the secret");
    assert_eq!(reloaded.as_bytes(), secret.as_bytes());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(second.secret_path())
            .expect("secret metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "the persisted secret must not be group/world readable"
        );
    }

    std::fs::remove_dir_all(&dir).expect("cleanup temp dir");
}

#[test]
fn empty_persisted_secret_is_rejected() {
    let dir = std::env::temp_dir().join(format!("peri-bgw-secret-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let sock = dir.join("gateway.sock");
    std::fs::write(dir.join("gateway.secret"), b"  \n").expect("write empty secret");

    let config = parse(&[
        "--cdp-url",
        "ws://127.0.0.1:8827",
        "--registry-sock",
        sock.to_str().expect("temp path is UTF-8"),
    ])
    .expect("valid config");
    let error = config
        .resolve_secret()
        .expect_err("an empty secret file must fail startup");
    assert!(matches!(error, ConfigError::EmptySecretFile { .. }));

    std::fs::remove_dir_all(&dir).expect("cleanup temp dir");
}

#[test]
fn session_urls_use_the_bound_port() {
    let config = cdp_only(&["--cdp-url", "ws://127.0.0.1:8827"]);
    assert_eq!(config.monitor_base_url(), "http://127.0.0.1:7391");
    assert_eq!(config.session_url("abc"), "http://127.0.0.1:7391/s/abc/");
}

#[test]
fn unspecified_bind_is_advertised_as_loopback() {
    let ipv4 = cdp_only(&["--cdp-url", "ws://127.0.0.1:8827", "--bind", "0.0.0.0:7391"]);
    assert_eq!(ipv4.monitor_base_url(), "http://127.0.0.1:7391");

    let ipv6 = cdp_only(&["--cdp-url", "ws://127.0.0.1:8827", "--bind", "[::]:7391"]);
    assert_eq!(ipv6.monitor_base_url(), "http://[::1]:7391");
}

#[test]
fn dev_flag_is_optional() {
    let plain = cdp_only(&["--cdp-url", "ws://127.0.0.1:8827"]);
    assert!(!plain.dev);

    let dev = cdp_only(&["--cdp-url", "ws://127.0.0.1:8827", "--dev"]);
    assert!(dev.dev);
}

#[test]
fn dev_flag_rejects_an_inline_value() {
    let error = parse(&["--cdp-url", "ws://127.0.0.1:8827", "--dev=true"])
        .expect_err("--dev is a boolean flag");
    assert!(matches!(error, ConfigError::UnexpectedValue { flag } if flag == "--dev"));
}

#[test]
fn dev_http_base_strips_the_devtools_path() {
    for (cdp_url, expected) in [
        ("ws://127.0.0.1:8827", "http://127.0.0.1:8827"),
        (
            "ws://127.0.0.1:8827/devtools/browser?owner=peri",
            "http://127.0.0.1:8827",
        ),
        (
            "ws://127.0.0.1:8827/devtools/page/t1",
            "http://127.0.0.1:8827",
        ),
        (
            "wss://browser.test:9222/devtools/browser",
            "https://browser.test:9222",
        ),
    ] {
        let config = cdp_only(&["--cdp-url", cdp_url]);
        assert_eq!(config.dev_http_base(), expected);
    }
}

#[test]
fn prepare_registry_socket_removes_a_stale_file() {
    // Keep the path far below the 104-byte sun_path limit.
    let path = std::path::PathBuf::from(format!("/tmp/peri-bgw-stale-{}.sock", Uuid::new_v4()));
    std::fs::write(&path, b"stale").unwrap();
    prepare_registry_socket(&path).expect("stale socket is removed");
    assert!(!path.exists());
}

#[test]
fn prepare_registry_socket_refuses_a_live_listener() {
    let path = std::path::PathBuf::from(format!("/tmp/peri-bgw-live-{}.sock", Uuid::new_v4()));
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let error = prepare_registry_socket(&path).expect_err("a live gateway must not be replaced");
    assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse);
    drop(listener);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn ownership_timeouts_default_to_the_readme_values() {
    let config = cdp_only(&["--cdp-url", "ws://127.0.0.1:8827"]);
    assert_eq!(config.idle_yield_ms, DEFAULT_IDLE_YIELD_MS);
    assert_eq!(config.yield_timeout_ms, DEFAULT_YIELD_TIMEOUT_MS);
    assert_eq!(
        config.request_human_timeout_ms,
        DEFAULT_REQUEST_HUMAN_TIMEOUT_MS
    );
    assert_eq!(DEFAULT_IDLE_YIELD_MS, 15_000);
    assert_eq!(DEFAULT_YIELD_TIMEOUT_MS, 5_000);
    assert_eq!(DEFAULT_REQUEST_HUMAN_TIMEOUT_MS, 120_000);
}

#[test]
fn ownership_timeouts_come_from_the_environment() {
    let env = HashMap::from([
        (CDP_URL_ENV, "ws://127.0.0.1:8827"),
        (IDLE_YIELD_ENV, "30000"),
        (YIELD_TIMEOUT_ENV, "2500"),
        (REQUEST_HUMAN_TIMEOUT_ENV, "90000"),
    ]);
    let config = parse_with_env(&[], &env).expect("valid config");
    assert_eq!(config.idle_yield_ms, 30_000);
    assert_eq!(config.yield_timeout_ms, 2_500);
    assert_eq!(config.request_human_timeout_ms, 90_000);
}

#[test]
fn cli_timeouts_override_the_environment() {
    let env = HashMap::from([
        (CDP_URL_ENV, "ws://127.0.0.1:8827"),
        (IDLE_YIELD_ENV, "30000"),
    ]);
    let config = parse_with_env(&["--idle-yield-ms", "1000", "--yield-timeout-ms=750"], &env)
        .expect("valid config");
    assert_eq!(config.idle_yield_ms, 1_000);
    assert_eq!(config.yield_timeout_ms, 750);
    assert_eq!(
        config.request_human_timeout_ms,
        DEFAULT_REQUEST_HUMAN_TIMEOUT_MS
    );
}

#[test]
fn ownership_timeouts_reject_zero_garbage_and_absurd_values() {
    for (flag, value) in [
        ("--idle-yield-ms", "0"),
        ("--yield-timeout-ms", "0"),
        ("--request-human-timeout-ms", "0"),
        ("--idle-yield-ms", "soon"),
        ("--yield-timeout-ms", "-1"),
        ("--request-human-timeout-ms", "86400001"),
    ] {
        let error = parse(&["--cdp-url", "ws://127.0.0.1:8827", flag, value])
            .expect_err("invalid timeout must be rejected");
        assert!(
            matches!(error, ConfigError::InvalidTimeout { .. }),
            "{flag}={value}: {error}"
        );
    }

    // The documented upper bound itself is accepted.
    let config = cdp_only(&[
        "--cdp-url",
        "ws://127.0.0.1:8827",
        "--idle-yield-ms",
        "86400000",
    ]);
    assert_eq!(config.idle_yield_ms, MAX_TIMEOUT_MS);
}

#[test]
fn ownership_settings_follow_the_config() {
    use crate::ownership::Settings;

    let config = cdp_only(&[
        "--cdp-url",
        "ws://127.0.0.1:8827",
        "--idle-yield-ms",
        "100",
        "--yield-timeout-ms",
        "200",
        "--request-human-timeout-ms",
        "300",
    ]);
    let settings = Settings::from_config(&config);
    assert_eq!(settings.idle_yield, std::time::Duration::from_millis(100));
    assert_eq!(
        settings.yield_timeout,
        std::time::Duration::from_millis(200)
    );
    assert_eq!(
        settings.request_human_timeout,
        std::time::Duration::from_millis(300)
    );
}
