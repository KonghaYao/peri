use std::path::Path;

use peri_config::{ConfigurationInputs, ConfigurationScope, ConfigurationSnapshot};

use super::*;

fn snapshot_for(
    cwd: &Path,
    global_path: &Path,
    global: &str,
    project: &str,
    environment: HashMap<String, String>,
) -> ConfigurationSnapshot {
    let scope = ConfigurationScope::new(cwd.to_path_buf(), global_path.to_path_buf()).unwrap();
    ConfigurationSnapshot::resolve(
        scope,
        ConfigurationInputs {
            global: Some(global.to_string()),
            project: Some(project.to_string()),
            environment: environment.into_iter().collect(),
            ..Default::default()
        },
    )
    .unwrap()
}

fn install_enabled_plugin(claude_home: &Path) {
    use crate::plugin::types::{InstallScope, InstalledPlugin, InstalledPlugins};
    use crate::plugin::PluginOrigin;

    let plugin_dir = claude_home.join("plugins/cache/market/sample/1.0.0");
    std::fs::create_dir_all(plugin_dir.join(".claude-plugin")).unwrap();
    std::fs::write(
        plugin_dir.join(".claude-plugin/plugin.json"),
        r#"{"name":"sample","version":"1.0.0","mcpServers":{"duplicate":{"command":"frozen-global"},"unique":{"command":"frozen-plugin"}}}"#,
    )
    .unwrap();
    std::fs::create_dir_all(claude_home.join("plugins")).unwrap();
    let installed = InstalledPlugins {
        version: 2,
        plugins: vec![InstalledPlugin {
            id: "sample@market".to_string(),
            name: "sample".to_string(),
            version: "1.0.0".to_string(),
            marketplace: "market".to_string(),
            install_path: plugin_dir,
            scope: InstallScope::User,
            project_path: None,
            origin: PluginOrigin::PeriInstalled,
        }],
    };
    std::fs::write(
        claude_home.join("plugins/installed_plugins.json"),
        serde_json::to_string(&installed).unwrap(),
    )
    .unwrap();
    std::fs::write(
        claude_home.join("settings.json"),
        r#"{"enabledPlugins":["sample@market"]}"#,
    )
    .unwrap();
}

#[test]
fn deployment_can_skip_plugin_discovery_and_builtin_overlay() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let claude_home = temp.path().join("claude");
    install_enabled_plugin(&claude_home);
    let snapshot = snapshot_for(
        &cwd,
        &temp.path().join("settings.json"),
        "{}",
        "{}",
        HashMap::new(),
    );
    let (config, plugin_sources) = load_merged_config_from_snapshot_with_capabilities(
        &cwd,
        &claude_home,
        &snapshot,
        false,
        false,
    )
    .unwrap();
    assert!(config.mcp_servers.is_empty());
    assert!(plugin_sources.is_empty());
}

#[test]
fn snapshot_loader_rejects_a_mismatched_workspace_before_loading_plugins() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("project");
    let other_cwd = temp.path().join("other");
    let global_path = temp.path().join("settings.json");
    let snapshot = snapshot_for(&cwd, &global_path, "{}", "{}", HashMap::new());
    let error = load_merged_config_from_snapshot(
        &other_cwd,
        &temp.path().join("missing-plugins"),
        &snapshot,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        McpConfigError::SnapshotScopeMismatch { .. }
    ));
}

#[test]
fn snapshot_loader_freezes_files_and_environment_and_merges_plugins() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "mcp::config::snapshot_tests::snapshot_loader_process_probe",
            "--ignored",
        ])
        .env("PERI_MCP_CACHE", "on")
        .env("PERI_MCP_BUILTIN", "on")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "invoked in an isolated process by the snapshot freeze test"]
fn snapshot_loader_process_probe() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("project");
    let claude_home = temp.path().join("claude");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&claude_home).unwrap();
    let global_path = temp.path().join("settings.json");
    let project_path = cwd.join(".mcp.json");
    let plugin_dir = claude_home.join("plugins/cache/market/sample/1.0.0");
    let plugin_data = plugin_dir.join(".claude-plugin/data");
    let global = serde_json::json!({
        "mcpServers": {
            "global": {
                "command": "frozen-global",
                "env": {
                    "CLAUDE_PLUGIN_ROOT": plugin_dir.to_string_lossy(),
                    "CLAUDE_PLUGIN_DATA": plugin_data.to_string_lossy()
                }
            },
            "shared": {"command": "global-shared"}
        },
        "mcpCache": true
    })
    .to_string();
    let project = r#"{"mcpServers":{"project":{"command":"frozen-project"},"shared":{"command":"project-shared"}}}"#;
    std::fs::write(&global_path, &global).unwrap();
    std::fs::write(&project_path, project).unwrap();
    install_enabled_plugin(&claude_home);
    let snapshot = snapshot_for(
        &cwd,
        &global_path,
        &global,
        project,
        HashMap::from([
            (MCP_CACHE_ENV.to_string(), "off".to_string()),
            ("PERI_MCP_BUILTIN".to_string(), "off".to_string()),
        ]),
    );
    std::fs::write(&global_path, "invalid-global-after-snapshot").unwrap();
    std::fs::write(&project_path, "invalid-project-after-snapshot").unwrap();

    let (config, plugin_sources) =
        load_merged_config_from_snapshot(&cwd, &claude_home, &snapshot).unwrap();
    assert_eq!(config.mcp_cache, Some(false));
    assert_eq!(
        McpCachePolicy::from_setting(config.mcp_cache),
        McpCachePolicy::Disabled
    );
    assert_eq!(
        config.mcp_servers["global"].command.as_deref(),
        Some("frozen-global")
    );
    assert_eq!(
        config.mcp_servers["project"].command.as_deref(),
        Some("frozen-project")
    );
    assert_eq!(
        config.mcp_servers["shared"].command.as_deref(),
        Some("project-shared")
    );
    assert!(config.mcp_servers.contains_key("plugin:sample:unique"));
    assert!(!config.mcp_servers.contains_key("plugin:sample:duplicate"));
    assert_eq!(
        plugin_sources.get("plugin:sample:unique").unwrap(),
        "sample@market"
    );
    assert!(!config.mcp_servers.contains_key("workspace"));

    let bare = load_bare_config_from_snapshot(&snapshot).unwrap();
    assert_eq!(bare.mcp_cache, Some(false));
    assert!(bare.mcp_servers.is_empty());

    let enabled_snapshot = snapshot_for(
        &cwd,
        &global_path,
        "{}",
        "{}",
        HashMap::from([
            (MCP_CACHE_ENV.to_string(), "off".to_string()),
            ("PERI_MCP_BUILTIN".to_string(), "on".to_string()),
        ]),
    );
    let enabled_bare = load_bare_config_from_snapshot(&enabled_snapshot).unwrap();
    assert!(enabled_bare.mcp_servers.contains_key("workspace"));
    assert_eq!(enabled_bare.mcp_servers.len(), 1);
}
