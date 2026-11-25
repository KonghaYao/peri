use super::*;

fn availability(gate: &str, url: Option<&str>, token: Option<&str>) -> AppsAvailability {
    resolve_for_test(gate, url, token)
}

#[test]
fn gate_off_disables_everything_even_with_config() {
    for gate in ["", "0", "false", "off", "nope"] {
        let resolved = availability(gate, Some("http://127.0.0.1:7391"), Some("secret"));
        assert!(
            matches!(resolved, AppsAvailability::Disabled),
            "gate={gate:?}"
        );
        assert!(resolved.enabled().is_none());
    }
}

#[test]
fn gate_on_requires_url_and_token() {
    for gate in ["1", "true", "TRUE", " yes ", "On"] {
        assert!(matches!(
            availability(gate, None, Some("secret")),
            AppsAvailability::Unconfigured {
                missing: VIEW_URL_ENV
            }
        ));
        assert!(matches!(
            availability(gate, Some("http://127.0.0.1:7391"), None),
            AppsAvailability::Unconfigured {
                missing: VIEW_TOKEN_ENV
            }
        ));
        assert!(matches!(
            availability(gate, Some("   "), Some("secret")),
            AppsAvailability::Unconfigured {
                missing: VIEW_URL_ENV
            }
        ));
        assert!(matches!(
            availability(gate, Some("http://127.0.0.1:7391"), Some("  ")),
            AppsAvailability::Unconfigured {
                missing: VIEW_TOKEN_ENV
            }
        ));
    }
}

#[test]
fn enabled_config_normalizes_url_and_redacts_token() {
    let AppsAvailability::Enabled(config) =
        availability("1", Some("http://127.0.0.1:7391/"), Some(" secret "))
    else {
        panic!("must be enabled");
    };
    assert_eq!(config.view_url, "http://127.0.0.1:7391");
    assert_eq!(config.token.expose(), "secret");
    assert_eq!(config.discovery_interval, DISCOVERY_INTERVAL);
    // Debug 不泄漏 token。
    let debug = format!("{config:?}");
    assert!(!debug.contains("secret"), "{debug}");
    assert!(debug.contains("<redacted>"), "{debug}");
}

#[test]
fn invalid_urls_are_treated_as_missing() {
    for url in [
        "ftp://127.0.0.1:7391",
        "not a url",
        "http://user:pass@127.0.0.1:7391",
        "http://user@127.0.0.1:7391",
    ] {
        assert!(
            matches!(
                availability("1", Some(url), Some("secret")),
                AppsAvailability::Unconfigured {
                    missing: VIEW_URL_ENV
                }
            ),
            "{url}"
        );
    }
}

#[test]
fn https_and_trailing_slashes_are_accepted() {
    let AppsAvailability::Enabled(config) =
        availability("on", Some("https://gw.example/"), Some("token"))
    else {
        panic!("must be enabled");
    };
    assert_eq!(config.view_url, "https://gw.example");
}

/// A4：生产入口读完 token 后必须把它从进程环境移除，避免被模型可执行的
/// Bash / MCP stdio 子进程继承（同一份凭据能驱动宿主私有控制面）。
#[test]
#[serial_test::serial]
fn take_from_env_scrubs_token_after_capturing_it() {
    const GATE: &str = "PERI_MCP_APPS_TUI";
    let saved_gate = std::env::var_os(GATE);
    let saved_url = std::env::var_os(VIEW_URL_ENV);
    let saved_token = std::env::var_os(VIEW_TOKEN_ENV);
    // SAFETY: serial 测试独占本进程环境；断言后再恢复原值。
    unsafe {
        std::env::set_var(GATE, "1");
        std::env::set_var(VIEW_URL_ENV, "http://127.0.0.1:7391");
        std::env::set_var(VIEW_TOKEN_ENV, "scrub-secret");
    }
    let availability = take_from_env();
    let token_in_env = std::env::var_os(VIEW_TOKEN_ENV);
    // SAFETY: 同上，恢复进入测试前的环境，避免污染其他测试。
    unsafe {
        match saved_gate {
            Some(value) => std::env::set_var(GATE, value),
            None => std::env::remove_var(GATE),
        }
        match saved_url {
            Some(value) => std::env::set_var(VIEW_URL_ENV, value),
            None => std::env::remove_var(VIEW_URL_ENV),
        }
        match saved_token {
            Some(value) => std::env::set_var(VIEW_TOKEN_ENV, value),
            None => std::env::remove_var(VIEW_TOKEN_ENV),
        }
    }
    let AppsAvailability::Enabled(config) = availability else {
        panic!("gate/url/token 齐全时必须启用");
    };
    assert_eq!(config.token.expose(), "scrub-secret", "配置仍需持有凭据");
    assert!(
        token_in_env.is_none(),
        "token 不能继续留在子进程可见的环境里"
    );
}

/// 开关关闭时也可能导出过 token（例如先启动网关）；凭据不能留给子进程。
#[test]
#[serial_test::serial]
fn take_from_env_scrubs_token_even_when_gate_is_off() {
    const GATE: &str = "PERI_MCP_APPS_TUI";
    let saved_gate = std::env::var_os(GATE);
    let saved_token = std::env::var_os(VIEW_TOKEN_ENV);
    // SAFETY: serial 测试独占本进程环境；断言后再恢复原值。
    unsafe {
        std::env::set_var(GATE, "0");
        std::env::set_var(VIEW_TOKEN_ENV, "gate-off-secret");
    }
    let availability = take_from_env();
    let token_in_env = std::env::var_os(VIEW_TOKEN_ENV);
    // SAFETY: 同上，恢复进入测试前的环境。
    unsafe {
        match saved_gate {
            Some(value) => std::env::set_var(GATE, value),
            None => std::env::remove_var(GATE),
        }
        match saved_token {
            Some(value) => std::env::set_var(VIEW_TOKEN_ENV, value),
            None => std::env::remove_var(VIEW_TOKEN_ENV),
        }
    }
    assert!(matches!(availability, AppsAvailability::Disabled));
    assert!(token_in_env.is_none(), "开关关闭也必须移除宿主凭据");
}
