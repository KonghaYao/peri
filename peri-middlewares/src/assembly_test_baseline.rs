use super::*;

// ── 契约用例 ─────────────────────────────────────────────────────────────────

/// 蓝本槽位顺序 = 行为契约（7 组 21 槽，禁止重排；波 4 演进 C2 新增
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
///
/// v4 wave 4（Git Watch 下沉）：`GitWatch` 槽位删除（git ref 变化改由 builtin
/// `workspace` 实例的 `workspace://git/ref` 资源 + 2026-07-28 订阅回传），
/// 其余槽位相对顺序不变；槽位总数随之 22 → 21。
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
            "Plugin",
            "Skills",
            "SkillPreload",
            "AtMention",
            "Image",
            // 第二组：工作区观察类注入器（W3-C1 后不含文件/终端工具提供器；
            // wave 4 后不含 GitWatch——git ref 变化走 builtin 实例订阅回传）
            "GitAttribution",
            // 第三组：Todo
            "Todo",
            // 第四组：Hook 哨兵
            "Hook",
            // 第五组：Permission + AskUser + SubAgent（2026-08-15 职责拆分）
            "Permission",
            "AskUser",
            "SubAgent",
            // 第六组：MCP / Workflow / ToolSearch
            "Mcp",
            "Workflow",
            "ToolSearch",
            // 第七组：Goal（Goal 在链最后）
            "Goal",
        ]
    );
    assert_eq!(slots.len(), 18, "蓝本槽位恰 18 个");
}

fn slot_name(slot: &ChainSlot) -> &'static str {
    match slot {
        ChainSlot::DefaultSystemPrompt => "DefaultSystemPrompt",
        ChainSlot::Lang => "Lang",
        ChainSlot::AgentsMd => "AgentsMd",
        ChainSlot::Plugin => "Plugin",
        ChainSlot::Skills => "Skills",
        ChainSlot::SkillPreload => "SkillPreload",
        ChainSlot::AtMention => "AtMention",
        ChainSlot::Image => "Image",
        ChainSlot::GitAttribution => "GitAttribution",
        ChainSlot::Todo => "Todo",
        ChainSlot::Hook => "Hook",
        ChainSlot::Permission => "Permission",
        ChainSlot::AskUser => "AskUser",
        ChainSlot::SubAgent => "SubAgent",
        ChainSlot::Mcp => "Mcp",
        ChainSlot::Workflow => "Workflow",
        ChainSlot::ToolSearch => "ToolSearch",
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
            "PluginMiddleware",
            "SkillsMiddleware",
            "SkillPreloadMiddleware",
            "AtMentionMiddleware",
            "ImageMiddleware",
            "GitAttributionMiddleware",
            "TodoMiddleware",
            "PermissionMiddleware",
            "HumanInTheLoopMiddleware",
            "SubAgentMiddleware",
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
        // 随 v4-part-4 W3-C1 的 Filesystem / Terminal 两槽位删除再各前移 1，
        // 随 v4 wave 4 的 GitWatch 槽位删除再前移 1，
        // 随 W5 的 AgentDefine 槽位摘除（plan §6.3）再前移 1。
        assert_eq!(
            names.iter().position(|n| n == "HumanInTheLoopMiddleware"),
            Some(11),
            "mode {mode:?}: AskUser 位置漂移"
        );
        assert_eq!(
            names.iter().position(|n| n == "PermissionMiddleware"),
            Some(10),
            "mode {mode:?}: Permission 位置漂移"
        );
        // 条件中间件（Hook/MCP/Workflow/Goal）不应出现
        for cond in [
            "HookMiddleware",
            "McpMiddleware",
            "WorkflowMiddleware",
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

/// 条件注册矩阵：MCP / Workflow / Goal 开关组合。
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

    let mut with_goal = base_context();
    with_goal.goal_controller = Some(Arc::new(FakeGoalController));
    let names_goal = assemble_names(&with_goal);
    assert_eq!(
        names_goal.last().map(String::as_str),
        Some("GoalMiddleware")
    );
}

/// 全开组合：完整序列精确断言（Hook 2 组 + MCP + Workflow + Goal）。
#[test]
fn full_config_chain_order() {
    let mut ctx = base_context();
    ctx.hook_groups = vec![vec![make_hook()], vec![make_hook()]];
    ctx.mcp_pool = Some(Arc::new(McpClientPool::new_empty()));
    ctx.workflow_executor = Some(Arc::new(FakeAgentExecutor));
    ctx.goal_controller = Some(Arc::new(FakeGoalController));

    let names = assemble_names(&ctx);
    assert_eq!(
        names,
        vec![
            "DefaultSystemPromptMiddleware",
            "LangMiddleware",
            "AgentsMdMiddleware",
            "PluginMiddleware",
            "SkillsMiddleware",
            "SkillPreloadMiddleware",
            "AtMentionMiddleware",
            "ImageMiddleware",
            "GitAttributionMiddleware",
            "TodoMiddleware",
            "HookMiddleware",
            "HookMiddleware",
            "PermissionMiddleware",
            "HumanInTheLoopMiddleware",
            "SubAgentMiddleware",
            "McpMiddleware",
            "WorkflowMiddleware",
            "ToolSearch",
            "GoalMiddleware",
        ]
    );
}

#[tokio::test]
async fn workflow_agent_definition_requires_the_resource_face() {
    // W5：workflow agent 定义与 SubAgent 同源——本地三来源经 builtin `workspace`
    // 实例的 `agent://{scope}/{id}/agent.md` 读取；无池 ZST 工厂没有资源面 ⇒
    // 明确报错，**不回落磁盘**（X4/J5）。
    let temp = tempfile::tempdir().unwrap();
    let agents_dir = temp.path().join(".claude/agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("explorer.md"),
        "---\nname: explorer\ndescription: Project override\nmodel: opus\n---\n\nProject explorer persona.",
    )
    .unwrap();

    let error = default_workflow_middleware_factory()
        .resolve_agent_definition("explorer", temp.path().to_str().unwrap())
        .await
        .unwrap_err();
    assert!(
        error.contains("unavailable"),
        "无资源面必须报「面不可用」而不是读盘：{error}"
    );

    let error = default_workflow_middleware_factory()
        .resolve_agent_definition("plan", temp.path().to_str().unwrap())
        .await
        .unwrap_err();
    assert!(error.contains("unavailable"), "{error}");

    let error = default_workflow_middleware_factory()
        .resolve_agent_definition("does-not-exist", temp.path().to_str().unwrap())
        .await
        .unwrap_err();
    assert!(error.contains("unavailable"), "{error}");
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

#[tokio::test]
async fn workflow_face_rejects_remote_agent_ids_without_an_approval_seam() {
    // F2（安全）：workflow 面没有批准 broker ⇒ 远端 id 必须显式拒绝；本地 id 的
    // 行为不受影响（本用例只锁拒绝分支，本地分支由上面的资源面用例覆盖）。
    let temp = tempfile::tempdir().unwrap();
    let error = default_workflow_middleware_factory()
        .resolve_agent_definition("mcp__external__reviewer", temp.path().to_str().unwrap())
        .await
        .unwrap_err();
    assert!(
        error.contains("remote agent definitions are not available"),
        "远端 id 必须被拒绝（无批准面）：{error}"
    );
    assert!(
        !error.contains("unavailable (MCP workspace face is not assembled)"),
        "拒绝发生在读取之前（不是面缺席的错误）：{error}"
    );
}
