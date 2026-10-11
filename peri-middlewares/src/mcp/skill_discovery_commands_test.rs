use super::*;

// ─── mcp_route_entries（决策 1 命令面转换）─────────────────────────────────

/// 纯函数转换：SkillMetadata → mcp 域 RouteEntry（fullname `{server}:{skill}`
/// 小写、首段取 server 名末段、kind=McpSkill、provenance=Mcp+Discovered、
/// handler=McpSkillReleaser 放行跳板）。
#[test]
fn mcp_route_entries_converts_skills() {
    let registry = Arc::new(McpSkillRegistry::new());
    let skills = vec![
        SkillMetadata {
            name: "mcp__demo__AlphaSkill".to_string(),
            aliases: Vec::new(),
            description: "Alpha skill".to_string(),
            path: PathBuf::new(),
            source: SkillSource::Mcp,
            plugin_name: None,
            origin: Some(SkillOrigin::Mcp {
                server: "demo".to_string(),
                uri: "skill://demo/alpha/SKILL.md".to_string(),
            }),
            content: None,
            resources: vec![],
            frontmatter: None,
        },
        // 同名消歧后形态：路径段作为 skill 名
        SkillMetadata {
            name: "mcp__demo__alpha/sub".to_string(),
            aliases: Vec::new(),
            description: "Sub skill".to_string(),
            path: PathBuf::new(),
            source: SkillSource::Mcp,
            plugin_name: None,
            origin: None,
            content: None,
            resources: vec![],
            frontmatter: None,
        },
    ];
    let entries = mcp_route_entries(&registry, "demo", &skills);
    assert_eq!(entries.len(), 2, "全部转换");
    let e = &entries[0];
    assert_eq!(e.fullname, "demo:alphaskill", "fullname 小写归一");
    assert!(e.aliases.is_empty());
    assert_eq!(e.description, "Alpha skill");
    assert_eq!(e.kind, CommandEntryKind::McpSkill);
    assert_eq!(e.category, None);
    assert!(e.args_schema.is_none());
    assert_eq!(
        e.provenance.source,
        CommandSource::Mcp {
            server: "demo".to_string()
        }
    );
    assert_eq!(e.provenance.lifecycle, CommandLifecycle::Discovered);
    assert_eq!(
        entries[1].fullname, "demo:alpha/sub",
        "路径段形态保留（词法允许 /）"
    );
}

/// plugin 提供的 server key 形如 `plugin:{plugin}:{server}`（loader.rs:541），
/// 含冒号会突破词法 2 段上限 → 词法首段取末段；纯 server 名不变。
#[test]
fn mcp_route_entries_plugin_server_takes_last_segment() {
    let registry = Arc::new(McpSkillRegistry::new());
    let skills = vec![SkillMetadata {
        name: "mcp__plugin:p1:demosrv__beta".to_string(),
        aliases: Vec::new(),
        description: "Beta skill".to_string(),
        path: PathBuf::new(),
        source: SkillSource::Mcp,
        plugin_name: None,
        origin: None,
        content: None,
        resources: vec![],
        frontmatter: None,
    }];
    let entries = mcp_route_entries(&registry, "plugin:p1:demosrv", &skills);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].fullname, "demosrv:beta");
    assert_eq!(
        entries[0].provenance.source,
        CommandSource::Mcp {
            server: "demosrv".to_string()
        }
    );
}

/// P1-1：`mcp_source_key` 与 `mcp_route_entries` 的 fullname 首段派生
/// 同构（单一派生点 `mcp_namespace`）——断连注销前缀 `{末段}:` 才能
/// 命中条目 fullname（决策 1：键 = server 名末段，无 `mcp:` 域前缀）。
#[test]
fn mcp_source_key_matches_route_entry_namespace() {
    // 纯 server 名：键 = demo（不变）
    assert_eq!(mcp_source_key("demo"), "demo");
    // plugin server key：键 = demosrv（末段，非原名）
    assert_eq!(mcp_source_key("plugin:p1:demosrv"), "demosrv");
    // 键与条目 fullname 前缀一致（注销前缀 `{键}:` 命中 fullname）
    let registry = Arc::new(McpSkillRegistry::new());
    let entries = mcp_route_entries(
        &registry,
        "plugin:p1:demosrv",
        &[SkillMetadata {
            name: "mcp__plugin:p1:demosrv__beta".to_string(),
            aliases: Vec::new(),
            description: "Beta skill".to_string(),
            ..SkillMetadata::default()
        }],
    );
    let key = mcp_source_key("plugin:p1:demosrv");
    for e in &entries {
        assert!(
            e.fullname.starts_with(&format!("{key}:")),
            "条目 fullname {} 必须以注销前缀 {key}: 开头",
            e.fullname
        );
    }
    // 大小写归一同源：大写 server 名同样派生小写键（与词法键一致）
    assert_eq!(mcp_source_key("Plugin:P1:DemoSrv"), "demosrv");
}

/// 审查 B1 防线 1：保留词法域 server 不进命令面——来源键派生标记 +
/// `finish_command_source` 跳过注册（元数据面不受影响，工具面仍可用）。
#[test]
fn mcp_source_key_reserved_domain_skipped() {
    for reserved in ["core", "ui", "plugin", "user", "mcp"] {
        assert!(
            mcp_namespace_reserved(reserved),
            "{reserved} 应判定为保留域"
        );
        // 纯 server 名末段命中保留域也判定（plugin:pa:core → core）。
        assert!(mcp_namespace_reserved(&format!("plugin:pa:{reserved}")));
    }
    assert!(!mcp_namespace_reserved("demo"));
    assert!(!mcp_namespace_reserved("plugin:pa:demosrv"));

    // finish_command_source：保留域 server 不产生任何命令面条目。
    let command_registry = Arc::new(CommandRegistry::new());
    let meta_registry = Arc::new(McpSkillRegistry::new());
    let skills = vec![SkillMetadata {
        name: "mcp__core__hello".to_string(),
        aliases: Vec::new(),
        description: "Core-named skill".to_string(),
        ..SkillMetadata::default()
    }];
    let token_core: HandleToken = Arc::new(101u32);
    command_registry.mark_source_started("core", token_core.clone());
    finish_command_source(
        &Some(command_registry.clone()),
        &meta_registry,
        "core",
        token_core,
        &skills,
    );
    assert!(
        command_registry.resolve("core:hello").is_none(),
        "保留域 server 的 skill 不得注册为命令"
    );
    // 对照：非保留域正常注册。
    let skills2 = vec![SkillMetadata {
        name: "mcp__demo__hello".to_string(),
        aliases: Vec::new(),
        description: "Demo skill".to_string(),
        ..SkillMetadata::default()
    }];
    let token_demo: HandleToken = Arc::new(102u32);
    command_registry.mark_source_started("demo", token_demo.clone());
    finish_command_source(
        &Some(command_registry.clone()),
        &meta_registry,
        "demo",
        token_demo,
        &skills2,
    );
    assert!(
        command_registry.resolve("demo:hello").is_some(),
        "非保留域 server 正常注册"
    );
}

#[test]
fn external_workspace_name_keeps_prefixed_skill_command() {
    let command_registry = Arc::new(CommandRegistry::new());
    let registry = Arc::new(McpSkillRegistry::new());
    let token: HandleToken = Arc::new(103u32);
    command_registry.mark_source_started("workspace", token.clone());
    finish_command_source(
        &Some(Arc::clone(&command_registry)),
        &registry,
        "workspace",
        token,
        &[SkillMetadata {
            name: "mcp__workspace__hello".to_string(),
            description: "External hello".to_string(),
            ..SkillMetadata::default()
        }],
    );
    assert!(command_registry.resolve("/workspace:hello").is_some());
    assert!(command_registry.resolve("/hello").is_none());
}

/// 缺 `mcp__{server}__` 前缀的 skill 名 → 跳过（warn），不产出条目。
#[test]
fn mcp_route_entries_skips_unprefixed_name() {
    let registry = Arc::new(McpSkillRegistry::new());
    let skills = vec![SkillMetadata {
        name: "other__name".to_string(),
        aliases: Vec::new(),
        description: "No prefix".to_string(),
        path: PathBuf::new(),
        source: SkillSource::Mcp,
        plugin_name: None,
        origin: None,
        content: None,
        resources: vec![],
        frontmatter: None,
    }];
    let entries = mcp_route_entries(&registry, "demo", &skills);
    assert!(entries.is_empty(), "缺前缀条目跳过");
}

/// run_discovery 命令面双写（决策 1）：规范模式发现完成后，命令面
/// 注册表收到 `srv:alpha` 条目（kind=McpSkill、provenance=Mcp+Discovered）。
#[tokio::test]
async fn run_discovery_writes_command_registry() {
    let ok_text = "---\nname: alpha\ndescription: Alpha skill\n---\n\n# Alpha\n";
    let (client_io, server_io) = tokio::io::duplex(8192);
    tokio::spawn(spec_skill_server(
        server_io,
        vec![SpecSkill {
            uri: "skill://alpha/SKILL.md",
            name: "alpha",
            description: "Alpha skill",
            text: ok_text,
            digest_override: None,
            get_text: None,
            get_error: false,
            get_wrong_uri: false,
        }],
        None,
        None,
        false,
    ));
    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let handle = make_spec_handle(&running);
    let reg = Arc::new(McpSkillRegistry::new());
    let cmd_reg = Arc::new(CommandRegistry::new());
    let token: HandleToken = Arc::new(9u32);
    reg.mark_discovery_started("srv", token.clone());
    cmd_reg.mark_source_started("srv", token.clone());
    let cancel = AgentCancellationToken::new();
    run_discovery(
        reg.clone(),
        Some(cmd_reg.clone()),
        handle,
        token.clone(),
        cancel,
    )
    .await;

    let entries = cmd_reg.snapshot();
    assert_eq!(entries.len(), 1, "命令面注册 1 条");
    let e = &entries[0];
    assert_eq!(e.fullname, "srv:alpha");
    assert_eq!(e.kind, CommandEntryKind::McpSkill);
    assert_eq!(
        e.provenance.source,
        CommandSource::Mcp {
            server: "srv".to_string()
        }
    );
    assert_eq!(e.provenance.lifecycle, CommandLifecycle::Discovered);
    assert_eq!(e.description, "Alpha skill");
}

/// cancel 提前退出 → 命令面 Started 回退（与元数据面对齐，下轮可重试）。
#[tokio::test]
async fn run_discovery_cancel_clears_command_source_started() {
    let reg = Arc::new(McpSkillRegistry::new());
    let cmd_reg = Arc::new(CommandRegistry::new());
    let token: HandleToken = Arc::new(10u32);
    let (client_io, server_io) = tokio::io::duplex(8192);
    let first_done = Arc::new(tokio::sync::Notify::new());
    tokio::spawn(raw_skill_server(
        server_io,
        vec![RespondRule {
            segment: "alpha",
            delay: std::time::Duration::from_secs(2),
            error: false,
        }],
        Some(Arc::clone(&first_done)),
        None,
    ));
    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let handle = make_discovery_handle(
        &running,
        vec![
            resource("skill://srv/zebra/SKILL.md"),
            resource("skill://srv/alpha/SKILL.md"),
        ],
    );
    reg.mark_discovery_started("srv", token.clone());
    cmd_reg.mark_source_started("srv", token.clone());
    let cancel = AgentCancellationToken::new();
    let discovery = tokio::spawn(run_discovery(
        reg.clone(),
        Some(cmd_reg.clone()),
        handle,
        token.clone(),
        cancel.clone(),
    ));

    // zebra 立即响应、alpha 延迟 2s → 首条响应后触发 cancel
    first_done.notified().await;
    cancel.cancel();
    discovery.await.unwrap();
    assert!(
        reg.discovery_state("srv").is_none(),
        "cancel 后元数据面 Started 已回退"
    );
    // 命令面 Started 回退后，来源无状态 → 下轮 before_agent 重新 to_discover；
    // 注册表无公开 sources 查询，用无条目 + 无 on_change 侧证（cancel 不触发）。
    assert!(cmd_reg.snapshot().is_empty(), "cancel 路径不注册条目");
}
