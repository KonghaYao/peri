use super::*;

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
    let error = load_merged_config_full_with_paths(
        &cwd,
        &claude_home,
        &global_path,
        &BuiltinInjectionPolicy::none(),
    )
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
    let error = load_merged_config_full_with_paths(
        &cwd,
        &claude_home,
        &missing_global_path(dir.path()),
        &BuiltinInjectionPolicy::none(),
    )
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
    let error = load_merged_config_full_with_paths(
        &cwd,
        &claude_home,
        &global_path,
        &BuiltinInjectionPolicy::none(),
    )
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

    let error = load_merged_config_full_with_paths(
        &cwd,
        &claude_home,
        &missing_global_path(dir.path()),
        &BuiltinInjectionPolicy::none(),
    )
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
            ..Default::default()
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

    let (merged, _) = load_merged_config_full_with_paths(
        &cwd,
        &claude_home,
        &global_path,
        &BuiltinInjectionPolicy::all(),
    )
    .unwrap();
    let sys = merged.mcp_servers.get("sys").expect("应有 sys");
    assert_eq!(sys.system_mcp, Some(true));
    assert_eq!(sys.system_mcp_tools, Some(Vec::new()));
    assert_ne!(sys.system_mcp_tools, None, "Some([]) 与 None 必须可区分");

    // 展开不得把 [] 变成 None，也不得凭空产生工具名。
    let expanded = expand_server_config(sys);
    assert_eq!(expanded.system_mcp_tools, Some(Vec::new()));

    // 写回（切换 disabled）必须被拒绝：`disabled + system_mcp` 是非法组合，
    // 写回路径不得静默去掉其中一个开关，也不得写入一个下一个会话加载不了的配置。
    let before = std::fs::read_to_string(&project_path).unwrap();
    let error = set_server_disabled_with_paths(&cwd, &global_path, "sys", true)
        .expect_err("对 system server 置 disabled 必须在写盘前拒绝");
    assert!(
        matches!(
            error,
            McpConfigError::InvalidServer {
                source:
                    peri_acp_types::plugin::McpServerConfigValidationError::DisabledWithSystemMcp,
                ..
            }
        ),
        "实际错误: {error}"
    );
    let after = std::fs::read_to_string(&project_path).unwrap();
    assert_eq!(before, after, "被拒绝的写入不得改动既有文件");
    // 文件仍可加载，显式空数组与 system 声明逐字保留。
    let reloaded = load_from_path(&project_path).unwrap();
    assert_eq!(
        reloaded.mcp_servers["sys"].system_mcp_tools,
        Some(Vec::new())
    );
    assert_eq!(reloaded.mcp_servers["sys"].system_mcp, Some(true));
    assert_eq!(reloaded.mcp_servers["sys"].disabled, None);
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

    let (merged, _) = load_merged_config_full_with_paths(
        &cwd,
        &claude_home,
        &missing_global_path(dir.path()),
        &BuiltinInjectionPolicy::all(),
    )
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

    let (merged, _) = load_merged_config_full_with_paths(
        &cwd,
        &claude_home,
        &global_path,
        &BuiltinInjectionPolicy::all(),
    )
    .unwrap();
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

    let (merged, _) = load_merged_config_full_with_paths(
        &cwd,
        &claude_home,
        &global_path,
        &BuiltinInjectionPolicy::all(),
    )
    .unwrap();
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
    // 同一份 `all()` 策略下 builtin 默认层已注入：上面的去重结论是在**注入发生**
    // 的前提下得到的（注入点在 step 4 去重之后，builtin 条目不进 manual_hashes）。
    for instance in peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES {
        assert!(
            merged.mcp_servers.contains_key(instance.name),
            "默认层应注入 builtin 实例 {}，实际 keys: {:?}",
            instance.name,
            merged.mcp_servers.keys().collect::<Vec<_>>()
        );
    }

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

    // 切换 disabled 必须被拒绝（M7）：`disabled + system_mcp` 非法，写回路径
    // 既不静默去掉开关，也不写出后续加载不了的配置；既有数组逐字保留。
    let before = std::fs::read_to_string(&global_path).unwrap();
    assert!(set_server_disabled_with_paths(&cwd, &global_path, "sys", true).is_err());
    assert_eq!(
        before,
        std::fs::read_to_string(&global_path).unwrap(),
        "被拒绝的写入不得改动既有文件"
    );
    let reloaded = load_global_config(&global_path).unwrap();
    assert_eq!(reloaded.mcp_servers["sys"].disabled, None);
    assert_eq!(
        reloaded.mcp_servers["sys"].system_mcp_tools.as_deref(),
        Some(["z".to_string(), "a".to_string(), "z".to_string()].as_slice())
    );
}
