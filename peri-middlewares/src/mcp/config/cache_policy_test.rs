use super::*;

#[test]
fn cache_setting_rejects_null_and_non_boolean_values() {
    assert_eq!(McpConfigFile::default().mcp_cache, None);
    for enabled in [true, false] {
        let config: McpConfigFile =
            serde_json::from_value(serde_json::json!({"mcpCache": enabled})).unwrap();
        assert_eq!(config.mcp_cache, Some(enabled));
    }
    for value in [
        serde_json::Value::Null,
        serde_json::json!("false"),
        serde_json::json!(0),
        serde_json::json!({}),
    ] {
        assert!(
            serde_json::from_value::<McpConfigFile>(serde_json::json!({"mcpCache": value}))
                .is_err()
        );
    }
}

#[test]
fn cache_environment_boolean_parser_is_strict_and_redacted() {
    assert_eq!(cache_policy::parse_environment(None).unwrap(), None);
    for value in ["true", "1", "on", " TRUE ", "On"] {
        assert_eq!(
            cache_policy::parse_environment(Some(value)).unwrap(),
            Some(true)
        );
    }
    for value in ["false", "0", "off", " FALSE ", "Off"] {
        assert_eq!(
            cache_policy::parse_environment(Some(value)).unwrap(),
            Some(false)
        );
    }
    for value in ["", "auto", "secret-invalid-value"] {
        let error = cache_policy::parse_environment(Some(value)).unwrap_err();
        assert!(matches!(error, McpConfigError::InvalidCacheEnvironment));
        assert!(!error.to_string().contains("secret-invalid-value"));
    }
}

#[test]
fn cache_merge_is_disable_dominant_across_all_sources() {
    for global in [None, Some(true), Some(false)] {
        for project in [None, Some(true), Some(false)] {
            for environment in [None, Some(true), Some(false)] {
                let settings = [global, project, environment];
                let expected = if settings.contains(&Some(false)) {
                    Some(false)
                } else if settings.contains(&Some(true)) {
                    Some(true)
                } else {
                    None
                };
                assert_eq!(cache_policy::merge_settings(&settings), expected);
            }
        }
    }
}

#[test]
fn cache_only_files_are_merged_without_changing_builtin_overlay() {
    let directory = tempfile::tempdir().unwrap();
    let global_path = directory.path().join("settings.json");
    let project_path = directory.path().join(".mcp.json");
    let claude_home = directory.path().join("claude");
    for global in [true, false] {
        for project in [true, false] {
            std::fs::write(
                &global_path,
                serde_json::json!({"mcpCache": global}).to_string(),
            )
            .unwrap();
            std::fs::write(
                &project_path,
                serde_json::json!({"mcpCache": project}).to_string(),
            )
            .unwrap();
            let (config, _) = load_merged_config_full_with_paths(
                directory.path(),
                &claude_home,
                &global_path,
                &super::super::builtin::BuiltinInjectionPolicy::all(),
            )
            .unwrap();
            assert_eq!(config.mcp_cache, Some(global && project));
            assert!(config.mcp_servers.contains_key("workspace"));
            assert!(config.mcp_servers.contains_key("web"));
        }
    }
}

#[test]
fn global_cache_setting_uses_nested_precedence_and_validates_shadowed_values() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    std::fs::write(&path, r#"{"mcpCache":false,"config":{"mcpCache":true}}"#).unwrap();
    assert_eq!(load_global_config(&path).unwrap().mcp_cache, Some(true));
    std::fs::write(&path, r#"{"mcpCache":false,"config":{"mcpServers":{}}}"#).unwrap();
    assert_eq!(load_global_config(&path).unwrap().mcp_cache, Some(false));
    for content in [
        r#"{"mcpCache":null,"config":{"mcpCache":false}}"#,
        r#"{"mcpCache":true,"config":{"mcpCache":"false"}}"#,
    ] {
        std::fs::write(&path, content).unwrap();
        assert!(load_global_config(&path).is_err());
    }
}

#[test]
fn updating_server_disabled_preserves_cache_policy_and_rejects_invalid_policy() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    std::fs::write(
        &path,
        r#"{"mcpCache":false,"mcpServers":{"external":{"command":"echo"}}}"#,
    )
    .unwrap();
    set_server_disabled_with_paths(directory.path(), &path, "external", true).unwrap();
    let config = load_global_config(&path).unwrap();
    assert_eq!(config.mcp_cache, Some(false));
    assert_eq!(config.mcp_servers["external"].disabled, Some(true));
    let invalid = r#"{"mcpCache":null,"mcpServers":{"external":{"command":"echo"}}}"#;
    std::fs::write(&path, invalid).unwrap();
    assert!(set_server_disabled_with_paths(directory.path(), &path, "external", true).is_err());
    assert_eq!(std::fs::read_to_string(path).unwrap(), invalid);
}

#[test]
fn cache_environment_is_applied_by_the_real_provider_in_isolated_processes() {
    for (environment, global) in [
        ("off", "true"),
        ("on", "false"),
        ("on", "true"),
        ("invalid", "true"),
    ] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "mcp::config::cache_policy_tests::cache_environment_process_probe",
                "--ignored",
            ])
            .env("PERI_MCP_CACHE", environment)
            .env("PERI_MCP_BUILTIN", "off")
            .env("PERI_CACHE_TEST_GLOBAL", global)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
#[ignore = "invoked in an isolated process by the environment source test"]
fn cache_environment_process_probe() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    let global = std::env::var("PERI_CACHE_TEST_GLOBAL")
        .unwrap()
        .parse::<bool>()
        .unwrap();
    std::fs::write(&path, serde_json::json!({"mcpCache": global}).to_string()).unwrap();
    std::fs::write(directory.path().join(".mcp.json"), r#"{"mcpCache":true}"#).unwrap();
    peri_config::io::set_global_config_path(Some(path));
    let environment = std::env::var("PERI_MCP_CACHE").unwrap();
    let config = load_merged_config_full(directory.path(), &directory.path().join("claude"));
    let bare = load_bare_config();
    if environment == "invalid" {
        assert!(matches!(
            config,
            Err(McpConfigError::InvalidCacheEnvironment)
        ));
        assert!(matches!(bare, Err(McpConfigError::InvalidCacheEnvironment)));
    } else {
        assert_eq!(
            config.unwrap().0.mcp_cache,
            Some(environment == "on" && global)
        );
        assert_eq!(bare.unwrap().mcp_cache, Some(environment == "on"));
    }
}
