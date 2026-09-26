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
        system_mcp: None,
        system_mcp_tools: None,
        system_mcp_timeout: None,
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

#[test]
fn test_system_mcp_transport_rejects_invalid_typed_config() {
    // 公开 struct 可手工构造：Deserialize 不是唯一闸门，建传输前同样要过契约校验。
    for tools in [Some(Vec::new()), Some(vec!["search".to_string()])] {
        let config = McpServerConfig {
            command: Some("npx".to_string()),
            system_mcp: Some(false),
            system_mcp_tools: tools,
            ..test_config()
        };
        let result = TransportConfig::try_from(&config);
        assert!(
            matches!(
                result,
                Err(TransportError::InvalidSystemConfig(
                    McpServerConfigValidationError::SystemMcpToolsRequiresSystemMcp
                ))
            ),
            "手工构造的非法 System 配置必须被传输层拒绝"
        );
    }

    // 合法组合不受影响。
    let valid = McpServerConfig {
        command: Some("npx".to_string()),
        system_mcp: Some(true),
        system_mcp_tools: Some(Vec::new()),
        ..test_config()
    };
    assert!(TransportConfig::try_from(&valid).is_ok());
}

// ── builtin（IF-D1）──────────────────────────────────────────────────────────

/// builtin 身份唯一来源 = `source`（运行时标记），且优先于 command / url 判定。
fn builtin_config(instance: &str) -> McpServerConfig {
    McpServerConfig {
        source: Some(ConfigSource::Builtin {
            instance: instance.to_string(),
        }),
        ..test_config()
    }
}

#[test]
fn test_try_from_builtin_source_marker() {
    let config = builtin_config("web");
    let tc = TransportConfig::try_from(&config).expect("builtin 标记必须建立 Builtin 传输");
    match &tc {
        TransportConfig::Builtin { instance } => assert_eq!(instance, "web"),
        _ => panic!("Expected Builtin"),
    }
    assert_eq!(tc.kind(), TransportKind::Builtin);

    // 声明了 command / url 也不改变判定：source 是身份的单一事实源。
    let config = McpServerConfig {
        command: Some("npx".to_string()),
        url: Some("https://example.com/mcp".to_string()),
        ..builtin_config("artifact")
    };
    match TransportConfig::try_from(&config).unwrap() {
        TransportConfig::Builtin { instance } => assert_eq!(instance, "artifact"),
        _ => panic!("Expected Builtin（source 优先于 command / url）"),
    }

    // 非 builtin 的 source 不改变既有分支。
    let mut project = stdio_config();
    project.source = Some(ConfigSource::Project(std::path::PathBuf::from(
        "/tmp/.mcp.json",
    )));
    assert!(matches!(
        TransportConfig::try_from(&project).unwrap(),
        TransportConfig::Stdio { .. }
    ));
}

#[test]
fn test_builtin_unknown_instance_is_typed_error() {
    // 已实现实例可解析；预留未实现名与未知名一律 typed error（不含路径 / env / 凭据）。
    assert!(require_known_builtin_instance("web").is_ok());
    assert!(require_known_builtin_instance("artifact").is_ok());
    for unknown in ["cron", "lsp", "workspace", "some-user-server", ""] {
        let err = require_known_builtin_instance(unknown).expect_err("未注册实例名必须被拒绝");
        assert!(matches!(err, TransportError::UnknownBuiltinInstance { .. }));
        let text = err.to_string();
        assert!(
            text.contains("builtin MCP 实例未注册"),
            "错误分类文本缺失: {text}"
        );
        if !unknown.is_empty() {
            assert!(text.contains(unknown), "错误必须含实例名: {text}");
        }
    }
}

#[test]
fn test_stdin_and_http_branches_unchanged() {
    // 三分类不得改变既有两臂：形态、字段与 kind 逐位保持。
    let stdio = TransportConfig::try_from(&stdio_config()).unwrap();
    match &stdio {
        TransportConfig::Stdio { command, args, env } => {
            assert_eq!(command, "echo");
            assert_eq!(args, &vec!["hello".to_string()]);
            assert_eq!(env.get("KEY").map(String::as_str), Some("val"));
        }
        _ => panic!("Expected Stdio"),
    }
    assert_eq!(stdio.kind(), TransportKind::Stdio);

    let http = TransportConfig::try_from(&http_config()).unwrap();
    match &http {
        TransportConfig::StreamableHttp {
            url,
            headers,
            oauth,
        } => {
            assert_eq!(url, "https://example.com/mcp");
            assert_eq!(
                headers.get("Auth").map(String::as_str),
                Some("Bearer token")
            );
            assert!(oauth.is_none());
        }
        _ => panic!("Expected StreamableHttp"),
    }
    assert_eq!(http.kind(), TransportKind::Http);

    // (None, None) 仍是 InvalidConfig，不得被 builtin 分支吃掉。
    assert!(matches!(
        TransportConfig::try_from(&test_config()),
        Err(TransportError::InvalidConfig)
    ));

    // 三分类常量的冻结取值（A16/IF-D1：builtin 同进程握手，5 s 足够）。
    assert_eq!(BUILTIN_CONNECT_TIMEOUT, std::time::Duration::from_secs(5));
    assert_ne!(
        BUILTIN_CONNECT_TIMEOUT,
        super::super::client::STDIO_CONNECT_TIMEOUT
    );
    assert_ne!(
        BUILTIN_CONNECT_TIMEOUT,
        super::super::client::HTTP_CONNECT_TIMEOUT
    );
}
