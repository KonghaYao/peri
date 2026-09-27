use tempfile::NamedTempFile;

use super::*;
use crate::plugin::PluginOrigin;

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

    remove_server_from_config(dir.path(), "server-a").unwrap();

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

    assert!(remove_server_from_config(dir.path(), "nonexistent").is_ok());

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

    let (config, plugin_sources) =
        load_merged_config_full_with_paths(&cwd, &claude_home, &missing_global_path(dir.path()))
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

    let (_config, plugin_sources) =
        load_merged_config_full_with_paths(&cwd, &claude_home, &missing_global_path(dir.path()))
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

    let (config, _plugin_sources) =
        load_merged_config_full_with_paths(&cwd, &claude_home, &missing_global_path(dir.path()))
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

// ─── System MCP 配置失败闭环（契约 1 / 4 的配置部分）──────────────────────

/// 契约层固定规则正文：断言错误里必须能看见它，而不是被吞成空配置。
const SYSTEM_TOOLS_RULE: &str = "system_mcp_tools requires system_mcp = true";

/// 断言错误是 ParseError，且路径与规则正文都被保留。
fn assert_parse_error(error: McpConfigError, expected_path: &Path) {
    let McpConfigError::ParseError { path, source } = error else {
        panic!("非法配置必须返回 ParseError，实际: {error}");
    };
    assert_eq!(path, expected_path.display().to_string());
    assert!(
        source.to_string().contains(SYSTEM_TOOLS_RULE),
        "规则正文必须保留在解析错误中: {source}"
    );
}

#[test]
fn test_system_mcp_project_rejects_tools_without_true() {
    // 契约 1：无 system_mcp = true 却声明 system_mcp_tools（含显式 []）必须失败。
    const CASES: [&str; 4] = [
        r#"{"system_mcp_tools":[]}"#,
        r#"{"system_mcp_tools":["search"]}"#,
        r#"{"system_mcp":false,"system_mcp_tools":[]}"#,
        r#"{"system_mcp":false,"system_mcp_tools":["search"]}"#,
    ];
    for server in CASES {
        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().join(".mcp.json");
        std::fs::write(
            &project_path,
            format!(r#"{{"mcpServers":{{"sys":{server}}}}}"#),
        )
        .unwrap();

        let error = load_from_path(&project_path).expect_err("非法组合必须失败");
        assert_parse_error(error, &project_path);
    }
}

#[test]
fn test_system_mcp_global_rejects_invalid_maps() {
    // nested / top-level 各自非法都必须失败，而不是 Ok(empty)。
    let nested_invalid = r#"{"config":{"mcpServers":{"sys":{"system_mcp_tools":["a"]}}}}"#;
    let top_level_invalid = r#"{"mcpServers":{"sys":{"system_mcp_tools":["a"]}}}"#;
    for content in [nested_invalid, top_level_invalid] {
        let dir = tempfile::tempdir().unwrap();
        let settings_path = dir.path().join("settings.json");
        std::fs::write(&settings_path, content).unwrap();

        let error = load_global_config(&settings_path).expect_err("非法 map 必须失败");
        assert_parse_error(error, &settings_path);
    }

    // 双 map：nested 合法 + top-level 非法——备用 map 也要被拒绝，
    // 不能因为选择的是 nested 就让非法 top-level 静默通过。
    let dir = tempfile::tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");
    std::fs::write(
        &settings_path,
        r#"{"config":{"mcpServers":{"ok":{"command":"npx"}}},"mcpServers":{"sys":{"system_mcp_tools":[]}}}"#,
    )
    .unwrap();
    let error = load_global_config(&settings_path).expect_err("非法备用 map 必须失败");
    assert_parse_error(error, &settings_path);

    // 双 map 均合法：仍按 nested > top-level 选择。
    std::fs::write(
        &settings_path,
        r#"{"config":{"mcpServers":{"chosen":{"command":"npx"}}},"mcpServers":{"fallback":{"command":"uvx"}}}"#,
    )
    .unwrap();
    let config = load_global_config(&settings_path).unwrap();
    assert_eq!(config.mcp_servers.len(), 1);
    assert!(config.mcp_servers.contains_key("chosen"));
}

#[test]
fn test_system_mcp_merged_errors_are_not_empty_success() {
    // 全局非法：不得退化成功空配置。
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let claude_home = dir.path().join(".claude-test");
    std::fs::create_dir_all(&claude_home).unwrap();
    let global_path = missing_global_path(dir.path());
    std::fs::write(
        &global_path,
        r#"{"mcpServers":{"sys":{"system_mcp_tools":[]}}}"#,
    )
    .unwrap();
    let error = load_merged_config_full_with_paths(&cwd, &claude_home, &global_path)
        .expect_err("全局非法配置必须失败");
    assert_parse_error(error, &global_path);

    // 项目非法：同样失败（非法文件不是缺文件）。
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let claude_home = dir.path().join(".claude-test");
    std::fs::create_dir_all(&claude_home).unwrap();
    let project_path = cwd.join(".mcp.json");
    std::fs::write(
        &project_path,
        r#"{"mcpServers":{"sys":{"system_mcp":false,"system_mcp_tools":["a"]}}}"#,
    )
    .unwrap();
    let error =
        load_merged_config_full_with_paths(&cwd, &claude_home, &missing_global_path(dir.path()))
            .expect_err("项目非法配置必须失败");
    assert_parse_error(error, &project_path);

    // 非法低优先级配置即便被有效项目同名覆盖也必须拒绝。
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let claude_home = dir.path().join(".claude-test");
    std::fs::create_dir_all(&claude_home).unwrap();
    let global_path = missing_global_path(dir.path());
    std::fs::write(
        &global_path,
        r#"{"mcpServers":{"sys":{"system_mcp_tools":[]}}}"#,
    )
    .unwrap();
    std::fs::write(
        cwd.join(".mcp.json"),
        r#"{"mcpServers":{"sys":{"command":"npx","system_mcp":true}}}"#,
    )
    .unwrap();
    let error = load_merged_config_full_with_paths(&cwd, &claude_home, &global_path)
        .expect_err("被覆盖的非法配置也必须拒绝");
    assert_parse_error(error, &global_path);
}

#[test]
fn test_system_mcp_plugin_strict_error_reaches_merge() {
    // 插件来源非法：严格插件路径必须把错误带到合并入口，而不是返回空 plugins。
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let claude_home = dir.path().join(".claude-test");
    std::fs::create_dir_all(&claude_home).unwrap();
    let plugin_dir = install_enabled_plugin(&claude_home, "p1", r#"{"srv":"servers/.mcp.json"}"#);
    let servers_dir = plugin_dir.join("servers");
    std::fs::create_dir_all(&servers_dir).unwrap();
    let broken = servers_dir.join(".mcp.json");
    std::fs::write(
        &broken,
        r#"{"mcpServers":{"bad":{"system_mcp_tools":["tool"]}}}"#,
    )
    .unwrap();

    let error =
        load_merged_config_full_with_paths(&cwd, &claude_home, &missing_global_path(dir.path()))
            .expect_err("插件非法 MCP 配置必须失败");

    let McpConfigError::PluginLoadError { source } = &error else {
        panic!("插件来源非法应返回 PluginLoadError，实际: {error}");
    };
    let crate::plugin::LoaderError::McpConfigInvalid { path, message } = source else {
        panic!("插件 MCP 配置无效应保留 McpConfigInvalid，实际: {source}");
    };
    assert_eq!(path, &broken);
    assert_eq!(message, &format!("bad: {SYSTEM_TOOLS_RULE}"));
    // 错误链必须保留固定规则正文：面板/日志可以据此定位。
    assert!(error.to_string().contains(SYSTEM_TOOLS_RULE));
}

#[test]
fn test_system_mcp_typed_validation_includes_disabled() {
    // disabled 不是绕过校验的通道；typed 构造（非 serde 路径）也要被拒绝。
    for disabled in [None, Some(true), Some(false)] {
        let mut servers = HashMap::new();
        servers.insert(
            "sys".to_string(),
            McpServerConfig {
                disabled,
                system_mcp: Some(false),
                system_mcp_tools: Some(Vec::new()),
                ..test_config()
            },
        );
        let config = McpConfigFile {
            mcp_servers: servers,
        };

        let error = validate_config(&config).expect_err("disabled 不能绕过校验");
        let McpConfigError::InvalidServer {
            server_name,
            source,
        } = error
        else {
            panic!("typed 校验必须返回 InvalidServer");
        };
        assert_eq!(server_name, "sys");
        assert_eq!(
            source,
            peri_acp_types::plugin::McpServerConfigValidationError::SystemMcpToolsRequiresSystemMcp
        );

        let display = validate_config(&config).unwrap_err().to_string();
        assert_eq!(
            display,
            format!("MCP 服务器配置无效: sys: {SYSTEM_TOOLS_RULE}")
        );
    }
}

#[test]
fn test_system_mcp_empty_tools_survive_config_pipeline() {
    // 契约 4 配置部分：显式 [] 经加载 → 合并 → 展开 → 写回 → 再载入仍可区分于 None。
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let claude_home = dir.path().join(".claude-test");
    std::fs::create_dir_all(&claude_home).unwrap();
    let global_path = missing_global_path(dir.path());
    let project_path = cwd.join(".mcp.json");
    std::fs::write(
        &project_path,
        r#"{"mcpServers":{"sys":{"command":"echo","system_mcp":true,"system_mcp_tools":[]}}}"#,
    )
    .unwrap();

    let (merged, _) = load_merged_config_full_with_paths(&cwd, &claude_home, &global_path).unwrap();
    let sys = merged.mcp_servers.get("sys").expect("应有 sys");
    assert_eq!(sys.system_mcp, Some(true));
    assert_eq!(sys.system_mcp_tools, Some(Vec::new()));
    assert_ne!(sys.system_mcp_tools, None, "Some([]) 与 None 必须可区分");

    // 展开不得把 [] 变成 None，也不得凭空产生工具名。
    let expanded = expand_server_config(sys);
    assert_eq!(expanded.system_mcp_tools, Some(Vec::new()));

    // 写回（切换 disabled）后原始空数组仍在文件里、仍能无损读回。
    set_server_disabled_with_paths(&cwd, &global_path, "sys", true).unwrap();
    let raw = std::fs::read_to_string(&project_path).unwrap();
    let written: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let tools = &written["mcpServers"]["sys"]["system_mcp_tools"];
    assert!(
        tools.as_array().is_some_and(|tools| tools.is_empty()),
        "写回必须保留显式空数组（不得省略该 key）: {raw}"
    );
    let reloaded = load_from_path(&project_path).unwrap();
    assert_eq!(
        reloaded.mcp_servers["sys"].system_mcp_tools,
        Some(Vec::new())
    );
    assert_eq!(reloaded.mcp_servers["sys"].disabled, Some(true));
}

#[test]
fn test_system_mcp_tools_survive_expansion_and_namespace() {
    // 契约 3 配置部分：工具数组字面量保真，所属 namespace 由 server key 决定。
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let claude_home = dir.path().join(".claude-test");
    std::fs::create_dir_all(&claude_home).unwrap();
    let tools = r#"["Search","search","${PLUGIN_TOOL_VAR}",""]"#;
    install_enabled_plugin(
        &claude_home,
        "p1",
        &format!(r#"{{"srv":{{"command":"node","system_mcp":true,"system_mcp_tools":{tools}}}}}"#),
    );

    let (merged, _) =
        load_merged_config_full_with_paths(&cwd, &claude_home, &missing_global_path(dir.path()))
            .unwrap();
    let srv = merged
        .mcp_servers
        .get("plugin:p1:srv")
        .expect("插件 server key 应带 plugin:{name}: 前缀");
    assert_eq!(
        srv.system_mcp_tools.as_deref(),
        Some(
            ["Search", "search", "${PLUGIN_TOOL_VAR}", ""]
                .map(String::from)
                .as_slice()
        ),
        "顺序/大小写/重复项/空串/变量占位符字面量都不得改写，也不得加 MCP 前缀"
    );

    // global/project 同名覆盖是整条替换（不是数组拼接），source 随覆盖更新。
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let claude_home = dir.path().join(".claude-test");
    std::fs::create_dir_all(&claude_home).unwrap();
    let global_path = missing_global_path(dir.path());
    std::fs::write(
        &global_path,
        r#"{"mcpServers":{"sys":{"command":"echo","system_mcp":true,"system_mcp_tools":["global-tool"]}}}"#,
    )
    .unwrap();
    let project_path = cwd.join(".mcp.json");
    std::fs::write(
        &project_path,
        r#"{"mcpServers":{"sys":{"command":"echo","system_mcp":true,"system_mcp_tools":["project-tool"]}}}"#,
    )
    .unwrap();

    let (merged, _) = load_merged_config_full_with_paths(&cwd, &claude_home, &global_path).unwrap();
    let sys = merged.mcp_servers.get("sys").expect("应有 sys");
    assert_eq!(
        sys.system_mcp_tools.as_deref(),
        Some(["project-tool".to_string()].as_slice()),
        "同名覆盖必须是整条替换，不跨来源拼接"
    );
    assert_eq!(sys.source, Some(ConfigSource::Project(project_path)));
}

#[test]
fn test_system_mcp_dedup_preserves_required_namespaces() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let claude_home = dir.path().join(".claude-test");
    std::fs::create_dir_all(&claude_home).unwrap();
    let global_path = missing_global_path(dir.path());
    // 插件（System 声明 + 普通声明各一）；手动配置与插件 server 内容完全一致，
    // 使内容 hash 相等——去重规则本身成为唯一变量。
    let plugin_dir = install_enabled_plugin(
        &claude_home,
        "p1",
        r#"{
            "sys-dup":{"command":"node","args":["s.js"],"system_mcp":true,"system_mcp_tools":["t"]},
            "plain-dup":{"command":"node","args":["p.js"]}
        }"#,
    );
    let plugin_env = serde_json::json!({
        "CLAUDE_PLUGIN_ROOT": plugin_dir.to_string_lossy(),
        "CLAUDE_PLUGIN_DATA": plugin_dir.join(".claude-plugin").join("data").to_string_lossy(),
    });
    let manual = serde_json::json!({"mcpServers": {
        "sys-manual": {
            "command":"node","args":["s.js"],
            "system_mcp":true,"system_mcp_tools":["t"],
            "env": plugin_env,
        },
        "plain-manual": {"command":"node","args":["p.js"],"env": plugin_env},
    }});
    std::fs::write(&global_path, serde_json::to_string(&manual).unwrap()).unwrap();

    let (merged, _) = load_merged_config_full_with_paths(&cwd, &claude_home, &global_path).unwrap();
    assert!(
        merged.mcp_servers.contains_key("plugin:p1:sys-dup"),
        "System MCP 不得因跨 namespace 内容相同被去重删除，实际 keys: {:?}",
        merged.mcp_servers.keys().collect::<Vec<_>>()
    );
    assert!(
        !merged.mcp_servers.contains_key("plugin:p1:plain-dup"),
        "普通 MCP 既有内容去重仍必须生效，实际 keys: {:?}",
        merged.mcp_servers.keys().collect::<Vec<_>>()
    );

    // hash 必须覆盖 System 字段：变更它们视为不同服务器。
    let base = McpServerConfig {
        command: Some("node".into()),
        system_mcp: Some(true),
        system_mcp_tools: Some(vec!["t".into()]),
        ..test_config()
    };
    assert_ne!(
        server_config_hash(&base),
        server_config_hash(&test_config())
    );
    let mut other_tools = base.clone();
    other_tools.system_mcp_tools = Some(vec!["t2".into()]);
    assert_ne!(server_config_hash(&base), server_config_hash(&other_tools));
}

#[test]
fn test_system_mcp_disabled_write_rejects_invalid_input() {
    // 写入口在修改前校验：非法输入不写盘、不改字节，disabled 不是绕过通道。
    for disabled in [true, false] {
        // 项目文件非法
        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().join(".mcp.json");
        let invalid = r#"{"mcpServers":{"sys":{"system_mcp_tools":[]}}}"#;
        std::fs::write(&project_path, invalid).unwrap();
        let error = set_server_disabled_with_paths(
            dir.path(),
            &missing_global_path(dir.path()),
            "sys",
            disabled,
        )
        .expect_err("项目非法配置必须拒绝写盘");
        assert_parse_error(error, &project_path);
        assert_eq!(std::fs::read_to_string(&project_path).unwrap(), invalid);

        // 全局 nested 非法
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("project");
        std::fs::create_dir_all(&cwd).unwrap();
        let global_path = missing_global_path(dir.path());
        let invalid =
            r#"{"config":{"mcpServers":{"sys":{"system_mcp_tools":[]}}},"otherSetting":42}"#;
        std::fs::write(&global_path, invalid).unwrap();
        let error = set_server_disabled_with_paths(&cwd, &global_path, "sys", disabled)
            .expect_err("全局 nested 非法配置必须拒绝写盘");
        assert_parse_error(error, &global_path);
        assert_eq!(std::fs::read_to_string(&global_path).unwrap(), invalid);

        // 全局 top-level 非法
        let invalid = r#"{"mcpServers":{"sys":{"system_mcp":false,"system_mcp_tools":["a"]}}}"#;
        std::fs::write(&global_path, invalid).unwrap();
        let error = set_server_disabled_with_paths(&cwd, &global_path, "sys", disabled)
            .expect_err("全局 top-level 非法配置必须拒绝写盘");
        assert_parse_error(error, &global_path);
        assert_eq!(std::fs::read_to_string(&global_path).unwrap(), invalid);
    }
}

#[test]
fn test_system_mcp_remove_rejects_invalid_input() {
    // 删除非法条目不是修复通道：目标非法或其它 server 非法都拒绝，文件不变。
    let dir = tempfile::tempdir().unwrap();
    let project_path = dir.path().join(".mcp.json");
    let target_invalid = r#"{"mcpServers":{"sys":{"system_mcp_tools":[]}}}"#;
    std::fs::write(&project_path, target_invalid).unwrap();
    let error =
        remove_server_from_config_with_paths(dir.path(), &missing_global_path(dir.path()), "sys")
            .expect_err("删除非法目标也必须拒绝");
    assert_parse_error(error, &project_path);
    assert_eq!(
        std::fs::read_to_string(&project_path).unwrap(),
        target_invalid
    );

    let sibling_invalid =
        r#"{"mcpServers":{"victim":{"command":"npx"},"sys":{"system_mcp_tools":[]}}}"#;
    std::fs::write(&project_path, sibling_invalid).unwrap();
    let error = remove_server_from_config_with_paths(
        dir.path(),
        &missing_global_path(dir.path()),
        "victim",
    )
    .expect_err("同文件其它 server 非法也必须拒绝");
    assert_parse_error(error, &project_path);
    assert_eq!(
        std::fs::read_to_string(&project_path).unwrap(),
        sibling_invalid
    );

    // 全局 nested / top-level
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let global_path = missing_global_path(dir.path());
    let nested_invalid =
        r#"{"config":{"mcpServers":{"victim":{"command":"npx"},"sys":{"system_mcp_tools":[]}}}}"#;
    std::fs::write(&global_path, nested_invalid).unwrap();
    let error = remove_server_from_config_with_paths(&cwd, &global_path, "victim")
        .expect_err("全局 nested 非法必须拒绝");
    assert_parse_error(error, &global_path);
    assert_eq!(
        std::fs::read_to_string(&global_path).unwrap(),
        nested_invalid
    );

    let top_level_invalid =
        r#"{"mcpServers":{"victim":{"command":"npx"},"sys":{"system_mcp_tools":[]}}}"#;
    std::fs::write(&global_path, top_level_invalid).unwrap();
    let error = remove_server_from_config_with_paths(&cwd, &global_path, "victim")
        .expect_err("全局 top-level 非法必须拒绝");
    assert_parse_error(error, &global_path);
    assert_eq!(
        std::fs::read_to_string(&global_path).unwrap(),
        top_level_invalid
    );
}

#[test]
fn test_system_mcp_write_preserves_remaining_tools() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let global_path = missing_global_path(dir.path());
    std::fs::write(
        &global_path,
        r#"{"config":{"mcpServers":{
            "sys":{"command":"echo","system_mcp":true,"system_mcp_tools":["z","a","z"]},
            "plain":{"command":"npx"}
        }},"otherSetting":42}"#,
    )
    .unwrap();

    // 删除普通 server：其余 System 数组顺序与值不变，其它 settings 字段仍在。
    remove_server_from_config_with_paths(&cwd, &global_path, "plain").unwrap();
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&global_path).unwrap()).unwrap();
    assert!(value["config"]["mcpServers"].get("plain").is_none());
    assert_eq!(value["otherSetting"], 42);
    let reloaded = load_global_config(&global_path).unwrap();
    assert_eq!(
        reloaded.mcp_servers["sys"].system_mcp_tools.as_deref(),
        Some(["z".to_string(), "a".to_string(), "z".to_string()].as_slice())
    );

    // 切换 disabled：数组仍原样保留（顺序与重复项都不动）。
    set_server_disabled_with_paths(&cwd, &global_path, "sys", true).unwrap();
    let reloaded = load_global_config(&global_path).unwrap();
    assert_eq!(reloaded.mcp_servers["sys"].disabled, Some(true));
    assert_eq!(
        reloaded.mcp_servers["sys"].system_mcp_tools.as_deref(),
        Some(["z".to_string(), "a".to_string(), "z".to_string()].as_slice())
    );

    // 显式 [] 写回不得被省略。
    let empty_path = dir.path().join("empty-settings.json");
    std::fs::write(
        &empty_path,
        r#"{"mcpServers":{"sys":{"command":"echo","system_mcp":true,"system_mcp_tools":[]}}}"#,
    )
    .unwrap();
    set_server_disabled_with_paths(&cwd, &empty_path, "sys", true).unwrap();
    let raw = std::fs::read_to_string(&empty_path).unwrap();
    let written: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert!(
        written["mcpServers"]["sys"]["system_mcp_tools"]
            .as_array()
            .is_some_and(|tools| tools.is_empty()),
        "写回必须保留显式空数组（不得省略该 key）: {raw}"
    );
}
