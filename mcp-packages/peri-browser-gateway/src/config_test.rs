use super::*;

fn parse(args: &[&str], values: &[(&str, &str)]) -> Result<Config, ConfigError> {
    Config::parse_from(args.iter().map(|arg| (*arg).to_owned()), |key| {
        values
            .iter()
            .find_map(|(k, v)| (*k == key).then(|| (*v).to_owned()))
            .or_else(|| (key == "HOME").then(|| "/fixture/home".to_owned()))
    })
}

#[test]
fn default_bind_and_suggested_endpoint_do_not_require_a_running_engine() {
    let config = parse(&[], &[]).unwrap();
    assert_eq!(config.bind.to_string(), "127.0.0.1:7391");
    assert_eq!(config.sandbox_bind, Some("127.0.0.1:7392".parse().unwrap()));
    assert_eq!(config.app_html, None);
    let browser = config.browser;
    assert_eq!(
        browser.suggested_endpoint,
        "ws://127.0.0.1:18971/devtools/browser"
    );
    assert_eq!(browser.owner, "peri-browser");
    assert_eq!(browser.session_name, browser.owner);
    assert_eq!(
        browser.kitesurf_home,
        PathBuf::from("/fixture/home/.kitesurf")
    );
    assert_eq!(
        browser.provision_lock,
        std::env::temp_dir().join("kitesurf-provision.lock")
    );
    assert_eq!(browser.provision_lock_wait, Duration::from_secs(10));
    assert_eq!(browser.lock_stale_after, Duration::from_secs(30));
    assert_eq!(browser.connect_timeout, Duration::from_secs(5));
    assert_eq!(browser.page_wait, Duration::from_secs(30));
    assert_eq!(browser.idle_yield, Duration::from_secs(15));
    assert_eq!(browser.reclaim_wait, Duration::from_secs(2));
    assert_eq!(browser.yield_timeout, Duration::from_secs(60));
    assert_eq!(browser.request_human_timeout, Duration::from_secs(300));
    assert_eq!(browser.keep_alive(), Duration::from_secs(360));
}

#[test]
fn sandbox_bind_is_loopback_by_default_and_can_be_overridden_or_disabled() {
    assert_eq!(
        parse(&[], &[("PERI_BROWSER_SANDBOX_BIND", "[::1]:7400")])
            .unwrap()
            .sandbox_bind,
        Some("[::1]:7400".parse().unwrap())
    );
    assert_eq!(
        parse(
            &["--sandbox-bind", "127.0.0.1:7401"],
            &[("PERI_BROWSER_SANDBOX_BIND", "127.0.0.1:7400")],
        )
        .unwrap()
        .sandbox_bind,
        Some("127.0.0.1:7401".parse().unwrap())
    );
    for off in ["off", "none"] {
        assert_eq!(
            parse(&["--sandbox-bind", off], &[]).unwrap().sandbox_bind,
            None,
            "sandbox bind {off}"
        );
    }
    for invalid in [
        "--sandbox-bind=0.0.0.0:7392",
        "--sandbox-bind=127.0.0.1:0",
        "--sandbox-bind=localhost:7392",
        "--sandbox-bind=",
    ] {
        assert!(
            matches!(parse(&[invalid], &[]), Err(ConfigError::InvalidSandboxBind)),
            "{invalid}"
        );
    }
}

#[test]
fn view_token_comes_from_the_flag_or_the_environment_and_is_absent_by_default() {
    assert_eq!(parse(&[], &[]).unwrap().view_token, None);
    assert_eq!(
        parse(&["--view-token", "  cli-token  "], &[])
            .unwrap()
            .view_token
            .as_deref(),
        Some("cli-token")
    );
    assert_eq!(
        parse(&[], &[("PERI_BROWSER_VIEW_TOKEN", "env-token")])
            .unwrap()
            .view_token
            .as_deref(),
        Some("env-token")
    );
    assert_eq!(
        parse(
            &["--view-token", "cli-token"],
            &[("PERI_BROWSER_VIEW_TOKEN", "env-token")],
        )
        .unwrap()
        .view_token
        .as_deref(),
        Some("cli-token")
    );
    for blank in [&["--view-token="][..], &["--view-token", "  "][..]] {
        assert!(
            matches!(
                parse(blank, &[]),
                Err(ConfigError::EmptyValue("PERI_BROWSER_VIEW_TOKEN"))
            ),
            "{blank:?}"
        );
    }
}

#[test]
fn app_html_comes_from_the_flag_or_the_environment_and_rejects_blank_values() {
    assert_eq!(
        parse(&["--app-html", "/fixture/app.html"], &[])
            .unwrap()
            .app_html,
        Some(PathBuf::from("/fixture/app.html"))
    );
    assert_eq!(
        parse(
            &[],
            &[("PERI_BROWSER_APP_HTML", " /fixture/from-env.html ")],
        )
        .unwrap()
        .app_html,
        Some(PathBuf::from("/fixture/from-env.html"))
    );
    assert_eq!(
        parse(
            &["--app-html=/fixture/cli.html"],
            &[("PERI_BROWSER_APP_HTML", "/fixture/from-env.html")],
        )
        .unwrap()
        .app_html,
        Some(PathBuf::from("/fixture/cli.html"))
    );
    for blank in [&["--app-html="][..], &["--app-html", "  "][..]] {
        assert!(
            matches!(
                parse(blank, &[]),
                Err(ConfigError::EmptyValue("PERI_BROWSER_APP_HTML"))
            ),
            "{blank:?}"
        );
    }
    assert!(matches!(
        parse(&["--app-html"], &[]),
        Err(ConfigError::MissingValue("PERI_BROWSER_APP_HTML"))
    ));
}

#[test]
fn private_network_access_is_on_by_default_and_env_can_disable_it() {
    let lookup = |value: Option<&'static str>| {
        move |key: &str| {
            (key == "PERI_BROWSER_ALLOW_PRIVATE_NETWORK")
                .then(|| value.unwrap_or_default().to_owned())
        }
    };
    assert!(allow_private_network(&lookup(None)));
    for off in ["0", "false", "off", "no"] {
        assert!(
            !allow_private_network(&lookup(Some(off))),
            "PERI_BROWSER_ALLOW_PRIVATE_NETWORK={off}"
        );
    }
    assert!(allow_private_network(&lookup(Some("1"))));
}

#[test]
fn environment_supplies_browser_and_monitor_settings_without_mutating_process_env() {
    let c = parse(
        &[],
        &[
            ("PERI_BROWSER_MONITOR_BIND", "[::1]:0"),
            ("KITESURF_CDP_URL", "http://localhost:9001/path?ignored=yes"),
            ("PERI_BROWSER_GATEWAY_SECRET", "fixture-value"),
            ("PERI_BROWSER_OWNER", " sticky-owner "),
            ("PERI_BROWSER_SESSION_NAME", " monitor label "),
            ("KITESURF_INSTALL_DIR", "/fixture/install"),
            ("PATH", "/fixture/bin"),
            ("PERI_BROWSER_PROVISION_LOCK", "/fixture/provision.lock"),
            ("PERI_BROWSER_PROVISION_LOCK_WAIT_MS", "7"),
            ("PERI_BROWSER_LOCK_STALE_MS", "8"),
            ("PERI_BROWSER_CONNECT_TIMEOUT_MS", "9"),
            ("PERI_BROWSER_PAGE_WAIT_MS", "10"),
            ("PERI_BROWSER_IDLE_YIELD_MS", "11"),
            ("PERI_BROWSER_RECLAIM_WAIT_MS", "12"),
            ("PERI_BROWSER_YIELD_TIMEOUT_MS", "13"),
            ("PERI_BROWSER_REQUEST_HUMAN_TIMEOUT_MS", "14"),
        ],
    )
    .unwrap();
    assert_eq!(c.bind.to_string(), "[::1]:0");
    assert!(c.secret == b"fixture-value");
    let b = c.browser;
    assert_eq!(b.suggested_endpoint, "ws://127.0.0.1:9001/devtools/browser");
    assert_eq!(b.owner, "sticky-owner");
    assert_eq!(b.session_name, "monitor label");
    assert_eq!(b.kitesurf_home, PathBuf::from("/fixture/install"));
    assert_eq!(b.path.as_deref(), Some("/fixture/bin"));
    assert_eq!(b.provision_lock, PathBuf::from("/fixture/provision.lock"));
    for (duration, ms) in [
        (b.provision_lock_wait, 7),
        (b.lock_stale_after, 8),
        (b.connect_timeout, 9),
        (b.page_wait, 10),
        (b.idle_yield, 11),
        (b.reclaim_wait, 12),
        (b.yield_timeout, 13),
        (b.request_human_timeout, 14),
    ] {
        assert_eq!(duration, Duration::from_millis(ms));
    }
    assert_eq!(b.keep_alive(), Duration::from_secs(300));
}

#[test]
fn cli_overrides_environment_in_both_flag_forms() {
    let c = parse(
        &[
            "--bind=127.0.0.1:0",
            "--secret",
            "fixture-cli",
            "--cdp-url=http://[::1]:9002/devtools/page/old",
            "--idle-yield-ms=21",
            "--yield-timeout-ms",
            "22",
            "--request-human-timeout-ms=450000",
        ],
        &[
            ("PERI_BROWSER_MONITOR_BIND", "0.0.0.0:1"),
            ("KITESURF_CDP_URL", "invalid"),
            ("PERI_BROWSER_GATEWAY_SECRET", "fixture-env"),
            ("PERI_BROWSER_IDLE_YIELD_MS", "1"),
            ("PERI_BROWSER_YIELD_TIMEOUT_MS", "2"),
            ("PERI_BROWSER_REQUEST_HUMAN_TIMEOUT_MS", "3"),
        ],
    )
    .unwrap();
    assert_eq!(c.bind.to_string(), "127.0.0.1:0");
    assert!(c.secret == b"fixture-cli");
    assert_eq!(
        c.browser.suggested_endpoint,
        "ws://[::1]:9002/devtools/browser"
    );
    assert_eq!(c.browser.idle_yield, Duration::from_millis(21));
    assert_eq!(c.browser.yield_timeout, Duration::from_millis(22));
    assert_eq!(c.browser.keep_alive(), Duration::from_secs(510));
}

#[test]
fn local_addresses_only_and_retired_flags_are_rejected() {
    for bind in [
        "localhost:12",
        "127.0.0.2:12",
        "0.0.0.0:12",
        "[::]:12",
        "127.0.0.1",
    ] {
        assert!(matches!(
            parse(&["--bind", bind], &[]),
            Err(ConfigError::InvalidBind)
        ));
    }
    for url in [
        "https://127.0.0.1:12",
        "http://example.test:12",
        "http://127.0.0.1",
        "ws://name:password@127.0.0.1:12",
    ] {
        assert!(matches!(
            parse(&["--cdp-url", url], &[]),
            Err(ConfigError::InvalidCdp(_))
        ));
    }
    for flag in ["--dev", "--registry-sock", "--http", "unknown"] {
        assert!(matches!(parse(&[flag], &[]), Err(ConfigError::UnknownFlag)));
    }
    assert!(matches!(
        parse(&["--cdp-url", "--bind"], &[]),
        Err(ConfigError::MissingValue("KITESURF_CDP_URL"))
    ));
}

#[test]
fn rejects_empty_secret_missing_home_and_out_of_range_budgets() {
    assert!(matches!(
        parse(&["--secret", " "], &[]),
        Err(ConfigError::EmptyValue("PERI_BROWSER_GATEWAY_SECRET"))
    ));
    assert!(matches!(
        Config::parse_from([], |_| None),
        Err(ConfigError::MissingHome)
    ));
    for value in ["0", "-1", "1.5", "86400001", "18446744073709551616"] {
        assert!(matches!(
            parse(&[], &[("PERI_BROWSER_PAGE_WAIT_MS", value)]),
            Err(ConfigError::InvalidTimeout("PERI_BROWSER_PAGE_WAIT_MS"))
        ));
    }
    let browser = parse(&[], &[("PERI_BROWSER_PAGE_WAIT_MS", "86400000")])
        .unwrap()
        .browser;
    assert!(browser.keep_alive() >= browser.structural_budget() + Duration::from_secs(60));
}

#[test]
fn absent_secret_is_unique_to_each_start() {
    let a = parse(&[], &[]).unwrap().secret;
    let b = parse(&[], &[]).unwrap().secret;
    assert_eq!(a.len(), 32);
    assert_eq!(b.len(), 32);
    assert!(a != b);
}
