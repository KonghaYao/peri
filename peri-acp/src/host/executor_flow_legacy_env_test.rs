//! Legacy snapshot environment projection through production stage and child assembly.
use crate::host::executor_flow_tests::dynamic_tests;
#[cfg(not(windows))]
use crate::host::executor_flow_tests::CapturePromptModel;
use crate::session::executor::FrozenSessionData;
use crate::session::frozen_snapshot;
use peri_acp_types::agents::AgentOverrides;
use peri_acp_types::frozen::FrozenRuntimeEnv;
use peri_acp_types::messages::BaseMessage;
use peri_agent::session::subagent::{
    ForkDirectiveKind, SessionFactory, SubagentCancelPolicy, SubagentLlmSource, SubagentSpawnConfig,
};
#[cfg(not(windows))]
use peri_model::Model;
use std::sync::Arc;
use std::sync::Mutex;

const LEGACY_SYSTEM: &str = "OLD_BASE\n<env>\nOriginal environment bytes\n</env>";

/// Decode the historical wire format instead of manufacturing an in-memory legacy flag.
fn make_legacy(version: u64, runtime: bool, embedded_external: bool) -> FrozenSessionData {
    let mut context = crate::host::executor_flow_tests::make_sentinel_frozen()
        .v2_frozen()
        .clone();
    context.system_prompt = Arc::from(if embedded_external {
        "OLD_BASE\n<agent_instructions>legacy client instructions</agent_instructions>"
    } else {
        LEGACY_SYSTEM
    });
    context.runtime_env = runtime.then(|| FrozenRuntimeEnv {
        platform: "frozen-platform".into(),
        os_version: "frozen-os".into(),
        is_git_repo: true,
    });
    let frozen = FrozenSessionData::from_frozen_parts(context, Some(Arc::from("FROZEN_LOCAL")));
    let mut wire: serde_json::Value =
        serde_json::from_str(&frozen_snapshot::encode_frozen_snapshot(&frozen).unwrap()).unwrap();
    wire["version"] = serde_json::json!(version);
    wire["data"]
        .as_object_mut()
        .unwrap()
        .remove("environment_prompt");
    if version == 1 {
        wire["data"]
            .as_object_mut()
            .unwrap()
            .remove("external_instructions");
    }
    frozen_snapshot::decode_frozen_snapshot(&wire.to_string()).unwrap()
}

/// Restoring an old root keeps its original request; only a rebuilt identity gets a suffix.
#[tokio::test]
async fn legacy_env_main_override_keeps_original_base() {
    for version in [1, 2] {
        for override_identity in [false, true] {
            let requests = Arc::new(Mutex::new(Vec::new()));
            let model: Arc<dyn Model> = Arc::new(CapturePromptModel {
                requests: Arc::clone(&requests),
            });
            let mut ctx =
                crate::host::executor_flow_tests::make_session_context("legacy-main").await;
            ctx.primary_llm_factory = Some(Arc::new(move || Arc::clone(&model)));
            let frozen = make_legacy(version, true, false);
            let overrides = override_identity.then(|| AgentOverrides {
                persona: Some("DERIVED_PERSONA".into()),
                ..Default::default()
            });
            let (out, _) = crate::host::executor_flow_tests::make_stage_build(&ctx)(
                crate::host::executor_flow_tests::make_stage_request(frozen.clone(), overrides),
            )
            .unwrap();
            out.context
                .runtime
                .llm
                .generate_reasoning(&[], &[], None)
                .await
                .unwrap();
            let wire = crate::host::executor_flow_tests::system_text(&requests.lock().unwrap()[0]);
            assert_eq!(wire.matches("<env>").count(), 1, "{wire}");
            assert_eq!(
                out.session.store().frozen.system_prompt.as_ref(),
                LEGACY_SYSTEM
            );
            assert!(frozen.v2_frozen().environment_prompt.is_none());
            if override_identity {
                assert!(wire.contains("DERIVED_PERSONA"));
                assert!(wire.contains("Today's date: 1999-12-31"));
                assert!(wire.contains("Platform: frozen-platform"));
                assert!(wire.contains(&format!("Working directory: {}", ctx.cwd)));
                assert!(wire.ends_with("Session ID: legacy-main\n</env>"));
            } else {
                assert!(wire.starts_with(LEGACY_SYSTEM));
                assert!(!wire.contains("Session ID:"));
            }
            // Re-saving and loading the prepared root must retain this distinction.
            let prepared =
                FrozenSessionData::from_frozen_parts(out.session.store().frozen.clone(), None);
            let restored = frozen_snapshot::decode_frozen_snapshot(
                &frozen_snapshot::encode_frozen_snapshot(&prepared).unwrap(),
            )
            .unwrap();
            assert_eq!(restored.system_prompt(), LEGACY_SYSTEM);
            assert!(restored.v2_frozen().legacy_embedded_environment);
            assert!(restored.v2_frozen().environment_prompt.is_none());
        }
    }
}

/// Both defined and fork children receive the frozen environment with their own session ID.
#[tokio::test]
async fn legacy_env_child_and_fork_receive_own_identity() {
    for version in [1, 2] {
        let mut ctx = crate::host::executor_flow_tests::make_session_context(&format!(
            "legacy-parent-{version}"
        ))
        .await;
        let frozen = make_legacy(version, true, false);
        let workspace = tempfile::tempdir().unwrap();
        let _resources =
            dynamic_tests::bind_child_fixture_resources(&mut ctx, workspace.path(), &frozen).await;
        let (parent, _) = crate::host::executor_flow_tests::make_stage_build(&ctx)(
            crate::host::executor_flow_tests::make_stage_request(frozen.clone(), None),
        )
        .unwrap();
        for fork in [false, true] {
            let requests = Arc::new(Mutex::new(Vec::new()));
            let model = Arc::new(CapturePromptModel {
                requests: Arc::clone(&requests),
            });
            // async-only：spawn 只登记后台任务；用例必须在同一临时库上下一次写入前
            // 等它结算，否则多个 child 的写事务会叠加并耗尽连接池。
            let task_manager = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
            let capabilities = crate::session::subagent_chain_capabilities(
                &frozen.meta_harness().disabled_middlewares,
            );
            let sections = crate::session::build_collected_sections_with_capabilities(
                frozen.meta_harness(),
                None,
                frozen.language(),
                &capabilities,
            );
            let template = crate::prompt::PromptTemplate::new(frozen.meta_harness(), &sections);
            let env =
                crate::prompt::PromptEnv::frozen(&ctx.cwd, frozen.date(), frozen.runtime_env());
            let child = SessionFactory::spawn_subagent(
                Some(&parent.session),
                SubagentSpawnConfig {
                    agent_name: "legacy-child".into(),
                    prompt: "finish".into(),
                    parent_messages: if fork {
                        vec![BaseMessage::human("parent context")]
                    } else {
                        vec![]
                    },
                    cancel_policy: SubagentCancelPolicy::Cascade,
                    max_iterations: 1,
                    fork_directive_kind: fork.then_some(ForkDirectiveKind::Fork),
                    skill_names: vec![],
                    llm: SubagentLlmSource::model(model, "capture"),
                    chain_assembler: Arc::new(
                        peri_middlewares::subagent::SubagentChainAssemblerImpl::with_registry(None),
                    ),
                    tools: vec![],
                    tool_filter: peri_agent::session::tool_catalog::ToolFilterPolicy::canonical(
                        Some(vec![]),
                        vec![],
                    ),
                    system_prompt: Some(template.render(&env, ctx.agent_catalog.as_ref())),
                    tool_invocation_resolver: None,
                    compact_config: None,
                    context_budget: None,
                    compact_llm: None,
                    session_resources: ctx.session_resources.clone(),
                    event_handler: None,
                    bg_event_sender: None,
                    task_manager: Some(Arc::clone(&task_manager)),
                    on_bg_complete: None,
                    langfuse_bridge: None,
                    on_subagent_start: None,
                    on_subagent_stop: None,
                    register_runtime: None,
                    deregister_runtime: None,
                    parent_agent_id: None,
                    parent_tool_call_id: Some(uuid::Uuid::now_v7().to_string()),
                    cancel_token: None,
                    cwd: Some(ctx.cwd.clone()),
                    parent_thread_id: Some(ctx.session_id.clone()),
                    frozen_claude_md: None,
                    frozen_claude_local_md: None,
                    frozen_skill_summary: None,
                    frozen_date: None,
                },
            )
            .await
            .unwrap();
            // 后台子 Agent 在 spawn 返回后才进入 Reason；等待首个请求落账。
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while requests.lock().unwrap().is_empty() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "child 未在超时内发出首个模型请求"
                );
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            {
                let requests = requests.lock().unwrap();
                assert!(!requests.is_empty());
                for request in requests.iter() {
                    let wire = crate::host::executor_flow_tests::system_text(request);
                    assert_eq!(wire.matches("<env>").count(), 1, "{wire}");
                    assert!(wire.contains("Platform: frozen-platform"));
                    assert!(wire.contains("Today's date: 1999-12-31"));
                    assert!(
                        wire.ends_with(&format!("Session ID: {}\n</env>", child.child_thread_id))
                    );
                    assert!(!wire.contains(LEGACY_SYSTEM));
                }
            }
            assert!(!child.session.store().frozen.legacy_embedded_environment);
            assert!(child.session.store().frozen.environment_prompt.is_some());
            wait_background_child(&task_manager).await;
        }
    }
}

/// 等待后台 child 结算完毕：spawn 返回不代表执行结束，下一轮写入前必须排空。
async fn wait_background_child(manager: &peri_agent::agent::async_tasks::TaskManager) {
    use peri_acp_types::tasks::TaskManager as _;

    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !manager.is_execution_idle() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("background child must settle before the next spawn");
}

/// Legacy missing facts remain unavailable; modern explicit absence and rejected V1 stay absent.
#[tokio::test]
async fn legacy_env_fallback_respects_snapshot_provenance() {
    let ctx = crate::host::executor_flow_tests::make_session_context("legacy-provenance").await;
    let frozen = make_legacy(2, false, false);
    let prepared =
        crate::session::prepare_derived_environment(&frozen, &ctx.cwd, ctx.agent_catalog.as_ref());
    let body = prepared.v2_frozen().environment_prompt.as_deref().unwrap();
    assert!(body.contains("unavailable"), "{body}");
    assert!(body.contains("1999-12-31"));
    assert_eq!(prepared.claude_local_md(), Some("FROZEN_LOCAL"));
    for frozen in [
        crate::host::executor_flow_tests::make_sentinel_frozen(),
        make_legacy(1, true, true),
    ] {
        let prepared = crate::session::prepare_derived_environment(
            &frozen,
            &ctx.cwd,
            ctx.agent_catalog.as_ref(),
        );
        assert!(prepared.v2_frozen().environment_prompt.is_none());
        assert_eq!(prepared.system_prompt(), frozen.system_prompt());
    }
    let frozen = make_legacy(1, true, true);
    let (out, _) = crate::host::executor_flow_tests::make_stage_build(&ctx)(
        crate::host::executor_flow_tests::make_stage_request(
            frozen,
            Some(AgentOverrides::default()),
        ),
    )
    .unwrap();
    let error = out
        .context
        .runtime
        .llm
        .generate_reasoning(&[], &[], None)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("embedded external instructions"),
        "{error}"
    );
}

/// Workflow capability projection and the final bridge preserve the legacy frozen environment.
#[tokio::test]
async fn legacy_env_workflow_request_appends_current_session_after_external_instructions() {
    use peri_agent::agent::model_bridge::AgentModelBridge;
    use peri_agent::agent::react::ReactLLM;
    for version in [1, 2] {
        let ctx =
            crate::host::executor_flow_tests::make_session_context("legacy-workflow-parent").await;
        let frozen = make_legacy(version, true, false);
        let prepared = crate::session::prepare_derived_environment(
            &frozen,
            &ctx.cwd,
            ctx.agent_catalog.as_ref(),
        );
        let capabilities = crate::host::workflow_agent::workflow_capabilities(
            &prepared.meta_harness().disabled_middlewares,
            false,
            false,
        );
        let system = crate::host::workflow_agent::project_workflow_system_prompt(
            &prepared,
            &capabilities,
            ctx.agent_catalog.as_ref(),
            &ctx.cwd,
        );
        let requests = Arc::new(Mutex::new(Vec::new()));
        let model = Arc::new(CapturePromptModel {
            requests: Arc::clone(&requests),
        });
        let bridge = AgentModelBridge::new(model)
            .with_system(system)
            .with_environment_prompt(prepared.v2_frozen().environment_prompt.clone())
            .with_session_id("workflow-execution")
            .with_external_instructions(Some(Arc::from("WORKFLOW_EXTERNAL")));
        bridge.generate_reasoning(&[], &[], None).await.unwrap();
        let wire = crate::host::executor_flow_tests::system_text(&requests.lock().unwrap()[0]);
        assert_eq!(wire.matches("<env>").count(), 1);
        assert!(wire.find("WORKFLOW_EXTERNAL").unwrap() < wire.find("<env>").unwrap());
        assert!(wire.contains("Platform: frozen-platform"));
        assert!(wire.ends_with("Session ID: workflow-execution\n</env>"));
        assert_eq!(frozen.system_prompt(), LEGACY_SYSTEM);
    }
}
