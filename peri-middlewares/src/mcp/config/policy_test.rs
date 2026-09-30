use super::*;
use crate::mcp::builtin::BuiltinInjectionPolicy;
use peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES;

#[test]
fn explicit_policy_gates_defaults_without_gating_configuration_io() {
    let directory = tempfile::tempdir().unwrap();
    let global_path = directory.path().join("settings.json");
    let claude_home = directory.path().join("claude");
    std::fs::write(
        &global_path,
        r#"{"mcpServers":{"external":{"command":"echo"}}}"#,
    )
    .unwrap();

    for (policy, enabled) in [
        (BuiltinInjectionPolicy::all(), true),
        (BuiltinInjectionPolicy::none(), false),
    ] {
        let (config, _) = load_merged_config_full_with_paths(
            directory.path(),
            &claude_home,
            &global_path,
            &policy,
        )
        .unwrap();
        assert_eq!(
            config.mcp_servers["external"].source,
            Some(ConfigSource::Global(global_path.clone()))
        );
        for instance in BUILTIN_MCP_INSTANCES {
            assert_eq!(config.mcp_servers.contains_key(instance.name), enabled);
        }
    }
}

#[test]
fn builtin_closure_list_survives_configuration_reads_and_disabled_updates() {
    let directory = tempfile::tempdir().unwrap();
    let global_path = directory.path().join("settings.json");
    let claude_home = directory.path().join("claude");
    let closures: serde_json::Map<String, serde_json::Value> = BUILTIN_MCP_INSTANCES
        .iter()
        .map(|instance| {
            (
                instance.name.to_string(),
                serde_json::json!({"disabled": true}),
            )
        })
        .collect();
    std::fs::write(
        &global_path,
        serde_json::json!({"mcpServers": closures}).to_string(),
    )
    .unwrap();

    let (config, _) = load_merged_config_full_with_paths(
        directory.path(),
        &claude_home,
        &global_path,
        &BuiltinInjectionPolicy::all(),
    )
    .unwrap();
    for instance in BUILTIN_MCP_INSTANCES {
        let entry = &config.mcp_servers[instance.name];
        assert_eq!(entry.disabled, Some(true));
        assert_eq!(entry.system_mcp, None);
        assert_eq!(
            entry.source,
            Some(ConfigSource::Builtin {
                instance: instance.instance.to_string()
            })
        );
    }

    set_server_disabled_with_paths(directory.path(), &global_path, "workspace", false).unwrap();
    let (config, _) = load_merged_config_full_with_paths(
        directory.path(),
        &claude_home,
        &global_path,
        &BuiltinInjectionPolicy::all(),
    )
    .unwrap();
    for instance in BUILTIN_MCP_INSTANCES {
        let entry = &config.mcp_servers[instance.name];
        assert_eq!(
            entry.disabled,
            (instance.name != "workspace").then_some(true)
        );
        assert_eq!(
            entry.system_mcp,
            (instance.name == "workspace").then_some(true)
        );
    }
}

#[test]
fn disabled_policy_does_not_bypass_builtin_admission() {
    let directory = tempfile::tempdir().unwrap();
    let global_path = directory.path().join("settings.json");
    let claude_home = directory.path().join("claude");
    for fragment in [
        serde_json::json!({"command": "echo"}),
        serde_json::json!({"disabled": true, "system_mcp": true}),
    ] {
        std::fs::write(
            &global_path,
            serde_json::json!({"mcpServers": {"workspace": fragment}}).to_string(),
        )
        .unwrap();
        let error = load_merged_config_full_with_paths(
            directory.path(),
            &claude_home,
            &global_path,
            &BuiltinInjectionPolicy::none(),
        )
        .unwrap_err();
        assert!(matches!(error,
            McpConfigError::ReservedBuiltinInstanceName { name }
            | McpConfigError::BuiltinClosureFragmentInvalid { name }
            if name == "workspace"
        ));
    }
}
