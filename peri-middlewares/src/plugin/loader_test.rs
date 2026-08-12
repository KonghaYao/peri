use tempfile::tempdir;

use peri_resources::lsp::config::LspConfigSource;

use super::*;
use crate::plugin::types::{
    InstallScope, InstalledPlugin, PluginAgent, PluginCommand, PluginCommandEntry, PluginOrigin,
};

pub(crate) fn make_manifest_with_commands(commands: Vec<PluginCommand>) -> PluginManifest {
    let entries: Vec<PluginCommandEntry> =
        commands.into_iter().map(PluginCommandEntry::Full).collect();
    PluginManifest {
        name: "test-plugin".into(),
        version: "1.0.0".into(),
        description: String::new(),
        author: None,
        commands: if entries.is_empty() {
            None
        } else {
            Some(entries)
        },
        agents: None,
        skills: None,
        hooks: None,
        mcp_servers: None,
        lsp_servers: None,
        output_styles: None,
        channels: None,
        options: None,
        settings: None,
        extra: serde_json::json!({}),
    }
}

#[test]
fn test_parse_command_md_with_shell() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cmd.md");
    std::fs::write(&path, "---\nshell: echo hello\n---\nBody content").unwrap();
    let (fm, body) = parse_command_md(&path).unwrap();
    assert_eq!(fm.shell.as_deref(), Some("echo hello"));
    assert_eq!(body.trim(), "Body content");
}

#[test]
fn test_parse_command_md_with_all_fields() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cmd.md");
    std::fs::write(
            &path,
            "---\nshell: echo hi\neffort: low\nmodel: opus\ndescription: Test cmd\nargs:\n  - foo\n---\nBody",
        )
        .unwrap();
    let (fm, _) = parse_command_md(&path).unwrap();
    assert_eq!(fm.shell.as_deref(), Some("echo hi"));
    assert_eq!(fm.effort.as_deref(), Some("low"));
    assert_eq!(fm.model.as_deref(), Some("opus"));
    assert_eq!(fm.description.as_deref(), Some("Test cmd"));
    assert!(fm.args.is_some());
}

#[test]
fn test_parse_command_md_no_frontmatter() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cmd.md");
    std::fs::write(&path, "Just plain markdown").unwrap();
    let (fm, body) = parse_command_md(&path).unwrap();
    assert!(fm.shell.is_none());
    assert_eq!(body, "Just plain markdown");
}

#[test]
fn test_parse_command_md_file_not_found() {
    let result = parse_command_md(Path::new("/nonexistent/cmd.md"));
    assert!(result.is_none());
}

#[test]
fn test_extract_commands_single() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("commands")).unwrap();
    std::fs::write(dir.path().join("commands/test.md"), "---\n---\nContent").unwrap();

    let manifest = make_manifest_with_commands(vec![PluginCommand {
        path: "commands/test.md".into(),
        name: None,
        description: None,
    }]);

    let entries = extract_commands(&manifest, dir.path(), "my-plugin");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "my-plugin:test");
}

#[test]
fn test_extract_commands_multiple() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("commands")).unwrap();
    std::fs::write(dir.path().join("commands/a.md"), "---\n---\nA").unwrap();
    std::fs::write(dir.path().join("commands/b.md"), "---\n---\nB").unwrap();

    let manifest = make_manifest_with_commands(vec![
        PluginCommand {
            path: "commands/a.md".into(),
            name: None,
            description: None,
        },
        PluginCommand {
            path: "commands/b.md".into(),
            name: None,
            description: None,
        },
    ]);

    let entries = extract_commands(&manifest, dir.path(), "p");
    assert_eq!(entries.len(), 2);
}

#[test]
fn test_extract_commands_missing_file() {
    let manifest = make_manifest_with_commands(vec![PluginCommand {
        path: "commands/missing.md".into(),
        name: None,
        description: None,
    }]);
    let entries = extract_commands(&manifest, Path::new("/tmp"), "p");
    assert!(entries.is_empty());
}

#[test]
fn test_extract_commands_explicit_name() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("commands")).unwrap();
    std::fs::write(dir.path().join("commands/x.md"), "---\n---\nX").unwrap();

    let manifest = make_manifest_with_commands(vec![PluginCommand {
        path: "commands/x.md".into(),
        name: Some("my-cmd".into()),
        description: None,
    }]);

    let entries = extract_commands(&manifest, dir.path(), "p");
    assert_eq!(entries[0].name, "p:my-cmd");
}

#[test]
fn test_extract_commands_none() {
    let manifest = make_manifest_with_commands(vec![]);
    let entries = extract_commands(&manifest, Path::new("/tmp"), "p");
    assert!(entries.is_empty());
}

#[test]
fn test_extract_commands_frontmatter_description() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("commands")).unwrap();
    std::fs::write(
        dir.path().join("commands/x.md"),
        "---\ndescription: FM desc\n---\nBody",
    )
    .unwrap();

    let manifest = make_manifest_with_commands(vec![PluginCommand {
        path: "commands/x.md".into(),
        name: None,
        description: Some("manifest desc".into()),
    }]);

    let entries = extract_commands(&manifest, dir.path(), "p");
    assert_eq!(entries[0].description, "FM desc");
}

#[test]
fn test_extract_skills_paths() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("skills").join("code-review")).unwrap();
    // 函数要求 SKILL.md 存在于技能目录中
    std::fs::write(
        dir.path()
            .join("skills")
            .join("code-review")
            .join("SKILL.md"),
        "---\n---\n",
    )
    .unwrap();

    let mut manifest = make_manifest_with_commands(vec![]);
    manifest.skills = Some(vec!["skills/code-review".into()]);

    let paths = extract_skills_paths(&manifest, dir.path(), "test-plugin");
    assert_eq!(paths.len(), 1);
    assert!(paths[0].path.ends_with("code-review"));
    assert_eq!(paths[0].source, SkillSource::Plugin);
    assert_eq!(paths[0].plugin_name.as_deref(), Some("test-plugin"));
}

#[test]
fn test_extract_skills_paths_missing_dir() {
    let mut manifest = make_manifest_with_commands(vec![]);
    manifest.skills = Some(vec!["nonexistent".into()]);

    let paths = extract_skills_paths(&manifest, Path::new("/tmp"), "test-plugin");
    assert!(paths.is_empty());
}

#[test]
fn test_extract_skills_paths_none() {
    let dir = tempdir().unwrap();
    let manifest = make_manifest_with_commands(vec![]);
    // no skills dir at all → fallback finds nothing
    let paths = extract_skills_paths(&manifest, dir.path(), "test-plugin");
    assert!(paths.is_empty());
}

#[test]
fn test_extract_skills_paths_fallback_returns_container_root() {
    let dir = tempdir().unwrap();
    let skill_dir = dir.path().join("skills").join("my-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(skill_dir.join("SKILL.md"), "---\nname: my-skill\n---\nbody").unwrap();

    // manifest has no skills field → fallback returns base_dir/skills/ as a single root;
    // scan_skill_roots will recursively find my-skill inside.
    let manifest = make_manifest_with_commands(vec![]);
    let paths = extract_skills_paths(&manifest, dir.path(), "test-plugin");
    assert_eq!(paths.len(), 1);
    assert!(paths[0].path.ends_with("skills"));
    assert_eq!(paths[0].source, SkillSource::Plugin);
}

#[test]
fn test_extract_skills_paths_fallback_returns_root_even_without_skill_md() {
    let dir = tempdir().unwrap();
    let skill_dir = dir.path().join("skills").join("incomplete");
    std::fs::create_dir_all(&skill_dir).unwrap();
    // no SKILL.md inside, but fallback still returns the container root;
    // scan_skill_roots will simply find no skills inside.

    let manifest = make_manifest_with_commands(vec![]);
    let paths = extract_skills_paths(&manifest, dir.path(), "test-plugin");
    assert_eq!(
        paths.len(),
        1,
        "fallback 仍返回容器根，由 scan_skill_roots 决定是否含 skill"
    );
    assert!(paths[0].path.ends_with("skills"));
}

#[test]
fn test_extract_agents_paths() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("agents")).unwrap();
    std::fs::write(dir.path().join("agents/reviewer.md"), "content").unwrap();

    let mut manifest = make_manifest_with_commands(vec![]);
    manifest.agents = Some(vec![PluginAgent {
        path: "agents/reviewer.md".into(),
        name: "reviewer".into(),
    }]);

    let paths = extract_agents_paths(&manifest, dir.path());
    assert_eq!(paths.len(), 1);
}

#[test]
fn test_extract_agents_paths_missing() {
    let mut manifest = make_manifest_with_commands(vec![]);
    manifest.agents = Some(vec![PluginAgent {
        path: "agents/missing.md".into(),
        name: "missing".into(),
    }]);

    let paths = extract_agents_paths(&manifest, Path::new("/tmp"));
    assert!(paths.is_empty());
}

#[test]
fn test_extract_agents_paths_none() {
    let manifest = make_manifest_with_commands(vec![]);
    let paths = extract_agents_paths(&manifest, Path::new("/tmp"));
    assert!(paths.is_empty());
}

#[test]
fn test_extract_mcp_servers() {
    let mut manifest = make_manifest_with_commands(vec![]);
    let mut servers = HashMap::new();
    servers.insert(
        "s1".into(),
        McpServerEntry::Config(Box::new(McpServerConfig {
            command: Some("node".into()),
            args: None,
            env: None,
            url: None,
            headers: None,
            oauth: None,
            disabled: None,
            source: None,
        })),
    );
    manifest.mcp_servers = Some(servers);

    let result = extract_mcp_servers(&manifest, Path::new("/tmp"));
    assert_eq!(result.len(), 1);
    assert!(result.contains_key("s1"));
}

#[test]
fn test_extract_mcp_servers_none() {
    let manifest = make_manifest_with_commands(vec![]);
    let result = extract_mcp_servers(&manifest, Path::new("/tmp"));
    assert!(result.is_empty());
}

#[test]
fn test_extract_mcp_servers_file_path_ref() {
    let dir = tempdir().unwrap();
    let plugin_dir = dir.path().join("my-plugin");
    let servers_dir = plugin_dir.join("servers");
    std::fs::create_dir_all(&servers_dir).unwrap();

    // 创建 .mcp.json 文件
    let mcp_json = r#"{"mcpServers":{"db":{"command":"sqlite3","args":["test.db"]}}}"#;
    std::fs::write(servers_dir.join(".mcp.json"), mcp_json).unwrap();

    let mut manifest = make_manifest_with_commands(vec![]);
    let mut servers = HashMap::new();
    servers.insert(
        "db".into(),
        McpServerEntry::FilePath("servers/.mcp.json".into()),
    );
    manifest.mcp_servers = Some(servers);

    let result = extract_mcp_servers(&manifest, &plugin_dir);
    assert_eq!(result.len(), 1);
    assert!(result.contains_key("db"));
    assert_eq!(result["db"].command.as_deref(), Some("sqlite3"));
}

#[test]
fn test_extract_mcp_servers_file_path_not_found() {
    let dir = tempdir().unwrap();
    let mut manifest = make_manifest_with_commands(vec![]);
    let mut servers = HashMap::new();
    servers.insert(
        "missing".into(),
        McpServerEntry::FilePath("nonexistent/.mcp.json".into()),
    );
    manifest.mcp_servers = Some(servers);

    let result = extract_mcp_servers(&manifest, dir.path());
    assert!(result.is_empty());
}

#[test]
fn test_extract_mcp_servers_fallback_mcp_json_standard_format() {
    let dir = tempdir().unwrap();
    // No mcpServers in manifest → should fall back to .mcp.json at plugin root
    std::fs::write(
        dir.path().join(".mcp.json"),
        r#"{"mcpServers":{"srv":{"command":"npx","args":["test"]}}}"#,
    )
    .unwrap();

    let manifest = make_manifest_with_commands(vec![]);
    let result = extract_mcp_servers(&manifest, dir.path());
    assert_eq!(result.len(), 1);
    assert!(result.contains_key("srv"));
    assert_eq!(result["srv"].command.as_deref(), Some("npx"));
}

#[test]
fn test_extract_mcp_servers_fallback_mcp_json_flat_format() {
    let dir = tempdir().unwrap();
    // Flat format like context7: {"serverName": {...}} without mcpServers wrapper
    std::fs::write(
        dir.path().join(".mcp.json"),
        r#"{"context7":{"command":"npx","args":["-y","@upstash/context7-mcp"]}}"#,
    )
    .unwrap();

    let manifest = make_manifest_with_commands(vec![]);
    let result = extract_mcp_servers(&manifest, dir.path());
    assert_eq!(result.len(), 1);
    assert!(result.contains_key("context7"));
    assert_eq!(result["context7"].command.as_deref(), Some("npx"));
    assert_eq!(
        result["context7"].args.as_ref().unwrap(),
        &vec!["-y", "@upstash/context7-mcp"]
    );
}

#[test]
fn test_extract_mcp_servers_manifest_has_priority_over_fallback() {
    let dir = tempdir().unwrap();
    // manifest has mcpServers → fallback should NOT be used
    std::fs::write(
        dir.path().join(".mcp.json"),
        r#"{"fallbackSrv":{"command":"fallback-cmd"}}"#,
    )
    .unwrap();

    let mut manifest = make_manifest_with_commands(vec![]);
    let mut servers = HashMap::new();
    servers.insert(
        "inline".into(),
        McpServerEntry::Config(Box::new(McpServerConfig {
            command: Some("inline-cmd".into()),
            args: None,
            env: None,
            url: None,
            headers: None,
            oauth: None,
            disabled: None,
            source: None,
        })),
    );
    manifest.mcp_servers = Some(servers);

    let result = extract_mcp_servers(&manifest, dir.path());
    assert_eq!(result.len(), 1);
    assert!(result.contains_key("inline"));
    assert_eq!(result["inline"].command.as_deref(), Some("inline-cmd"));
}

#[test]
fn test_load_mcp_json_file_flat_format_multiple_servers() {
    let dir = tempdir().unwrap();
    let mcp_json_path = dir.path().join("test.mcp.json");
    std::fs::write(
        &mcp_json_path,
        r#"{"srv1":{"command":"cmd1"},"srv2":{"url":"https://example.com"}}"#,
    )
    .unwrap();

    let result = super::load_mcp_json_file(&mcp_json_path).unwrap();
    assert_eq!(result.len(), 2);
    assert!(result.contains_key("srv1"));
    assert!(result.contains_key("srv2"));
}

#[test]
fn test_load_mcp_json_file_standard_format() {
    let dir = tempdir().unwrap();
    let mcp_json_path = dir.path().join("test.mcp.json");
    std::fs::write(
        &mcp_json_path,
        r#"{"mcpServers":{"srv":{"command":"echo","args":["hi"]}}}"#,
    )
    .unwrap();

    let result = super::load_mcp_json_file(&mcp_json_path).unwrap();
    assert_eq!(result.len(), 1);
    assert!(result.contains_key("srv"));
}

#[test]
fn test_load_mcp_json_file_nonexistent() {
    let result = super::load_mcp_json_file(Path::new("/nonexistent/mcp.json"));
    assert!(result.is_none());
}

#[test]
fn test_load_mcp_json_file_invalid_json() {
    let dir = tempdir().unwrap();
    let mcp_json_path = dir.path().join("bad.mcp.json");
    std::fs::write(&mcp_json_path, b"not json").unwrap();
    let result = super::load_mcp_json_file(&mcp_json_path);
    assert!(result.is_none());
}

#[test]
fn test_merge_plugin_mcp_servers() {
    let mut p1 = LoadedPlugin {
        name: "plugin-a".into(),
        version: "1.0.0".into(),
        install_path: PathBuf::new(),
        manifest: make_manifest_with_commands(vec![]),
        commands: vec![],
        skills_roots: vec![],
        agents_dirs: vec![],
        mcp_servers: HashMap::new(),
        data_path: PathBuf::new(),
        hooks_config: None,
        marketplace: String::new(),
    };
    p1.mcp_servers.insert(
        "db".into(),
        McpServerConfig {
            command: Some("pg".into()),
            args: None,
            env: None,
            url: None,
            headers: None,
            oauth: None,
            disabled: None,
            source: None,
        },
    );

    let mut p2 = LoadedPlugin {
        name: "plugin-b".into(),
        version: "1.0.0".into(),
        install_path: PathBuf::new(),
        manifest: make_manifest_with_commands(vec![]),
        commands: vec![],
        skills_roots: vec![],
        agents_dirs: vec![],
        mcp_servers: HashMap::new(),
        data_path: PathBuf::new(),
        hooks_config: None,
        marketplace: String::new(),
    };
    p2.mcp_servers.insert(
        "db".into(),
        McpServerConfig {
            command: Some("mongo".into()),
            args: None,
            env: None,
            url: None,
            headers: None,
            oauth: None,
            disabled: None,
            source: None,
        },
    );

    let merged = merge_plugin_mcp_servers(&[p1, p2]);
    assert_eq!(merged.len(), 2);
    assert!(merged.contains_key("plugin:plugin-a:db"));
    assert!(merged.contains_key("plugin:plugin-b:db"));
}

#[test]
fn test_load_plugins_success() {
    let dir = tempdir().unwrap();
    let plugin_dir = dir.path().join("my-plugin");
    std::fs::create_dir_all(plugin_dir.join(".claude-plugin")).unwrap();
    std::fs::write(
        plugin_dir.join(".claude-plugin").join("plugin.json"),
        r#"{"name":"my-plugin","version":"1.0.0"}"#,
    )
    .unwrap();

    let installed = InstalledPlugins {
        version: 2,
        plugins: vec![InstalledPlugin {
            id: "my-plugin@test".into(),
            name: "my-plugin".into(),
            version: "1.0.0".into(),
            marketplace: "test".into(),
            install_path: plugin_dir,
            scope: InstallScope::User,
            project_path: None,
            origin: PluginOrigin::PeriInstalled,
        }],
    };

    let loaded = load_plugins(&installed).unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].name, "my-plugin");
}

#[test]
fn test_load_plugins_empty() {
    let installed = InstalledPlugins::default();
    let loaded = load_plugins(&installed).unwrap();
    assert!(loaded.is_empty());
}

#[test]
fn test_load_plugins_invalid_manifest() {
    let dir = tempdir().unwrap();
    let installed = InstalledPlugins {
        version: 2,
        plugins: vec![InstalledPlugin {
            id: "bad@test".into(),
            name: "bad".into(),
            version: "1.0.0".into(),
            marketplace: "test".into(),
            install_path: dir.path().join("empty"),
            scope: InstallScope::User,
            project_path: None,
            origin: PluginOrigin::PeriInstalled,
        }],
    };

    let loaded = load_plugins(&installed).unwrap();
    assert!(loaded.is_empty());
}

#[test]
fn test_load_plugins_synthetic_manifest_fallback() {
    let dir = tempdir().unwrap();

    // 创建插件缓存目录（无 plugin.json）
    let plugin_install_path = dir
        .path()
        .join("cache")
        .join("test-mkt")
        .join("lsp-plugin")
        .join("1.0.0");
    std::fs::create_dir_all(&plugin_install_path).unwrap();

    // marketplaces_cache_dir() 读取的是 ~/.claude/plugins/marketplaces，
    // 测试中无法覆盖。直接测试 try_generate_synthetic_manifest_fallback 函数，
    // 验证当 marketplace 缓存不在默认路径时返回 false。
    let result =
        try_generate_synthetic_manifest_fallback(&plugin_install_path, "lsp-plugin", "test-mkt");

    // 由于 marketplace 缓存不在默认路径，fallback 应该返回 false
    assert!(!result);
}

#[test]
fn test_try_generate_synthetic_manifest_fallback_no_marketplace() {
    let dir = tempdir().unwrap();
    let plugin_path = dir.path().join("some-plugin");

    // marketplace 为空时应该返回 false
    let result = try_generate_synthetic_manifest_fallback(&plugin_path, "some-plugin", "");
    assert!(!result);
}

#[test]
fn test_try_generate_synthetic_manifest_fallback_already_has_manifest() {
    let dir = tempdir().unwrap();
    let plugin_path = dir.path().join("has-manifest");
    std::fs::create_dir_all(plugin_path.join(".claude-plugin")).unwrap();
    std::fs::write(
        plugin_path.join(".claude-plugin").join("plugin.json"),
        r#"{"name":"existing","version":"1.0.0"}"#,
    )
    .unwrap();

    // 已有 plugin.json 时不应覆盖
    let result = try_generate_synthetic_manifest_fallback(&plugin_path, "has-manifest", "test-mkt");
    assert!(!result);

    // 原有内容保持不变
    let content =
        std::fs::read_to_string(plugin_path.join(".claude-plugin").join("plugin.json")).unwrap();
    assert!(content.contains("existing"));
}

#[test]
fn test_load_enabled_plugins() {
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

    let loaded = load_enabled_plugins(dir.path(), None).unwrap();
    assert_eq!(loaded.len(), 1);
}

#[test]
fn test_load_enabled_plugins_disabled() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("plugins")).unwrap();
    let installed_json = serde_json::to_string(&InstalledPlugins {
        version: 2,
        plugins: vec![InstalledPlugin {
            id: "my-plugin@test".into(),
            name: "my-plugin".into(),
            version: "1.0.0".into(),
            marketplace: "test".into(),
            install_path: dir.path().join("fake"),
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

    let settings = r#"{"enabledPlugins":[]}"#;
    std::fs::write(dir.path().join("settings.json"), settings).unwrap();

    let loaded = load_enabled_plugins(dir.path(), None).unwrap();
    assert!(loaded.is_empty());
}

#[test]
fn test_plugin_command_provider_empty() {
    let provider = PluginCommandProvider::new(&[]);
    assert!(provider.commands().is_empty());
}

#[test]
fn test_plugin_command_provider_multiple() {
    let loaded = vec![
        LoadedPlugin {
            name: "p1".into(),
            version: "1.0.0".into(),
            install_path: PathBuf::new(),
            manifest: make_manifest_with_commands(vec![]),
            commands: vec![
                CommandEntry {
                    name: "p1:cmd1".into(),
                    description: "d1".into(),
                    source: CommandSource::Builtin,
                },
                CommandEntry {
                    name: "p1:cmd2".into(),
                    description: "d2".into(),
                    source: CommandSource::Builtin,
                },
            ],
            skills_roots: vec![],
            agents_dirs: vec![],
            mcp_servers: HashMap::new(),
            data_path: PathBuf::new(),
            hooks_config: None,
            marketplace: String::new(),
        },
        LoadedPlugin {
            name: "p2".into(),
            version: "1.0.0".into(),
            install_path: PathBuf::new(),
            manifest: make_manifest_with_commands(vec![]),
            commands: vec![
                CommandEntry {
                    name: "p2:cmd3".into(),
                    description: "d3".into(),
                    source: CommandSource::Builtin,
                },
                CommandEntry {
                    name: "p2:cmd4".into(),
                    description: "d4".into(),
                    source: CommandSource::Builtin,
                },
            ],
            skills_roots: vec![],
            agents_dirs: vec![],
            mcp_servers: HashMap::new(),
            data_path: PathBuf::new(),
            hooks_config: None,
            marketplace: String::new(),
        },
    ];

    let provider = PluginCommandProvider::new(&loaded);
    assert_eq!(provider.commands().len(), 4);
}

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

    let result = load_enabled_plugins_aggregated(dir.path(), None);
    assert_eq!(result.plugins.len(), 1);
    assert_eq!(result.plugins[0].name, "my-plugin");
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
    assert_eq!(names, vec!["ecc:deploy", "ecc:rollback"]);
    let deploy = entries.iter().find(|e| e.name == "ecc:deploy").unwrap();
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
    assert_eq!(entries[0].name, "p:standalone");
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

#[test]
fn test_load_lsp_servers_aggregated_injects_plugin_root_env() {
    // 插件 lspServers 经 lsp_config_from_plugin 转换：env 注入 CLAUDE_PLUGIN_ROOT，
    // 插件根相对命令 ${CLAUDE_PLUGIN_ROOT}/bin/server 可解析（回归：曾手写构造 env: None）
    let dir = tempdir().unwrap();
    let plugin_dir = dir.path().join("lsp-plugin");
    std::fs::create_dir_all(plugin_dir.join(".claude-plugin")).unwrap();
    std::fs::write(
        plugin_dir.join(".claude-plugin").join("plugin.json"),
        r#"{
            "name": "lsp-plugin",
            "version": "1.0.0",
            "lspServers": [
                {
                    "name": "my-server",
                    "command": "${CLAUDE_PLUGIN_ROOT}/bin/server",
                    "args": ["--stdio"],
                    "extensionToLanguage": {".txt": "plaintext"}
                }
            ]
        }"#,
    )
    .unwrap();

    std::fs::create_dir_all(dir.path().join("plugins")).unwrap();
    let installed_json = serde_json::to_string(&InstalledPlugins {
        version: 2,
        plugins: vec![InstalledPlugin {
            id: "lsp-plugin@test".into(),
            name: "lsp-plugin".into(),
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

    let settings = r#"{"enabledPlugins":["lsp-plugin@test"]}"#;
    std::fs::write(dir.path().join("settings.json"), settings).unwrap();

    let result = load_enabled_plugins_aggregated(dir.path(), None);
    assert_eq!(result.all_lsp_servers.len(), 1);
    let server = &result.all_lsp_servers[0];
    // env 注入断言：配置含 CLAUDE_PLUGIN_ROOT（插件根）
    let env = server.env.as_ref().expect("插件 LSP 配置应注入 env");
    assert_eq!(
        env.get("CLAUDE_PLUGIN_ROOT").map(|s| s.as_str()),
        Some(plugin_dir.to_string_lossy().as_ref())
    );
    // 插件根相对命令按注入 env 展开
    assert_eq!(
        server.command,
        format!("{}/bin/server", plugin_dir.display())
    );
    // manifest 字段映射保留（name 命名空间化、args、extensionToLanguage、source）
    assert_eq!(server.name, "plugin:lsp-plugin:my-server");
    assert_eq!(server.args, vec!["--stdio"]);
    assert_eq!(
        server.extension_to_language.get(".txt").unwrap(),
        "plaintext"
    );
    assert_eq!(
        server.source,
        Some(LspConfigSource::Plugin {
            plugin_name: "lsp-plugin".to_string()
        })
    );
}
