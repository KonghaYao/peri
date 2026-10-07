//! H1/H2：真实生产子链请求捕获（ACP 装配 + 真实 middlewares + bridge）。
//!
//! 与 `executor_flow_dynamic_test.rs` 的差别：本模块用**真实**生产子链装配器
//! （`assemble::child_chain_assembler` → `SubagentChainAssemblerImpl` +
//! `build_subagent_middlewares`：AgentsMd→Skills→[SkillPreload]→Todo→[ToolSearch]）
//! 与真实 durable 子会话，捕获子 Agent 的最终 `ModelRequest`，覆盖 H1
//! （`before_agent` 之后的真实贡献到达请求）与 H2（按子链真实装配的能力声明）。
//! **不使用**空链或合成中间件替身。

use super::*;

#[cfg(not(windows))]
mod captured {
    use super::*;
    use peri_agent::session::subagent::{
        ForkDirectiveKind, SessionFactory, SubagentCancelPolicy, SubagentLlmSource,
        SubagentRunMode, SubagentSpawnConfig,
    };
    use peri_agent::tools::BaseTool;
    use serde_json::{json, Value};

    const IDENTITY: &str = "CHILD_PROJECTED_IDENTITY_SENTINEL";
    const PARENT_ANCHOR: &str = "PARENT_TURN_MARKER";

    /// 冻结输入 sentinel：父身份 + 项目指令 + 技能摘要；`disabled` 决定子链关闭位。
    fn chain_frozen(disabled: &[&str]) -> FrozenSessionData {
        use peri_acp_types::meta_harness::MetaHarnessState;
        use peri_agent::session::FrozenContext;

        FrozenSessionData::from_frozen_parts(
            FrozenContext::builder()
                .system_prompt("BASE_FROZEN_SYSTEM_SENTINEL")
                .claude_md("FROZEN_CLAUDE_SENTINEL")
                .skill_summary("FROZEN_SKILLS_SENTINEL")
                .date("1999-12-31")
                .meta_harness(MetaHarnessState {
                    disabled_middlewares: disabled.iter().map(|name| (*name).to_string()).collect(),
                    ..Default::default()
                })
                .build(),
            None,
        )
    }

    /// 默认 `is_direct() == false` 的夹具工具：证明真实 ToolSearch 目录进入请求面。
    struct DeferredFixtureTool;

    #[async_trait]
    impl BaseTool for DeferredFixtureTool {
        fn name(&self) -> &str {
            "FixtureDeferredTool"
        }

        fn description(&self) -> &str {
            "fixture deferred tool for the production child chain"
        }

        fn parameters(&self) -> Value {
            json!({"type": "object", "properties": {}})
        }

        async fn invoke(
            &self,
            _: Value,
            _: peri_acp_types::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok("\"fixture\"".into())
        }
    }

    #[derive(Clone, Copy)]
    struct ChildOptions {
        fork: bool,
        background: bool,
    }

    impl ChildOptions {
        const DEFINED: Self = Self {
            fork: false,
            background: false,
        };
        const FORK: Self = Self {
            fork: true,
            background: false,
        };
        const DEFINED_BACKGROUND: Self = Self {
            fork: false,
            background: true,
        };
        const FORK_BACKGROUND: Self = Self {
            fork: true,
            background: true,
        };
    }

    /// 经真实 ACP 装配面跑一次子 Agent（Sync 或 Background），返回捕获到的请求。
    async fn run_captured_child(disabled: &[&str], options: ChildOptions) -> Vec<ModelRequest> {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let child_model = Arc::new(CapturePromptModel {
            requests: Arc::clone(&requests),
        });
        let tmp = tempfile::tempdir().unwrap();
        let (mut ctx, manager) =
            make_session_context_with_manager("child-chain-capture", &tmp).await;
        let sentinel = chain_frozen(disabled);
        let workspace = tempfile::tempdir().unwrap();
        let _resources =
            dynamic_tests::bind_child_fixture_resources(&mut ctx, workspace.path(), &sentinel)
                .await;
        let invocation_id = dynamic_tests::prepare_child_fixture_intent(&ctx).await;
        let (out, _) = make_stage_build(&ctx)(make_stage_request(sentinel.clone(), None)).unwrap();
        // 真实生产子链装配器（cold 路径经同一 `child_chain_assembler` 工厂）。
        let chain_assembler =
            crate::host::assemble::child_chain_assembler(&manager, &ctx.session_id);
        let mut config = SubagentSpawnConfig {
            agent_name: "captured-child".into(),
            prompt: "finish".into(),
            parent_messages: options
                .fork
                .then(|| vec![BaseMessage::human(PARENT_ANCHOR)])
                .unwrap_or_default(),
            cancel_policy: SubagentCancelPolicy::Independent,
            max_iterations: 1,
            fork_directive_kind: options.fork.then_some(ForkDirectiveKind::Fork),
            run_mode: SubagentRunMode::Sync,
            skill_names: vec![],
            llm: SubagentLlmSource::model(
                execution_fixture::wrap_model(Arc::clone(&child_model) as Arc<dyn Model>),
                "capture-model",
            ),
            chain_assembler,
            tools: vec![Arc::new(DeferredFixtureTool) as Arc<dyn BaseTool>],
            tool_filter: Arc::new(|_| true),
            system_prompt: Some(IDENTITY.into()),
            tool_invocation_resolver: Some(crate::host::assemble::child_tool_invocation_resolver()),
            compact_config: None,
            context_budget: None,
            compact_llm: None,
            session_resources: ctx.session_resources.clone(),
            event_handler: None,
            bg_event_sender: None,
            task_manager: None,
            on_bg_complete: None,
            langfuse_bridge: None,
            on_subagent_start: None,
            on_subagent_stop: None,
            register_runtime: None,
            deregister_runtime: None,
            parent_agent_id: None,
            parent_invocation_id: Some(invocation_id),
            cancel_token: None,
            cwd: None,
            parent_thread_id: Some(ctx.session_id.clone()),
            frozen_claude_md: None,
            frozen_claude_local_md: None,
            frozen_skill_summary: None,
            frozen_date: None,
        };
        if options.background {
            config.run_mode = SubagentRunMode::Background;
            config.task_manager =
                Some(Arc::new(peri_agent::agent::async_tasks::TaskManager::new()));
            let (bg_tx, _bg_rx) = tokio::sync::mpsc::unbounded_channel();
            config.bg_event_sender = Some(bg_tx);
        }
        let spawned = SessionFactory::spawn_subagent(Some(&out.session), config)
            .await
            .expect("真实子链 spawn 必须成功");
        if options.background {
            assert!(spawned.task_id.is_some(), "后台模式必须登记任务");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while requests.lock().unwrap().is_empty() && std::time::Instant::now() < deadline {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            assert!(
                !requests.lock().unwrap().is_empty(),
                "后台子会话必须发出模型请求"
            );
        } else {
            assert!(!spawned.interrupted, "sync 子 Agent 必须跑完首轮");
        }
        let captured = requests.lock().unwrap().clone();
        assert_eq!(captured.len(), 1, "首轮恰一个模型请求");
        captured
    }

    /// 断言冻结输入 / 身份 / 延迟目录在请求面各自恰一次（H1）。
    fn assert_single_contributions(request: &ModelRequest) {
        let system = system_text(request);
        assert_eq!(
            system.matches(IDENTITY).count(),
            1,
            "子身份恰一次: {system}"
        );
        assert_eq!(
            system.matches("FROZEN_CLAUDE_SENTINEL").count(),
            1,
            "项目指令（冻结 AgentsMd 贡献）恰一次: {system}"
        );
        assert_eq!(
            system.matches("FROZEN_SKILLS_SENTINEL").count(),
            1,
            "技能摘要（冻结 Skills 贡献）恰一次: {system}"
        );
        assert_eq!(
            system.matches("## Deferred Tools").count(),
            1,
            "延迟工具目录恰一次: {system}"
        );
        assert!(
            system.contains("FixtureDeferredTool"),
            "延迟工具条目必须进入请求面: {system}"
        );
        // H2：父身份（= 父能力声明载体）不得进入子请求面。
        assert!(
            !system.contains("BASE_FROZEN_SYSTEM_SENTINEL"),
            "父冻结系统不得作为子身份/子声明: {system}"
        );
    }

    /// H2：模型面工具目录来自子链真实装配——延迟入口在、被检索工具不直接暴露、
    /// 父能力工具（Agent / AskUserQuestion / Workflow）不存在。
    fn assert_child_tool_surface(request: &ModelRequest) {
        let names: Vec<&str> = request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        for expected in ["SearchExtraTools", "ExecuteExtraTool"] {
            assert!(
                names.contains(&expected),
                "延迟工具发现/执行入口必须在模型面工具目录: {names:?}"
            );
        }
        for forbidden in [
            "FixtureDeferredTool",
            "Agent",
            "AskUserQuestion",
            "Workflow",
        ] {
            assert!(
                !names.contains(&forbidden),
                "子链不得声明能力 {forbidden}: {names:?}"
            );
        }
    }

    /// 定义型前台子链：项目指令 / 技能摘要 / 延迟工具目录与子身份各恰一次。
    #[tokio::test]
    async fn real_child_chain_first_request_carries_instructions_skills_deferred_and_identity_once()
    {
        let captured = run_captured_child(&[], ChildOptions::DEFINED).await;
        assert_single_contributions(&captured[0]);
        assert_child_tool_surface(&captured[0]);
    }

    /// fork 前台：父消息作为 ancestor 进入对话体，身份与贡献仍恰一次。
    #[tokio::test]
    async fn fork_child_chain_request_keeps_parent_ancestors_and_single_identity() {
        let captured = run_captured_child(&[], ChildOptions::FORK).await;
        assert_single_contributions(&captured[0]);
        let conversation = captured[0]
            .messages
            .iter()
            .filter(|message| !matches!(message, ModelMessage::System { .. }))
            .map(|message| message.text_content().unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            conversation.contains(PARENT_ANCHOR),
            "fork 子请求必须携带父上下文: {conversation}"
        );
        assert_eq!(
            conversation.matches(IDENTITY).count(),
            0,
            "身份不得再写入对话体: {conversation}"
        );
    }

    /// 后台（定义型与 fork）：请求在后台任务内发出，请求面契约与前台一致。
    #[tokio::test]
    async fn background_child_requests_keep_the_same_single_contribution_contract() {
        for options in [
            ChildOptions::DEFINED_BACKGROUND,
            ChildOptions::FORK_BACKGROUND,
        ] {
            let captured = run_captured_child(&[], options).await;
            assert_single_contributions(&captured[0]);
        }
    }

    /// 关闭位生效：AgentsMd / Skills / ToolSearch 关闭后对应贡献缺席，身份仍在。
    #[tokio::test]
    async fn disabled_child_capabilities_do_not_reach_the_first_request() {
        let captured = run_captured_child(
            &["AgentsMdMiddleware", "SkillsMiddleware", "ToolSearch"],
            ChildOptions::DEFINED,
        )
        .await;
        let system = system_text(&captured[0]);
        assert_eq!(
            system.matches(IDENTITY).count(),
            1,
            "身份不因贡献关闭位消失: {system}"
        );
        assert!(
            !system.contains("FROZEN_CLAUDE_SENTINEL"),
            "关闭 AgentsMd 后项目指令不得出现: {system}"
        );
        assert!(
            !system.contains("FROZEN_SKILLS_SENTINEL"),
            "关闭 Skills 后技能摘要不得出现: {system}"
        );
        assert!(
            !system.contains("## Deferred Tools"),
            "关闭 ToolSearch 后延迟工具目录不得出现: {system}"
        );
        let names: Vec<&str> = captured[0]
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        assert!(
            !names.contains(&"SearchExtraTools") && !names.contains(&"ExecuteExtraTool"),
            "关闭 ToolSearch 后延迟入口不得出现在工具目录: {names:?}"
        );
        assert!(
            !names.contains(&"Agent"),
            "子链不得声明 Agent 能力: {names:?}"
        );
    }
}
