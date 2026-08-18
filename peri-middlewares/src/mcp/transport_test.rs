use super::*;

fn test_config() -> McpServerConfig {
    McpServerConfig {
        command: None,
        args: None,
        env: None,
        url: None,
        headers: None,
        oauth: None,
        disabled: None,
        protocol_version: None,
        subscriptions: None,
        source: None,
    }
}

fn stdio_config() -> McpServerConfig {
    McpServerConfig {
        command: Some("echo".to_string()),
        args: Some(vec!["hello".to_string()]),
        env: Some(HashMap::from([("KEY".to_string(), "val".to_string())])),
        ..test_config()
    }
}

fn http_config() -> McpServerConfig {
    McpServerConfig {
        url: Some("https://example.com/mcp".to_string()),
        headers: Some(HashMap::from([(
            "Auth".to_string(),
            "Bearer token".to_string(),
        )])),
        ..test_config()
    }
}

#[test]
fn test_try_from_stdio_config() {
    let config = stdio_config();
    let tc = TransportConfig::try_from(&config).unwrap();
    match tc {
        TransportConfig::Stdio { command, args, env } => {
            assert_eq!(command, "echo");
            assert_eq!(args, vec!["hello"]);
            assert_eq!(env.get("KEY").unwrap(), "val");
        }
        _ => panic!("Expected Stdio"),
    }
}

#[test]
fn test_try_from_http_config() {
    let config = http_config();
    let tc = TransportConfig::try_from(&config).unwrap();
    match tc {
        TransportConfig::StreamableHttp {
            url,
            headers,
            oauth,
        } => {
            assert_eq!(url, "https://example.com/mcp");
            assert_eq!(headers.get("Auth").unwrap(), "Bearer token");
            assert!(oauth.is_none());
        }
        _ => panic!("Expected StreamableHttp"),
    }
}

#[test]
fn test_try_from_empty_config() {
    let config = test_config();
    let result = TransportConfig::try_from(&config);
    assert!(matches!(result, Err(TransportError::InvalidConfig)));
}

#[test]
fn test_try_from_stdio_priority_over_url() {
    let config = McpServerConfig {
        command: Some("npx".to_string()),
        url: Some("https://example.com".to_string()),
        ..test_config()
    };
    let tc = TransportConfig::try_from(&config).unwrap();
    assert!(matches!(tc, TransportConfig::Stdio { .. }));
}

#[test]
fn test_try_from_defaults() {
    let config = McpServerConfig {
        command: Some("cat".to_string()),
        ..test_config()
    };
    let tc = TransportConfig::try_from(&config).unwrap();
    match tc {
        TransportConfig::Stdio { args, env, .. } => {
            assert!(args.is_empty());
            assert!(env.is_empty());
        }
        _ => panic!("Expected Stdio"),
    }
}

#[test]
fn test_build_transport_returns_config() {
    let config = stdio_config();
    let result = TransportConfig::try_from(&config);
    assert!(result.is_ok());
}

#[test]
fn test_build_transport_invalid() {
    let config = test_config();
    let result = TransportConfig::try_from(&config);
    assert!(result.is_err());
}

#[test]
fn test_oauth_field_populated_when_enabled() {
    let config = McpServerConfig {
        url: Some("https://example.com".into()),
        oauth: Some(super::super::config::OAuthConfig {
            client_id: Some("app".into()),
            ..Default::default()
        }),
        ..test_config()
    };
    let tc = TransportConfig::try_from(&config).unwrap();
    match tc {
        TransportConfig::StreamableHttp { oauth, .. } => {
            assert!(oauth.is_some());
        }
        _ => panic!("Expected StreamableHttp"),
    }
}

#[test]
fn test_oauth_field_skipped_when_disabled() {
    let config = McpServerConfig {
        url: Some("https://example.com".into()),
        oauth: Some(super::super::config::OAuthConfig {
            enabled: Some(false),
            ..Default::default()
        }),
        ..test_config()
    };
    let tc = TransportConfig::try_from(&config).unwrap();
    match tc {
        TransportConfig::StreamableHttp { oauth, .. } => {
            assert!(oauth.is_none());
        }
        _ => panic!("Expected StreamableHttp"),
    }
}
