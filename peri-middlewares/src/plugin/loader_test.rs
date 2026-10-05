use tempfile::tempdir;

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
        output_styles: None,
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
    assert_eq!(entries[0].name, "plugin:my-plugin:test");
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
    assert_eq!(entries[0].name, "plugin:p:my-cmd");
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

// ── plugin_route_entries（Phase 6 B2 转换函数；P2-1 直测）──────────────────

/// 最小事件 sink（占位 handler execute 需要 CommandContext）。
struct NoopEventSink;

#[async_trait::async_trait]
impl peri_acp_types::event::EventSink for NoopEventSink {
    async fn push_event(
        &self,
        _session_id: &str,
        _event: &peri_acp_types::event::ExecutorEvent,
        _context_window: u32,
    ) {
    }

    async fn push_done(&self, _session_id: &str, _stop_reason: &str, _request_id: Option<&str>) {}
}

/// 转换主路径：fullname / 剥离 plugin: 前缀的 provenance / kind / lifecycle
/// 全量锁定（P0-1 回归：未剥离前缀 → namespace 段校验失败 →
/// ProvenanceMismatch 全量拒绝，本用例直接暴露）。
#[test]
fn test_plugin_route_entries_full() {
    let entries = vec![CommandEntry {
        name: "plugin:ecc:deploy".into(),
        description: "Deploy to prod".into(),
        source: CommandSource::Plugin {
            path: PathBuf::from("/tmp/ecc/deploy.md"),
        },
    }];
    let routes = plugin_route_entries(&entries);
    assert_eq!(routes.len(), 1);
    let r = &routes[0];
    // fullname 原样使用（三层形态 `plugin:{plugin}:{cmd}`）
    assert_eq!(r.fullname, "plugin:ecc:deploy");
    assert!(r.aliases.is_empty(), "插件条目不得带 alias");
    assert_eq!(r.description, "Deploy to prod");
    // kind / lifecycle
    assert_eq!(r.kind, CommandEntryKind::Command, "plugin 域暂归 Command");
    assert_eq!(r.provenance.lifecycle, CommandLifecycle::Connected);
    assert!(r.category.is_none());
    assert!(r.args_schema.is_none());
    // provenance.source：plugin: 前缀必须剥离（P0-1）
    match &r.provenance.source {
        RouteCommandSource::Plugin { name } => {
            assert_eq!(name, "ecc", "plugin: 前缀必须剥离，实际: {name}");
        }
        other => panic!("source 应为 Plugin，实际: {other:?}"),
    }
    // 与词法 namespace 段一致（register 域校验通过面，设计 §58）
    assert_eq!(r.provenance.source.namespace(), Some("ecc"));
}

/// 占位 handler 语义：execute → `Inject` 空串（fall-through 进 agent 管线，
/// 命令不被吞——与 McpSkillPlaceholder / PassthroughPlaceholder 同构）。
#[tokio::test]
async fn test_plugin_route_entries_handler_is_placeholder_inject() {
    let entries = vec![CommandEntry {
        name: "plugin:ecc:deploy".into(),
        description: String::new(),
        source: CommandSource::Builtin,
    }];
    let routes = plugin_route_entries(&entries);
    let outcome = routes[0]
        .handler
        .execute(CommandContext::new(
            "test-session".into(),
            vec![],
            "/tmp".into(),
            Arc::new(NoopEventSink),
            tokio_util::sync::CancellationToken::new(),
            Default::default(),
        ))
        .await;
    assert!(
        matches!(&outcome, CommandOutcome::Inject(s) if s.is_empty()),
        "占位 handler 应 Inject 空串，实际 outcome 非 Inject"
    );
}

/// 词法异常 name 全量跳过：单层 `plugin:x`（缺末段 cmd）、非 plugin 域
/// `foo:bar`、无冒号裸名（register 阶段词法校验兜底）。
#[test]
fn test_plugin_route_entries_skips_lexically_abnormal() {
    let entries = vec![
        CommandEntry {
            name: "plugin:x".into(),
            description: String::new(),
            source: CommandSource::Builtin,
        },
        CommandEntry {
            name: "foo:bar".into(),
            description: String::new(),
            source: CommandSource::Builtin,
        },
        CommandEntry {
            name: "nocolon".into(),
            description: String::new(),
            source: CommandSource::Builtin,
        },
        // 合法条目共存时仅合法者产出（过滤不得误伤）
        CommandEntry {
            name: "plugin:p:standalone".into(),
            description: String::new(),
            source: CommandSource::Builtin,
        },
    ];
    let routes = plugin_route_entries(&entries);
    assert_eq!(routes.len(), 1, "词法异常 name 应跳过，仅合法条目产出");
    assert_eq!(routes[0].fullname, "plugin:p:standalone");
    assert_eq!(
        routes[0].provenance.source.namespace(),
        Some("p"),
        "合法条目 provenance 剥离 plugin: 前缀"
    );
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
    // provider 会递归发现其中的 my-skill（宿主侧扫描已删除）。
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
    // provider 在其中不会发现任何 skill（宿主侧扫描已删除）。

    let manifest = make_manifest_with_commands(vec![]);
    let paths = extract_skills_paths(&manifest, dir.path(), "test-plugin");
    assert_eq!(
        paths.len(),
        1,
        "fallback 仍返回容器根，是否含 skill 由 provider 扫描决定"
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
            subscriptions: None,
            system_mcp: None,
            system_mcp_tools: None,
            system_mcp_timeout: None,
            source: None,
        })),
    );
    manifest.mcp_servers = Some(servers);

    let result = extract_mcp_servers(&manifest, Path::new("/tmp")).unwrap();
    assert_eq!(result.len(), 1);
    assert!(result.contains_key("s1"));
}

#[test]
fn test_extract_mcp_servers_none() {
    let manifest = make_manifest_with_commands(vec![]);
    let result = extract_mcp_servers(&manifest, Path::new("/tmp")).unwrap();
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

    let result = extract_mcp_servers(&manifest, &plugin_dir).unwrap();
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

    // 被引用的文件缺失是「未声明」，不是非法配置；非法的 *内容* 才失败。
    let result = extract_mcp_servers(&manifest, dir.path()).unwrap();
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
    let result = extract_mcp_servers(&manifest, dir.path()).unwrap();
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
    let result = extract_mcp_servers(&manifest, dir.path()).unwrap();
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
            subscriptions: None,
            system_mcp: None,
            system_mcp_tools: None,
            system_mcp_timeout: None,
            source: None,
        })),
    );
    manifest.mcp_servers = Some(servers);

    let result = extract_mcp_servers(&manifest, dir.path()).unwrap();
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

    let result = super::load_mcp_json_file(&mcp_json_path).unwrap().unwrap();
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

    let result = super::load_mcp_json_file(&mcp_json_path).unwrap().unwrap();
    assert_eq!(result.len(), 1);
    assert!(result.contains_key("srv"));
}

#[test]
fn test_load_mcp_json_file_nonexistent() {
    let result = super::load_mcp_json_file(Path::new("/nonexistent/mcp.json"));
    assert!(matches!(result, Ok(None)), "缺失文件是未声明，不是错误");
}

#[test]
fn test_load_mcp_json_file_invalid_json() {
    let dir = tempdir().unwrap();
    let mcp_json_path = dir.path().join("bad.mcp.json");
    std::fs::write(&mcp_json_path, b"not json").unwrap();
    let error =
        super::load_mcp_json_file(&mcp_json_path).expect_err("非法 JSON 不得被当作可跳过条目");
    let LoaderError::McpConfigInvalid { path, message } = error else {
        panic!("非法 MCP 文件应返回 McpConfigInvalid");
    };
    assert_eq!(path, mcp_json_path);
    assert!(message.contains("JSON 语法错误"), "message: {message}");
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
            subscriptions: None,
            system_mcp: None,
            system_mcp_tools: None,
            system_mcp_timeout: None,
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
            subscriptions: None,
            system_mcp: None,
            system_mcp_tools: None,
            system_mcp_timeout: None,
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
        .join("some-plugin")
        .join("1.0.0");
    std::fs::create_dir_all(&plugin_install_path).unwrap();

    // marketplaces_cache_dir() 读取的是 ~/.claude/plugins/marketplaces，
    // 测试中无法覆盖。直接测试 try_generate_synthetic_manifest_fallback 函数，
    // 验证当 marketplace 缓存不在默认路径时返回 false。
    let result =
        try_generate_synthetic_manifest_fallback(&plugin_install_path, "some-plugin", "test-mkt");

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

#[path = "loader_catalog_test.rs"]
mod catalog;

#[test]
fn retired_channels_do_not_become_command_frontmatter() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("command.md");
    std::fs::write(
        &path,
        "---\ndescription: normal command\nchannels:\n  - name: legacy\n    mcpServer: legacy-server\n---\nNormal command body\n",
    )
    .unwrap();

    let (frontmatter, body) = parse_command_md(&path).unwrap();
    assert_eq!(frontmatter.description.as_deref(), Some("normal command"));
    assert!(frontmatter.shell.is_none());
    assert!(frontmatter.args.is_none());
    assert!(frontmatter.model.is_none());
    assert_eq!(body.trim(), "Normal command body");
}
