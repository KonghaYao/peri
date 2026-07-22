use super::*;

#[test]
fn test_plugin_manifest_minimal() {
    let json = r#"{"name":"test-plugin","version":"1.0.0"}"#;
    let manifest: PluginManifest = serde_json::from_str(json).unwrap();
    assert_eq!(manifest.name, "test-plugin");
    assert_eq!(manifest.version, "1.0.0");
    assert!(manifest.description.is_empty());
    assert!(manifest.author.is_none());
    assert!(manifest.commands.is_none());
    assert!(manifest.agents.is_none());
    assert!(manifest.skills.is_none());
    assert!(manifest.hooks.is_none());
    assert!(manifest.mcp_servers.is_none());
    assert!(manifest.lsp_servers.is_none());
    assert!(manifest.output_styles.is_none());
    assert!(manifest.channels.is_none());
    assert!(manifest.options.is_none());
    assert!(manifest.settings.is_none());
}

#[test]
fn test_plugin_manifest_full() {
    let json = r#"{
            "name": "full-plugin",
            "version": "2.0.0",
            "description": "A full plugin",
            "author": {"name": "Test Author", "url": "https://example.com"},
            "commands": [{"path": "/commands/test.md", "name": "test", "description": "Test command"}],
            "agents": [{"path": "/agents/test.md", "name": "test-agent"}],
            "skills": ["/skills/test-skill"],
            "hooks": {},
            "mcpServers": {
                "test-server": {
                    "command": "node",
                    "args": ["server.js"]
                }
            },
            "lspServers": [{"name": "test-lsp", "command": "test-lsp-binary", "args": []}],
            "outputStyles": ["compact"],
            "channels": [{"name": "test-channel", "mcpServer": "test-server"}],
            "options": [{"name": "opt1", "description": "Option 1", "type": "string", "default": "val1"}],
            "settings": {"key": "value"}
        }"#;
    let manifest: PluginManifest = serde_json::from_str(json).unwrap();
    assert_eq!(manifest.name, "full-plugin");
    assert_eq!(manifest.version, "2.0.0");
    assert_eq!(manifest.description, "A full plugin");
    assert_eq!(manifest.author.as_ref().unwrap().name, "Test Author");
    assert_eq!(manifest.commands.as_ref().unwrap().len(), 1);
    assert_eq!(manifest.agents.as_ref().unwrap().len(), 1);
    assert_eq!(manifest.skills.as_ref().unwrap().len(), 1);
    assert!(manifest.mcp_servers.is_some());
    let mcp = manifest.mcp_servers.as_ref().unwrap();
    match mcp.get("test-server").unwrap() {
        McpServerEntry::Config(cfg) => {
            assert_eq!(cfg.command.as_deref(), Some("node"));
        }
        McpServerEntry::FilePath(_) => panic!("expected Config variant"),
    }
    assert_eq!(manifest.lsp_servers.as_ref().unwrap().len(), 1);
    assert_eq!(manifest.channels.as_ref().unwrap().len(), 1);
    assert_eq!(manifest.options.as_ref().unwrap().len(), 1);
}

#[test]
fn test_plugin_manifest_mcp_servers_rename() {
    let json =
        r#"{"name":"p","version":"1.0.0","mcpServers":{"srv":{"command":"cmd","args":["-a"]}}}"#;
    let manifest: PluginManifest = serde_json::from_str(json).unwrap();
    let servers = manifest.mcp_servers.unwrap();
    assert!(servers.contains_key("srv"));
    match &servers["srv"] {
        McpServerEntry::Config(cfg) => {
            assert_eq!(cfg.command.as_deref(), Some("cmd"));
        }
        McpServerEntry::FilePath(_) => panic!("expected Config variant"),
    }
}

#[test]
fn test_mcp_server_entry_file_path() {
    let json = r#"{"name":"p","version":"1.0.0","mcpServers":{"srv":"./path/to/.mcp.json"}}"#;
    let manifest: PluginManifest = serde_json::from_str(json).unwrap();
    let servers = manifest.mcp_servers.unwrap();
    match servers.get("srv").unwrap() {
        McpServerEntry::FilePath(path) => assert_eq!(path, "./path/to/.mcp.json"),
        McpServerEntry::Config(_) => panic!("expected FilePath variant"),
    }
}

#[test]
fn test_mcp_server_entry_inline_config() {
    let json = r#"{"name":"p","version":"1.0.0","mcpServers":{"srv":{"command":"node","args":["server.js"]}}}"#;
    let manifest: PluginManifest = serde_json::from_str(json).unwrap();
    let servers = manifest.mcp_servers.unwrap();
    match servers.get("srv").unwrap() {
        McpServerEntry::Config(cfg) => {
            assert_eq!(cfg.command.as_deref(), Some("node"));
        }
        McpServerEntry::FilePath(_) => panic!("expected Config variant"),
    }
}

#[test]
fn test_marketplace_source_github() {
    let json = r#"{"source":"github","repo":"anthropics/claude-plugins-official"}"#;
    let source: MarketplaceSource = serde_json::from_str(json).unwrap();
    match source {
        MarketplaceSource::GitHub { repo } => {
            assert_eq!(repo, "anthropics/claude-plugins-official")
        }
        _ => panic!("expected GitHub variant"),
    }
}

#[test]
fn test_marketplace_source_url() {
    let json = r#"{"source":"url","url":"https://example.com/marketplace.json"}"#;
    let source: MarketplaceSource = serde_json::from_str(json).unwrap();
    match source {
        MarketplaceSource::Url { url } => {
            assert_eq!(url, "https://example.com/marketplace.json")
        }
        _ => panic!("expected Url variant"),
    }
}

#[test]
fn test_installed_plugins_default() {
    let default = InstalledPlugins::default();
    assert_eq!(default.version, 2);
    assert!(default.plugins.is_empty());
}

#[test]
fn test_installed_plugins_claude_code_object_format() {
    let json = r#"{
            "version": 2,
            "plugins": {
                "typescript-lsp@claude-plugins-official": [
                    {
                        "scope": "user",
                        "installPath": "/Users/test/.claude/plugins/cache/claude-plugins-official/typescript-lsp/1.0.0",
                        "version": "1.0.0",
                        "installedAt": "2026-04-03T11:48:01.555Z",
                        "gitCommitSha": "abc123"
                    }
                ],
                "frontend-design@claude-plugins-official": [
                    {
                        "scope": "user",
                        "installPath": "/Users/test/.claude/plugins/cache/claude-plugins-official/frontend-design/7ed523140f50",
                        "version": "7ed523140f50"
                    }
                ]
            }
        }"#;
    let installed: InstalledPlugins = serde_json::from_str(json).unwrap();
    assert_eq!(installed.version, 2);
    assert_eq!(installed.plugins.len(), 2);

    let mut plugins = installed.plugins.clone();
    plugins.sort_by(|a, b| a.name.cmp(&b.name));

    assert_eq!(plugins[0].id, "frontend-design@claude-plugins-official");
    assert_eq!(plugins[0].name, "frontend-design");
    assert_eq!(plugins[0].version, "7ed523140f50");

    assert_eq!(plugins[1].id, "typescript-lsp@claude-plugins-official");
    assert_eq!(plugins[1].name, "typescript-lsp");
    assert_eq!(plugins[1].marketplace, "claude-plugins-official");
    assert_eq!(plugins[1].version, "1.0.0");
    assert_eq!(plugins[1].scope, InstallScope::User);
    assert!(plugins[1].install_path.ends_with("typescript-lsp/1.0.0"));
}

#[test]
fn test_installed_plugins_internal_array_format() {
    let json = r#"{
            "version": 2,
            "plugins": [
                {
                    "id": "test@marketplace",
                    "name": "test",
                    "version": "1.0.0",
                    "marketplace": "marketplace",
                    "install_path": "/tmp/test",
                    "scope": "User"
                }
            ]
        }"#;
    let installed: InstalledPlugins = serde_json::from_str(json).unwrap();
    assert_eq!(installed.plugins.len(), 1);
    assert_eq!(installed.plugins[0].id, "test@marketplace");
}

#[test]
fn test_installed_plugins_id_without_at() {
    let json = r#"{
            "version": 2,
            "plugins": {
                "standalone-plugin": [
                    {
                        "scope": "project",
                        "installPath": "/tmp/standalone",
                        "version": "2.0.0"
                    }
                ]
            }
        }"#;
    let installed: InstalledPlugins = serde_json::from_str(json).unwrap();
    assert_eq!(installed.plugins.len(), 1);
    assert_eq!(installed.plugins[0].id, "standalone-plugin");
    assert_eq!(installed.plugins[0].name, "standalone-plugin");
    assert_eq!(installed.plugins[0].marketplace, "");
    assert_eq!(installed.plugins[0].scope, InstallScope::Project);
}

#[test]
fn test_install_scope_default() {
    assert_eq!(InstallScope::default(), InstallScope::User);
}

#[test]
fn test_known_marketplace_deserialize() {
    let json = r#"{
            "source": {"source":"github","repo":"test/repo"},
            "installLocation": "/tmp/test",
            "autoUpdate": true,
            "lastUpdated": "2025-01-01T00:00:00Z"
        }"#;
    let km: KnownMarketplace = serde_json::from_str(json).unwrap();
    match &km.source {
        MarketplaceSource::GitHub { repo } => assert_eq!(repo, "test/repo"),
        _ => panic!("expected GitHub variant"),
    }
    assert_eq!(km.install_location, "/tmp/test");
    assert!(km.auto_update);
    assert_eq!(km.last_updated, "2025-01-01T00:00:00Z");
}

#[test]
fn test_known_marketplace_without_auto_update() {
    let json = r#"{
            "source": {"source":"github","repo":"test/repo"},
            "installLocation": "/tmp/test",
            "lastUpdated": "2025-01-01T00:00:00Z"
        }"#;
    let km: KnownMarketplace = serde_json::from_str(json).unwrap();
    assert!(!km.auto_update); // default value
    assert_eq!(km.install_location, "/tmp/test");
    assert_eq!(km.last_updated, "2025-01-01T00:00:00Z");
}

#[test]
fn test_plugin_manifest_serialization_roundtrip() {
    let original = PluginManifest {
        name: "roundtrip".into(),
        version: "1.2.3".into(),
        description: "test".into(),
        author: Some(PluginAuthor {
            name: "Author".into(),
            url: Some("https://example.com".into()),
        }),
        commands: Some(vec![PluginCommandEntry::Full(PluginCommand {
            path: "/cmd.md".into(),
            name: Some("cmd".into()),
            description: Some("desc".into()),
        })]),
        agents: Some(vec![PluginAgent {
            path: "/agent.md".into(),
            name: "agent".into(),
        }]),
        skills: Some(vec!["/skill".into()]),
        hooks: None,
        mcp_servers: None,
        lsp_servers: None,
        output_styles: None,
        channels: None,
        options: None,
        settings: None,
        extra: serde_json::json!({}),
    };
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: PluginManifest = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.name, original.name);
    assert_eq!(deserialized.version, original.version);
    assert_eq!(deserialized.description, original.description);
    assert_eq!(
        deserialized.author.as_ref().unwrap().name,
        original.author.as_ref().unwrap().name
    );
    assert_eq!(deserialized.commands.as_ref().unwrap().len(), 1);
    assert_eq!(deserialized.agents.as_ref().unwrap().len(), 1);
    assert_eq!(deserialized.skills.as_ref().unwrap().len(), 1);
}

#[test]
fn test_plugin_manifest_commands_string_array() {
    // Claude Code 支持 "commands": ["path/to/commands/"] 这种字符串目录路径格式
    let json = r#"{"name":"ecc","version":"1.0.0","commands":["./commands/", {"path": "./extra/my-cmd.md", "name": "my-cmd"}]}"#;
    let manifest: PluginManifest = serde_json::from_str(json).unwrap();
    let cmds = manifest.commands.unwrap();
    assert_eq!(cmds.len(), 2);
    match &cmds[0] {
        PluginCommandEntry::Path(path) => assert_eq!(path, "./commands/"),
        _ => panic!("expected Path variant"),
    }
    match &cmds[1] {
        PluginCommandEntry::Full(cmd) => {
            assert_eq!(cmd.path, "./extra/my-cmd.md");
            assert_eq!(cmd.name.as_deref(), Some("my-cmd"));
        }
        _ => panic!("expected Full variant"),
    }
}

#[test]
fn test_skills_field_string() {
    let json = r#"{"name":"p","skills":"./skills/"}"#;
    let manifest: PluginManifest = serde_json::from_str(json).unwrap();
    assert_eq!(
        manifest.skills.as_ref().unwrap(),
        &vec!["./skills/".to_string()]
    );
}

#[test]
fn test_skills_field_array() {
    let json = r#"{"name":"p","skills":["./a/","./b/"]}"#;
    let manifest: PluginManifest = serde_json::from_str(json).unwrap();
    assert_eq!(
        manifest.skills.as_ref().unwrap(),
        &vec!["./a/".to_string(), "./b/".to_string()]
    );
}

#[test]
fn test_skills_field_null() {
    let json = r#"{"name":"p","skills":null}"#;
    let manifest: PluginManifest = serde_json::from_str(json).unwrap();
    assert!(manifest.skills.is_none());
}

#[test]
fn test_skills_field_absent() {
    let json = r#"{"name":"p"}"#;
    let manifest: PluginManifest = serde_json::from_str(json).unwrap();
    assert!(manifest.skills.is_none());
}

// === PluginOrigin 测试 ===

#[test]
fn test_plugin_origin_default() {
    assert_eq!(PluginOrigin::default(), PluginOrigin::PeriInstalled);
}

#[test]
fn test_plugin_origin_is_external() {
    // Peri 安装的不是外部
    assert!(!PluginOrigin::PeriInstalled.is_external());
    // Claude Code 相关的是外部
    assert!(PluginOrigin::ClaudeCodeInstalled.is_external());
    assert!(PluginOrigin::UserClaude.is_external());
    assert!(PluginOrigin::ProjectClaude.is_external());
}

#[test]
fn test_plugin_origin_serde_roundtrip() {
    // PeriInstalled (default) — 不写入 serde 输出
    let origin = PluginOrigin::PeriInstalled;
    let json = serde_json::to_string(&origin).unwrap();
    // default 序列化后可以反序列化回来
    let back: PluginOrigin = serde_json::from_str(&json).unwrap();
    assert_eq!(back, PluginOrigin::PeriInstalled);

    // ClaudeCodeInstalled
    let json = r#""claude-installed""#;
    let origin: PluginOrigin = serde_json::from_str(json).unwrap();
    assert_eq!(origin, PluginOrigin::ClaudeCodeInstalled);
    let back = serde_json::to_string(&origin).unwrap();
    assert_eq!(back, r#""claude-installed""#);

    // UserClaude
    let json = r#""claude-user""#;
    let origin: PluginOrigin = serde_json::from_str(json).unwrap();
    assert_eq!(origin, PluginOrigin::UserClaude);

    // ProjectClaude
    let json = r#""claude-project""#;
    let origin: PluginOrigin = serde_json::from_str(json).unwrap();
    assert_eq!(origin, PluginOrigin::ProjectClaude);
}

#[test]
fn test_installed_plugin_deserialize_missing_origin() {
    // 旧 JSON 缺 origin 字段 → default PeriInstalled
    let json = r#"{
        "id": "test@mkt",
        "name": "test",
        "version": "1.0.0",
        "marketplace": "mkt",
        "install_path": "/tmp/test",
        "scope": "User"
    }"#;
    let plugin: InstalledPlugin = serde_json::from_str(json).unwrap();
    assert_eq!(plugin.origin, PluginOrigin::PeriInstalled);
}

#[test]
fn test_claude_code_format_deserialize_sets_origin() {
    // Claude Code 对象格式 JSON 解析出来的 origin == ClaudeCodeInstalled
    let json = r#"{"version":2,"plugins":{"p@mkt":[{"scope":"user","installPath":"/tmp/p","version":"1.0.0"}]}}"#;
    let installed: InstalledPlugins = serde_json::from_str(json).unwrap();
    assert_eq!(installed.plugins.len(), 1);
    assert_eq!(
        installed.plugins[0].origin,
        PluginOrigin::ClaudeCodeInstalled
    );
}

// === PluginManifest extra 字段测试 ===

#[test]
fn test_plugin_manifest_extra_fields_preserved_on_roundtrip() {
    // 含未知字段的 JSON
    let json = r#"{
        "name": "p",
        "version": "1.0.0",
        "customField": "value",
        "anotherNewField": 42
    }"#;
    let manifest: PluginManifest = serde_json::from_str(json).unwrap();
    // 未知字段收集到 extra
    assert_eq!(manifest.extra["customField"], "value");
    assert_eq!(manifest.extra["anotherNewField"], 42);
    // roundtrip 不丢字段
    let out = serde_json::to_string(&manifest).unwrap();
    let manifest2: PluginManifest = serde_json::from_str(&out).unwrap();
    assert_eq!(manifest2.extra["customField"], "value");
    assert_eq!(manifest2.extra["anotherNewField"], 42);
}

#[test]
fn test_plugin_manifest_unknown_field_stored_in_extra() {
    let json = r#"{"name":"p","version":"1.0.0","unknownSetting":"yes"}"#;
    let manifest: PluginManifest = serde_json::from_str(json).unwrap();
    // extra 非空，含未知字段
    assert!(manifest.extra.is_object());
    assert_eq!(manifest.extra["unknownSetting"].as_str().unwrap(), "yes");
}

#[test]
fn test_plugin_manifest_extra_default_empty_object() {
    // 最小 JSON（仅已知字段）
    let json = r#"{"name":"p","version":"1.0"}"#;
    let manifest: PluginManifest = serde_json::from_str(json).unwrap();
    // extra == {}（空 Object）
    assert_eq!(manifest.extra, serde_json::json!({}));
}

// === PluginManifest extra 字段测试结束 ===
