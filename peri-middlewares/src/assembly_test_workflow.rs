use super::*;

#[path = "assembly_workflow_mcp_test.rs"]
mod mcp_owner;

// ── Workflow agent 链过滤（设计 §2.5 第 3 装配入口）───────────────────────────

/// Workflow agent 工具列表按 disabled 集合连坐过滤。
///
/// A6 面③：迁移后 workflow agent 的 Web / Artifact 能力来自 builtin 实例的
/// direct bridge，而不是裸名 middleware 工具——因此本用例必须用**带池**的工厂
/// 才能观察到该能力面（无池构造器的行为与迁移前的非 Web 部分逐位一致）。
#[test]
fn workflow_build_tools_filters_disabled() {
    let factory = default_workflow_middleware_factory();
    // 无池构造器：不产生任何 builtin bridge（既有调用点语义不变）。
    let without_pool = factory.build_tools(
        "/tmp/contract-test",
        &std::collections::HashSet::new(),
        None,
        None,
    );
    let without_pool_names: Vec<&str> = without_pool.iter().map(|t| t.name()).collect();
    assert!(
        !without_pool_names
            .iter()
            .any(|name| name.starts_with("mcp__")),
        "无池工厂不得凭空产生 MCP 工具: {without_pool_names:?}"
    );

    // 带池工厂：全开时声明 `direct` 的 builtin 工具进入工具列表，deferred 的缺席
    // （面④是 direct-only 面：`open_builtin_bridges` 只保留 direct，本条链没有
    // ToolSearch 可发现）。
    let pool_factory =
        default_workflow_middleware_factory_with_pool(Some(pool_with_builtin_instances()));
    let all = pool_factory.build_tools(
        "/tmp/contract-test",
        &std::collections::HashSet::new(),
        None,
        None,
    );
    let all_names: Vec<&str> = all.iter().map(|t| t.name()).collect();
    for (name, direct) in peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES
        .iter()
        .flat_map(|instance| instance.tools.iter())
        .map(|declaration| (declaration.effective_name, declaration.direct))
    {
        let entry = all.iter().find(|tool| tool.name() == name);
        if direct {
            let entry = entry.unwrap_or_else(|| {
                panic!("workflow agent 工具列表应含 direct builtin 工具 {name}: {all_names:?}")
            });
            assert_eq!(
                entry.is_direct(),
                direct,
                "workflow agent 侧的 {name} 必须保持声明的直连性（A6 面③）"
            );
        } else {
            assert!(
                entry.is_none(),
                "workflow agent 工具列表（面④）不得含 deferred builtin 工具 {name}：\
                 deferred 工具不进 direct-only 面（`open_builtin_bridges` 只保留 direct；\
                 这两条链没有 ToolSearch 可发现，注入即模型看不见的注册项）: {all_names:?}"
            );
        }
    }
    for expected in ["SkillTool", "DiscoverSkillsTool"] {
        assert!(
            all_names.contains(&expected),
            "全开时工具 {expected} 应存在: {all_names:?}"
        );
    }
    // workspace 实例的 7 个工具按原名进入 workflow 工具面。
    let workspace = peri_acp_types::builtin_mcp::find("workspace")
        .expect("workspace 必须是已实现实例（W3-A 冻结注册表）");
    for declaration in workspace.tools {
        assert!(
            all_names.contains(&declaration.effective_name),
            "全开时 workflow 工具 {} 应存在: {all_names:?}",
            declaration.effective_name
        );
    }

    let cases: &[(&str, &[&str])] = &[("SkillsMiddleware", &["SkillTool", "DiscoverSkillsTool"])];
    for (mw, expected_gone) in cases {
        let disabled: std::collections::HashSet<String> = std::iter::once(mw.to_string()).collect();
        let tools = pool_factory.build_tools("/tmp/contract-test", &disabled, None, None);
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        for tool in *expected_gone {
            assert!(
                !names.contains(tool),
                "workflow disabled {mw} 后工具 {tool} 仍存在: {names:?}"
            );
        }
    }

    // v4-part-4 W3-C1：workspace 实例关闭键（`WorkspaceMiddleware`）在面④同样生效——
    // 7 个 `*` 一个不剩。名字面从注册表派生。
    {
        let disabled: std::collections::HashSet<String> =
            std::iter::once("WorkspaceMiddleware".to_string()).collect();
        let tools = pool_factory.build_tools("/tmp/contract-test", &disabled, None, None);
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        for declaration in workspace.tools {
            assert!(
                !names.contains(&declaration.effective_name),
                "workflow disabled WorkspaceMiddleware 后 {} 仍存在: {names:?}",
                declaration.effective_name
            );
        }
        assert!(
            names.contains(&"WebSearch"),
            "关闭 WorkspaceMiddleware 不得影响 web 实例: {names:?}"
        );
    }

    // builtin 实例关闭：workflow agent 的工具列表同样归零（IF-D10 面③）。
    for (policy_key, gone, survivor) in [
        ("WebMiddleware", "WebSearch", "artifact"),
        ("ArtifactMiddleware", "artifact", "WebSearch"),
    ] {
        let disabled: std::collections::HashSet<String> =
            std::iter::once(policy_key.to_string()).collect();
        let tools = pool_factory.build_tools("/tmp/contract-test", &disabled, None, None);
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(
            !names.contains(&gone),
            "workflow disabled {policy_key} 后 {gone} 仍存在: {names:?}"
        );
        assert!(
            names.contains(&survivor),
            "workflow disabled {policy_key} 不得影响 {survivor}: {names:?}"
        );
    }

    // **全部** builtin 实例都关闭：workflow 面 builtin 工具一个不剩。
    // 关闭键从注册表派生（硬编码少量键的写法会漏掉 workspace / cron 实例）。
    let all_keys: std::collections::HashSet<String> =
        peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES
            .iter()
            .map(|instance| instance.policy_key.to_string())
            .collect();
    let tools = pool_factory.build_tools("/tmp/contract-test", &all_keys, None, None);
    assert!(
        !tools.iter().any(|tool| tool.name().starts_with("mcp__")),
        "全部 builtin 实例都关闭后 workflow 面不得残留 MCP 工具: {:?}",
        tools.iter().map(|t| t.name()).collect::<Vec<_>>()
    );
    assert!(
        tools.iter().any(|tool| tool.name() == "SkillTool"),
        "关闭 builtin 实例不得影响非 builtin 工具面: {:?}",
        tools.iter().map(|t| t.name()).collect::<Vec<_>>()
    );
}

/// Workflow agent 中间件链按 disabled 集合过滤，未禁用项保持原相对顺序。
#[test]
fn workflow_build_middlewares_filters_disabled() {
    let factory = default_workflow_middleware_factory();
    // 全开：完整链序（与迁移前一致，顺序是行为契约）
    let all = factory.build_middlewares(
        &workflow_context_with_disabled(&[]),
        "contract-model",
        &["test-skill".to_string()],
        None,
    );
    let all_names: Vec<&str> = all.iter().map(|m| m.name()).collect();
    assert_eq!(
        all_names,
        vec![
            "AgentsMdMiddleware",
            "SkillsMiddleware",
            "SkillPreloadMiddleware",
            "GitAttributionMiddleware",
            "TodoMiddleware",
            "PermissionMiddleware",
        ]
    );

    // 逐个禁用：链上消失；剩余项相对顺序不变
    for mw in [
        "AgentsMdMiddleware",
        "SkillsMiddleware",
        "SkillPreloadMiddleware",
        "GitAttributionMiddleware",
        "TodoMiddleware",
        "PermissionMiddleware",
    ] {
        let middlewares = factory.build_middlewares(
            &workflow_context_with_disabled(&[mw]),
            "contract-model",
            &["test-skill".to_string()],
            None,
        );
        let names: Vec<&str> = middlewares.iter().map(|m| m.name()).collect();
        assert!(
            !names.contains(&mw),
            "workflow disabled {mw} 后仍出现在链上: {names:?}"
        );
        // 未禁用项保持原顺序
        let baseline: Vec<&str> = all_names.iter().copied().filter(|n| *n != mw).collect();
        assert_eq!(names, baseline, "disabled {mw} 后剩余顺序漂移");
    }
}

/// workflow agent 的壳工具提供面（v4-part-4 W3-C1 改写）。
///
/// **前提变更（原用例 `workflow_shell_tools_reject_execution_after_session_owner_closes`）**：
/// 原断言「workflow 面恰有 2 个裸名 `Bash`（`build_tools` 一个 + `TerminalMiddleware`
/// 的 `collect_tools` 一个），且二者在会话 owner 关闭后拒绝执行」——两个提供面在
/// W3-C1 后都已消失（`TerminalMiddleware` 类型删除、`workflow_tools` 的裸名段摘除），
/// 裸名 `Bash` 在面④**任何输入下**都为 0 项。壳工具的唯一提供面是 builtin `workspace`
/// 实例的桥（其 `TaskManager` 经 AW3-11 的 session 级 seam 注入，**不**经本工厂的
/// `execution_manager`——该形参因此已无消费点）。
///
/// 因此本条改写为提供面的 XOR 断言（恰有其一），并把「owner 关闭后拒绝发起新执行」
/// 这条**行为保证**移到它现在真正所在的一层：`BashTool` + `TaskManager`
/// （`middleware/terminal_test.rs` 的
/// `test_bash_rejects_new_execution_after_session_owner_closes`，对所有消费面逐字等价）。
#[tokio::test]
async fn workflow_shell_tool_face_is_the_workspace_bridge() {
    let fixture = tempfile::tempdir().unwrap();
    let cwd = fixture.path().to_str().unwrap();

    // 无池侧：壳工具本体不存在（裸名 `Bash` 0 项）。
    let no_pool = default_workflow_middleware_factory().build_tools(
        cwd,
        &std::collections::HashSet::new(),
        None,
        None,
    );
    assert_eq!(
        no_pool.iter().filter(|tool| tool.name() == "Bash").count(),
        0,
        "无池侧 workflow 面不得再有裸名 Bash（W3-C1 摘除）: {:?}",
        no_pool.iter().map(|t| t.name()).collect::<Vec<_>>()
    );

    // 带池侧：唯一形态是 builtin `workspace` 实例的桥，直连性 = 注册表声明。
    let with_pool =
        default_workflow_middleware_factory_with_pool(Some(pool_with_builtin_instances()))
            .build_tools(cwd, &std::collections::HashSet::new(), None, None);
    let declared = peri_acp_types::builtin_mcp::find("workspace")
        .expect("workspace 必须是已实现实例")
        .tools
        .iter()
        .find(|tool| tool.original_name == "Bash")
        .expect("workspace 必须声明 Bash");
    let shell = with_pool
        .iter()
        .find(|tool| tool.name() == declared.effective_name)
        .unwrap_or_else(|| {
            panic!(
                "带池侧 workflow 面应含 {}: {:?}",
                declared.effective_name,
                with_pool.iter().map(|t| t.name()).collect::<Vec<_>>()
            )
        });
    assert!(
        shell.is_direct(),
        "{} 的直连性必须等于注册表声明（AW3-03）",
        declared.effective_name
    );
    assert_eq!(
        with_pool
            .iter()
            .filter(|tool| tool.name() == "Bash")
            .count(),
        1
    );
}
