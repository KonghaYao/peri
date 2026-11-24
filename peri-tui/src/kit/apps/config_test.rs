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
