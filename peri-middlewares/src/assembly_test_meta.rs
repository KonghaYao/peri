use super::*;

// ─── MetaHarness（设计 §2.5）：middleware 关闭契约测试 ────────────────────────

use crate::hitl::HumanInTheLoopMiddleware;
use peri_acp_types::meta_harness::{MIDDLEWARE_NAMES, MIDDLEWARE_TOOL_NAMES};

/// 每个已知 middleware 名单独禁用：链上不出现、空 disabled 时完整链序不变。
#[test]
fn meta_harness_disables_each_known_middleware() {
    let baseline = assemble_names(&base_context());
    for name in MIDDLEWARE_NAMES {
        let mut ctx = base_context();
        ctx.meta_harness_disabled.insert(name.to_string());
        let names = assemble_names(&ctx);
        assert!(
            !names.iter().any(|n| n == name),
            "disabled {name} 后仍出现在链上: {names:?}"
        );
    }
    // 空 disabled 与默认配置完全一致（default_config_produces_canonical_chain 的
    // 基线由本断言再次锁定，防止过滤逻辑误伤未禁用 middleware）。
    assert_eq!(assemble_names(&base_context()), baseline);
}

/// 条件注册 middleware：即使运行条件满足，disabled 后也不构造（构造副作用
/// 语义——不构造实例、不设置 notifier）。
#[test]
fn meta_harness_disables_conditional_middleware_despite_conditions() {
    // MCP：pool 存在 + disabled → 不注册（notifier 不设置）
    let mut ctx = base_context();
    ctx.mcp_pool = Some(Arc::new(McpClientPool::new_empty()));
    ctx.meta_harness_disabled
        .insert("McpMiddleware".to_string());
    let names = assemble_names(&ctx);
    assert!(!names.iter().any(|n| n == "McpMiddleware"), "{names:?}");

    // Workflow：executor 存在 + disabled → 不注册 adaptor
    let mut ctx = base_context();
    ctx.workflow_executor = Some(Arc::new(FakeAgentExecutor));
    ctx.meta_harness_disabled
        .insert("WorkflowMiddleware".to_string());
    let names = assemble_names(&ctx);
    assert!(
        !names.iter().any(|n| n == "WorkflowMiddleware"),
        "{names:?}"
    );

    // LSP：配置存在 + host pool 注入（运行条件满足）+ 关闭键 disabled → 不注册同步槽位。
    // 两个关闭键都会让槽位不装（A7/A8 交叉矩阵）：`LspMiddleware` = builtin 实例工具面
    // **且**同步目标；`LspSyncMiddleware` = 只关同步。四组合矩阵见
    // `lsp_slot_omitted_when_instance_or_sync_closed`，此处只锁「条件满足但关闭后不装」。
    let mut ctx = base_context();
    ctx.lsp_servers = vec![make_lsp_config()];
    ctx.lsp_pool = Some(peri_mcp_lsp::create_host_lsp_pool(
        "/tmp/contract-test",
        &ctx.lsp_servers,
    ));
    ctx.meta_harness_disabled
        .insert("LspMiddleware".to_string());
    let names = assemble_names(&ctx);
    assert!(!names.iter().any(|n| n == "LspSyncMiddleware"), "{names:?}");

    // Goal：controller 存在 + disabled → 不注册
    let mut ctx = base_context();
    ctx.goal_controller = Some(Arc::new(FakeGoalController));
    ctx.meta_harness_disabled
        .insert("GoalMiddleware".to_string());
    let names = assemble_names(&ctx);
    assert!(!names.iter().any(|n| n == "GoalMiddleware"), "{names:?}");

    // Hook：hook group 存在 + disabled → 全部组不展开
    let mut ctx = base_context();
    ctx.hook_groups = vec![vec![make_hook()], vec![make_hook()]];
    ctx.meta_harness_disabled
        .insert("HookMiddleware".to_string());
    let names = assemble_names(&ctx);
    assert!(!names.iter().any(|n| n == "HookMiddleware"), "{names:?}");
}

/// A6/R23 的关闭矩阵：四种输入 × 四个可观察面。
///
/// 输入：`WebMiddleware=false` / `ArtifactMiddleware=false` / 两者都 false /
/// `McpMiddleware=false`。四个面：① direct 能力面（模型可见的直连工具）、
/// ② deferred 目录面（ToolSearch 摘要与检索的来源）、③ subagent `parent_tools`、
/// ④ workflow agent 工具列表。断言全部落在可观察能力面，不依赖中间量。
///
/// H-03（S2）：面①/②是全量注册面（含 deferred bridge），直连性严格对齐声明表
/// `declaration.direct`；面③/④是 **direct-only** 面（`open_builtin_bridges` 只保留
/// direct），`direct: false` 的工具必须缺席——期望值一律从声明表派生，双向断言。
#[test]
fn builtin_capability_closure_matrix_covers_all_faces() {
    use crate::subagent::SubAgentMiddleware;

    let all_builtin: Vec<&'static str> = peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES
        .iter()
        .flat_map(|instance| instance.tools.iter())
        .map(|declaration| declaration.effective_name)
        .collect();
    let all_declarations: Vec<_> = peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES
        .iter()
        .flat_map(|instance| instance.tools.iter())
        .collect();

    // (关闭的实例名列表, 进入 disabled 的策略键列表)
    let both_keys: Vec<&str> = peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES
        .iter()
        .map(|instance| instance.policy_key)
        .collect();
    let cases: Vec<(Vec<&'static str>, Vec<&str>)> = {
        let mut cases = Vec::new();
        for instance in peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES {
            cases.push((
                instance
                    .tools
                    .iter()
                    .map(|declaration| declaration.effective_name)
                    .collect::<Vec<_>>(),
                vec![instance.policy_key],
            ));
        }
        cases.push((all_builtin.clone(), both_keys.clone()));
        cases
    };

    // 逐工具的直连性**只从声明表派生**（strict：不硬编码 true/false）。
    let declared_direct = |tool: &str| -> bool {
        all_declarations
            .iter()
            .find(|declaration| declaration.effective_name == tool)
            .unwrap_or_else(|| panic!("声明表应含 builtin 工具 {tool}"))
            .direct
    };

    for (closed_tools, policy_keys) in &cases {
        let disabled: std::collections::HashSet<String> =
            policy_keys.iter().map(|key| key.to_string()).collect();
        assert!(!disabled.is_empty(), "关闭输入必须映射到至少一个策略键");

        let mut ctx = base_context();
        ctx.mcp_pool = Some(pool_with_builtin_instances());
        ctx.meta_harness_disabled = disabled.clone();

        // 面①/②：链工具集合里不得残留该实例的 bridge（direct 与 deferred 都不行）。
        let out = build_middleware_chain(&ProductionChainAssembler, &ctx);
        let collected: Vec<(String, bool)> = out
            .chain
            .collect_tools(&ctx.cwd)
            .into_iter()
            .map(|tool| (tool.name().to_string(), tool.is_direct()))
            .collect();
        let survivors: Vec<&str> = all_builtin
            .iter()
            .copied()
            .filter(|tool| !closed_tools.contains(tool))
            .collect();
        for tool in closed_tools {
            assert!(
                !collected.iter().any(|(name, _)| name == tool),
                "[{policy_keys:?}] 关闭后能力面①/②不得含 {tool}: {collected:?}"
            );
        }
        for tool in &survivors {
            let entry = collected.iter().find(|(name, _)| name == tool);
            assert!(
                entry.is_some(),
                "[{policy_keys:?}] 未关闭实例的 {tool} 必须仍在能力面①/②: {collected:?}"
            );
            // 面①/② 是**全量**注册面（含 deferred bridge）：直连性必须严格等于
            // 声明表 `declaration.direct`（IF-D13，不硬编码）。
            assert_eq!(
                entry.map(|(_, direct)| *direct),
                Some(declared_direct(tool)),
                "[{policy_keys:?}] 未关闭实例的 {tool} 直连性必须严格等于声明表 direct: {collected:?}"
            );
        }

        // 面③（subagent `parent_tools`）与面④（workflow agent 工具列表）是
        // **direct-only** 面（生产契约，不是缺陷）：`open_builtin_bridges` 只保留
        // `is_direct()` 的 bridge，因为 deferred 工具在这两条链上都没有 ToolSearch
        // 可发现，注入它们只会变成模型看不见的注册项。因此按声明表 `direct` 双向
        // 分流——`direct: true` 必须在场，`direct: false` 必须缺席。
        let direct_only_faces: Vec<(&str, bool)> = survivors
            .iter()
            .map(|tool| (*tool, declared_direct(tool)))
            .collect();

        // 面③：subagent parent_tools。
        let subagent = out
            .subagent_mw
            .expect("矩阵需要 SubAgentMiddleware")
            .downcast_arc::<SubAgentMiddleware>()
            .unwrap_or_else(|_| panic!("装配产物必须可还原为 SubAgentMiddleware"));
        let parent_tool = subagent.build_tool("/tmp/contract-test");
        let parent_names: Vec<&str> = parent_tool.parent_tools.iter().map(|t| t.name()).collect();
        for tool in closed_tools {
            assert!(
                !parent_names.contains(tool),
                "[{policy_keys:?}] parent_tools（面③）不得含 {tool}: {parent_names:?}"
            );
        }
        for (tool, direct) in &direct_only_faces {
            if *direct {
                assert!(
                    parent_names.contains(tool),
                    "[{policy_keys:?}] parent_tools（面③）应保留 direct 工具 {tool}: {parent_names:?}"
                );
            } else {
                assert!(
                    !parent_names.contains(tool),
                    "[{policy_keys:?}] parent_tools（面③）不得含 deferred 工具 {tool}：\
                     deferred 工具不进 direct-only 面（`open_builtin_bridges` 只保留 direct；\
                     这两条链没有 ToolSearch 可发现，注入即模型看不见的注册项）: {parent_names:?}"
                );
            }
        }

        // 面④：workflow agent 工具列表（生产入口的带池工厂）。
        let workflow_tools =
            default_workflow_middleware_factory_with_pool(Some(pool_with_builtin_instances()))
                .build_tools("/tmp/contract-test", &disabled, None);
        let workflow_names: Vec<&str> = workflow_tools.iter().map(|t| t.name()).collect();
        for tool in closed_tools {
            assert!(
                !workflow_names.contains(tool),
                "[{policy_keys:?}] workflow agent 工具列表（面④）不得含 {tool}: {workflow_names:?}"
            );
        }
        for (tool, direct) in &direct_only_faces {
            if *direct {
                assert!(
                    workflow_names.contains(tool),
                    "[{policy_keys:?}] workflow agent 工具列表（面④）应保留 direct 工具 {tool}: {workflow_names:?}"
                );
            } else {
                assert!(
                    !workflow_names.contains(tool),
                    "[{policy_keys:?}] workflow agent 工具列表（面④）不得含 deferred 工具 {tool}：\
                     deferred 工具不进 direct-only 面（`open_builtin_bridges` 只保留 direct；\
                     这两条链没有 ToolSearch 可发现，注入即模型看不见的注册项）: {workflow_names:?}"
                );
            }
        }
    }

    // `McpMiddleware=false`：整个 MCP 槽位不构造 —— 主链与 workflow 面都不该出现
    // 任何 MCP 工具（builtin 能力随槽位关闭，而不是「天然关闭」）。
    let mut mcp_off = base_context();
    mcp_off.mcp_pool = Some(pool_with_builtin_instances());
    mcp_off
        .meta_harness_disabled
        .insert("McpMiddleware".to_string());
    let collected_off = assemble_tool_names(&mcp_off);
    assert!(
        !collected_off.iter().any(|name| name.starts_with("mcp__")),
        "McpMiddleware 关闭后主链不得含 MCP 工具: {collected_off:?}"
    );
    let workflow_off =
        default_workflow_middleware_factory_with_pool(Some(pool_with_builtin_instances()))
            .build_tools(
                "/tmp/contract-test",
                &["McpMiddleware".to_string()].into_iter().collect(),
                None,
            );
    assert!(
        !workflow_off
            .iter()
            .any(|tool| tool.name().starts_with("mcp__")),
        "McpMiddleware 关闭后 workflow 面不得含 MCP 工具: {:?}",
        workflow_off.iter().map(|t| t.name()).collect::<Vec<_>>()
    );
}

/// SubAgentMiddleware 关闭 → 关联构造联动置空（parent_tools 不注入、
/// subagent_mw 槽位 None、链上不注册、SubAgent 工具消失——禁止半开状态）。
#[test]
fn meta_harness_disables_subagent_middleware_fully() {
    let mut ctx = base_context();
    ctx.meta_harness_disabled
        .insert("SubAgentMiddleware".to_string());
    let out = build_middleware_chain(&ProductionChainAssembler, &ctx);

    assert!(
        out.subagent_mw.is_none(),
        "SubAgentMiddleware 关闭后 subagent_mw 槽位必须为 None"
    );
    let names: Vec<&str> = out.chain.names();
    assert!(
        !names.contains(&"SubAgentMiddleware"),
        "链上不应出现 SubAgentMiddleware: {names:?}"
    );
    let tool_names: Vec<String> = out
        .chain
        .collect_tools(&ctx.cwd)
        .into_iter()
        .map(|t| t.name().to_string())
        .collect();
    assert!(
        !tool_names
            .iter()
            .any(|n| n == "Agent" || n == "AgentResult"),
        "SubAgent 工具不应出现: {tool_names:?}"
    );
}

/// 工具连坐语义：关闭持有 middleware 后其全部工具从链收集结果消失。
///
/// Web / Artifact / workspace 已迁移为 builtin 实例（A6/A7/W3-C1）：对应的关闭键走
/// 注册表 `policy_key` → `closed_instances` 的**同一份**映射，断言落在可观察能力面
/// （链工具集合），不再依赖 middleware 连坐。
///
/// v4-part-4 W3-C1（前提变更）：原 `("FilesystemMiddleware", &[6 裸名])` /
/// `("TerminalMiddleware", &["Bash"])` 两行的前提已消失——两个键不再是
/// `MIDDLEWARE_NAMES` 成员（不再是链槽位键），7 个裸名也不再出现在任何装配面。
/// 改写成**生效形态**的 XOR 断言（plan §5 规则 3）：全开时 7 个 `*`
/// 恰在场、7 个裸名恰不在场；`WorkspaceMiddleware: false` 时 7 个 effective name
/// 全部消失。名字面一律从注册表派生，不写第二份清单。
#[test]
fn meta_harness_disabled_tools_removed_from_chain() {
    // 链槽位键（middleware 连坐）：Skills / SubAgent 两项保留为对照。
    let cases: &[(&str, &[&str])] = &[
        ("SkillsMiddleware", &["SkillTool", "DiscoverSkillsTool"]),
        ("SubAgentMiddleware", &["Agent", "AgentResult"]),
    ];
    for (mw, expected_gone) in cases {
        let mut ctx = base_context();
        ctx.meta_harness_disabled.insert(mw.to_string());
        let tool_names = assemble_tool_names(&ctx);
        for tool in *expected_gone {
            assert!(
                !tool_names.iter().any(|n| n == tool),
                "disabled {mw} 后工具 {tool} 仍可见: {tool_names:?}"
            );
        }
    }

    // 全开（含 builtin pool）：direct 工具按原名进入链工具面。
    let open_names = {
        let mut ctx = base_context();
        ctx.mcp_pool = Some(pool_with_builtin_instances());
        assemble_tool_names(&ctx)
    };
    for instance in peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES {
        for declaration in instance.tools {
            assert!(
                open_names.iter().any(|n| n == declaration.effective_name),
                "全开时 builtin 工具 {} 必须在链上: {open_names:?}",
                declaration.effective_name
            );
        }
    }

    // builtin 实例关闭：关闭键按注册表 policy_key 命中，工具面同时归零。
    for instance in peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES {
        let mut ctx = base_context();
        ctx.mcp_pool = Some(pool_with_builtin_instances());
        ctx.meta_harness_disabled
            .insert(instance.policy_key.to_string());
        let tool_names = assemble_tool_names(&ctx);
        for declaration in instance.tools {
            assert!(
                !tool_names.iter().any(|n| n == declaration.effective_name),
                "disabled {} 后 builtin 工具 {} 仍可见: {tool_names:?}",
                instance.policy_key,
                declaration.effective_name
            );
        }
        // 另一实例不受影响（存活证据取任一个未关闭实例的工具）。
        let survivor = peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES
            .iter()
            .filter(|other| other.name != instance.name)
            .flat_map(|other| other.tools.iter())
            .map(|tool| tool.effective_name)
            .next()
            .expect("注册表内至少两个实例（关闭语义的对照面）");
        assert!(
            tool_names.iter().any(|n| n == survivor),
            "关闭 {} 不得影响 {survivor}: {tool_names:?}",
            instance.policy_key
        );
    }
}

/// 提问通道连坐语义：关闭新 HumanInTheLoopMiddleware 后 AskUserQuestion
/// 从链收集结果消失（2026-08-15 拆分后纳入关闭面，原"始终注册"测试反转）。
#[test]
fn meta_harness_ask_user_tool_follows_hitl_disabled() {
    // 全开：AskUserQuestion 在工具集中
    let mut ctx = base_context();
    let tool_names = assemble_tool_names(&ctx);
    assert!(
        tool_names.iter().any(|n| n == "AskUserQuestion"),
        "全开时 AskUserQuestion 应可见"
    );

    // 关闭 HumanInTheLoopMiddleware：AskUserQuestion 消失；其余不变
    ctx.meta_harness_disabled
        .insert("HumanInTheLoopMiddleware".to_string());
    let tool_names = assemble_tool_names(&ctx);
    assert!(
        !tool_names.iter().any(|n| n == "AskUserQuestion"),
        "关闭 HumanInTheLoopMiddleware 后 AskUserQuestion 应消失: {tool_names:?}"
    );
    assert!(
        tool_names.iter().any(|n| n == "TodoWrite"),
        "关闭提问通道不影响其他工具: {tool_names:?}"
    );

    // 关闭 PermissionMiddleware 不影响 AskUserQuestion（审批/提问独立开关）
    let mut ctx2 = base_context();
    ctx2.meta_harness_disabled
        .insert("PermissionMiddleware".to_string());
    let tool_names = assemble_tool_names(&ctx2);
    assert!(
        tool_names.iter().any(|n| n == "AskUserQuestion"),
        "关闭 PermissionMiddleware 后 AskUserQuestion 应保留"
    );

    // 宿主级 shared_tools 不再注册任何工具（生产路径写入点归零）
    build_middleware_chain(&ProductionChainAssembler, &ctx2);
    assert!(
        !ctx2.shared_tools.read().contains_key("AskUserQuestion"),
        "shared_tools 不应再注册 AskUserQuestion（移入链 collect_tools）"
    );
}

/// parent_tools（子 agent 继承工具）按 builtin 实例关闭键过滤：
/// 关闭实例后 SubAgent 继承工具中无该实例的工具，且**任何**情况下都没有 7 个
/// 文件/终端裸名；
/// builtin 实例按 A6 面③ 以 **direct** bridge 保留（迁移后子 agent 仍能用
/// `*` / `*`，deferred 工具不进该面），关闭后归零。
///
/// v4-part-4 W3-C1（前提变更）：原 `("FilesystemMiddleware", …)` / `("TerminalMiddleware", …)`
/// 两行的前提已消失（键不再是 middleware 名、`build_parent_tools` 里的裸名提供面已摘除）
/// ⇒ 改写为 `WorkspaceMiddleware` 的生效形态断言：关闭后该实例 7 项全部消失，
/// **且** 7 个裸名在任何装配面都不出现（名字面从注册表派生）。
#[test]
fn meta_harness_disabled_parent_tools_filtered() {
    use crate::subagent::SubAgentMiddleware;

    for instance in peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES {
        let mut ctx = base_context();
        // 需要 builtin pool：否则 parent_tools 的 builtin 面为空，断言成空转。
        ctx.mcp_pool = Some(pool_with_builtin_instances());
        ctx.meta_harness_disabled
            .insert(instance.policy_key.to_string());
        let out = build_middleware_chain(&ProductionChainAssembler, &ctx);
        let subagent = out
            .subagent_mw
            .expect("parent_tools 过滤测试需要 SubAgentMiddleware")
            .downcast_arc::<SubAgentMiddleware>()
            .unwrap_or_else(|_| panic!("装配产物必须可还原为 SubAgentMiddleware"));
        let tool = subagent.build_tool("/tmp/contract-test");
        let parent_names: Vec<&str> = tool.parent_tools.iter().map(|t| t.name()).collect();
        for declaration in instance.tools {
            assert!(
                !parent_names.contains(&declaration.effective_name),
                "disabled {} 后 parent_tools 仍含 {}: {parent_names:?}",
                instance.policy_key,
                declaration.effective_name
            );
        }
    }

    // 面③（parent_tools）：**direct-only** 面（`open_builtin_bridges` 只保留 direct）。
    // 未关闭时声明 `direct: true` 的 builtin 工具必须在场且直连性等于声明；
    // 声明 `direct: false` 的（cron 三工具 + lsp 单工具）必须缺席——deferred 工具在
    // 这条链上没有 ToolSearch 可发现，注入即模型看不见的注册项。
    let mut ctx = base_context();
    ctx.mcp_pool = Some(pool_with_builtin_instances());
    let out = build_middleware_chain(&ProductionChainAssembler, &ctx);
    let subagent = out
        .subagent_mw
        .expect("parent_tools 断言需要 SubAgentMiddleware")
        .downcast_arc::<SubAgentMiddleware>()
        .unwrap_or_else(|_| panic!("装配产物必须可还原为 SubAgentMiddleware"));
    let tool = subagent.build_tool("/tmp/contract-test");
    let declared: Vec<(&str, bool)> = peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES
        .iter()
        .flat_map(|instance| instance.tools.iter())
        .map(|declaration| (declaration.effective_name, declaration.direct))
        .collect();
    let parent_names: Vec<&str> = tool.parent_tools.iter().map(|t| t.name()).collect();
    for (name, direct) in &declared {
        let entry = tool
            .parent_tools
            .iter()
            .find(|parent| parent.name() == *name);
        if *direct {
            let entry = entry.unwrap_or_else(|| {
                panic!("parent_tools 应含 direct builtin 工具 {name}: {parent_names:?}")
            });
            assert_eq!(
                entry.is_direct(),
                *direct,
                "parent_tools 中的 {name} 直连性必须等于注册表声明（IF-D13，A6 面②）"
            );
        } else {
            assert!(
                entry.is_none(),
                "parent_tools 不得含 deferred builtin 工具 {name}：deferred 工具不进 \
                 direct-only 面（`open_builtin_bridges` 只保留 direct；这两条链没有 \
                 ToolSearch 可发现，注入即模型看不见的注册项）: {parent_names:?}"
            );
        }
    }

    // 关闭 Web 实例：只影响该实例的 bridge（面②的关闭过滤）。
    let mut closed_ctx = base_context();
    closed_ctx.mcp_pool = Some(pool_with_builtin_instances());
    closed_ctx
        .meta_harness_disabled
        .insert("WebMiddleware".to_string());
    let closed_out = build_middleware_chain(&ProductionChainAssembler, &closed_ctx);
    let closed_subagent = closed_out
        .subagent_mw
        .expect("parent_tools 关闭断言需要 SubAgentMiddleware")
        .downcast_arc::<SubAgentMiddleware>()
        .unwrap_or_else(|_| panic!("装配产物必须可还原为 SubAgentMiddleware"));
    let closed_tool = closed_subagent.build_tool("/tmp/contract-test");
    let closed_names: Vec<&str> = closed_tool.parent_tools.iter().map(|t| t.name()).collect();
    let web = peri_acp_types::builtin_mcp::find("web").expect("web 实例有声明");
    assert!(
        !web.tools
            .iter()
            .any(|tool| closed_names.contains(&tool.effective_name)),
        "关闭 WebMiddleware 后 parent_tools 不得含 web 实例工具: {closed_names:?}"
    );
    assert!(
        closed_names.contains(&"artifact"),
        "关闭 WebMiddleware 不得影响 artifact 实例: {closed_names:?}"
    );

    // SubAgentMiddleware 关闭 → parent_tools 完全不构造（subagent_mw 槽位 None）
    let mut ctx = base_context();
    ctx.meta_harness_disabled
        .insert("SubAgentMiddleware".to_string());
    let out = build_middleware_chain(&ProductionChainAssembler, &ctx);
    assert!(
        out.subagent_mw.is_none(),
        "SubAgentMiddleware 关闭后 parent_tools 不应注入（槽位联动置空）"
    );
}

/// 「已知键全集」既不缺项也不重复（A7 的三条同时成立）。
///
/// 1. 链槽位名 == `MIDDLEWARE_NAMES` 去掉 builtin 实例策略键后的集合；
/// 2. builtin 策略键集合 == 声明表 `policy_key` 集合（常量或声明表漂移即红）；
/// 3. 槽位名 ∩ 策略键 == ∅（两表语义不重叠）。
///
/// 与 §3 IF-D7 A 节的等价表述一致：`槽位名 ∪ 策略键 == MIDDLEWARE_NAMES ∪
/// BUILTIN_INSTANCE_POLICY_KEYS`。`BUILTIN_INSTANCE_POLICY_KEYS` 常量由 S-02
/// （W3，`meta_harness.rs`）引入——本断言用声明表派生同一集合，因此在
/// 「I-03 → S-02」两个时点都成立且强度不降。
#[test]
fn middleware_names_match_production_blueprint() {
    let blueprint = production_blueprint();
    let slot_names: std::collections::HashSet<&str> =
        blueprint.iter().map(slot_middleware_name).collect();
    let policy_keys: std::collections::HashSet<&str> =
        peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES
            .iter()
            .map(|instance| instance.policy_key)
            .collect();
    let const_names: std::collections::HashSet<&str> = MIDDLEWARE_NAMES.iter().copied().collect();

    assert!(
        !policy_keys.is_empty(),
        "builtin 实例策略键不得为空（关闭语义的来源）"
    );
    assert_eq!(
        policy_keys.len(),
        peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES.len(),
        "policy_key 必须在声明表内唯一"
    );
    assert!(
        slot_names.is_disjoint(&policy_keys),
        "链槽位名与 builtin 策略键必须语义不重叠: {slot_names:?} / {policy_keys:?}"
    );
    // 1：MIDDLEWARE_NAMES 去掉策略键后必须恰为链槽位名。
    let const_slots: std::collections::HashSet<&str> =
        const_names.difference(&policy_keys).copied().collect();
    assert_eq!(
        slot_names, const_slots,
        "MIDDLEWARE_NAMES 必须恰为「链槽位名 ∪ builtin 策略键」（不缺项、不重复）"
    );
}

/// 「常量去掉全部已迁移裸名」== 各 middleware 静态工具名并集。
///
/// 工具名漂移即防御剔除面失真（新增 middleware 工具须同步两处）。
///
/// v4-part-2 W3（A7/IF-D7 B 节）：`WebFetch` / `WebSearch` / `artifact` 已迁移为
/// builtin MCP 实例（`*` / `artifact`），不再是任何
/// middleware 的静态工具；`MIDDLEWARE_TOOL_NAMES` 侧的删除归 **S-02**
/// （`peri-acp-types/src/meta_harness.rs`），本文件归 I-03，故断言写成与
/// `middleware_names_match_production_blueprint` 同构的**等价形态**：常量去掉
/// 已迁移名字后必须与各 middleware 静态工具名并集相等。S-02 删除前后都成立且
/// 强度不降（删除后 `difference` 为空集，即严格集合相等）。
///
/// v4-part-4 W3-C1：`FilesystemMiddleware` / `TerminalMiddleware` 类型被删，
/// 其 7 个静态工具名（Read / Write / Edit / Glob / Grep / folder_operations / Bash）
/// 随之从"静态工具并集"消失，并从 `MIDDLEWARE_TOOL_NAMES` 删除——两侧同时退场，
/// 本断言的等价形态因此仍成立（迁移名一律从**声明表**派生，不新建第二张反查表，
/// IF-D15 / §9 规则 11）。
#[test]
fn middleware_tool_names_match_static_tool_sets() {
    let migrated_bare_names: std::collections::HashSet<&str> =
        peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES
            .iter()
            .flat_map(|instance| instance.tools.iter())
            .map(|declaration| declaration.original_name)
            .collect();
    assert!(
        !migrated_bare_names.is_empty(),
        "迁移名集合不得为空（本断言的省略面）"
    );

    let static_tools: std::collections::HashSet<&str> = HumanInTheLoopMiddleware::tool_names()
        .into_iter()
        .chain([
            // SkillsMiddleware
            "SkillTool",
            "DiscoverSkillsTool",
            // SubAgentMiddleware
            "Agent",
            "AgentResult",
            // WorkflowMiddleware（peri-workflow::tool::WorkflowTool）
            "Workflow",
            // TodoMiddleware
            "TodoWrite",
            // ToolSearch
            "ToolSearch",
            "SearchExtraTools",
            "ExecuteExtraTool",
            // （LSP 已迁 builtin 实例 `mcp__lsp__LSP`，不再是 middleware 静态工具）
            // GoalMiddleware
            "goal",
            // McpMiddleware（静态部分）
            "DiscoverMCP",
            "mcp_read_resource",
        ])
        .collect();
    // Web / Artifact / workspace 能力已不在 middleware 提供面上（迁移为 builtin 实例）。
    assert!(
        static_tools.is_disjoint(&migrated_bare_names),
        "已迁移的 builtin 工具不得再出现在 middleware 静态工具集中: {static_tools:?}"
    );

    let const_tools: std::collections::HashSet<&str> =
        MIDDLEWARE_TOOL_NAMES.iter().copied().collect();
    let const_tools_wo_migrated: std::collections::HashSet<&str> = const_tools
        .difference(&migrated_bare_names)
        .copied()
        .collect();
    assert_eq!(
        static_tools, const_tools_wo_migrated,
        "MIDDLEWARE_TOOL_NAMES（去掉已迁移裸名后）必须与各 middleware 静态工具名并集一致"
    );
}
