//! H1/H2：真实生产子链首请求捕获（ACP 装配 + 真实 middlewares + bridge）。
//!
//! 与 `executor_flow_dynamic_test.rs` 的差别：本模块用**真实**生产子链装配器
//! （`assemble::child_chain_assembler` → `SubagentChainAssemblerImpl` +
//! `build_subagent_middlewares`：AgentsMd→Skills→[SkillPreload]→Todo→[ToolSearch]）
//! 与真实 durable 子会话，捕获子 Agent 首个 `ModelRequest` 的最终 system 面，
//! 覆盖 H1（`before_agent` 之后的真实贡献到达首请求）与 H2（按子链真实装配的
//! 能力声明）。**不使用**空链或合成中间件替身。

use super::*;

#[cfg(not(windows))]
mod captured {
    use super::*;
    use peri_agent::session::subagent::{
        SessionFactory, SubagentCancelPolicy, SubagentLlmSource, SubagentRunMode,
        SubagentSpawnConfig,
    };
    use peri_agent::tools::BaseTool;
    use serde_json::{json, Value};

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

    /// 默认 `is_direct() == false` 的夹具工具：证明真实 ToolSearch 目录进入首请求。
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

    /// 经真实 ACP 装配面跑一次 Sync 子 Agent，返回捕获到的首请求。
    async fn run_captured_child(disabled: &[&str]) -> ModelRequest {
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
        // 真实生产子链装配器（同一实现：cold 路径也经 `child_chain_assembler`）。
        let chain_assembler =
            crate::host::assemble::child_chain_assembler(&manager, &ctx.session_id);
        let spawned = SessionFactory::spawn_subagent(
            Some(&out.session),
            SubagentSpawnConfig {
                agent_name: "captured-child".into(),
                prompt: "finish".into(),
                parent_messages: vec![],
                cancel_policy: SubagentCancelPolicy::Cascade,
                max_iterations: 1,
                fork_directive_kind: None,
                run_mode: SubagentRunMode::Sync,
                skill_names: vec![],
                llm: SubagentLlmSource::model(
                    execution_fixture::wrap_model(Arc::clone(&child_model) as Arc<dyn Model>),
                    "capture-model",
                ),
                chain_assembler,
                tools: vec![Arc::new(DeferredFixtureTool) as Arc<dyn BaseTool>],
                tool_filter: Arc::new(|_| true),
                system_prompt: Some("CHILD_PROJECTED_IDENTITY_SENTINEL".into()),
                tool_invocation_resolver: Some(
                    crate::host::assemble::child_tool_invocation_resolver(),
                ),
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
            },
        )
        .await
        .expect("真实子链 spawn 必须成功");
        assert!(!spawned.interrupted, "sync 子 Agent 必须跑完首轮");
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1, "首请求恰一次（Sync 单轮）");
        requests[0].clone()
    }

    /// 真实生产子链首请求：项目指令 / 技能摘要 / 延迟工具目录与子身份各恰一次。
    #[tokio::test]
    async fn real_child_chain_first_request_carries_instructions_skills_deferred_and_identity_once()
    {
        let request = run_captured_child(&[]).await;
        let system = system_text(&request);
        assert_eq!(
            system.matches("CHILD_PROJECTED_IDENTITY_SENTINEL").count(),
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
            "延迟工具条目必须进入首请求: {system}"
        );
        // H2：父身份（= 父能力声明载体）不得进入子请求面。
        assert!(
            !system.contains("BASE_FROZEN_SYSTEM_SENTINEL"),
            "父冻结系统不得作为子身份/子声明: {system}"
        );
    }

    /// 关闭位生效：AgentsMd / Skills / ToolSearch 关闭后对应贡献缺席，身份仍在。
    #[tokio::test]
    async fn disabled_child_capabilities_do_not_reach_the_first_request() {
        let request =
            run_captured_child(&["AgentsMdMiddleware", "SkillsMiddleware", "ToolSearch"]).await;
        let system = system_text(&request);
        assert_eq!(
            system.matches("CHILD_PROJECTED_IDENTITY_SENTINEL").count(),
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
    }
}
