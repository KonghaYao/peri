use super::*;

#[test]
fn test_load_no_plugins_aggregated() {
    let result = load_enabled_plugins_aggregated(Path::new("/nonexistent/path"), None);
    assert!(result.plugins.is_empty());
    assert!(result.all_skill_roots.is_empty());
    assert!(result.all_mcp_servers.is_empty());
    assert!(result.all_agent_dirs.is_empty());
    assert!(result.all_commands.is_empty());
    assert!(result.all_hooks.is_empty());
}

#[test]
fn test_load_enabled_plugins_aggregated() {
    let dir = tempdir().unwrap();
    let plugin_dir = dir.path().join("my-plugin");
    std::fs::create_dir_all(plugin_dir.join(".claude-plugin")).unwrap();
    std::fs::write(
        plugin_dir.join(".claude-plugin").join("plugin.json"),
        r#"{"name":"my-plugin","version":"1.0.0"}"#,
    )
    .unwrap();

    std::fs::create_dir_all(dir.path().join("plugins")).unwrap();
    let installed_json = serde_json::to_string(&InstalledPlugins {
        version: 2,
        plugins: vec![InstalledPlugin {
            id: "my-plugin@test".into(),
            name: "my-plugin".into(),
            version: "1.0.0".into(),
            marketplace: "test".into(),
            install_path: plugin_dir.clone(),
            scope: InstallScope::User,
            project_path: None,
            origin: PluginOrigin::PeriInstalled,
        }],
    })
    .unwrap();
    std::fs::write(
        dir.path().join("plugins").join("installed_plugins.json"),
        installed_json,
    )
    .unwrap();

    let settings = r#"{"enabledPlugins":["my-plugin@test"]}"#;
    std::fs::write(dir.path().join("settings.json"), settings).unwrap();

    // Hindsight 使用的包装格式；manifest 不声明 hooks，必须从约定文件加载。
    std::fs::create_dir_all(plugin_dir.join("hooks")).unwrap();
    std::fs::write(
        plugin_dir.join("hooks/hooks.json"),
        r#"{"hooks": {
            "SessionStart": [{"hooks": [{"type": "command", "command": "python3 \"${CLAUDE_PLUGIN_ROOT}/scripts/session_start.py\"", "timeout": 5}]}],
            "UserPromptSubmit": [{"hooks": [{"type": "command", "command": "python3 \"${CLAUDE_PLUGIN_ROOT}/scripts/recall.py\"", "timeout": 12}]}],
            "Stop": [{"hooks": [{"type": "command", "command": "python3 \"${CLAUDE_PLUGIN_ROOT}/scripts/retain.py\"", "timeout": 15, "async": true}]}],
            "SessionEnd": [{"hooks": [{"type": "command", "command": "python3 \"${CLAUDE_PLUGIN_ROOT}/scripts/session_end.py\"", "timeout": 10}]}]
        }}"#,
    )
    .unwrap();

    let result = load_enabled_plugins_aggregated(dir.path(), None);
    assert_eq!(result.plugins.len(), 1);
    assert_eq!(result.plugins[0].name, "my-plugin");
    assert!(result.plugins[0].hooks_config.is_some());
    assert_eq!(result.all_hooks.len(), 4);
    use crate::hooks::types::{HookEvent, HookType};
    for (event, script, timeout, async_run) in [
        (HookEvent::SessionStart, "session_start.py", 5, false),
        (HookEvent::UserPromptSubmit, "recall.py", 12, false),
        (HookEvent::Stop, "retain.py", 15, true),
        (HookEvent::SessionEnd, "session_end.py", 10, false),
    ] {
        let hook = result
            .all_hooks
            .iter()
            .find(|hook| hook.event == event)
            .unwrap();
        assert_eq!(hook.plugin_root, plugin_dir);
        assert_eq!(hook.plugin_name, "my-plugin");
        assert_eq!(hook.plugin_data_dir, plugin_dir.join(".claude-plugin/data"));
        assert!(matches!(
            &hook.hook,
            HookType::Command { command, timeout: Some(actual_timeout), async_run: actual_async, .. }
                if command == &format!("python3 \"${{CLAUDE_PLUGIN_ROOT}}/scripts/{script}\"")
                    && *actual_timeout == timeout && *actual_async == async_run
        ));
    }
}

#[test]
fn test_load_plugin_skill_dirs_aggregated() {
    let dir = tempdir().unwrap();
    let plugin_dir = dir.path().join("skill-plugin");
    std::fs::create_dir_all(plugin_dir.join(".claude-plugin")).unwrap();
    std::fs::create_dir_all(plugin_dir.join("skills").join("my-skill")).unwrap();
    std::fs::write(
        plugin_dir.join("skills").join("my-skill").join("SKILL.md"),
        "---\n---\n",
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join(".claude-plugin").join("plugin.json"),
        r#"{"name":"skill-plugin","version":"1.0.0","skills":["skills/my-skill"]}"#,
    )
    .unwrap();

    std::fs::create_dir_all(dir.path().join("plugins")).unwrap();
    let installed_json = serde_json::to_string(&InstalledPlugins {
        version: 2,
        plugins: vec![InstalledPlugin {
            id: "skill-plugin@test".into(),
            name: "skill-plugin".into(),
            version: "1.0.0".into(),
            marketplace: "test".into(),
            install_path: plugin_dir.clone(),
            scope: InstallScope::User,
            project_path: None,
            origin: PluginOrigin::PeriInstalled,
        }],
    })
    .unwrap();
    std::fs::write(
        dir.path().join("plugins").join("installed_plugins.json"),
        installed_json,
    )
    .unwrap();

    let settings = r#"{"enabledPlugins":["skill-plugin@test"]}"#;
    std::fs::write(dir.path().join("settings.json"), settings).unwrap();

    let result = load_enabled_plugins_aggregated(dir.path(), None);
    assert_eq!(result.all_skill_roots.len(), 1);
    assert!(result.all_skill_roots[0].path.ends_with("my-skill"));
}

#[test]
fn test_extract_commands_string_directory() {
    // 测试 "commands": ["./commands/"] 字符串目录路径格式
    let dir = tempdir().unwrap();
    let cmd_dir = dir.path().join("commands");
    std::fs::create_dir_all(&cmd_dir).unwrap();
    std::fs::write(
        cmd_dir.join("deploy.md"),
        "---\ndescription: Deploy to production\n---\nDeploy",
    )
    .unwrap();
    std::fs::write(cmd_dir.join("rollback.md"), "---\n---\nRollback").unwrap();

    // 直接构造 PluginCommandEntry::Path 来测试目录扫描
    let direct_manifest = PluginManifest {
        commands: Some(vec![PluginCommandEntry::Path("commands".into())]),
        ..make_manifest_with_commands(vec![])
    };

    let entries = extract_commands(&direct_manifest, dir.path(), "ecc");
    assert_eq!(entries.len(), 2);
    let mut names: Vec<_> = entries.iter().map(|e| e.name.clone()).collect();
    names.sort();
    assert_eq!(names, vec!["plugin:ecc:deploy", "plugin:ecc:rollback"]);
    let deploy = entries
        .iter()
        .find(|e| e.name == "plugin:ecc:deploy")
        .unwrap();
    assert_eq!(deploy.description, "Deploy to production");
}

#[test]
fn test_extract_commands_string_directory_nonexistent() {
    let manifest = PluginManifest {
        commands: Some(vec![PluginCommandEntry::Path("nonexistent_dir".into())]),
        ..make_manifest_with_commands(vec![])
    };
    let entries = extract_commands(&manifest, Path::new("/tmp"), "p");
    assert!(entries.is_empty());
}

#[test]
fn test_extract_commands_string_single_file() {
    // 字符串也可以是指向单个 .md 文件的路径
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("standalone.md"), "---\n---\nContent").unwrap();

    let manifest = PluginManifest {
        commands: Some(vec![PluginCommandEntry::Path("standalone.md".into())]),
        ..make_manifest_with_commands(vec![])
    };

    let entries = extract_commands(&manifest, dir.path(), "p");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "plugin:p:standalone");
}

// ===== 项目级插件发现测试 =====

#[test]
fn test_load_enabled_plugins_with_project_settings() {
    // 场景：项目 settings 有 ["a"]，用户 settings 有 ["b"]，只启用 "a"（项目替换用户）
    let dir = tempdir().unwrap();
    let plugin_a_dir = dir.path().join("plugin-a");
    let plugin_b_dir = dir.path().join("plugin-b");
    std::fs::create_dir_all(plugin_a_dir.join(".claude-plugin")).unwrap();
    std::fs::write(
        plugin_a_dir.join(".claude-plugin").join("plugin.json"),
        r#"{"name":"plugin-a","version":"1.0.0"}"#,
    )
    .unwrap();
    std::fs::create_dir_all(plugin_b_dir.join(".claude-plugin")).unwrap();
    std::fs::write(
        plugin_b_dir.join(".claude-plugin").join("plugin.json"),
        r#"{"name":"plugin-b","version":"1.0.0"}"#,
    )
    .unwrap();

    // 用户级 settings.json (claude_dir)
    let claude_dir = dir.path().join("claude");
    std::fs::create_dir_all(claude_dir.join("plugins")).unwrap();
    let installed_json = serde_json::to_string(&InstalledPlugins {
        version: 2,
        plugins: vec![
            InstalledPlugin {
                id: "plugin-a@test".into(),
                name: "plugin-a".into(),
                version: "1.0.0".into(),
                marketplace: "test".into(),
                install_path: plugin_a_dir.clone(),
                scope: InstallScope::User,
                project_path: None,
                origin: PluginOrigin::PeriInstalled,
            },
            InstalledPlugin {
                id: "plugin-b@test".into(),
                name: "plugin-b".into(),
                version: "1.0.0".into(),
                marketplace: "test".into(),
                install_path: plugin_b_dir.clone(),
                scope: InstallScope::User,
                project_path: None,
                origin: PluginOrigin::PeriInstalled,
            },
        ],
    })
    .unwrap();
    std::fs::write(
        claude_dir.join("plugins").join("installed_plugins.json"),
        installed_json,
    )
    .unwrap();
    // 用户级启用 "plugin-b@test"
    std::fs::write(
        claude_dir.join("settings.json"),
        r#"{"enabledPlugins":["plugin-b@test"]}"#,
    )
    .unwrap();

    // 项目级 settings.json (cwd/.claude/settings.json)
    let cwd = dir.path().join("project");
    std::fs::create_dir_all(cwd.join(".claude")).unwrap();
    std::fs::write(
        cwd.join(".claude").join("settings.json"),
        r#"{"enabledPlugins":["plugin-a@test"]}"#,
    )
    .unwrap();

    let loaded = load_enabled_plugins(&claude_dir, Some(&cwd)).unwrap();
    assert_eq!(loaded.len(), 1, "项目级替换用户级，只应启用 plugin-a");
    assert_eq!(loaded[0].name, "plugin-a");
}

#[test]
fn test_load_enabled_plugins_project_fallback_to_user() {
    // 场景：项目 settings 存在但无 enabledPlugins 字段，回退用户级
    let dir = tempdir().unwrap();
    let plugin_dir = dir.path().join("my-plugin");
    std::fs::create_dir_all(plugin_dir.join(".claude-plugin")).unwrap();
    std::fs::write(
        plugin_dir.join(".claude-plugin").join("plugin.json"),
        r#"{"name":"my-plugin","version":"1.0.0"}"#,
    )
    .unwrap();

    let claude_dir = dir.path().join("claude");
    std::fs::create_dir_all(claude_dir.join("plugins")).unwrap();
    let installed_json = serde_json::to_string(&InstalledPlugins {
        version: 2,
        plugins: vec![InstalledPlugin {
            id: "my-plugin@test".into(),
            name: "my-plugin".into(),
            version: "1.0.0".into(),
            marketplace: "test".into(),
            install_path: plugin_dir.clone(),
            scope: InstallScope::User,
            project_path: None,
            origin: PluginOrigin::PeriInstalled,
        }],
    })
    .unwrap();
    std::fs::write(
        claude_dir.join("plugins").join("installed_plugins.json"),
        installed_json,
    )
    .unwrap();
    std::fs::write(
        claude_dir.join("settings.json"),
        r#"{"enabledPlugins":["my-plugin@test"]}"#,
    )
    .unwrap();

    // 项目级 settings 存在但没有 enabledPlugins 字段
    let cwd = dir.path().join("project");
    std::fs::create_dir_all(cwd.join(".claude")).unwrap();
    std::fs::write(
        cwd.join(".claude").join("settings.json"),
        r#"{"hooks": {}}"#,
    )
    .unwrap();

    let loaded = load_enabled_plugins(&claude_dir, Some(&cwd)).unwrap();
    assert_eq!(loaded.len(), 1, "项目级无 enabledPlugins 应回退用户级");
    assert_eq!(loaded[0].name, "my-plugin");
}

#[test]
fn test_load_enabled_plugins_project_empty_enabled() {
    // 场景：项目 settings enabledPlugins 为空数组，回退用户级
    let dir = tempdir().unwrap();
    let plugin_dir = dir.path().join("my-plugin");
    std::fs::create_dir_all(plugin_dir.join(".claude-plugin")).unwrap();
    std::fs::write(
        plugin_dir.join(".claude-plugin").join("plugin.json"),
        r#"{"name":"my-plugin","version":"1.0.0"}"#,
    )
    .unwrap();

    let claude_dir = dir.path().join("claude");
    std::fs::create_dir_all(claude_dir.join("plugins")).unwrap();
    let installed_json = serde_json::to_string(&InstalledPlugins {
        version: 2,
        plugins: vec![InstalledPlugin {
            id: "my-plugin@test".into(),
            name: "my-plugin".into(),
            version: "1.0.0".into(),
            marketplace: "test".into(),
            install_path: plugin_dir.clone(),
            scope: InstallScope::User,
            project_path: None,
            origin: PluginOrigin::PeriInstalled,
        }],
    })
    .unwrap();
    std::fs::write(
        claude_dir.join("plugins").join("installed_plugins.json"),
        installed_json,
    )
    .unwrap();
    std::fs::write(
        claude_dir.join("settings.json"),
        r#"{"enabledPlugins":["my-plugin@test"]}"#,
    )
    .unwrap();

    // 项目级 settings 有 enabledPlugins 但为空数组
    let cwd = dir.path().join("project");
    std::fs::create_dir_all(cwd.join(".claude")).unwrap();
    std::fs::write(
        cwd.join(".claude").join("settings.json"),
        r#"{"enabledPlugins":[]}"#,
    )
    .unwrap();

    let loaded = load_enabled_plugins(&claude_dir, Some(&cwd)).unwrap();
    assert_eq!(
        loaded.len(),
        1,
        "项目级 enabledPlugins 为空数组应回退用户级"
    );
    assert_eq!(loaded[0].name, "my-plugin");
}

#[test]
fn test_load_enabled_plugins_missing_project() {
    // 场景：cwd=None，仅读用户级 settings（向后兼容）
    let dir = tempdir().unwrap();
    let plugin_dir = dir.path().join("my-plugin");
    std::fs::create_dir_all(plugin_dir.join(".claude-plugin")).unwrap();
    std::fs::write(
        plugin_dir.join(".claude-plugin").join("plugin.json"),
        r#"{"name":"my-plugin","version":"1.0.0"}"#,
    )
    .unwrap();

    let claude_dir = dir.path().join("claude");
    std::fs::create_dir_all(claude_dir.join("plugins")).unwrap();
    let installed_json = serde_json::to_string(&InstalledPlugins {
        version: 2,
        plugins: vec![InstalledPlugin {
            id: "my-plugin@test".into(),
            name: "my-plugin".into(),
            version: "1.0.0".into(),
            marketplace: "test".into(),
            install_path: plugin_dir.clone(),
            scope: InstallScope::User,
            project_path: None,
            origin: PluginOrigin::PeriInstalled,
        }],
    })
    .unwrap();
    std::fs::write(
        claude_dir.join("plugins").join("installed_plugins.json"),
        installed_json,
    )
    .unwrap();
    std::fs::write(
        claude_dir.join("settings.json"),
        r#"{"enabledPlugins":["my-plugin@test"]}"#,
    )
    .unwrap();

    let loaded = load_enabled_plugins(&claude_dir, None).unwrap();
    assert_eq!(loaded.len(), 1, "cwd=None 时行为不变，仅读用户 settings");
    assert_eq!(loaded[0].name, "my-plugin");
}

// ─── MCP 专用严格插件路径（契约 1 的插件来源入口）─────────────────────────

const SYSTEM_TOOLS_RULE: &str = "system_mcp_tools requires system_mcp = true";

/// 在 `claude_home` 下安装一个插件并启用它；`manifest_mcp` 为 `mcpServers` 字段的
/// 原始 JSON 文本（`None` 表示 manifest 不声明该字段）。
fn install_plugin(claude_home: &Path, name: &str, manifest_mcp: Option<&str>) -> PathBuf {
    let plugin_dir = claude_home
        .join("plugins")
        .join("cache")
        .join("mkt")
        .join(name)
        .join("1.0.0");
    std::fs::create_dir_all(plugin_dir.join(".claude-plugin")).unwrap();
    let mcp_field = manifest_mcp
        .map(|raw| format!(r#","mcpServers":{raw}"#))
        .unwrap_or_default();
    std::fs::write(
        plugin_dir.join(".claude-plugin").join("plugin.json"),
        format!(r#"{{"name":"{name}","version":"1.0.0"{mcp_field}}}"#),
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

fn assert_mcp_config_invalid(error: LoaderError, expected_path: &Path, rule: &str) {
    let LoaderError::McpConfigInvalid { path, message } = error else {
        panic!("严格 MCP 路径应返回 McpConfigInvalid，实际: {error}");
    };
    assert_eq!(path, expected_path);
    assert!(
        message.contains(rule),
        "固定规则正文必须保留（message={message}）"
    );
}

#[test]
fn test_system_mcp_plugin_strict_sources_reject_invalid() {
    // 文件引用（wrapped）：非法内容 → McpConfigInvalid，且不部分接纳合法兄弟条目。
    let dir = tempdir().unwrap();
    let claude_home = dir.path().join(".claude-test");
    let plugin_dir = install_plugin(
        &claude_home,
        "wrapped",
        Some(r#"{"srv":"servers/.mcp.json"}"#),
    );
    let servers_dir = plugin_dir.join("servers");
    std::fs::create_dir_all(&servers_dir).unwrap();
    let wrapped = servers_dir.join(".mcp.json");
    std::fs::write(
        &wrapped,
        r#"{"mcpServers":{"legal":{"command":"npx"},"bad":{"system_mcp_tools":["t"]}}}"#,
    )
    .unwrap();
    let error =
        load_enabled_plugins_for_mcp(&claude_home, None).expect_err("非法 wrapped 配置必须失败");
    assert_mcp_config_invalid(error, &wrapped, SYSTEM_TOOLS_RULE);

    // 文件引用（flat）：同样失败。
    std::fs::write(
        &wrapped,
        r#"{"legal":{"command":"npx"},"bad":{"system_mcp":false,"system_mcp_tools":[]}}"#,
    )
    .unwrap();
    let error =
        load_enabled_plugins_for_mcp(&claude_home, None).expect_err("非法 flat 配置必须失败");
    assert_mcp_config_invalid(error, &wrapped, SYSTEM_TOOLS_RULE);

    // 根 .mcp.json 回退：manifest 未声明 mcpServers 时读取，非法同样失败。
    let dir = tempdir().unwrap();
    let claude_home = dir.path().join(".claude-test");
    let plugin_dir = install_plugin(&claude_home, "fallback", None);
    let root_mcp = plugin_dir.join(".mcp.json");
    std::fs::write(
        &root_mcp,
        r#"{"mcpServers":{"bad":{"system_mcp_tools":["t"]}}}"#,
    )
    .unwrap();
    let error =
        load_enabled_plugins_for_mcp(&claude_home, None).expect_err("非法根 .mcp.json 必须失败");
    assert_mcp_config_invalid(error, &root_mcp, SYSTEM_TOOLS_RULE);

    // 内联 manifest：清单解析本身失败（内联 DTO 走契约层 Deserialize），
    // 错误仍保留路径与固定规则正文，而不是被当作未安装跳过。
    let dir = tempdir().unwrap();
    let claude_home = dir.path().join(".claude-test");
    let plugin_dir = install_plugin(
        &claude_home,
        "inline",
        Some(r#"{"bad":{"system_mcp_tools":["t"]}}"#),
    );
    let error =
        load_enabled_plugins_for_mcp(&claude_home, None).expect_err("非法内联 MCP 配置必须失败");
    let text = error.to_string();
    assert!(
        text.contains(&plugin_manifest_path(&plugin_dir).display().to_string()),
        "错误必须带清单路径: {text}"
    );
    assert!(
        text.contains(SYSTEM_TOOLS_RULE),
        "错误必须带固定规则正文: {text}"
    );
}

#[test]
fn test_system_mcp_plugin_invalid_manifest_has_no_fallback() {
    // 已存在但非法的清单：不得 synthetic overwrite、不得从根配置兜底、文件字节不变。
    let dir = tempdir().unwrap();
    let claude_home = dir.path().join(".claude-test");
    let plugin_dir = install_plugin(&claude_home, "broken-manifest", None);
    let manifest_path = plugin_manifest_path(&plugin_dir);
    let broken = r#"{"name":"broken-manifest","version":"1.0.0","mcpServers":{"bad":{"system_mcp_tools":["t"]}}}"#;
    std::fs::write(&manifest_path, broken).unwrap();
    // 根配置里放一个合法 server：非法清单不允许借它兜底「修复」。
    std::fs::write(
        plugin_dir.join(".mcp.json"),
        r#"{"mcpServers":{"ok":{"command":"npx"}}}"#,
    )
    .unwrap();

    let error = load_enabled_plugins_for_mcp(&claude_home, None).expect_err("非法现存清单必须失败");
    assert!(
        error.to_string().contains(SYSTEM_TOOLS_RULE),
        "必须保留清单解析的明确错误: {error}"
    );
    assert_eq!(
        std::fs::read_to_string(&manifest_path).unwrap(),
        broken,
        "非法清单不得被覆盖或修复"
    );
}

#[test]
fn test_system_mcp_plugin_empty_manifest_map_has_no_fallback() {
    // manifest 显式声明空 map：属于「已声明」，不得从根 .mcp.json 加载额外服务器。
    let dir = tempdir().unwrap();
    let claude_home = dir.path().join(".claude-test");
    let plugin_dir = install_plugin(&claude_home, "empty-map", Some("{}"));
    std::fs::write(
        plugin_dir.join(".mcp.json"),
        r#"{"mcpServers":{"root-srv":{"command":"npx"}}}"#,
    )
    .unwrap();

    let plugins = load_enabled_plugins_for_mcp(&claude_home, None).unwrap();
    assert_eq!(plugins.len(), 1);
    assert!(
        plugins[0].mcp_servers.is_empty(),
        "显式空 map 不得触发根配置回退: {:?}",
        plugins[0].mcp_servers.keys().collect::<Vec<_>>()
    );

    // 未声明（manifest 无 mcpServers 字段）才允许根回退。
    let dir = tempdir().unwrap();
    let claude_home = dir.path().join(".claude-test");
    let plugin_dir = install_plugin(&claude_home, "no-decl", None);
    std::fs::write(
        plugin_dir.join(".mcp.json"),
        r#"{"mcpServers":{"root-srv":{"command":"npx"}}}"#,
    )
    .unwrap();
    let plugins = load_enabled_plugins_for_mcp(&claude_home, None).unwrap();
    assert!(plugins[0].mcp_servers.contains_key("root-srv"));
}

#[test]
fn test_system_mcp_plugin_lenient_aggregate_keeps_other_capabilities() {
    // 宽容聚合路径（展示/面板）保持产品行为：坏 MCP 声明不阻止该插件的
    // hooks 装配，但错误必须被记录而不是静默丢弃。
    let dir = tempdir().unwrap();
    let claude_home = dir.path().join(".claude-test");
    let plugin_dir = install_plugin(
        &claude_home,
        "lenient",
        Some(r#"{"srv":"servers/.mcp.json"}"#),
    );
    let servers_dir = plugin_dir.join("servers");
    std::fs::create_dir_all(&servers_dir).unwrap();
    std::fs::write(
        servers_dir.join(".mcp.json"),
        r#"{"mcpServers":{"bad":{"system_mcp_tools":["t"]}}}"#,
    )
    .unwrap();
    // 插件的其它能力（hooks 约定文件）必须仍然装配。
    std::fs::create_dir_all(plugin_dir.join("hooks")).unwrap();
    std::fs::write(
        plugin_dir.join("hooks").join("hooks.json"),
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"echo hi"}]}]}}"#,
    )
    .unwrap();

    let aggregated = load_enabled_plugins_aggregated(&claude_home, None);
    assert_eq!(aggregated.plugins.len(), 1, "坏 MCP 配置不得让插件整体消失");
    assert!(
        aggregated.all_mcp_servers.is_empty(),
        "非法 MCP 声明不得进入聚合目录"
    );
    assert!(!aggregated.all_hooks.is_empty(), "插件的其它能力必须保留");
    assert_eq!(aggregated.all_hooks[0].plugin_name, "lenient");
}

/// 目录树快照（相对 root 的路径集合），用于证明只读发现没有写副作用。
fn snapshot_paths(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                out.push(path.strip_prefix(root).unwrap_or(&path).to_path_buf());
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

/// 只读准备发现：清单缺失必须失败并定位插件，不得生成合成清单（无写副作用）。
#[test]
fn test_readonly_discovery_missing_manifest_fails_without_repair() {
    let dir = tempdir().unwrap();
    let claude_home = dir.path().join(".claude-test");
    let plugin_dir = install_plugin(&claude_home, "ghost", None);
    let manifest_path = plugin_dir.join(".claude-plugin").join("plugin.json");
    std::fs::remove_file(&manifest_path).unwrap();

    let before = snapshot_paths(&plugin_dir);
    let error = load_enabled_plugins_aggregated_readonly(&claude_home, None)
        .expect_err("readonly discovery must not silently skip a plugin with a missing manifest");
    let message = error.to_string();
    assert!(
        message.contains("ghost") && message.contains(&manifest_path.display().to_string()),
        "error must locate the offending plugin and manifest path: {message}"
    );
    assert!(
        !manifest_path.exists(),
        "readonly discovery must not synthesize a manifest"
    );
    assert_eq!(
        before,
        snapshot_paths(&plugin_dir),
        "readonly discovery must not write plugin files"
    );
}

/// 同一布局下宽容聚合保持既有产品行为（跳过该插件、返回空结果），与只读路径明确分离。
#[test]
fn test_lenient_aggregate_still_skips_missing_manifest() {
    let dir = tempdir().unwrap();
    let claude_home = dir.path().join(".claude-test");
    let plugin_dir = install_plugin(&claude_home, "ghost", None);
    std::fs::remove_file(plugin_dir.join(".claude-plugin").join("plugin.json")).unwrap();

    let aggregated = load_enabled_plugins_aggregated(&claude_home, None);
    assert!(
        aggregated.plugins.is_empty(),
        "宽容聚合仍跳过缺失清单的插件，不因准备路径严格化而改变"
    );
}

/// 非法清单在只读路径上以可定位错误失败，不被当作用户未安装而跳过。
#[test]
fn test_readonly_discovery_invalid_manifest_fails() {
    let dir = tempdir().unwrap();
    let claude_home = dir.path().join(".claude-test");
    let plugin_dir = install_plugin(&claude_home, "broken", None);
    std::fs::write(
        plugin_dir.join(".claude-plugin").join("plugin.json"),
        "{ not json",
    )
    .unwrap();

    let error = load_enabled_plugins_aggregated_readonly(&claude_home, None)
        .expect_err("readonly discovery must fail on an invalid manifest");
    let message = error.to_string();
    assert!(
        message.contains("broken") && message.contains(&plugin_dir.display().to_string()),
        "error must locate the offending plugin: {message}"
    );
}

/// 有效插件：只读聚合成功、无写副作用，且同输入重复调用结果一致。
#[test]
fn test_readonly_discovery_is_side_effect_free_and_repeatable() {
    let dir = tempdir().unwrap();
    let claude_home = dir.path().join(".claude-test");
    install_plugin(&claude_home, "ok", None);

    let before = snapshot_paths(&claude_home);
    let first = load_enabled_plugins_aggregated_readonly(&claude_home, None).unwrap();
    let second = load_enabled_plugins_aggregated_readonly(&claude_home, None).unwrap();

    assert_eq!(first.plugins.len(), 1);
    assert_eq!(first.plugins[0].name, second.plugins[0].name);
    assert_eq!(first.all_commands.len(), second.all_commands.len());
    assert_eq!(first.all_skill_roots.len(), second.all_skill_roots.len());
    assert_eq!(first.all_agent_dirs.len(), second.all_agent_dirs.len());
    assert_eq!(
        before,
        snapshot_paths(&claude_home),
        "readonly discovery must not write any file under the plugin home"
    );
}
