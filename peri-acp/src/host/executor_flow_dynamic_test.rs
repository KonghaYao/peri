use super::*;

/// [回归测试] production 首个 Reason 必须看到同一 middleware chain 在
/// before_agent 后生成的 ToolSearch dynamic prompt contribution。
#[cfg(not(windows))]
#[tokio::test]
async fn test_production_first_reason_sees_after_before_agent_dynamic_contributions() {
    use peri_acp_types::session::{MessageKind, MessageSource, QueuedMessage};
    use peri_agent::agent::stages::{run_react_loop, LoopResult};

    let requests = Arc::new(Mutex::new(Vec::new()));
    let model = Arc::new(CapturePromptModel {
        requests: Arc::clone(&requests),
    }) as Arc<dyn Model>;
    let mut ctx = make_session_context("dynamic-first-reason").await;
    ctx.primary_llm_factory = Some(Arc::new(move || Arc::clone(&model)));
    let frozen = frozen_with_dynamic_prompt_policy("DYNAMIC_BASE_SENTINEL", &[]);
    let (out, _) = make_stage_build(&ctx)(make_stage_request(frozen, None)).unwrap();
    let parent = Arc::clone(&out.session);
    out.context.session.queue.push(QueuedMessage::new(
        MessageKind::Prompt,
        MessageSource::UserInput,
        BaseMessage::human("finish"),
    ));

    let loop_result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        run_react_loop(out.context, 1),
    )
    .await
    .expect("真实 loop 不得挂起");

    assert!(matches!(loop_result, LoopResult::Completed));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let system = system_text(&requests[0]);
    let tool_search_offset = system
        .find("## Deferred Tools")
        .expect("首个 Reason 缺少 ToolSearch contribution");
    assert_eq!(system.matches("DYNAMIC_BASE_SENTINEL").count(), 1);
    assert_eq!(system.matches("## Deferred Tools").count(), 1);
    let tool_search = &system[tool_search_offset..];
    assert_eq!(
        tool_search
            .matches("Discover deferred tools by name or keyword")
            .count(),
        1
    );
    assert_eq!(
        tool_search
            .matches("Invoke a registered deferred tool by name")
            .count(),
        1
    );
    assert_eq!(
        &*parent.store().frozen.system_prompt,
        "DYNAMIC_BASE_SENTINEL"
    );
    assert!(!parent
        .store()
        .frozen
        .system_prompt
        .contains("## Deferred Tools"));
}

#[derive(Clone)]
struct RecordingSubagentAssembler {
    context: Arc<Mutex<Option<peri_agent::session::subagent::SubagentChainContext>>>,
}

impl peri_agent::session::subagent::SubagentChainAssembler for RecordingSubagentAssembler {
    fn assemble(
        &self,
        ctx: &peri_agent::session::subagent::SubagentChainContext,
    ) -> peri_agent::middleware::MiddlewareChain {
        *self.context.lock().unwrap() = Some(ctx.clone());
        peri_agent::middleware::MiddlewareChain::new()
    }
}

struct FixedAnswerReactLlm;

#[async_trait]
impl peri_agent::agent::react::ReactLLM for FixedAnswerReactLlm {
    async fn generate_reasoning(
        &self,
        _messages: &[peri_agent::messages::BaseMessage],
        _tools: &[&dyn peri_agent::tools::BaseTool],
        _streaming: Option<peri_agent::agent::react::StreamingContext>,
    ) -> peri_agent::error::AgentResult<peri_agent::agent::react::Reasoning> {
        Ok(peri_agent::agent::react::Reasoning::with_answer(
            "done", "done",
        ))
    }
}

/// [回归测试] production stage 创建的主 Session 必须保存 session/new 的完整
/// snapshot；一级 child 与其链装配 context 继续复用同一冻结事实和 disabled set。
#[tokio::test]
async fn test_production_stage_propagates_frozen_snapshot_to_main_and_child() {
    use peri_agent::session::subagent::{
        SessionFactory, SubagentCancelPolicy, SubagentRunMode, SubagentSpawnConfig,
    };

    let mut ctx = make_session_context("frozen-production-parent").await;
    ctx.language = Some("en-US".into());
    let stage_build = make_stage_build(&ctx);
    let sentinel = make_sentinel_frozen();
    let (out, _) = stage_build(make_stage_request(sentinel.clone(), None)).unwrap();
    let parent_frozen = &out.session.store().frozen;
    assert_eq!(&*parent_frozen.system_prompt, sentinel.system_prompt());
    assert_eq!(&*parent_frozen.claude_md, "FROZEN_CLAUDE_SENTINEL");
    assert_eq!(&*parent_frozen.skill_summary, "FROZEN_SKILLS_SENTINEL");
    assert_eq!(&*parent_frozen.date, "1999-12-31");
    assert_eq!(parent_frozen.language.as_deref(), Some("zh-CN"));
    assert_eq!(parent_frozen.meta_harness, *sentinel.meta_harness());
    let local = out
        .session
        .subagent_host()
        .and_then(|host| host.frozen_claude_local_md.as_ref().map(|v| (**v).clone()));
    assert_eq!(local.as_deref(), Some("FROZEN_LOCAL_SENTINEL"));

    let recorded = Arc::new(Mutex::new(None));
    let child = SessionFactory::spawn_subagent(
        Some(&out.session),
        SubagentSpawnConfig {
            agent_name: "frozen-child".into(),
            prompt: "finish".into(),
            parent_messages: vec![],
            cancel_policy: SubagentCancelPolicy::Cascade,
            max_iterations: 1,
            fork_directive_kind: None,
            run_mode: SubagentRunMode::Sync,
            skill_names: vec![],
            llm: Box::new(FixedAnswerReactLlm),
            chain_assembler: Arc::new(RecordingSubagentAssembler {
                context: Arc::clone(&recorded),
            }),
            tools: vec![],
            tool_filter: peri_agent::session::tool_catalog::ToolFilterPolicy::canonical(
                Some(vec![]),
                vec![],
            ),
            system_prompt: None,
            tool_invocation_resolver: None,
            compact_config: None,
            context_budget: None,
            compact_llm: None,
            session_resources: None,
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
            cancel_token: None,
            cwd: None,
            parent_thread_id: None,
            frozen_claude_md: None,
            frozen_claude_local_md: local,
            frozen_skill_summary: None,
            frozen_date: None,
        },
    )
    .await
    .expect("child spawn 必须成功");
    let child_frozen = &child.session.store().frozen;
    assert_eq!(&*child_frozen.system_prompt, "BASE_FROZEN_SYSTEM_SENTINEL");
    assert_eq!(&*child_frozen.claude_md, "FROZEN_CLAUDE_SENTINEL");
    assert_eq!(&*child_frozen.skill_summary, "FROZEN_SKILLS_SENTINEL");
    assert_eq!(&*child_frozen.date, "1999-12-31");
    assert_eq!(child_frozen.language.as_deref(), Some("zh-CN"));
    assert_eq!(child_frozen.meta_harness, *sentinel.meta_harness());
    let child_ctx = recorded
        .lock()
        .unwrap()
        .clone()
        .expect("child chain context");
    assert_eq!(
        child_ctx.frozen_claude_md.as_deref(),
        Some("FROZEN_CLAUDE_SENTINEL")
    );
    assert_eq!(
        child_ctx.frozen_claude_local_md.as_deref(),
        Some("FROZEN_LOCAL_SENTINEL")
    );
    assert_eq!(
        child_ctx.frozen_skill_summary.as_deref(),
        Some("FROZEN_SKILLS_SENTINEL")
    );
    assert_eq!(
        child_ctx.meta_harness_disabled,
        sentinel.meta_harness().disabled_middlewares
    );
}

/// [回归测试] override 只生成 model-facing effective prompt；其语言、段落覆盖
/// 与 disabled policy 必须来自 frozen snapshot，不能被当轮 SessionContext 覆盖。
#[cfg(not(windows))]
#[tokio::test]
async fn test_production_stage_uses_frozen_language_and_keeps_override_out_of_base_prompt() {
    use peri_acp_types::agents::AgentOverrides;

    let requests = Arc::new(Mutex::new(Vec::new()));
    let model = Arc::new(CapturePromptModel {
        requests: Arc::clone(&requests),
    }) as Arc<dyn Model>;
    let mut ctx = make_session_context("frozen-language-override").await;
    ctx.language = Some("en-US".into());
    ctx.primary_llm_factory = Some(Arc::new(move || Arc::clone(&model)));
    let sentinel = make_sentinel_frozen();
    let stage_build = make_stage_build(&ctx);
    let (out, _) = stage_build(make_stage_request(
        sentinel.clone(),
        Some(AgentOverrides {
            persona: Some("OVERRIDE_PERSONA_MARKER".into()),
            ..Default::default()
        }),
    ))
    .unwrap();

    out.context
        .runtime
        .llm
        .generate_reasoning(&[], &[], None)
        .await
        .expect("capturing model 必须返回固定完成响应");

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let effective = requests[0].messages[0]
        .text_content()
        .expect("首条必须是 system message");
    assert!(effective.contains("OVERRIDE_PERSONA_MARKER"), "{effective}");
    assert!(
        effective.contains("FROZEN_SECTION_OVERRIDE_MARKER"),
        "{effective}"
    );
    assert!(
        effective.contains("Always respond in Simplified Chinese"),
        "{effective}"
    );
    assert!(
        !effective.contains("Always respond in en-US"),
        "{effective}"
    );
    assert!(
        effective.contains("Today's date: 1999-12-31"),
        "{effective}"
    );
    assert_eq!(effective.matches("Today's date:").count(), 1, "{effective}");
    assert_eq!(
        &*out.session.store().frozen.system_prompt,
        "BASE_FROZEN_SYSTEM_SENTINEL"
    );
    assert!(!out
        .session
        .store()
        .frozen
        .system_prompt
        .contains("OVERRIDE_PERSONA_MARKER"));
    assert_eq!(
        out.session.store().frozen.meta_harness.disabled_middlewares,
        sentinel.meta_harness().disabled_middlewares
    );
}

/// [回归测试] 空 CLAUDE/skills 是冻结的“缺席”而非 legacy None。即使文件在
/// snapshot 后出现，真实 before_agent lifecycle 也不得把 marker 注入贡献。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn test_production_stage_keeps_empty_frozen_prompt_inputs_after_late_files_appear() {
    use peri_acp_types::session::{MessageKind, MessageSource, QueuedMessage};
    use peri_agent::agent::stages::{run_react_loop, LoopResult};
    use peri_agent::session::FrozenContext;

    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(tmp.path());
    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(cwd.join(".claude/skills/late-skill")).unwrap();
    let frozen = FrozenSessionData::from_frozen_parts(
        FrozenContext::builder()
            .system_prompt("EMPTY_FROZEN_BASE")
            .claude_md("")
            .skill_summary("")
            .date("2026-08-25")
            .build(),
        None,
    );
    std::fs::write(cwd.join("CLAUDE.md"), "LATE_CLAUDE_MARKER").unwrap();
    std::fs::write(
        cwd.join(".claude/skills/late-skill/SKILL.md"),
        "---\nname: late-skill\ndescription: LATE_SKILL_MARKER\n---\nlate\n",
    )
    .unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let model = Arc::new(CapturePromptModel {
        requests: Arc::clone(&requests),
    }) as Arc<dyn Model>;
    let mut ctx = make_session_context("late-frozen-files").await;
    ctx.cwd = cwd.to_string_lossy().into_owned();
    ctx.primary_llm_factory = Some(Arc::new(move || Arc::clone(&model)));
    let stage_build = make_stage_build(&ctx);
    let (out, _) = stage_build(make_stage_request(frozen, None)).unwrap();
    let parent = Arc::clone(&out.session);
    let chain = Arc::clone(&out.context.runtime.middleware_chain);
    out.context.session.queue.push(QueuedMessage::new(
        MessageKind::Prompt,
        MessageSource::UserInput,
        BaseMessage::human("finish"),
    ));

    let loop_result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        run_react_loop(out.context, 1),
    )
    .await
    .expect("真实 loop 不得挂起");

    assert!(matches!(loop_result, LoopResult::Completed));
    let contributions = chain.collect_prompt_contributions();
    assert!(
        !contributions.contains("LATE_CLAUDE_MARKER"),
        "{contributions}"
    );
    assert!(
        !contributions.contains("LATE_SKILL_MARKER"),
        "{contributions}"
    );
    assert_eq!(&*parent.store().frozen.claude_md, "");
    assert_eq!(&*parent.store().frozen.skill_summary, "");

    let recorded = Arc::new(Mutex::new(None));
    let child = peri_agent::session::subagent::SessionFactory::spawn_subagent(
        Some(&parent),
        peri_agent::session::subagent::SubagentSpawnConfig {
            agent_name: "empty-frozen-child".into(),
            prompt: "finish".into(),
            parent_messages: vec![],
            cancel_policy: peri_agent::session::subagent::SubagentCancelPolicy::Cascade,
            max_iterations: 1,
            fork_directive_kind: None,
            run_mode: peri_agent::session::subagent::SubagentRunMode::Sync,
            skill_names: vec![],
            llm: Box::new(FixedAnswerReactLlm),
            chain_assembler: Arc::new(RecordingSubagentAssembler {
                context: Arc::clone(&recorded),
            }),
            tools: vec![],
            tool_filter: peri_agent::session::tool_catalog::ToolFilterPolicy::canonical(
                Some(vec![]),
                vec![],
            ),
            system_prompt: None,
            tool_invocation_resolver: None,
            compact_config: None,
            context_budget: None,
            compact_llm: None,
            session_resources: None,
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
            cancel_token: None,
            cwd: None,
            parent_thread_id: None,
            frozen_claude_md: None,
            frozen_claude_local_md: parent
                .subagent_host()
                .and_then(|host| host.frozen_claude_local_md.as_ref().map(|v| (**v).clone())),
            frozen_skill_summary: None,
            frozen_date: None,
        },
    )
    .await
    .expect("child spawn 必须成功");
    assert_eq!(&*child.session.store().frozen.claude_md, "");
    assert_eq!(&*child.session.store().frozen.skill_summary, "");
    let child_ctx = recorded.lock().unwrap().clone().expect("child context");
    assert_eq!(child_ctx.frozen_claude_md.as_deref(), Some(""));
    assert_eq!(child_ctx.frozen_skill_summary.as_deref(), Some(""));
}
