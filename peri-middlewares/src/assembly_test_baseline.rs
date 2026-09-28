use super::*;

// ── 契约用例 ─────────────────────────────────────────────────────────────────

/// 蓝本槽位顺序 = 行为契约（7 组 25 槽，禁止重排；波 4 演进 C2 新增
/// DefaultSystemPrompt / Lang 于第一组首位——渲染排序不依赖链序，契约 2）。
///
/// v4-part-2 W3（A7/A14）：`Web` / `Artifact` 两槽位随 `WebMiddleware` /
/// `ArtifactMiddleware` 提供面一并删除（能力改由 builtin MCP 实例提供），
/// 其余槽位**相对顺序不变**——本断言即「过滤掉被删两项后与上一批次逐项相等」。
///
/// v4-part-3 C-04：`Cron` 槽位同上删除（能力改由 `cron` builtin MCP 实例提供），
/// 其余槽位相对顺序不变。
///
/// v4-part-4 W3-C1：`Filesystem` / `Terminal` 两槽位同样删除（7 个文件/终端工具改由
/// builtin `workspace` 实例提供），其余槽位相对顺序不变。
#[test]
fn blueprint_sequence_is_canonical() {
    let slots = production_blueprint();
    let names: Vec<&str> = slots.iter().map(|s| slot_name(s)).collect();
    assert_eq!(
        names,
        vec![
            // 第一组：上下文注入器
            "DefaultSystemPrompt",
            "Lang",
            "AgentsMd",
            "AgentDefine",
            "Plugin",
            "Skills",
            "SkillPreload",
            "AtMention",
            "Image",
            // 第二组：工作区观察类注入器（W3-C1 后不含文件/终端工具提供器）
            "GitAttribution",
            "GitWatch",
            // 第三组：Todo
            "Todo",
            // 第四组：Hook 哨兵
            "Hook",
            // 第五组：Permission + AskUser + SubAgent（2026-08-15 职责拆分）
            "Permission",
            "AskUser",
            "SubAgent",
            // 第六组：MCP / Workflow / PTC / ToolSearch
            "Mcp",
            "Workflow",
            "Ptc",
            "ToolSearch",
            // 第七组：LSP / Goal（Goal 在链最后）
            "Lsp",
            "Goal",
        ]
    );
}

fn slot_name(slot: &ChainSlot) -> &'static str {
    match slot {
        ChainSlot::DefaultSystemPrompt => "DefaultSystemPrompt",
        ChainSlot::Lang => "Lang",
        ChainSlot::AgentsMd => "AgentsMd",
        ChainSlot::AgentDefine => "AgentDefine",
        ChainSlot::Plugin => "Plugin",
        ChainSlot::Skills => "Skills",
        ChainSlot::SkillPreload => "SkillPreload",
        ChainSlot::AtMention => "AtMention",
        ChainSlot::Image => "Image",
        ChainSlot::GitAttribution => "GitAttribution",
        ChainSlot::GitWatch => "GitWatch",
        ChainSlot::Todo => "Todo",
        ChainSlot::Hook => "Hook",
        ChainSlot::Permission => "Permission",
        ChainSlot::AskUser => "AskUser",
        ChainSlot::SubAgent => "SubAgent",
        ChainSlot::Mcp => "Mcp",
        ChainSlot::Workflow => "Workflow",
        ChainSlot::Ptc => "Ptc",
        ChainSlot::ToolSearch => "ToolSearch",
        ChainSlot::Lsp => "Lsp",
        ChainSlot::Goal => "Goal",
    }
}

/// 默认配置（全条件关闭）下的完整链序列，与迁移前 builder 完全一致。
#[test]
fn default_config_produces_canonical_chain() {
    let ctx = base_context();
    assert_eq!(
        assemble_names(&ctx),
        vec![
            "DefaultSystemPromptMiddleware",
            "LangMiddleware",
            "AgentsMdMiddleware",
            "AgentDefineMiddleware",
            "PluginMiddleware",
            "SkillsMiddleware",
            "SkillPreloadMiddleware",
            "AtMentionMiddleware",
            "ImageMiddleware",
            "GitAttributionMiddleware",
            "GitWatchMiddleware",
            "TodoMiddleware",
            "PermissionMiddleware",
            "HumanInTheLoopMiddleware",
            "SubAgentMiddleware",
            "PtcMiddleware",
            "ToolSearch",
        ]
    );
}

/// builtin `artifact` 实例默认可用（`artifact` direct）；
/// 单独关闭 `ArtifactMiddleware` 后仅移除该实例的工具，不影响 ToolSearch 元工具。
#[test]
fn builtin_artifact_instance_can_be_closed_independently() {
    let mut enabled = base_context();
    enabled.mcp_pool = Some(pool_with_builtin_instances());
    let enabled_tools = assemble_tool_names(&enabled);
    for expected in [
        "artifact",
        "WebSearch",
        "SearchExtraTools",
        "ExecuteExtraTool",
    ] {
        assert!(
            enabled_tools.iter().any(|name| name == expected),
            "未关闭时 {expected} 应可见: {enabled_tools:?}"
        );
    }

    let mut disabled = base_context();
    disabled.mcp_pool = Some(pool_with_builtin_instances());
    disabled
        .meta_harness_disabled
        .insert("ArtifactMiddleware".to_string());
    let disabled_tools = assemble_tool_names(&disabled);
    assert!(!disabled_tools.iter().any(|name| name == "artifact"));
    assert!(
        disabled_tools.iter().any(|name| name == "WebSearch"),
        "关闭 artifact 不影响 web 实例: {disabled_tools:?}"
    );
    for expected in ["SearchExtraTools", "ExecuteExtraTool"] {
        assert!(disabled_tools.iter().any(|name| name == expected));
    }
}

/// 权限模式不影响链组成与 Permission/AskUser 位置（四种模式一致）。
#[test]
fn permission_mode_keeps_chain_shape() {
    for mode in [
        PermissionMode::Default,
        PermissionMode::AcceptEdit,
        PermissionMode::AutoMode,
        PermissionMode::Bypass,
    ] {
        let mut ctx = base_context();
        ctx.permission_mode = SharedPermissionMode::new(mode);
        let names = assemble_names(&ctx);
        // 位置随 Web / Artifact 两槽位删除各前移 1（A7/A14 期望值同步），
        // 随 v4-part-3 C-04 的 Cron 槽位删除再前移 1，
        // 随 v4-part-4 W3-C1 的 Filesystem / Terminal 两槽位删除再各前移 1。
        assert_eq!(
            names.iter().position(|n| n == "HumanInTheLoopMiddleware"),
            Some(13),
            "mode {mode:?}: AskUser 位置漂移"
        );
        assert_eq!(
            names.iter().position(|n| n == "PermissionMiddleware"),
            Some(12),
            "mode {mode:?}: Permission 位置漂移"
        );
        // 条件中间件（Hook/MCP/Workflow/LSP/Goal）不应出现
        for cond in [
            "HookMiddleware",
            "McpMiddleware",
            "WorkflowMiddleware",
            "LspSyncMiddleware",
            "GoalMiddleware",
        ] {
            assert!(
                !names.contains(&cond.to_string()),
                "mode {mode:?}: 不应注册 {cond}"
            );
        }
    }
}

/// Hook 组非空 → 每组展开一个 HookMiddleware，插在 Todo 之后、HITL 之前。
#[test]
fn hook_groups_expand_hook_middleware() {
    let mut ctx = base_context();
    ctx.hook_groups = vec![vec![make_hook()], vec![make_hook(), make_hook()], vec![]];
    let names = assemble_names(&ctx);
    // 空组不展开；非空组各展开一个实例
    assert_eq!(
        names
            .iter()
            .filter(|n| n.as_str() == "HookMiddleware")
            .count(),
        2
    );
    let pos_todo = names.iter().position(|n| n == "TodoMiddleware").unwrap();
    let pos_hook1 = names.iter().position(|n| n == "HookMiddleware").unwrap();
    let pos_hitl = names
        .iter()
        .position(|n| n == "HumanInTheLoopMiddleware")
        .unwrap();
    assert!(
        pos_todo < pos_hook1 && pos_hook1 < pos_hitl,
        "Hook 组位置错误: {names:?}"
    );
}

#[test]
fn dynamic_mcp_registers_deferred_control_tool_at_mcp_slot() {
    let mut ctx = base_context();
    ctx.dynamic_mcp = Some(Arc::new(FakeDynamicDeployment));

    let names = assemble_names(&ctx);
    let tools = assemble_tool_names(&ctx);

    assert!(names.contains(&"DynamicMcpMiddleware".to_string()));
    assert!(tools.contains(&"DynamicMCP".to_string()));
    assert!(!tools
        .iter()
        .any(|tool| matches!(tool.as_str(), "DynamicMCP.load" | "DynamicMCP.unload")));
}

#[test]
fn dynamic_mcp_projection_is_bound_without_optional_registries() {
    let mut ctx = base_context();
    ctx.mcp_pool = Some(Arc::new(McpClientPool::new_empty()));
    ctx.dynamic_mcp = Some(Arc::new(FakeDynamicDeployment));
    assert!(ctx.mcp_skill_registry.is_none());
    assert!(ctx.command_registry.is_none());
    let projection = Arc::clone(&ctx.dynamic_mcp_projection);

    let names = assemble_names(&ctx);
    let names_again = assemble_names(&ctx);

    assert!(names.contains(&"McpMiddleware".to_string()));
    assert!(names_again.contains(&"McpMiddleware".to_string()));
    assert!(projection.lock().is_some());
}

/// 条件注册矩阵：MCP / Workflow / LSP / Goal 开关组合。
#[test]
fn conditional_registration_matrix() {
    // 单独开启
    let mut with_mcp = base_context();
    with_mcp.mcp_pool = Some(Arc::new(McpClientPool::new_empty()));
    let names_mcp = assemble_names(&with_mcp);
    let pos_mcp = names_mcp.iter().position(|n| n == "McpMiddleware").unwrap();
    let pos_sub = names_mcp
        .iter()
        .position(|n| n == "SubAgentMiddleware")
        .unwrap();
    let pos_ts = names_mcp.iter().position(|n| n == "ToolSearch").unwrap();
    assert!(
        pos_sub < pos_mcp && pos_mcp < pos_ts,
        "MCP 位置错误: {names_mcp:?}"
    );

    let mut with_wf = base_context();
    with_wf.workflow_executor = Some(Arc::new(FakeAgentExecutor));
    let names_wf = assemble_names(&with_wf);
    let pos_wf = names_wf
        .iter()
        .position(|n| n == "WorkflowMiddleware")
        .unwrap();
    let pos_sub_wf = names_wf
        .iter()
        .position(|n| n == "SubAgentMiddleware")
        .unwrap();
    let pos_ts_wf = names_wf.iter().position(|n| n == "ToolSearch").unwrap();
    assert!(
        pos_sub_wf < pos_wf && pos_wf < pos_ts_wf,
        "Workflow 位置错误: {names_wf:?}"
    );

    // LSP：配置非空 + host pool 注入 → 同步槽位在链上（H-03 单 pool 门控：
    // 只有配置没有 pool 不再装中间件）。
    let mut with_lsp = base_context();
    with_lsp.lsp_servers = vec![make_lsp_config()];
    with_lsp.lsp_pool = Some(create_host_lsp_pool(
        "/tmp/contract-test",
        &with_lsp.lsp_servers,
    ));
    let names_lsp = assemble_names(&with_lsp);
    let pos_lsp = names_lsp
        .iter()
        .position(|n| n == "LspSyncMiddleware")
        .unwrap();
    let pos_ts_lsp = names_lsp.iter().position(|n| n == "ToolSearch").unwrap();
    assert!(pos_ts_lsp < pos_lsp, "LSP 位置错误: {names_lsp:?}");

    let mut with_goal = base_context();
    with_goal.goal_controller = Some(Arc::new(FakeGoalController));
    let names_goal = assemble_names(&with_goal);
    assert_eq!(
        names_goal.last().map(String::as_str),
        Some("GoalMiddleware")
    );
}

/// host pool 端口注入 → 装配直接消费同一端口（A23：端口即消费面，不再 downcast），
/// LspSyncMiddleware 照常注册且位置不变。
#[test]
fn lsp_pool_port_injected_registers_middleware() {
    let mut ctx = base_context();
    ctx.lsp_servers = vec![make_lsp_config()];
    ctx.lsp_pool = Some(create_host_lsp_pool("/tmp/contract-test", &ctx.lsp_servers));

    let names = assemble_names(&ctx);
    let pos_lsp = names.iter().position(|n| n == "LspSyncMiddleware").unwrap();
    let pos_ts_lsp = names.iter().position(|n| n == "ToolSearch").unwrap();
    assert!(pos_ts_lsp < pos_lsp, "LSP 位置错误: {names:?}");
}

/// H-03 装配面：生产链上的 LSP 槽位只剩**同步**语义。
///
/// 三条断言：
/// 1. 蓝本内恰有一个 `LSP` 槽位，且没有任何槽位映射回旧名 `CronMiddleware`
///    （`ChainSlot::Cron` 已随 A8 删除，cron 能力只在 builtin 实例侧；槽位名与
///    builtin 策略键不重叠由 `middleware_names_match_production_blueprint` 另锁）；
/// 2. 配置非空 + 注入 host pool ⇒ 链上出现 `LspSyncMiddleware`，且链上**不出现**
///    中间件名 `LspMiddleware`（槽位名已迁移，旧名回流即红）；
/// 3. 无 LSP 配置 ⇒ 不出现 `LspSyncMiddleware`（配置非空仍是前置条件）。
#[test]
fn production_chain_has_only_lsp_sync_slot() {
    let blueprint = production_blueprint();
    let lsp_slots = blueprint
        .iter()
        .filter(|slot| matches!(slot, ChainSlot::Lsp))
        .count();
    assert_eq!(lsp_slots, 1, "蓝本必须恰有一个 Lsp 槽位: {blueprint:?}");
    let slot_names: Vec<&str> = blueprint.iter().map(slot_middleware_name).collect();
    assert!(
        !slot_names.contains(&"CronMiddleware"),
        "蓝本不得再出现 cron 链槽位（`ChainSlot::Cron` 已删除，cron 能力只在 builtin 实例侧）: {slot_names:?}"
    );

    // 有配置 + host pool：槽位装同步中间件，旧名不回流。
    let mut ctx = base_context();
    ctx.lsp_servers = vec![make_lsp_config()];
    ctx.lsp_pool = Some(create_host_lsp_pool("/tmp/contract-test", &ctx.lsp_servers));
    let names = assemble_names(&ctx);
    assert!(
        names.iter().any(|n| n == "LspSyncMiddleware"),
        "配置非空 + host pool 注入时链上应有 LspSyncMiddleware: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "LspMiddleware"),
        "槽位名已迁移到 LspSyncMiddleware，旧名不得回流到链上: {names:?}"
    );

    // 无配置：即使 pool 存在也不装（配置非空前置条件保留）。
    let mut no_config = base_context();
    no_config.lsp_pool = Some(create_host_lsp_pool("/tmp/contract-test", &[]));
    let names_no_config = assemble_names(&no_config);
    assert!(
        !names_no_config.iter().any(|n| n == "LspSyncMiddleware"),
        "无 LSP 配置时不得装同步中间件: {names_no_config:?}"
    );
}

/// H-03 / S1：两个 LSP 关闭键的交叉矩阵（`LspMiddleware` / `LspSyncMiddleware`）。
///
/// | `LspMiddleware` | `LspSyncMiddleware` | 同步槽位 |
/// |---|---|---|
/// | 开 | 开 | 装 |
/// | 关 | 开 | 不装（实例键同时关同步目标） |
/// | 开 | 关 | 不装 |
/// | 关 | 关 | 不装 |
///
/// 任何组合下链上都不出现 `LspMiddleware`——它是 builtin 实例的 policy key，
/// 不是链上中间件名（槽位名已迁移，旧名回流即红）。
#[test]
fn lsp_slot_omitted_when_instance_or_sync_closed() {
    for (instance_closed, sync_closed) in
        [(false, false), (true, false), (false, true), (true, true)]
    {
        let mut ctx = base_context();
        ctx.lsp_servers = vec![make_lsp_config()];
        ctx.lsp_pool = Some(create_host_lsp_pool("/tmp/contract-test", &ctx.lsp_servers));
        if instance_closed {
            ctx.meta_harness_disabled
                .insert("LspMiddleware".to_string());
        }
        if sync_closed {
            ctx.meta_harness_disabled
                .insert("LspSyncMiddleware".to_string());
        }

        let names = assemble_names(&ctx);
        let expected = !instance_closed && !sync_closed;
        assert_eq!(
            names.iter().any(|n| n == "LspSyncMiddleware"),
            expected,
            "LspMiddleware={instance_closed} / LspSyncMiddleware={sync_closed} 的装/不装不符（A7/A8 交叉矩阵）: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n == "LspMiddleware"),
            "LspMiddleware 是 builtin 实例 policy key、不是链上中间件名，任何组合都不得出现: {names:?}"
        );
    }
}

/// H-03 / S3：host 级 pool 工厂**无条件返回**（空配置也返回），且保持惰性。
///
/// 空配置 ⇒ `has_servers()` 为假 ⇒ `lsp` 实例工具面为空表但仍 ready（A6/A21）：
/// 不得用「返回 None / 不构造」表达「无配置」。惰性证据：本用例的配置命令是
/// 假命令（`make_lsp_config` 的 `test-lsp-bin`），若工厂在此拉起 language server
/// 进程，构造/断言就会失败。
#[test]
fn host_lsp_pool_factory_allows_empty_config() {
    let empty = create_host_lsp_pool("/tmp/contract-test", &[]);
    assert!(
        !empty.has_servers(),
        "空配置的 host pool 不得声称有可用 server"
    );

    let with_server = create_host_lsp_pool("/tmp/contract-test", &[make_lsp_config()]);
    assert!(
        with_server.has_servers(),
        "有配置时 host pool 应登记 server（只登记配置表，不拉进程）"
    );
}

/// H5：无插件但全局 settings.json 存在 `config.lspServers` 时，合并结果
/// 非空且 source 标记为 Global；装配级验证——会话级 pool 非空、
/// 链上注册 LspSyncMiddleware（此前无插件时 LSP 产品线静默不可用）。
#[test]
fn merged_lsp_servers_global_without_plugins_registers_middleware() {
    let temp = tempfile::tempdir().unwrap();
    let settings = temp.path().join("settings.json");
    std::fs::write(
        &settings,
        r#"{"config":{"lspServers":{"rust-analyzer":{"command":"rust-analyzer"}}}}"#,
    )
    .unwrap();

    let merged = load_merged_lsp_servers(&settings, Vec::new());
    assert_eq!(merged.len(), 1, "全局配置应单独生效");
    let server = &merged[0];
    assert_eq!(server.name, "rust-analyzer");
    assert!(
        matches!(server.source, Some(LspConfigSource::Global(ref p)) if p == &settings),
        "全局来源应标记 Global: {:?}",
        server.source
    );

    // 装配级：合并结果 → host 级 pool → 链上注册 LspSyncMiddleware
    let mut ctx = base_context();
    ctx.lsp_servers = merged.clone();
    ctx.lsp_pool = Some(create_host_lsp_pool("/tmp/contract-test", &ctx.lsp_servers));
    let names = assemble_names(&ctx);
    assert!(
        names.iter().any(|n| n == "LspSyncMiddleware"),
        "无插件但全局配置存在时 LspSyncMiddleware 应注册: {names:?}"
    );
}

/// H5：合并方向对齐 MCP（global < plugin）——同名 key 插件覆盖全局。
#[test]
fn merged_lsp_servers_plugin_overrides_global() {
    let temp = tempfile::tempdir().unwrap();
    let settings = temp.path().join("settings.json");
    std::fs::write(
        &settings,
        r#"{"config":{"lspServers":{"same":{"command":"global-bin"}}}}"#,
    )
    .unwrap();

    let plugin = LspServerConfig {
        name: "same".to_string(),
        command: "plugin-bin".to_string(),
        ..make_lsp_config()
    };
    let merged = load_merged_lsp_servers(&settings, vec![plugin]);
    assert_eq!(merged.len(), 1, "同名 key 应合并为一条");
    assert_eq!(merged[0].command, "plugin-bin", "插件应覆盖全局");
}

/// H5：settings.json 不存在或无 `lspServers` 字段时返回空 Vec
/// （装配处 `lsp_servers.is_empty()` 条件注册语义不变）。
#[test]
fn merged_lsp_servers_empty_without_global_config() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("missing.json");
    assert!(load_merged_lsp_servers(&missing, Vec::new()).is_empty());

    let no_lsp = temp.path().join("settings.json");
    std::fs::write(&no_lsp, r#"{"config":{"mcpServers":{}}}"#).unwrap();
    assert!(load_merged_lsp_servers(&no_lsp, Vec::new()).is_empty());
}

/// 全开组合：完整序列精确断言（Hook 2 组 + MCP + Workflow + LSP + Goal）。
#[test]
fn full_config_chain_order() {
    let mut ctx = base_context();
    ctx.hook_groups = vec![vec![make_hook()], vec![make_hook()]];
    ctx.mcp_pool = Some(Arc::new(McpClientPool::new_empty()));
    ctx.workflow_executor = Some(Arc::new(FakeAgentExecutor));
    ctx.lsp_servers = vec![make_lsp_config()];
    // H-03 单 pool 门控：同步槽位需要 host pool（只有配置不够）。
    ctx.lsp_pool = Some(create_host_lsp_pool("/tmp/contract-test", &ctx.lsp_servers));
    ctx.goal_controller = Some(Arc::new(FakeGoalController));

    let names = assemble_names(&ctx);
    assert_eq!(
        names,
        vec![
            "DefaultSystemPromptMiddleware",
            "LangMiddleware",
            "AgentsMdMiddleware",
            "AgentDefineMiddleware",
            "PluginMiddleware",
            "SkillsMiddleware",
            "SkillPreloadMiddleware",
            "AtMentionMiddleware",
            "ImageMiddleware",
            "GitAttributionMiddleware",
            "GitWatchMiddleware",
            "TodoMiddleware",
            "HookMiddleware",
            "HookMiddleware",
            "PermissionMiddleware",
            "HumanInTheLoopMiddleware",
            "SubAgentMiddleware",
            "McpMiddleware",
            "WorkflowMiddleware",
            "PtcMiddleware",
            "ToolSearch",
            "LspSyncMiddleware",
            "GoalMiddleware",
        ]
    );
}

#[test]
fn workflow_agent_type_uses_project_definition_before_built_in() {
    let temp = tempfile::tempdir().unwrap();
    let agents_dir = temp.path().join(".claude/agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("explorer.md"),
        "---\nname: explorer\ndescription: Project override\ntools: Read, Grep\ndisallowedTools: Grep\nmodel: opus\nmaxTurns: 7\nskills: [research]\n---\n\nProject explorer persona.",
    )
    .unwrap();

    let factory = default_workflow_middleware_factory();
    let definition = factory
        .resolve_agent_definition("explorer", temp.path().to_str().unwrap())
        .unwrap();

    assert_eq!(definition.model.as_deref(), Some("opus"));
    assert_eq!(
        definition.allowed_tools,
        Some(vec!["Read".into(), "Grep".into()])
    );
    assert_eq!(definition.disallowed_tools, vec!["Grep"]);
    assert_eq!(definition.skill_names, vec!["research"]);
    assert_eq!(definition.max_iterations, 7);
    assert_eq!(
        definition
            .prompt_overrides
            .as_ref()
            .and_then(|overrides| overrides.persona.as_deref()),
        Some("Project explorer persona.")
    );
}

#[test]
fn workflow_plan_definition_inherits_model_and_preserves_sandbox_write_dirs() {
    let temp = tempfile::tempdir().unwrap();
    let definition = default_workflow_middleware_factory()
        .resolve_agent_definition("plan", temp.path().to_str().unwrap())
        .unwrap();

    assert_eq!(definition.model, None);
    assert_eq!(definition.allowed_write_dirs, vec![".peri/plans/"]);
    assert!(definition
        .disallowed_tools
        .iter()
        .any(|tool| tool.eq_ignore_ascii_case("Write")));
}

#[test]
fn workflow_agent_type_rejects_unknown_definition() {
    let temp = tempfile::tempdir().unwrap();
    let error = default_workflow_middleware_factory()
        .resolve_agent_definition("does-not-exist", temp.path().to_str().unwrap())
        .unwrap_err();

    assert!(error.contains("cannot find agent definition 'does-not-exist'"));
}

/// [回归测试] workflow 真实 executor 在 Reason 流中取消时，
/// 必须返回 interrupted，不得将 stage-local cancel 降级成 runagent-threw。
#[tokio::test]
async fn test_workflow_executor_cancel_during_model_stream_is_interrupted() {
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let model: Arc<dyn Model> = Arc::new(CancelGateModel {
        entered: std::sync::Mutex::new(Some(entered_tx)),
    });
    let cancel = tokio_util::sync::CancellationToken::new();
    let mut ctx = workflow_context_with_disabled(&[]);
    ctx.cancel = Some(cancel.clone());
    ctx.model_factory = Arc::new(move |_model, _max_tokens, _observer| {
        peri_agent::agent::workflow::WorkflowModel {
            model: Arc::clone(&model),
            model_name: "cancel-gate".to_string(),
            tier: None,
        }
    });
    let executor = peri_agent::agent::workflow::WorkflowAgentExecutor::new(ctx);
    let task = tokio::spawn(async move {
        executor
            .execute(AgentRunParams {
                run_id: "cancel-workflow-run".to_string(),
                agent_id: 1,
                prompt: "wait for cancellation".to_string(),
                schema: None,
                model: None,
                max_tokens: None,
                agent_type: None,
                isolation: None,
                allowed_tools: None,
                label: None,
                phase: None,
            })
            .await
    });
    entered_rx.await.expect("workflow model stream 必须已返回");

    cancel.cancel();
    let result = task.await.expect("workflow executor task 不得 panic");

    match result {
        AgentRunResult::Dead { reason, detail } => {
            assert_eq!(reason.as_deref(), Some("interrupted"));
            assert!(
                detail
                    .as_deref()
                    .is_some_and(|text| text.contains("interrupted")),
                "workflow 取消详情必须明确: {detail:?}"
            );
        }
        other => panic!("workflow 取消必须返回 Dead(interrupted): {other:?}"),
    }
}

#[tokio::test]
async fn test_workflow_executor_forwarder_join_error_is_dead_and_failed_telemetry() {
    let model: Arc<dyn Model> = Arc::new(CompletedWorkflowModel);
    let telemetry = Arc::new(std::sync::Mutex::new(Vec::new()));
    let telemetry_for_hook = Arc::clone(&telemetry);
    let mut ctx = workflow_context_with_disabled(&[]);
    ctx.model_factory = Arc::new(move |_model, _max_tokens, _observer| {
        peri_agent::agent::workflow::WorkflowModel {
            model: Arc::clone(&model),
            model_name: "workflow-complete".to_string(),
            tier: None,
        }
    });
    ctx.forwarder_launcher = Arc::new(|_, _, _| {
        let handle = tokio::spawn(std::future::pending());
        handle.abort();
        handle
    });
    ctx.langfuse_hooks = Some(peri_agent::session::exec::executor::LangfuseHooks {
        on_turn_start: Arc::new(|_| {}),
        on_turn_end: Arc::new(move |outcome| {
            telemetry_for_hook.lock().unwrap().push(outcome);
            None
        }),
        bridge_factory: Arc::new(|_, _| None),
    });

    let result = peri_agent::agent::workflow::WorkflowAgentExecutor::new(ctx)
        .execute(AgentRunParams {
            run_id: "forwarder-failure-workflow-run".to_string(),
            agent_id: 1,
            prompt: "complete before forwarder failure".to_string(),
            schema: None,
            model: None,
            max_tokens: None,
            agent_type: None,
            isolation: None,
            allowed_tools: None,
            label: None,
            phase: None,
        })
        .await;

    assert!(matches!(
        result,
        AgentRunResult::Dead { reason: Some(reason), .. }
            if reason == "event-forwarder-failed"
    ));
    let outcomes = telemetry.lock().unwrap();
    assert_eq!(outcomes.len(), 1);
    assert!(matches!(
        &outcomes[0],
        peri_acp_types::session::TurnTelemetryOutcome::Failed { failure }
            if failure.kind == peri_acp_types::session::ExecutionFailureKind::Internal
    ));
}
