use std::collections::{BTreeMap, HashMap};

use super::parse_global;
use super::parse_project;
use super::McpCachePolicy;
use super::McpConfigError;
use super::MCP_BUILTIN_ENV;
use super::MCP_CACHE_ENV;

#[test]
fn global_prefers_nested_servers_but_validates_both_candidates() {
    let value = serde_json::json!({
        "config": {"mcpServers": {"chosen": {"command": "echo"}}},
        "mcpServers": {"fallback": {"command": "uvx"}}
    });
    let parsed = super::parse_global(&value).unwrap();
    assert!(parsed.mcp_servers.contains_key("chosen"));
    assert!(!parsed.mcp_servers.contains_key("fallback"));

    let invalid_shadowed = serde_json::json!({
        "config": {"mcpServers": {"chosen": {"command": "echo"}}},
        "mcpServers": {"bad": {"system_mcp_tools": []}}
    });
    assert!(super::parse_global(&invalid_shadowed).is_err());
}

#[test]
fn cache_policy_parses_environment_and_disable_wins() {
    let empty = BTreeMap::new();
    let mut environment = BTreeMap::from([(MCP_CACHE_ENV.to_string(), " ON ".to_string())]);
    let result = super::resolve(
        &serde_json::json!({"config": {"mcpCache": true}}),
        &serde_json::json!({"mcpCache": false}),
        &HashMap::new(),
        &environment,
    )
    .unwrap();
    assert_eq!(result.mcp_cache, Some(false));
    assert_eq!(
        McpCachePolicy::from_setting(result.mcp_cache),
        McpCachePolicy::Disabled
    );

    environment.insert(MCP_CACHE_ENV.to_string(), "invalid-secret".to_string());
    assert!(matches!(
        super::resolve(
            &serde_json::json!({}),
            &serde_json::json!({}),
            &HashMap::new(),
            &environment
        ),
        Err(McpConfigError::InvalidCacheEnvironment)
    ));
    assert_eq!(
        super::resolve(
            &serde_json::json!({}),
            &serde_json::json!({}),
            &HashMap::new(),
            &empty
        )
        .unwrap()
        .mcp_cache,
        None
    );
}

#[test]
fn builtin_switch_disables_only_for_off_and_zero() {
    assert!(super::builtin_enabled(&BTreeMap::new()));
    for value in ["off", " OFF ", "0"] {
        assert!(!super::builtin_enabled(&BTreeMap::from([(
            MCP_BUILTIN_ENV.to_string(),
            value.to_string(),
        )])));
    }
    for value in ["on", "false", "", "unknown"] {
        assert!(super::builtin_enabled(&BTreeMap::from([(
            MCP_BUILTIN_ENV.to_string(),
            value.to_string(),
        )])));
    }
}

#[test]
fn builtin_assembly_mode_distinguishes_legacy_and_single_exit() {
    use peri_acp_types::builtin_mcp::BuiltinAssemblyMode;

    assert_eq!(
        super::builtin_assembly_mode(&BTreeMap::new()),
        BuiltinAssemblyMode::SingleExit
    );
    for (setting, expected) in [
        ("legacy", BuiltinAssemblyMode::LegacyInstances),
        ("V4", BuiltinAssemblyMode::LegacyInstances),
        ("v5", BuiltinAssemblyMode::SingleExit),
        ("off", BuiltinAssemblyMode::Disabled),
    ] {
        assert_eq!(
            super::builtin_assembly_mode(&BTreeMap::from([(
                MCP_BUILTIN_ENV.to_string(),
                setting.to_string()
            )])),
            expected
        );
    }
}

#[test]
fn plugin_servers_are_namespaced_and_deduplicated_against_manual_servers() {
    assert_eq!(
        super::plugin_server_name("sample", "search"),
        "plugin:sample:search"
    );
    let shared = serde_json::json!({
        "mcpServers": {"manual": {"command": "echo", "args": ["same"]}}
    });
    let plugins = HashMap::from([
        (
            "plugin:sample:duplicate".to_string(),
            serde_json::from_value(serde_json::json!({
                "command": "echo", "args": ["same"]
            }))
            .unwrap(),
        ),
        (
            "plugin:sample:distinct".to_string(),
            serde_json::from_value(serde_json::json!({"command": "other"})).unwrap(),
        ),
    ]);
    let projection =
        super::resolve(&shared, &serde_json::json!({}), &plugins, &BTreeMap::new()).unwrap();
    assert!(!projection
        .mcp_servers
        .contains_key("plugin:sample:duplicate"));
    assert!(projection
        .mcp_servers
        .contains_key("plugin:sample:distinct"));
}

#[test]
fn project_config_rejects_non_boolean_cache_and_invalid_credentials() {
    assert!(super::parse_project(&serde_json::json!({"mcpCache": "false"})).is_err());
    let invalid_credentials = serde_json::json!({
        "mcpServers": {
            "http": {
                "url": "https://example.test",
                "oauth": {"clientSecret": 123}
            }
        }
    });
    assert!(super::parse_project(&invalid_credentials).is_err());
}

#[test]
fn parsing_errors_identify_invalid_fields() {
    for (document, field) in [
        (serde_json::json!({"mcpCache": "hidden-value"}), "mcpCache"),
        (
            serde_json::json!({"mcpServers": {"server": {"env": "hidden-value"}}}),
            "env",
        ),
    ] {
        for parse in [parse_global, parse_project] {
            let error = parse(&document).unwrap_err();
            let diagnostic = format!("{error} {error:?}");
            assert!(
                diagnostic.contains(field) || diagnostic.contains("hidden-value"),
                "{error:?}"
            );
        }
    }
}
