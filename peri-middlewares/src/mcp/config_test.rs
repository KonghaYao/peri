use std::path::PathBuf;

use tempfile::NamedTempFile;

use super::*;
use crate::mcp::builtin::BuiltinInjectionPolicy;
use crate::plugin::PluginOrigin;

// builtin 默认层是**显式策略**（IF-D3 / A1）：本文件每个 loader 调用点都必须给出
// `BuiltinInjectionPolicy::all()`（生产语义：注入两个 builtin 实例）或 `none()`
// （断言迁移前行为 / 只验证错误路径：不注入）。测试不得依赖 `PERI_MCP_BUILTIN`。

/// 测试用显式全局路径（不存在 → 空全局配置）：不读真实 `~/.peri/settings.json`。
fn missing_global_path(dir: &Path) -> PathBuf {
    dir.join("global-settings.json")
}

/// 在 `claude_home` 下安装一个已启用插件，`mcp_servers_json` 为 manifest `mcpServers` 的值。
fn install_enabled_plugin(claude_home: &Path, name: &str, mcp_servers_json: &str) -> PathBuf {
    use crate::plugin::types::{InstallScope, InstalledPlugin, InstalledPlugins};

    let plugin_dir = claude_home
        .join("plugins")
        .join("cache")
        .join("mkt")
        .join(name)
        .join("1.0.0");
    std::fs::create_dir_all(plugin_dir.join(".claude-plugin")).unwrap();
    std::fs::write(
        plugin_dir.join(".claude-plugin").join("plugin.json"),
        format!(r#"{{"name":"{name}","version":"1.0.0","mcpServers":{mcp_servers_json}}}"#),
    )
    .unwrap();

    let installed = InstalledPlugins {
        version: 2,
        plugins: vec![InstalledPlugin {
            id: format!("{name}@mkt"),
            name: name.to_string(),
            version: "1.0.0".into(),
            marketplace: "mkt".into(),
            install_path: plugin_dir.clone(),
            scope: InstallScope::User,
            project_path: None,
            origin: PluginOrigin::PeriInstalled,
        }],
    };
    std::fs::create_dir_all(claude_home.join("plugins")).unwrap();
    std::fs::write(
        claude_home.join("plugins").join("installed_plugins.json"),
        serde_json::to_string(&installed).unwrap(),
    )
    .unwrap();
    std::fs::write(
        claude_home.join("settings.json"),
        format!(r#"{{"enabledPlugins":["{name}@mkt"]}}"#),
    )
    .unwrap();
    plugin_dir
}

#[test]
fn test_load_from_nonexistent_path() {
    let result = load_from_path(Path::new("/nonexistent/path/file.json"));
    assert!(result.is_ok());
    assert!(result.unwrap().mcp_servers.is_empty());
}

#[test]
fn test_load_from_valid_json() {
    let mut f = NamedTempFile::new().unwrap();
    std::io::Write::write_all(
        &mut f,
        br#"{"mcpServers":{"fs":{"command":"npx","args":["-y","@mcp/filesystem"]}}}"#,
    )
    .unwrap();
    let config = load_from_path(f.path()).unwrap();
    assert_eq!(config.mcp_servers.len(), 1);
    assert_eq!(config.mcp_servers["fs"].command.as_deref(), Some("npx"));
    assert_eq!(config.mcp_servers["fs"].args.as_ref().unwrap().len(), 2);
}

#[test]
fn test_load_explicit_protocol_version() {
    let mut f = NamedTempFile::new().unwrap();
    std::io::Write::write_all(
        &mut f,
        br#"{"mcpServers":{"next":{"url":"https://example.com/mcp","protocolVersion":"2026-07-28"}}}"#,
    )
    .unwrap();

    let config = load_from_path(f.path()).unwrap();
    assert_eq!(
        config.mcp_servers["next"].protocol_version,
        Some(peri_acp_types::plugin::McpProtocolVersion::V2026_07_28)
    );
}

#[test]
fn test_load_unknown_protocol_version_fails() {
    let mut f = NamedTempFile::new().unwrap();
    std::io::Write::write_all(
        &mut f,
        br#"{"mcpServers":{"invalid":{"url":"https://example.com/mcp","protocolVersion":"unknown"}}}"#,
    )
    .unwrap();

    assert!(load_from_path(f.path()).is_err());
}

#[test]
fn test_load_from_invalid_json() {
    let mut f = NamedTempFile::new().unwrap();
    std::io::Write::write_all(&mut f, b"{invalid json}").unwrap();
    let result = load_from_path(f.path());
    assert!(matches!(result, Err(McpConfigError::ParseError { .. })));
}

#[test]
fn test_load_global_config() {
    let mut f = NamedTempFile::new().unwrap();
    std::io::Write::write_all(
        &mut f,
        br#"{"config":{"mcpServers":{"gh":{"url":"https://api.github.com"}}}}"#,
    )
    .unwrap();
    let config = load_global_config(f.path()).unwrap();
    assert_eq!(config.mcp_servers.len(), 1);
    assert_eq!(
        config.mcp_servers["gh"].url.as_deref(),
        Some("https://api.github.com")
    );
}

#[test]
fn test_load_global_config_top_level() {
    let mut f = NamedTempFile::new().unwrap();
    std::io::Write::write_all(&mut f, br#"{"mcpServers":{"gh":{"command":"npx"}}}"#).unwrap();
    let config = load_global_config(f.path()).unwrap();
    assert_eq!(config.mcp_servers.len(), 1);
    assert_eq!(config.mcp_servers["gh"].command.as_deref(), Some("npx"));
}

#[test]
fn test_expand_env_vars() {
    std::env::set_var("TEST_MCP_VAR", "hello");
    let result = expand_env_vars("prefix_${TEST_MCP_VAR}_suffix");
    assert_eq!(result, "prefix_hello_suffix");
    std::env::remove_var("TEST_MCP_VAR");
}

#[test]
fn test_expand_env_vars_missing() {
    let result = expand_env_vars("${NONEXISTENT_MCP_VAR_12345}");
    assert_eq!(result, "");
}

#[test]
fn test_expand_env_vars_no_braces() {
    let result = expand_env_vars("$NO_BRACE");
    assert_eq!(result, "$NO_BRACE");
}

#[test]
fn test_oauth_config_default_enabled() {
    let config = OAuthConfig::default();
    assert!(config.is_enabled());
}

#[test]
fn test_oauth_config_explicitly_disabled() {
    let config = OAuthConfig {
        enabled: Some(false),
        ..Default::default()
    };
    assert!(!config.is_enabled());
}

#[test]
fn test_oauth_config_deserialize() {
    let json = r#"{"clientId":"my-app","clientSecret":"${MY_SECRET}","scopes":["read","write"]}"#;
    let config: OAuthConfig = serde_json::from_str(json).unwrap();
    assert_eq!(config.client_id.as_deref(), Some("my-app"));
    assert_eq!(config.client_secret.as_deref(), Some("${MY_SECRET}"));
    assert_eq!(config.scopes.as_ref().unwrap().len(), 2);
}

#[test]
fn test_oauth_config_missing_fields() {
    let json = r#"{"clientId":"my-app"}"#;
    let config: OAuthConfig = serde_json::from_str(json).unwrap();
    assert_eq!(config.client_id.as_deref(), Some("my-app"));
    assert!(config.client_secret.is_none());
    assert!(config.scopes.is_none());
    assert!(config.enabled.is_none());
    assert!(config.is_enabled());
}

#[test]
fn test_mcp_server_config_oauth_field() {
    let json = r#"{"url":"https://example.com","oauth":{"clientId":"app"}}"#;
    let config: McpServerConfig = serde_json::from_str(json).unwrap();
    assert!(config.oauth.is_some());
    assert_eq!(config.oauth.unwrap().client_id.as_deref(), Some("app"));
}

#[test]
fn test_mcp_server_config_oauth_default() {
    let json = r#"{"command":"npx"}"#;
    let config: McpServerConfig = serde_json::from_str(json).unwrap();
    assert!(config.oauth.is_none());
}

#[test]
fn test_expand_server_config_oauth_client_secret() {
    std::env::set_var("TEST_OAUTH_SECRET", "secret123");
    let config = McpServerConfig {
        oauth: Some(OAuthConfig {
            client_secret: Some("${TEST_OAUTH_SECRET}".into()),
            ..Default::default()
        }),
        ..test_config()
    };
    let expanded = expand_server_config(&config);
    assert_eq!(
        expanded.oauth.unwrap().client_secret.as_deref(),
        Some("secret123")
    );
    std::env::remove_var("TEST_OAUTH_SECRET");
}

#[test]
fn test_merge_project_overrides_global() {
    let mut global = McpConfigFile::default();
    global.mcp_servers.insert(
        "fs".to_string(),
        McpServerConfig {
            command: Some("npx".into()),
            ..test_config()
        },
    );
    let mut project = McpConfigFile::default();
    project.mcp_servers.insert(
        "fs".to_string(),
        McpServerConfig {
            command: Some("uvx".into()),
            ..test_config()
        },
    );
    let mut merged = global;
    for (name, server_config) in project.mcp_servers {
        merged.mcp_servers.insert(name, server_config);
    }
    assert_eq!(merged.mcp_servers["fs"].command.as_deref(), Some("uvx"));
}

#[test]
fn test_merge_project_adds_new_server() {
    let mut global = McpConfigFile::default();
    global.mcp_servers.insert(
        "fs".to_string(),
        McpServerConfig {
            command: Some("npx".into()),
            ..test_config()
        },
    );
    let mut project = McpConfigFile::default();
    project.mcp_servers.insert(
        "gh".to_string(),
        McpServerConfig {
            url: Some("https://api.github.com".into()),
            ..test_config()
        },
    );
    let mut merged = global;
    for (name, server_config) in project.mcp_servers {
        merged.mcp_servers.insert(name, server_config);
    }
    assert_eq!(merged.mcp_servers.len(), 2);
    assert!(merged.mcp_servers.contains_key("fs"));
    assert!(merged.mcp_servers.contains_key("gh"));
}

#[test]
fn test_remove_server_from_project_config() {
    let dir = tempfile::tempdir().unwrap();
    let mcp_path = dir.path().join(".mcp.json");
    std::fs::write(
        &mcp_path,
        r#"{"mcpServers":{"server-a":{"command":"npx"},"server-b":{"command":"uvx"}}}"#,
    )
    .unwrap();

    remove_server_from_config_with_paths(dir.path(), &missing_global_path(dir.path()), "server-a")
        .unwrap();

    let content = std::fs::read_to_string(&mcp_path).unwrap();
    let config: McpConfigFile = serde_json::from_str(&content).unwrap();
    assert_eq!(config.mcp_servers.len(), 1);
    assert!(config.mcp_servers.contains_key("server-b"));
}

#[test]
fn test_remove_server_from_global_config_nested() {
    let dir = tempfile::tempdir().unwrap();
    let settings_dir = dir.path().join(".peri");
    std::fs::create_dir_all(&settings_dir).unwrap();
    let settings_path = settings_dir.join("settings.json");
    std::fs::write(
        &settings_path,
        r#"{"config":{"mcpServers":{"gh":{"url":"https://api.github.com"}}},"otherSetting":42}"#,
    )
    .unwrap();

    let empty_cwd = dir.path().join("empty_project");
    std::fs::create_dir_all(&empty_cwd).unwrap();
    remove_server_from_config_with_paths(&empty_cwd, &settings_path, "gh").unwrap();

    let content = std::fs::read_to_string(&settings_path).unwrap();
    let value: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert!(value["config"]["mcpServers"]
        .as_object()
        .unwrap()
        .is_empty());
    assert_eq!(value["otherSetting"], 42);
}

#[test]
fn test_remove_server_from_global_config_top_level() {
    let dir = tempfile::tempdir().unwrap();
    let settings_dir = dir.path().join(".peri");
    std::fs::create_dir_all(&settings_dir).unwrap();
    let settings_path = settings_dir.join("settings.json");
    std::fs::write(
        &settings_path,
        r#"{"mcpServers":{"fs":{"command":"npx"}},"otherSetting":42}"#,
    )
    .unwrap();

    let empty_cwd = dir.path().join("empty_project");
    std::fs::create_dir_all(&empty_cwd).unwrap();
    remove_server_from_config_with_paths(&empty_cwd, &settings_path, "fs").unwrap();

    let content = std::fs::read_to_string(&settings_path).unwrap();
    let value: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert!(value["mcpServers"].as_object().unwrap().is_empty());
    assert_eq!(value["otherSetting"], 42);
}

#[test]
fn test_remove_server_nonexistent_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".mcp.json"), r#"{"mcpServers":{}}"#).unwrap();
    let settings_dir = dir.path().join(".peri");
    std::fs::create_dir_all(&settings_dir).unwrap();
    std::fs::write(settings_dir.join("settings.json"), r#"{}"#).unwrap();

    assert!(remove_server_from_config_with_paths(
        dir.path(),
        &settings_dir.join("settings.json"),
        "nonexistent",
    )
    .is_ok());

    let content = std::fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
    assert_eq!(content, r#"{"mcpServers":{}}"#);
}

#[test]
fn test_server_config_hash_deterministic() {
    let cfg = McpServerConfig {
        command: Some("node".into()),
        args: Some(vec!["server.js".into()]),
        env: Some(HashMap::from([("KEY".into(), "val".into())])),
        ..test_config()
    };
    let h1 = server_config_hash(&cfg);
    let h2 = server_config_hash(&cfg);
    assert_eq!(h1, h2);
}

#[test]
fn test_server_config_hash_differs_on_command() {
    let a = McpServerConfig {
        command: Some("node".into()),
        ..test_config()
    };
    let b = McpServerConfig {
        command: Some("python".into()),
        ..test_config()
    };
    assert_ne!(server_config_hash(&a), server_config_hash(&b));
}

#[test]
fn test_server_config_hash_differs_on_args() {
    let a = McpServerConfig {
        command: Some("node".into()),
        args: Some(vec!["a.js".into()]),
        ..test_config()
    };
    let b = McpServerConfig {
        command: Some("node".into()),
        args: Some(vec!["b.js".into()]),
        ..test_config()
    };
    assert_ne!(server_config_hash(&a), server_config_hash(&b));
}

#[test]
fn test_expand_env_vars_with_context_plugin_root() {
    let result = expand_env_vars_with_context(
        "${CLAUDE_PLUGIN_ROOT}/server.js",
        Some(Path::new("/plugins/my-plugin")),
        None,
        None,
    );
    assert_eq!(result, "/plugins/my-plugin/server.js");
}

#[test]
fn test_expand_env_vars_with_context_plugin_data() {
    let result = expand_env_vars_with_context(
        "${CLAUDE_PLUGIN_DATA}/cache",
        None,
        Some(Path::new("/plugins/my-plugin/.claude-plugin/data")),
        None,
    );
    assert_eq!(result, "/plugins/my-plugin/.claude-plugin/data/cache");
}

#[test]
fn test_expand_env_vars_with_context_user_config() {
    let uc = HashMap::from([("apiKey".into(), "sk-123".into())]);
    let result = expand_env_vars_with_context("${user_config.apiKey}", None, None, Some(&uc));
    assert_eq!(result, "sk-123");
}

#[test]
fn test_expand_env_vars_with_context_fallback_to_env() {
    std::env::set_var("TEST_MCP_CTX_VAR", "hello");
    let result = expand_env_vars_with_context("${TEST_MCP_CTX_VAR}", None, None, None);
    assert_eq!(result, "hello");
    std::env::remove_var("TEST_MCP_CTX_VAR");
}

#[test]
fn test_load_merged_config_full_no_plugins() {
    let dir = tempfile::tempdir().unwrap();
    // 没有 settings.json，没有插件目录（显式全局路径 seam：不读真实 home）
    let (config, plugin_sources) = load_merged_config_full_with_paths(
        dir.path(),
        dir.path(),
        &missing_global_path(dir.path()),
        // 「空配置 → 空集合」断言的是迁移前行为，故显式零注入。
        &BuiltinInjectionPolicy::none(),
    )
    .unwrap();
    assert!(config.mcp_servers.is_empty());
    assert!(plugin_sources.is_empty());
}

#[test]
fn test_load_merged_config_full_with_plugin() {
    use crate::plugin::types::{InstallScope, InstalledPlugin, InstalledPlugins};
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let claude_home = dir.path().join(".claude-test");
    std::fs::create_dir_all(&claude_home).unwrap();

    // 创建插件目录和 plugin.json（含 MCP server）
    let plugin_dir = claude_home
        .join("plugins")
        .join("cache")
        .join("mkt")
        .join("p1")
        .join("1.0.0");
    std::fs::create_dir_all(plugin_dir.join(".claude-plugin")).unwrap();
    std::fs::write(
        plugin_dir.join(".claude-plugin").join("plugin.json"),
        r#"{
                "name":"p1",
                "version":"1.0.0",
                "mcpServers":{
                    "srv1":{"command":"echo","args":["hello"]}
                }
            }"#,
    )
    .unwrap();

    // 创建 installed_plugins.json
    std::fs::create_dir_all(claude_home.join("plugins")).unwrap();
    let installed = InstalledPlugins {
        version: 2,
        plugins: vec![InstalledPlugin {
            id: "p1@mkt".into(),
            name: "p1".into(),
            version: "1.0.0".into(),
            marketplace: "mkt".into(),
            install_path: plugin_dir.clone(),
            scope: InstallScope::User,
            project_path: None,
            origin: PluginOrigin::PeriInstalled,
        }],
    };
    std::fs::write(
        claude_home.join("plugins").join("installed_plugins.json"),
        serde_json::to_string(&installed).unwrap(),
    )
    .unwrap();

    // 创建 settings.json 启用插件
    std::fs::write(
        claude_home.join("settings.json"),
        r#"{"enabledPlugins":["p1@mkt"]}"#,
    )
    .unwrap();

    let (config, plugin_sources) = load_merged_config_full_with_paths(
        &cwd,
        &claude_home,
        &missing_global_path(dir.path()),
        &BuiltinInjectionPolicy::all(),
    )
    .unwrap();

    // 验证 env 注入
    let srv_config = config
        .mcp_servers
        .get("plugin:p1:srv1")
        .expect("应有 plugin:p1:srv1 服务器");
    let env = srv_config
        .env
        .as_ref()
        .expect("插件 MCP server 应有 env 字段（自动注入）");
    assert_eq!(
        env.get("CLAUDE_PLUGIN_ROOT").unwrap(),
        &plugin_dir.to_string_lossy().to_string(),
        "CLAUDE_PLUGIN_ROOT 应为插件安装路径"
    );
    let expected_data = plugin_dir
        .join(".claude-plugin")
        .join("data")
        .to_string_lossy()
        .to_string();
    assert_eq!(
        env.get("CLAUDE_PLUGIN_DATA").unwrap(),
        &expected_data,
        "CLAUDE_PLUGIN_DATA 应为插件数据路径"
    );

    assert!(
        plugin_sources.contains_key("plugin:p1:srv1"),
        "plugin_sources should contain plugin:p1:srv1, got: {:?}",
        plugin_sources
    );
    let source = plugin_sources.get("plugin:p1:srv1").unwrap();
    assert!(source.starts_with("p1@"), "expected p1@*, got: {}", source);
}

#[test]
fn test_load_merged_config_full_multiple_plugins() {
    use crate::plugin::types::{InstallScope, InstalledPlugin, InstalledPlugins};
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let claude_home = dir.path().join(".claude-test");
    std::fs::create_dir_all(&claude_home).unwrap();

    // Plugin A from marketplace "alpha"
    let plugin_a_dir = claude_home
        .join("plugins")
        .join("cache")
        .join("alpha")
        .join("pa")
        .join("1.0.0");
    std::fs::create_dir_all(plugin_a_dir.join(".claude-plugin")).unwrap();
    std::fs::write(
        plugin_a_dir.join(".claude-plugin").join("plugin.json"),
        r#"{"name":"pa","version":"1.0.0","mcpServers":{"srvA":{"command":"cmdA"}}}"#,
    )
    .unwrap();

    // Plugin B from marketplace "beta"
    let plugin_b_dir = claude_home
        .join("plugins")
        .join("cache")
        .join("beta")
        .join("pb")
        .join("2.0.0");
    std::fs::create_dir_all(plugin_b_dir.join(".claude-plugin")).unwrap();
    std::fs::write(
            plugin_b_dir.join(".claude-plugin").join("plugin.json"),
            r#"{"name":"pb","version":"2.0.0","mcpServers":{"srvB1":{"command":"cmdB1"},"srvB2":{"command":"cmdB2"}}}"#,
        ).unwrap();

    // installed_plugins.json
    std::fs::create_dir_all(claude_home.join("plugins")).unwrap();
    let installed = InstalledPlugins {
        version: 2,
        plugins: vec![
            InstalledPlugin {
                id: "pa@alpha".into(),
                name: "pa".into(),
                version: "1.0.0".into(),
                marketplace: "alpha".into(),
                install_path: plugin_a_dir.clone(),
                scope: InstallScope::User,
                project_path: None,
                origin: PluginOrigin::PeriInstalled,
            },
            InstalledPlugin {
                id: "pb@beta".into(),
                name: "pb".into(),
                version: "2.0.0".into(),
                marketplace: "beta".into(),
                install_path: plugin_b_dir.clone(),
                scope: InstallScope::User,
                project_path: None,
                origin: PluginOrigin::PeriInstalled,
            },
        ],
    };
    std::fs::write(
        claude_home.join("plugins").join("installed_plugins.json"),
        serde_json::to_string(&installed).unwrap(),
    )
    .unwrap();

    // settings.json
    std::fs::write(
        claude_home.join("settings.json"),
        r#"{"enabledPlugins":["pa@alpha","pb@beta"]}"#,
    )
    .unwrap();

    let (_config, plugin_sources) = load_merged_config_full_with_paths(
        &cwd,
        &claude_home,
        &missing_global_path(dir.path()),
        &BuiltinInjectionPolicy::all(),
    )
    .unwrap();
    assert!(
        plugin_sources.contains_key("plugin:pa:srvA"),
        "should contain plugin:pa:srvA, got: {:?}",
        plugin_sources
    );
    assert!(
        plugin_sources.contains_key("plugin:pb:srvB1"),
        "should contain plugin:pb:srvB1, got: {:?}",
        plugin_sources
    );
    assert!(
        plugin_sources.contains_key("plugin:pb:srvB2"),
        "should contain plugin:pb:srvB2, got: {:?}",
        plugin_sources
    );
    assert_eq!(plugin_sources.get("plugin:pa:srvA").unwrap(), "pa@alpha");
    assert_eq!(plugin_sources.get("plugin:pb:srvB1").unwrap(), "pb@beta");
}

#[test]
fn test_load_merged_config_full_plugin_env_preserves_existing() {
    use crate::plugin::types::{InstallScope, InstalledPlugin, InstalledPlugins};
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let claude_home = dir.path().join(".claude-test");
    std::fs::create_dir_all(&claude_home).unwrap();

    // 创建插件目录和 plugin.json（含 MCP server + 自定义 env）
    let plugin_dir = claude_home
        .join("plugins")
        .join("cache")
        .join("mkt")
        .join("p2")
        .join("1.0.0");
    std::fs::create_dir_all(plugin_dir.join(".claude-plugin")).unwrap();
    std::fs::write(
        plugin_dir.join(".claude-plugin").join("plugin.json"),
        r#"{
            "name":"p2",
            "version":"1.0.0",
            "mcpServers":{
                "srv2":{
                    "command":"node",
                    "args":["server.js"],
                    "env":{"MY_VAR":"my_value"}
                }
            }
        }"#,
    )
    .unwrap();

    // 创建 installed_plugins.json
    std::fs::create_dir_all(claude_home.join("plugins")).unwrap();
    let installed = InstalledPlugins {
        version: 2,
        plugins: vec![InstalledPlugin {
            id: "p2@mkt".into(),
            name: "p2".into(),
            version: "1.0.0".into(),
            marketplace: "mkt".into(),
            install_path: plugin_dir.clone(),
            scope: InstallScope::User,
            project_path: None,
            origin: PluginOrigin::PeriInstalled,
        }],
    };
    std::fs::write(
        claude_home.join("plugins").join("installed_plugins.json"),
        serde_json::to_string(&installed).unwrap(),
    )
    .unwrap();

    // 创建 settings.json 启用插件
    std::fs::write(
        claude_home.join("settings.json"),
        r#"{"enabledPlugins":["p2@mkt"]}"#,
    )
    .unwrap();

    let (config, _plugin_sources) = load_merged_config_full_with_paths(
        &cwd,
        &claude_home,
        &missing_global_path(dir.path()),
        &BuiltinInjectionPolicy::all(),
    )
    .unwrap();
    let srv_config = config
        .mcp_servers
        .get("plugin:p2:srv2")
        .expect("应有 plugin:p2:srv2 服务器");
    let env = srv_config.env.as_ref().expect("应有 env 字段");
    // 自定义 env 应保留
    assert_eq!(env.get("MY_VAR").unwrap(), "my_value");
    // CLAUDE_PLUGIN_ROOT 应被注入为实际路径
    assert_eq!(
        env.get("CLAUDE_PLUGIN_ROOT").unwrap(),
        &plugin_dir.to_string_lossy().to_string()
    );
    // CLAUDE_PLUGIN_DATA 应也被注入
    assert!(env.contains_key("CLAUDE_PLUGIN_DATA"));
}

#[path = "config/system_test.rs"]
mod system_tests;
