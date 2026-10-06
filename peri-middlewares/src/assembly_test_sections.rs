use super::*;

// ─── 波 4 演进 C2/C3：段落持有者（基础段 + gated 段）────────────────────

use crate::default_system_prompt::{DefaultSystemPromptMiddleware, LangMiddleware};
use crate::hitl::HumanInTheLoopMiddleware;
use crate::permission::PermissionMiddleware;
use crate::skills::SkillsMiddleware;
use crate::subagent::SubAgentMiddleware;

/// C2/C3 契约 3：链收集与渲染面静态声明一致（单一事实源，禁止双轨）——
/// 链上各段落持有者收集的段落（ID + 内容）与同一输入下的静态段声明
/// 逐项一致（C3：gated 段 10/11/13 持有者并入）。
#[test]
fn c2_chain_collection_matches_static_declaration() {
    let mut ctx = base_context();
    let overrides = AgentOverrides {
        persona: Some("chain persona".into()),
        tone: Some("chain tone".into()),
        proactiveness: None,
        mode: None,
    };
    ctx.agent_overrides = Some(overrides.clone());
    ctx.language = Some("zh-CN".to_string());

    let out = build_middleware_chain(&ProductionChainAssembler, &ctx);
    let collected = out.chain.collect_prompt_sections();

    let expected = DefaultSystemPromptMiddleware::sections(Some(&overrides))
        .into_iter()
        .chain(LangMiddleware::sections(Some("zh-CN")))
        .chain(PermissionMiddleware::sections())
        .chain(HumanInTheLoopMiddleware::sections())
        .chain(SubAgentMiddleware::sections())
        .chain(SkillsMiddleware::sections())
        .collect::<Vec<_>>();

    assert_eq!(collected.len(), expected.len(), "链收集段数与静态声明一致");
    for expect in &expected {
        let actual = collected
            .iter()
            .find(|s| s.id == expect.id)
            .unwrap_or_else(|| panic!("链收集缺少段落 {}", expect.id));
        assert_eq!(actual.content.as_str(), expect.content.as_str());
        assert_eq!(actual.zone, expect.zone);
        assert_eq!(actual.order, expect.order);
    }
    // persona / language 动态内容按同一输入生成（内容一致 = 禁止双轨）
    assert!(
        collected
            .iter()
            .find(|s| s.id == "persona")
            .unwrap()
            .content
            .as_str()
            .contains("chain persona"),
        "链收集 persona 内容与渲染面一致"
    );
}

/// C2 契约 3：关闭 DefaultSystemPromptMiddleware / LangMiddleware → 链上
/// 无持有者、收集结果无对应段落（基础段 + persona / language 全部消失）。
#[test]
fn c2_disable_holders_removes_sections_from_chain() {
    let mut ctx = base_context();
    ctx.meta_harness_disabled
        .insert("DefaultSystemPromptMiddleware".to_string());
    ctx.meta_harness_disabled
        .insert("LangMiddleware".to_string());
    let out = build_middleware_chain(&ProductionChainAssembler, &ctx);

    let names: Vec<&str> = out.chain.names();
    assert!(
        !names.contains(&"DefaultSystemPromptMiddleware"),
        "关闭后 DefaultSystemPromptMiddleware 不应在链上: {names:?}"
    );
    assert!(
        !names.contains(&"LangMiddleware"),
        "关闭后 LangMiddleware 不应在链上: {names:?}"
    );
    let collected_ids: Vec<&str> = out
        .chain
        .collect_prompt_sections()
        .iter()
        .map(|s| s.id)
        .collect();
    assert!(
        !collected_ids.contains(&"01_intro")
            && !collected_ids.contains(&"07_runtime")
            && !collected_ids.contains(&"persona")
            && !collected_ids.contains(&"language"),
        "关闭持有者后其段落不应被收集: {collected_ids:?}"
    );
}

/// 盲区闭合（任务 4，契约 3）：关闭 gated 段持有者 → 段落与工具同时消失。
/// - 关闭 PermissionMiddleware → 10_hitl 段落消失（审批无 collect_tools 工具，
///   仅验证段落）；
/// - 关闭 HumanInTheLoopMiddleware → 12_ask_user 段落 + AskUserQuestion 工具
///   同时消失（2026-08-15 拆分后提问通道纳入关闭面）；
/// - 关闭 SubAgentMiddleware → 11_subagent 段落 + Agent/AgentResult 工具同时消失；
/// - 关闭 SkillsMiddleware → 13_skills 段落 + SkillTool/DiscoverSkillsTool 同时消失。
///
/// 段落关闭盲区（3.4 记载：关闭后段落仍渲染内置内容）随本批闭合。
#[test]
fn meta_harness_disabling_gated_holder_removes_section_and_tools() {
    // 基线：默认装配下四段全部收集
    let baseline_ctx = base_context();
    let baseline_ids: Vec<&str> = build_middleware_chain(&ProductionChainAssembler, &baseline_ctx)
        .chain
        .collect_prompt_sections()
        .iter()
        .map(|s| s.id)
        .collect();
    for id in ["10_hitl", "11_subagent", "12_ask_user", "13_skills"] {
        assert!(
            baseline_ids.contains(&id),
            "基线装配应收集 {id}: {baseline_ids:?}"
        );
    }

    let cases: &[(&str, &str, &[&str])] = &[
        (
            "PermissionMiddleware",
            "10_hitl",
            &[], // 审批无 collect_tools 工具
        ),
        (
            "HumanInTheLoopMiddleware",
            "12_ask_user",
            &["AskUserQuestion"],
        ),
        (
            "SubAgentMiddleware",
            "11_subagent",
            &["Agent", "AgentResult"],
        ),
        (
            "SkillsMiddleware",
            "13_skills",
            &["SkillTool", "DiscoverSkillsTool"],
        ),
    ];
    for (mw, section_id, gone_tools) in cases {
        let mut ctx = base_context();
        ctx.meta_harness_disabled.insert(mw.to_string());
        let out = build_middleware_chain(&ProductionChainAssembler, &ctx);
        let collected_ids: Vec<&str> = out
            .chain
            .collect_prompt_sections()
            .iter()
            .map(|s| s.id)
            .collect();
        assert!(
            !collected_ids.contains(section_id),
            "关闭 {mw} 后段落 {section_id} 不应被收集: {collected_ids:?}"
        );
        let tool_names: Vec<String> = out
            .chain
            .collect_tools(&ctx.cwd)
            .into_iter()
            .map(|t| t.name().to_string())
            .collect();
        for tool in *gone_tools {
            assert!(
                !tool_names.iter().any(|n| n == tool),
                "关闭 {mw} 后工具 {tool} 仍可见: {tool_names:?}"
            );
        }
    }
}

/// 链收集与 `project_enabled_sections` 投影一致性（契约 3 显式视图）：
/// 链上收集到的 gated 段落 ID 集合 == 映射表投影（持有者在链上 → 段落开启）。
#[test]
fn chain_collected_gated_sections_match_projection() {
    use peri_agent::middleware::project_enabled_sections;
    use std::collections::HashSet;

    // 默认装配：持有者全部在链 → 投影包含全部三个 gated 段
    let ctx = base_context();
    let out = build_middleware_chain(&ProductionChainAssembler, &ctx);
    let names: HashSet<&str> = out.chain.names().into_iter().collect();
    let projected = project_enabled_sections(&names);
    for id in ["10_hitl", "11_subagent", "13_skills"] {
        assert!(
            projected.contains(id),
            "持有者装配时投影应开启 {id}: {projected:?}"
        );
    }
    // 关闭 SubAgentMiddleware → 投影与链收集同时失去 11_subagent
    let mut ctx = base_context();
    ctx.meta_harness_disabled
        .insert("SubAgentMiddleware".to_string());
    let out = build_middleware_chain(&ProductionChainAssembler, &ctx);
    let names: HashSet<&str> = out.chain.names().into_iter().collect();
    let projected = project_enabled_sections(&names);
    assert!(
        !projected.contains("11_subagent"),
        "关闭 SubAgentMiddleware 后投影应关闭 11_subagent: {projected:?}"
    );
    let collected_ids: HashSet<&str> = out
        .chain
        .collect_prompt_sections()
        .iter()
        .map(|s| s.id)
        .collect();
    assert!(
        !collected_ids.contains("11_subagent"),
        "链收集与投影一致（11_subagent 消失）: {collected_ids:?}"
    );
}

struct ReminderGoalController(std::sync::atomic::AtomicUsize);

#[async_trait]
impl GoalController for ReminderGoalController {
    async fn create_goal(&self, _: String) -> Result<(), String> {
        Ok(())
    }
    async fn complete_goal(&self) -> Result<(), String> {
        Ok(())
    }
    async fn block_goal(&self, _: String) -> Result<(), String> {
        Ok(())
    }
    async fn clear_goal(&self) -> Result<(), String> {
        Ok(())
    }
    async fn increment_continuation(&self) -> Result<(), String> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    fn snapshot(&self) -> GoalViewSnapshot {
        GoalViewSnapshot {
            objective: Some("核对后台交付".into()),
            status: Some(peri_agent::goal::GoalStatus::Active),
            ..Default::default()
        }
    }
}

/// [回归测试] fallback manager 曾仅注入工具，未注入完成提醒 probe 与子任务 host。
/// 经真实 stage builder 和生产 Todo/Goal 槽位装配，不手工设置 idle_should_wait。
#[tokio::test]
async fn test_stage_completion_reminders_share_assembled_task_manager() {
    use peri_acp_types::session::SessionInbox;
    use peri_acp_types::tasks::{BgTaskKind, BgTaskRegistration, TaskManager as _};
    use peri_agent::{
        agent::{react::AgentOutput, stages::middleware_runner},
        session::{
            exec::{
                executor::FrozenSessionData,
                stage_builder::{build_stage_context, StageBuildInput},
            },
            factory::{ChainAssembly, MiddlewareChainAssembler},
            FrozenContext, MessageQueue,
        },
    };
    struct CaptureAssembler(parking_lot::Mutex<Option<Arc<TaskManager>>>);
    impl MiddlewareChainAssembler for CaptureAssembler {
        type Context = AssemblyContext;
        type Output = ChainAssembly;
        fn assemble(&self, _: &[ChainSlot], ctx: &AssemblyContext) -> ChainAssembly {
            *self.0.lock() = Some(ctx.task_manager.clone());
            // 仅保留目标槽位，避免其他中间件读取用户配置或建立外部连接。
            // v4-part-4 W3-C1（前提变更）：原列表含 `ChainSlot::Terminal`——壳工具不再是
            // 链上中间件的提供面（见下方 `real_shell` 处的替代覆盖说明）。
            ProductionChainAssembler.assemble(
                &[ChainSlot::Todo, ChainSlot::SubAgent, ChainSlot::Goal],
                ctx,
            )
        }
    }
    for supplied in [true, false] {
        for kind in [BgTaskKind::Agent, BgTaskKind::Workflow, BgTaskKind::Shell] {
            let fixture = tempfile::tempdir().unwrap();
            let queue = MessageQueue::new();
            let input = StageBuildInput {
                recipient_lifecycle: 1,
                execution_admission_port: None,
                sdk_run_started: None,
                sdk_admission_observed: None,
                agent_catalog: Arc::new(crate::host_ports::NoopAgentCatalog),
                cwd: fixture.path().to_string_lossy().into_owned(),
                session_id: "completion-assembly".into(),
                cancel: Default::default(),
                broker: Arc::new(FakeBroker),
                permission_mode: SharedPermissionMode::new(PermissionMode::Default),
                plugin_skill_roots: vec![],
                plugin_loaded: vec![],
                hook_groups: vec![],
                session_start_source: None,
                cron_scheduler: None,
                mcp_pool: None,
                dynamic_mcp: None,
                session_mcp_capability: None,
                dynamic_mcp_projection: Arc::new(parking_lot::Mutex::new(None)),
                tool_search_index: Arc::new(ToolSearchIndex::new()),
                shared_tools: Arc::new(RwLock::new(BTreeMap::new())),
                workflow_executor: None,
                workflow_middleware: None,
                session_resources: None,
                thread_id: None,
                model_name: "test".into(),
                provider_name: "test".into(),
                context_window: 128_000,
                language: None,
                compact_config: Default::default(),
                retry_events: Default::default(),
                primary_llm_factory: Arc::new(|| Arc::new(FakeModel)),
                auto_classifier_factory: Arc::new(|| {
                    Arc::new(tokio::sync::Mutex::new(Box::new(FakeModel)))
                }),
                llm_factory: Arc::new(|_| Box::new(FakeLlm)),
                provider_fp: "test".into(),
                render_system_prompt: Arc::new(|_, _| String::new()),
                system_builder: Arc::new(|_, _| String::new()),
                langfuse_bridge_factory: None,
                shared_queue: queue.clone(),
                idle_inbox: Some(Arc::new(SessionInbox::new(Arc::new(queue)))),
                idle_suspended_flag: None,
                launch_cron_bridge: None,
                launch_mcp_subscription: None,
                mcp_skill_registry: None,
                command_registry: None,
                tool_invocation_resolver: Arc::new(peri_agent::tools::DirectToolInvocationResolver),
                compact_pre_hook: None,
                compact_post_hook: None,
                meta_harness_disabled: Default::default(),
            };
            let assembler = CaptureAssembler(parking_lot::Mutex::new(None));
            let manager = supplied.then(|| Arc::new(TaskManager::new()));
            let controller = Arc::new(ReminderGoalController(Default::default()));
            let (built, _) = build_stage_context(
                &input,
                &assembler,
                None,
                FrozenSessionData::from_frozen_parts(FrozenContext::builder().build(), None),
                Arc::new(FakeEventHandler),
                None,
                vec![],
                None,
                None,
                Default::default(),
                Some(controller.clone()),
                manager.clone(),
                None,
            )
            .unwrap();
            let assembled_manager = assembler.0.lock().clone().unwrap();
            if let Some(manager) = manager {
                assert!(
                    Arc::ptr_eq(&manager, &assembled_manager),
                    "不可替换注入的 session manager"
                );
            }
            let todo = built
                .context
                .runtime
                .tools
                .read()
                .get("TodoWrite")
                .unwrap()
                .clone();
            todo.invoke(
                serde_json::json!({
                    "requireCompletion": true,
                    "todos": [{"content": "等待后台交付", "status": "pending"}]
                }),
                peri_agent::tools::ToolContext::new(&[], &input.cwd),
            )
            .await
            .unwrap();
            // v4-part-4 W3-C1（前提变更）：原 `real_shell` 分支从**链上**取裸名 `Bash`，
            // 验证「壳工具端持有同一个 manager」。W3-C1 后链上不再有任何壳工具
            // （`TerminalMiddleware` 删除 + `workflow_tools` 裸名段摘除），该断言在本层
            // 无对象。替代覆盖：
            // ① 本用例新增的 XOR 断言——链工具集不得再出现裸名 `Bash`（壳工具唯一提供面
            //    是 builtin `workspace` 桥）；
            // ② manager 贯通由 workspace 实例侧承担：
            //    `mcp::builtin::workspace::tests::workspace_handler_bash_run_in_background_uses_injected_task_manager_over_wire`
            //    （AW3-11 seam 注入的 TaskManager 被 Bash 真实消费）。
            if kind == BgTaskKind::Shell {
                assert!(
                    !built.context.runtime.tools.read().contains_key("Bash"),
                    "W3-C1 后链上不得再有裸名壳工具：壳工具唯一提供面是 builtin workspace 桥"
                );
            }
            assembled_manager
                .register(BgTaskRegistration {
                    task_id: "pending".into(),
                    kind,
                    summary: "后台替身".into(),
                    pid: None,
                    kill: Some(Box::new(|| {})),
                })
                .unwrap();
            let output = AgentOutput::new("等待后台完成", 1);
            let result = middleware_runner::run_after_agent(&built.context, output.clone())
                .await
                .unwrap();
            assert!(
                result.block_continue.is_none(),
                "supplied={supplied}, {kind:?}: 装配后的后台任务必须抑制提醒"
            );
            assert!(built.session.queue().is_empty());
            assert_eq!(controller.0.load(std::sync::atomic::Ordering::SeqCst), 0);
            let host = built
                .session
                .subagent_host()
                .expect("主 session 应有子任务 host");
            assert!(Arc::ptr_eq(
                host.task_manager.as_ref().unwrap(),
                &assembled_manager
            ));
            assert!(assembled_manager.complete(
                "pending",
                peri_agent::agent::events::BackgroundTaskResult {
                    task_id: "pending".into(),
                    agent_name: "test".into(),
                    prompt_summary: String::new(),
                    success: true,
                    output: String::new(),
                    tool_calls_count: 0,
                    duration_ms: 0,
                    child_thread_id: None,
                    timed_out: false,
                    subagent_failure: None,
                    shell_output: None,
                }
            ));
            let result = middleware_runner::run_after_agent(&built.context, output.clone())
                .await
                .unwrap();
            assert_eq!(
                result.block_continue.as_deref(),
                Some("todo_require_completion")
            );
            assert_eq!(
                built.session.queue().drain_all().len(),
                1,
                "双开时 Todo 优先，不重复注入 Goal"
            );
            assert_eq!(controller.0.load(std::sync::atomic::Ordering::SeqCst), 0);
            todo.invoke(
                serde_json::json!({
                    "requireCompletion": true,
                    "todos": [{"content": "等待后台交付", "status": "completed"}]
                }),
                peri_agent::tools::ToolContext::new(&[], &input.cwd),
            )
            .await
            .unwrap();
            let result = middleware_runner::run_after_agent(&built.context, output.clone())
                .await
                .unwrap();
            assert_eq!(result.block_continue.as_deref(), Some("goal_active"));
            assert_eq!(built.session.queue().drain_all().len(), 1);
            assert_eq!(controller.0.load(std::sync::atomic::Ordering::SeqCst), 1);
            let mut blocked = output;
            blocked.block_continue = Some("stop_hook_block".into());
            let result = middleware_runner::run_after_agent(&built.context, blocked)
                .await
                .unwrap();
            assert_eq!(result.block_continue.as_deref(), Some("stop_hook_block"));
            assert!(built.session.queue().is_empty());
            assert_eq!(controller.0.load(std::sync::atomic::Ordering::SeqCst), 1);
        }
    }
}
