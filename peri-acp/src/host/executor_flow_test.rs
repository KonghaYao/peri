//! run_session_loop 完整装配路径测试（L5：executor 迁入 peri-agent 后留在
//! ACP 宿主侧的流程测试）。
//!
//! 归属说明：完整装配路径（continuation / turn 终态唯一）需要 stage 装配
//! 注入面（ACP 桥 + middlewares + prompt 渲染），frozen 渲染测试需要 ACP
//! 渲染面（`SessionManager::build_frozen_data`）——按归属留 ACP；keepgoing
//! 短路 / permission 通知纯函数测试随 `run_session_loop` 迁入
//! peri-agent（`session::exec::executor_test.rs`）。
//!
//! Mock 命名遵循 CLAUDE.md：`make_` 前缀（函数），`Mock` 前缀（结构体）。

use std::{
    ffi::OsString,
    sync::{Arc, Mutex, MutexGuard, OnceLock},
};

#[cfg(not(windows))]
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
#[cfg(not(windows))]
use futures::stream;
use peri_acp_types::{
    event::ExecutorEvent,
    interaction::{InteractionContext, InteractionResponse, UserInteractionBroker},
    messages::{BaseMessage, MessageContent},
    permission::{PermissionMode, SharedPermissionMode},
    store::ThreadStore,
};
use peri_agent::session::exec::executor_helpers::{
    ForwarderLauncherFn, StageBuildFn, StageBuildRequest,
};
use peri_agent::thread::FilesystemThreadStore;
use serial_test::serial;
use tokio_util::sync::CancellationToken as AgentCancellationToken;

use crate::session::executor::{
    run_session_loop, AutoClassifierFactory, FrozenSessionData, PromptStopReason, SessionContext,
    SubagentLlmFactory, TurnInput,
};
use crate::{
    provider::{LlmProvider, PeriConfig, ProfileConfig, Profiles, ProviderConfig, ProviderModels},
    session::{agent_pool::AgentPool, event_sink::EventSink, SessionManager},
};
use peri_middlewares::{host_ports::SkillsProvider, tool_search::ToolSearchIndex};
#[cfg(not(windows))]
use peri_model::{
    JsonObject, Model, ModelCapabilities, ModelMessage, ModelRequest, ModelResponse, ModelResult,
    ModelStream, ModelStreamEvent, StopReason, ToolCall,
};

#[cfg_attr(windows, allow(dead_code))]
static HOME_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[cfg_attr(windows, allow(dead_code))]
struct HomeGuard {
    _lock: MutexGuard<'static, ()>,
    previous: Option<OsString>,
}

#[cfg_attr(windows, allow(dead_code))]
impl HomeGuard {
    fn set(home: &std::path::Path) -> Self {
        let lock = HOME_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        let previous = std::env::var_os("HOME");
        std::env::set_var("HOME", home);
        Self {
            _lock: lock,
            previous,
        }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }
}

// ── Mock EventSink ─────────────────────────────────────────────────────────

/// Mock EventSink，记录所有 push_done 调用（含 request_id）与事件流。
struct MockEventSink {
    push_done_count: Mutex<usize>,
    push_done_stop_reasons: Mutex<Vec<String>>,
    pushed_events: Mutex<Vec<String>>,
    operations: Mutex<Vec<String>>,
}

impl MockEventSink {
    fn new() -> Self {
        Self {
            push_done_count: Mutex::new(0),
            push_done_stop_reasons: Mutex::new(Vec::new()),
            pushed_events: Mutex::new(Vec::new()),
            operations: Mutex::new(Vec::new()),
        }
    }

    fn push_done_count(&self) -> usize {
        *self.push_done_count.lock().unwrap()
    }
}

#[async_trait]
impl EventSink for MockEventSink {
    async fn push_event(&self, _session_id: &str, event: &ExecutorEvent, _context_window: u32) {
        let json = serde_json::to_string(event).unwrap_or_default();
        self.pushed_events.lock().unwrap().push(json.clone());
        self.operations.lock().unwrap().push(json);
    }

    async fn push_done(&self, _session_id: &str, stop_reason: &str, _request_id: Option<&str>) {
        *self.push_done_count.lock().unwrap() += 1;
        self.push_done_stop_reasons
            .lock()
            .unwrap()
            .push(stop_reason.to_string());
        self.operations
            .lock()
            .unwrap()
            .push(format!("done:{stop_reason}"));
    }
}

#[cfg(not(windows))]
struct CancelGateModel {
    entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

#[cfg(not(windows))]
#[async_trait]
impl Model for CancelGateModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_streaming: true,
            ..ModelCapabilities::default()
        }
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        cancellation: AgentCancellationToken,
    ) -> ModelResult<ModelStream> {
        if let Some(entered) = self.entered.lock().unwrap().take() {
            let _ = entered.send(());
        }
        Ok(ModelStream::with_parent_cancellation(
            stream::pending::<ModelResult<ModelStreamEvent>>(),
            cancellation,
        ))
    }
}

#[cfg(not(windows))]
struct FatalModel;

#[cfg(not(windows))]
#[async_trait]
impl Model for FatalModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_streaming: true,
            ..ModelCapabilities::default()
        }
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        _cancellation: AgentCancellationToken,
    ) -> ModelResult<ModelStream> {
        Err(peri_model::ModelError::http_status(
            500,
            "test-provider",
            Some("safe-request-id"),
        ))
    }
}

#[cfg(not(windows))]
struct CapturePromptModel {
    requests: Arc<Mutex<Vec<ModelRequest>>>,
}

#[cfg(not(windows))]
#[async_trait]
impl Model for CapturePromptModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_streaming: true,
            ..ModelCapabilities::default()
        }
    }

    async fn stream(
        &self,
        request: ModelRequest,
        cancellation: AgentCancellationToken,
    ) -> ModelResult<ModelStream> {
        self.requests.lock().unwrap().push(request);
        let response = ModelResponse::new(
            ModelMessage::assistant_text("done"),
            StopReason::EndTurn,
            None,
            None,
        )?;
        Ok(ModelStream::with_parent_cancellation(
            stream::iter(vec![Ok(ModelStreamEvent::Completed(response))]),
            cancellation,
        ))
    }
}

/// 空操作 broker：测试路径不会触发真实交互。
struct NoopBroker;

#[async_trait]
impl UserInteractionBroker for NoopBroker {
    async fn request(&self, _ctx: InteractionContext) -> InteractionResponse {
        InteractionResponse::Rejected
    }
}

// ── Helper 工厂函数 ─────────────────────────────────────────────────────────

/// 构造最小 SessionContext（flow 测试走预取消中断路径；stage 装配桥经
/// 真实 ACP 桥注入——与生产 host/prompt.rs 同模式；LLM 工厂从测试
/// LlmProvider + AgentPool 烘焙，装配路径实际调用）。
fn make_session_context(session_id: &str) -> SessionContext {
    // 事件广播宿主：发射端（EventPublisher 适配）与订阅端（subscribe 工厂）
    // 共享同一 Controller 实例，保持迁移前「publish/subscribe 同一广播」语义。
    let controller = Arc::new(peri_controller::Controller::new(
        Arc::new(FilesystemThreadStore::new(
            std::env::temp_dir().join(format!("peri-exec-flow-{}", uuid::Uuid::new_v4())),
        )) as Arc<dyn ThreadStore>,
    ));
    // 测试 LlmProvider + AgentPool + PeriConfig（与迁移前 executor_test 同源）
    let provider = LlmProvider::OpenAi {
        api_key: "test-key".to_string(),
        base_url: "https://api.example.com/v1".to_string(),
        model: "gpt-4o".to_string(),
        effort: None,
        max_tokens: 32000,
        context_1m: false,
        retry_observer: None,
    };
    let pool = Arc::new(parking_lot::Mutex::new(AgentPool::new()));
    let mut peri_config = PeriConfig::default();
    peri_config.config.active_alias = "sonnet".to_string();
    peri_config.config.providers = vec![ProviderConfig {
        id: "a".to_string(),
        provider_type: "openai".to_string(),
        api_key: "sk-test".to_string(),
        models: ProviderModels {
            sonnet: "gpt-4o".to_string(),
            ..Default::default()
        },
        ..Default::default()
    }];
    peri_config.config.profiles = Profiles {
        sonnet: ProfileConfig {
            provider: "a".to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    let peri_config = Arc::new(peri_config);
    let retry_events = pool.lock().retry_events.clone();

    // stage 装配 LLM 工厂（与生产 host/prompt.rs 同源：AgentPool 缓存 +
    // RetryObserver 烘焙；subagent 工厂烘焙 with_session_id）
    let primary_llm_factory: Option<Arc<dyn Fn() -> Arc<dyn peri_model::Model> + Send + Sync>> = {
        let pool = Arc::clone(&pool);
        let provider = provider.clone();
        let retry_events = retry_events.clone();
        Some(Arc::new(move || {
            let fp = crate::session::agent_pool::fingerprint(&provider);
            crate::session::agent_pool::AgentPool::get_or_create_subagent_llm(&pool, &fp, || {
                provider
                    .clone()
                    .with_retry_observer(Some(retry_events.as_retry_observer()))
                    .into_model()
            })
        }))
    };
    let auto_classifier_factory: Option<AutoClassifierFactory> = {
        let provider = provider.clone();
        let retry_events = retry_events.clone();
        Some(Arc::new(move || {
            Arc::new(tokio::sync::Mutex::new(
                provider
                    .clone()
                    .with_retry_observer(Some(retry_events.as_retry_observer()))
                    .into_model(),
            ))
        }))
    };
    let subagent_llm_factory: Option<SubagentLlmFactory> = {
        let provider = provider.clone();
        let peri_config = Arc::clone(&peri_config);
        let pool = Arc::clone(&pool);
        let retry_events = retry_events.clone();
        let sid = session_id.to_string();
        Some(Arc::new(move |model_alias: Option<&str>| {
            let (p, fp) = if let Some(alias) = model_alias {
                match LlmProvider::from_config_for_alias(&peri_config, alias) {
                    Some(p) => {
                        let fp = crate::session::agent_pool::fingerprint(&p);
                        (Some(p), fp)
                    }
                    None => {
                        let fp = crate::session::agent_pool::fingerprint(&provider);
                        (None, fp)
                    }
                }
            } else {
                let fp = crate::session::agent_pool::fingerprint(&provider);
                (None, fp)
            };
            let model: Arc<dyn peri_model::Model> =
                crate::session::agent_pool::AgentPool::get_or_create_subagent_llm(
                    &pool,
                    &fp,
                    || match &p {
                        Some(p) => p
                            .clone()
                            .with_retry_observer(Some(retry_events.as_retry_observer()))
                            .into_model(),
                        None => provider
                            .clone()
                            .with_retry_observer(Some(retry_events.as_retry_observer()))
                            .into_model(),
                    },
                );
            let mut llm = peri_agent::agent::model_bridge::AgentModelBridge::from_arc(model);
            llm = llm.with_session_id(sid.clone());
            Box::new(llm)
        }))
    };

    SessionContext {
        cwd: "/tmp".to_string(),
        provider_name: "OpenAI:gpt-4o".to_string(),
        provider_model_name: "gpt-4o".to_string(),
        provider_fp: "openai:gpt-4o".to_string(),
        effective_context_window: 200_000,
        claude_md_excludes: None,
        language: None,
        compact_config: Default::default(),
        get_cached_llm: None,
        fresh_auxiliary_model: None,
        store_llm: None,
        retry_events: Some(Arc::new(retry_events)),
        primary_llm_factory,
        auto_classifier_factory,
        subagent_llm_factory,
        session_id: session_id.to_string(),
        cancel: AgentCancellationToken::new(),
        broker: Arc::new(NoopBroker),
        permission_mode: SharedPermissionMode::new(PermissionMode::Bypass),
        session_access: None,
        thread_store: None,
        thread_id: None,
        plugin_skill_roots: vec![],
        plugin_agent_dirs: vec![],
        plugin_loaded: vec![],
        hook_groups: vec![],
        cron_scheduler: None,
        mcp_pool: None,
        dynamic_mcp: None,
        session_mcp_capability: None,
        dynamic_mcp_projection: Arc::new(parking_lot::Mutex::new(None)),
        channel_state: None,
        tool_search_index: Arc::new(ToolSearchIndex::default()),
        shared_tools: Arc::new(parking_lot::RwLock::new(Default::default())),
        lsp_servers: vec![],
        lsp_pool: None,
        workflow_executor: None,
        skills: Arc::new(SkillsProvider),
        workflow_middleware: None,
        event_publisher: Arc::new(crate::host::controller_ports::ControllerEventPublisher(
            controller.clone(),
        )),
        // 订阅端与发射端必须共享同一 Controller 广播（迁移前 executor 内部
        // 直接 `controller.subscribe()`）；接 PendingSubscriber 会导致事件泵
        // 收不到 TurnStarted/TurnEnded，破坏终态唯一断言。
        subscribe: {
            let controller = Arc::clone(&controller);
            Arc::new(move || {
                Box::new(
                    crate::host::controller_ports::ControllerSubscriptionAdapter(
                        controller.subscribe(),
                    ),
                )
            })
        },
        command_lookup: Arc::new(|_| None),
        compact_config_loader: Arc::new(Default::default),
        tool_invocation_resolver: Arc::new(
            peri_middlewares::tool_search::ExecuteExtraToolResolver::default(),
        ),
        session_start_source: None,
        request_id: None,
        allow_await_wake: false,
        continuation_notify: None,
        frozen_fallback_builder: None,
    }
}

/// 构造带真实 SessionManager + 已登记 session 的 SessionContext
///（可观察 v2 MessageQueue；stage 装配桥 + forwarder 真实注入）。
async fn make_session_context_with_manager(
    session_id: &str,
    tmp: &tempfile::TempDir,
) -> (SessionContext, SessionManager) {
    let mut ctx = make_session_context(session_id);
    let thread_store =
        Arc::new(FilesystemThreadStore::new(tmp.path().join("threads"))) as Arc<dyn ThreadStore>;
    let mut peri_config = PeriConfig::default();
    peri_config.config.active_alias = "sonnet".to_string();
    peri_config.config.providers = vec![ProviderConfig {
        id: "a".to_string(),
        provider_type: "openai".to_string(),
        api_key: "sk-test".to_string(),
        models: ProviderModels {
            sonnet: "gpt-4o".to_string(),
            ..Default::default()
        },
        ..Default::default()
    }];
    peri_config.config.profiles = Profiles {
        sonnet: ProfileConfig {
            provider: "a".to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    let sm = SessionManager::new(
        thread_store,
        LlmProvider::from_config(&peri_config).unwrap(),
        Arc::new(peri_config),
        SharedPermissionMode::new(PermissionMode::Bypass),
        None,
        None,
        None, // MCP 订阅端口（测试无）
        None, // Dynamic MCP（测试无）
        None, // 无 bg 场景：fallback NoopTaskManager
        Arc::new(SkillsProvider),
        Vec::new(), // plugin 命令条目（Phase 6 B2；测试无）
        Vec::new(), // plugin skill roots（C1；测试无）
    );
    sm.new_session_with_id(session_id, "/tmp")
        .await
        .expect("session 登记失败");
    ctx.session_access =
        Some(Arc::new(sm.clone()) as Arc<dyn peri_acp_types::session::SessionAccessPort>);
    (ctx, sm)
}

/// 构造 stage 装配桥（真实 ACP 桥，与生产 host/prompt.rs 同模式：ZST
/// ProductionChainAssembler + build_compact_hooks（测试 ctx hook_groups 为空
/// → (None, None)）；测试无 Langfuse → bridge factory None）。
fn make_stage_build(ctx: &SessionContext) -> StageBuildFn {
    let ctx_for_stage = ctx.clone();
    Arc::new(move |sbr| {
        let (compact_pre_hook, compact_post_hook) = crate::host::prompt::build_compact_hooks(
            &ctx_for_stage.hook_groups,
            &ctx_for_stage.cwd,
            &ctx_for_stage.session_id,
            &ctx_for_stage.provider_model_name,
        );
        crate::host::stage_builder::build_stage_context(
            &ctx_for_stage,
            &peri_middlewares::assembly::ProductionChainAssembler, // ZST 装配器
            compact_pre_hook,
            compact_post_hook,
            sbr.cached_llm.as_ref(),
            sbr.frozen_session,
            sbr.event_handler,
            sbr.agent_overrides,
            sbr.preload_skills,
            sbr.child_handler_factory,
            sbr.auxiliary_model,
            sbr.thread_persistence,
            sbr.goal_controller,
            sbr.task_manager,
            sbr.on_bg_complete,
            None, // langfuse_bridge_factory（测试无遥测）
        )
    })
}

/// 构造 forwarder 启动器（真实 spawn_eventbus_forwarder，无 Langfuse bridge）。
fn make_forwarder_launcher() -> ForwarderLauncherFn {
    Arc::new(|handles, _agent_id, on_event| {
        crate::event::spawn_eventbus_forwarder(handles, on_event, None);
    })
}

fn make_turn_input(
    event_sink: Arc<dyn EventSink>,
    content: MessageContent,
    continuation: bool,
    history: Vec<BaseMessage>,
    stage_build: StageBuildFn,
) -> TurnInput {
    TurnInput {
        event_sink,
        content,
        continuation,
        frozen: None,
        history,
        incoming_recalls: vec![],
        bg_results: vec![],
        langfuse: None,
        stage_build,
        forwarder_launcher: make_forwarder_launcher(),
    }
}

fn make_sentinel_frozen() -> FrozenSessionData {
    use peri_acp_types::meta_harness::MetaHarnessState;
    use peri_agent::session::FrozenContext;

    let mut meta = MetaHarnessState::default();
    meta.section_overrides.insert(
        "01_intro".into(),
        Arc::from("FROZEN_SECTION_OVERRIDE_MARKER"),
    );
    meta.disabled_middlewares.insert("SkillsMiddleware".into());
    meta.built_in_subagents_enabled = false;
    FrozenSessionData::from_frozen_parts(
        FrozenContext::builder()
            .system_prompt("BASE_FROZEN_SYSTEM_SENTINEL")
            .claude_md("FROZEN_CLAUDE_SENTINEL")
            .skill_summary("FROZEN_SKILLS_SENTINEL")
            .date("1999-12-31")
            .language(Some("zh-CN"))
            .meta_harness(meta)
            .build(),
        Some(Arc::from("FROZEN_LOCAL_SENTINEL")),
    )
}

fn make_stage_request(
    frozen_session: FrozenSessionData,
    agent_overrides: Option<peri_acp_types::agents::AgentOverrides>,
) -> StageBuildRequest {
    StageBuildRequest {
        cached_llm: None,
        frozen_session,
        event_handler: Arc::new(ParityFakeEventHandler),
        agent_overrides,
        preload_skills: vec![],
        child_handler_factory: None,
        auxiliary_model: None,
        thread_persistence: Default::default(),
        goal_controller: None,
        task_manager: None,
        on_bg_complete: None,
    }
}

#[cfg(not(windows))]
fn system_text(request: &ModelRequest) -> String {
    request.messages[0]
        .text_content()
        .expect("首条必须是 system message")
}

#[cfg(not(windows))]
fn frozen_with_dynamic_prompt_policy(
    base: &'static str,
    disabled_middlewares: &[&str],
) -> FrozenSessionData {
    use peri_acp_types::meta_harness::MetaHarnessState;
    use peri_agent::session::FrozenContext;

    FrozenSessionData::from_frozen_parts(
        FrozenContext::builder()
            .system_prompt(base)
            .claude_md("")
            .skill_summary("")
            .date("2026-08-25")
            .meta_harness(MetaHarnessState {
                disabled_middlewares: disabled_middlewares
                    .iter()
                    .map(|name| (*name).to_string())
                    .collect(),
                ..Default::default()
            })
            .build(),
        None,
    )
}

/// [回归测试] production 首个 Reason 必须看到同一 middleware chain 在
/// before_agent 后生成的 PTC 与 ToolSearch dynamic prompt contribution。
#[cfg(not(windows))]
#[tokio::test]
async fn test_production_first_reason_sees_after_before_agent_dynamic_contributions() {
    use peri_acp_types::session::{MessageKind, MessageSource, QueuedMessage};
    use peri_agent::agent::stages::{run_react_loop, LoopResult};

    let requests = Arc::new(Mutex::new(Vec::new()));
    let model = Arc::new(CapturePromptModel {
        requests: Arc::clone(&requests),
    }) as Arc<dyn Model>;
    let mut ctx = make_session_context("dynamic-first-reason");
    ctx.primary_llm_factory = Some(Arc::new(move || Arc::clone(&model)));
    let frozen = frozen_with_dynamic_prompt_policy("DYNAMIC_BASE_SENTINEL", &[]);
    let (out, _) = make_stage_build(&ctx)(make_stage_request(frozen, None));
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
    let ptc_offset = system
        .find("RunPtcCode programmatically executes")
        .expect("首个 Reason 缺少 PTC contribution");
    let tool_search_offset = system
        .find("## Deferred Tools")
        .expect("首个 Reason 缺少 ToolSearch contribution");
    assert!(ptc_offset < tool_search_offset, "{system}");
    assert_eq!(system.matches("DYNAMIC_BASE_SENTINEL").count(), 1);
    assert_eq!(system.matches("## Deferred Tools").count(), 1);
    let tool_search = &system[tool_search_offset..];
    assert_eq!(tool_search.matches("- RunPtcCode:").count(), 1);
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
    let ptc = &system[ptc_offset..tool_search_offset];
    assert_eq!(
        ptc.matches("RunPtcCode programmatically executes").count(),
        1
    );
    assert_eq!(ptc.matches("RPC-callable tool catalog").count(), 1);
    assert_eq!(
        &*parent.store().frozen.system_prompt,
        "DYNAMIC_BASE_SENTINEL"
    );
    assert!(!parent
        .store()
        .frozen
        .system_prompt
        .contains("## Deferred Tools"));
    assert!(!parent
        .store()
        .frozen
        .system_prompt
        .contains("RPC-callable tool catalog"));
}

/// [回归测试] fresh stage 必须从当前 filtered session-local 工具视图重算动态段，
/// 共享 ToolSearchIndex 不能泄漏上一 stage 的 PTC inventory/cache。
#[cfg(not(windows))]
#[tokio::test]
async fn test_production_fresh_stage_rebuild_recomputes_dynamic_contributions_from_disabled_view() {
    use peri_acp_types::ports::ToolSearchPort;
    use peri_acp_types::session::{MessageKind, MessageSource, QueuedMessage};
    use peri_agent::agent::stages::{run_react_loop, LoopResult};

    let requests = Arc::new(Mutex::new(Vec::new()));
    let model = Arc::new(CapturePromptModel {
        requests: Arc::clone(&requests),
    }) as Arc<dyn Model>;
    let shared_index = Arc::new(ToolSearchIndex::default());
    let mut first_ctx = make_session_context("dynamic-fresh-enabled");
    first_ctx.primary_llm_factory = {
        let model = Arc::clone(&model);
        Some(Arc::new(move || Arc::clone(&model)))
    };
    first_ctx.tool_search_index = Arc::clone(&shared_index) as Arc<dyn ToolSearchPort>;
    let first_frozen = frozen_with_dynamic_prompt_policy("DYNAMIC_FRESH_BASE_ONE", &[]);
    let (first, _) = make_stage_build(&first_ctx)(make_stage_request(first_frozen, None));
    let first_parent = Arc::clone(&first.session);
    first.context.session.queue.push(QueuedMessage::new(
        MessageKind::Prompt,
        MessageSource::UserInput,
        BaseMessage::human("first"),
    ));

    let first_result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        run_react_loop(first.context, 1),
    )
    .await
    .expect("第一 stage 不得挂起");

    assert!(matches!(first_result, LoopResult::Completed));
    {
        let captured = requests.lock().unwrap();
        assert_eq!(captured.len(), 1);
        let first_system = system_text(&captured[0]);
        assert!(first_system.contains("RunPtcCode programmatically executes"));
        let first_tool_search = first_system
            .find("## Deferred Tools")
            .map(|offset| &first_system[offset..])
            .expect("第一 stage 缺少 ToolSearch contribution");
        assert_eq!(first_tool_search.matches("- RunPtcCode:").count(), 1);
    }
    assert_eq!(
        &*first_parent.store().frozen.system_prompt,
        "DYNAMIC_FRESH_BASE_ONE"
    );
    drop(first_parent);
    drop(first.session);
    drop(first.event_handles);
    drop(first.todo_rx);
    drop(first.bg_event_rx);
    drop(first_ctx);

    let mut second_ctx = make_session_context("dynamic-fresh-disabled");
    second_ctx.primary_llm_factory = {
        let model = Arc::clone(&model);
        Some(Arc::new(move || Arc::clone(&model)))
    };
    second_ctx.tool_search_index = shared_index as Arc<dyn ToolSearchPort>;
    let second_frozen =
        frozen_with_dynamic_prompt_policy("DYNAMIC_FRESH_BASE_TWO", &["PtcMiddleware"]);
    let (second, _) = make_stage_build(&second_ctx)(make_stage_request(second_frozen, None));
    let second_parent = Arc::clone(&second.session);
    second.context.session.queue.push(QueuedMessage::new(
        MessageKind::Prompt,
        MessageSource::UserInput,
        BaseMessage::human("second"),
    ));

    let second_result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        run_react_loop(second.context, 1),
    )
    .await
    .expect("第二 stage 不得挂起");

    assert!(matches!(second_result, LoopResult::Completed));
    let captured = requests.lock().unwrap();
    assert_eq!(captured.len(), 2);
    let second_system = system_text(&captured[1]);
    assert_eq!(second_system.matches("DYNAMIC_FRESH_BASE_TWO").count(), 1);
    assert_eq!(second_system.matches("## Deferred Tools").count(), 1);
    assert!(second_system.contains("Discover deferred tools by name or keyword"));
    assert!(second_system.contains("Invoke a registered deferred tool by name"));
    assert!(!second_system.contains("RunPtcCode programmatically executes"));
    let second_tool_search = second_system
        .find("## Deferred Tools")
        .map(|offset| &second_system[offset..])
        .expect("第二 stage 缺少 ToolSearch contribution");
    assert!(!second_tool_search.contains("- RunPtcCode:"));
    assert!(!second_system.contains("RunPtcCode"));
    assert_eq!(
        &*second_parent.store().frozen.system_prompt,
        "DYNAMIC_FRESH_BASE_TWO"
    );
    assert!(!second_parent
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

    let mut ctx = make_session_context("frozen-production-parent");
    ctx.language = Some("en-US".into());
    let stage_build = make_stage_build(&ctx);
    let sentinel = make_sentinel_frozen();
    let (out, _) = stage_build(make_stage_request(sentinel.clone(), None));
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
            error_suggest_registry: None,
            tool_registry_snapshot: None,
            tool_invocation_resolver: None,
            compact_config: None,
            context_budget: None,
            compact_llm: None,
            thread_store: None,
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
    let mut ctx = make_session_context("frozen-language-override");
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
    ));

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
    let mut ctx = make_session_context("late-frozen-files");
    ctx.cwd = cwd.to_string_lossy().into_owned();
    ctx.primary_llm_factory = Some(Arc::new(move || Arc::clone(&model)));
    let stage_build = make_stage_build(&ctx);
    let (out, _) = stage_build(make_stage_request(frozen, None));
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
            error_suggest_registry: None,
            tool_registry_snapshot: None,
            tool_invocation_resolver: None,
            compact_config: None,
            context_budget: None,
            compact_llm: None,
            thread_store: None,
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

// ── run_session_loop: AsyncContinuation 内部续跑（非 keepgoing）─────────────

/// [AsyncContinuation] 内部续跑（continuation=true）不把空 user prompt 当
/// keepgoing：空历史 + 空 prompt 仍进入 agent 管线（绕过 keepgoing 空历史
/// short-circuit——后者会直接返回 ok=true/EndTurn）。
#[tokio::test]
async fn test_continuation_bypasses_keepgoing_short_circuit() {
    // Arrange：预取消 token，保证进入管线后快速中断（不触发真实 LLM 调用）
    let ctx = make_session_context("test-continuation");
    ctx.cancel.cancel();
    let stage_build = make_stage_build(&ctx);
    let mock_sink = Arc::new(MockEventSink::new());
    let turn = make_turn_input(
        Arc::clone(&mock_sink) as Arc<dyn EventSink>,
        MessageContent::text(""),
        true,
        vec![],
        stage_build,
    );

    // Act
    let result = run_session_loop(ctx, turn).await;

    // Assert：未走 keepgoing 短路（短路会返回 ok=true/EndTurn 且不构建 agent），
    // 而是进入管线后被预取消 token 中断（ok=false/Cancelled）。
    assert!(!result.ok, "continuation 不得走 keepgoing 空历史短路");
    assert_eq!(
        result.stop_reason,
        PromptStopReason::Cancelled,
        "进入管线后被预取消 token 中断"
    );
}

/// [Seam 2 / 验收⑤] turn 终态唯一 + terminal 事件位于 turn 全部输出之后。
///
/// §9 事件契约（docs/top-level.md）：terminal 事件必须位于该 turn 全部输出
/// 事件之后；turn 终态唯一（Completed 或 Interrupted）。本测试走预取消中断
/// 路径（Interrupted 终态）：断言 TurnStarted/TurnEnded 各恰好一次、
/// TurnEnded 是事件流最后一条且 status=Interrupted、协议出口 push_done
/// 恰好一次且 stop_reason=cancelled（与 TurnEnded 语义一致）。
#[tokio::test]
async fn test_turn_terminal_state_unique_and_last() {
    // Arrange：预取消 token，进入管线后立即中断（不触发真实 LLM 调用）
    let mock_sink = Arc::new(MockEventSink::new());
    let ctx = make_session_context("test-turn-terminal");
    ctx.cancel.cancel();
    let stage_build = make_stage_build(&ctx);
    let turn = make_turn_input(
        Arc::clone(&mock_sink) as Arc<dyn EventSink>,
        MessageContent::text(""),
        true,
        vec![],
        stage_build,
    );

    // Act
    let result = run_session_loop(ctx, turn).await;

    // Assert：终态唯一（Interrupted）
    assert!(!result.ok);
    assert_eq!(result.stop_reason, PromptStopReason::Cancelled);

    // terminal 事件唯一且位于全部输出之后
    let events = mock_sink.pushed_events.lock().unwrap();
    assert!(
        !events.is_empty(),
        "进入管线后应产生事件流（至少 TurnStarted + TurnEnded）"
    );
    let started = events
        .iter()
        .filter(|e| e.contains("\"turn_started\""))
        .count();
    let ended = events
        .iter()
        .filter(|e| e.contains("\"turn_ended\""))
        .count();
    assert_eq!(started, 1, "每个 turn 恰好一个 TurnStarted");
    assert_eq!(ended, 1, "每个 turn 恰好一个 terminal 事件（终态唯一）");
    let last = events.last().expect("事件流非空");
    assert!(
        last.contains("\"turn_ended\"") && last.contains("interrupted"),
        "terminal 事件必须位于该 turn 全部输出之后且 status=Interrupted: {last}"
    );
    drop(events);

    // 协议出口终态唯一，且与 TurnEnded 语义一致
    assert_eq!(
        mock_sink.push_done_count(),
        1,
        "终态信号（push_done）必须恰好一次"
    );
    assert_eq!(
        mock_sink
            .push_done_stop_reasons
            .lock()
            .unwrap()
            .last()
            .cloned(),
        Some("cancelled".to_string()),
        "push_done 终态与 TurnEnded(Interrupted) 语义一致"
    );
}

/// [回归测试] 取消发生在真实 ModelStream 已返回之后，ACP 仍必须
/// 对 PromptResult、TurnEnded 和 push_done 给出唯一且一致的 cancelled 终态。
#[cfg(not(windows))]
#[tokio::test]
async fn test_cancel_during_reason_has_one_interrupted_terminal() {
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let model: Arc<dyn Model> = Arc::new(CancelGateModel {
        entered: Mutex::new(Some(entered_tx)),
    });
    let mut ctx = make_session_context("test-cancel-during-reason");
    ctx.primary_llm_factory = Some(Arc::new(move || Arc::clone(&model)));
    let cancel = ctx.cancel.clone();
    let stage_build = make_stage_build(&ctx);
    let sink = Arc::new(MockEventSink::new());
    let turn = make_turn_input(
        Arc::clone(&sink) as Arc<dyn EventSink>,
        MessageContent::text("cancel while reasoning"),
        false,
        vec![],
        stage_build,
    );
    let task = tokio::spawn(async move { run_session_loop(ctx, turn).await });
    entered_rx.await.expect("primary model stream 必须已返回");

    cancel.cancel();
    let result = task.await.expect("session loop task 不得 panic");

    assert!(!result.ok);
    assert_eq!(result.stop_reason, PromptStopReason::Cancelled);
    assert!(result.failure.is_none(), "用户取消不得产生 fatal failure");
    let events: Vec<ExecutorEvent> = sink
        .pushed_events
        .lock()
        .unwrap()
        .iter()
        .map(|json| serde_json::from_str(json).unwrap())
        .collect();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ExecutorEvent::TurnStarted { .. }))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ExecutorEvent::TurnEnded { .. }))
            .count(),
        1
    );
    assert!(events.iter().any(|event| matches!(
        event,
        ExecutorEvent::TurnEnded {
            status: peri_acp_types::event::TurnStatus::Interrupted,
            error_kind: Some(peri_acp_types::event::TurnErrorKind::Interrupted),
            ..
        }
    )));
    assert!(!events
        .iter()
        .any(|event| matches!(event, ExecutorEvent::AgentExecutionFailed { .. })));
    assert_eq!(sink.push_done_count(), 1);
    assert_eq!(
        sink.push_done_stop_reasons.lock().unwrap().as_slice(),
        ["cancelled"]
    );
}

/// fatal 终态的兼容 failure 事件必须在唯一 TurnEnded 之前，
/// 且与 PromptResult 共用同一份脱敏文案；done 仅在事件泵排空后发送。
#[cfg(not(windows))]
#[tokio::test]
async fn test_fatal_failure_precedes_turn_end_and_done() {
    let model: Arc<dyn Model> = Arc::new(FatalModel);
    let mut ctx = make_session_context("test-fatal-terminal-order");
    ctx.primary_llm_factory = Some(Arc::new(move || Arc::clone(&model)));
    let stage_build = make_stage_build(&ctx);
    let sink = Arc::new(MockEventSink::new());
    let turn = make_turn_input(
        Arc::clone(&sink) as Arc<dyn EventSink>,
        MessageContent::text("trigger fatal model error"),
        false,
        vec![],
        stage_build,
    );

    let result = run_session_loop(ctx, turn).await;

    let failure = result.failure.expect("fatal model error 必须产生 failure");
    assert!(!failure.public_message.is_empty());
    assert!(!failure.public_message.contains("safe-request-id"));
    let operations = sink.operations.lock().unwrap();
    let failure_index = operations
        .iter()
        .position(|operation| operation.contains("agent_execution_failed"))
        .expect("必须发射 AgentExecutionFailed");
    let turn_end_index = operations
        .iter()
        .position(|operation| operation.contains("turn_ended"))
        .expect("必须发射 TurnEnded");
    let done_index = operations
        .iter()
        .position(|operation| operation == "done:end_turn")
        .expect("必须在 pump drain 后 push_done");
    assert!(failure_index < turn_end_index && turn_end_index < done_index);
    assert_eq!(
        operations
            .iter()
            .filter(|operation| operation.contains("turn_ended"))
            .count(),
        1
    );
    let failure_event: ExecutorEvent = serde_json::from_str(&operations[failure_index]).unwrap();
    match failure_event {
        ExecutorEvent::AgentExecutionFailed { message } => {
            assert_eq!(message, failure.public_message)
        }
        other => panic!("预期 AgentExecutionFailed，got: {other:?}"),
    }
    let turn_end: ExecutorEvent = serde_json::from_str(&operations[turn_end_index]).unwrap();
    assert!(matches!(
        turn_end,
        ExecutorEvent::TurnEnded {
            status: peri_acp_types::event::TurnStatus::Error,
            error_kind: Some(peri_acp_types::event::TurnErrorKind::LlmFailure),
            ..
        }
    ));
}

/// [AsyncContinuation] 内部续跑不写入空 human prompt：Phase 6 跳过 Prompt push，
/// v2 MessageQueue 不出现消息；对比 keepgoing（非空历史）会 push 一条空 Prompt。
#[tokio::test]
async fn test_continuation_skips_empty_prompt_push() {
    let tmp = tempfile::TempDir::new().unwrap();
    let session_id = "test-continuation-queue";
    let (ctx, sm) = make_session_context_with_manager(session_id, &tmp).await;
    ctx.cancel.cancel();
    let stage_build = make_stage_build(&ctx);
    let history = vec![BaseMessage::human("prior turn")];

    // Act 1：continuation=true（空 content + 非空历史）
    let turn = make_turn_input(
        Arc::new(MockEventSink::new()) as Arc<dyn EventSink>,
        MessageContent::text(""),
        true,
        history.clone(),
        stage_build.clone(),
    );
    let _ = run_session_loop(ctx, turn).await;

    // Assert 1：队列无任何消息（未写空 human）
    let queue = sm
        .get_session(session_id)
        .expect("session 应存在")
        .v2_message_queue
        .clone();
    assert!(
        queue.drain_all().is_empty(),
        "continuation 不得向 v2 queue 写入空 human prompt"
    );

    // Act 2：keepgoing（continuation=false，同为空 content）——对比组
    let mut ctx2 = make_session_context(session_id);
    ctx2.session_access =
        Some(Arc::new(sm.clone()) as Arc<dyn peri_acp_types::session::SessionAccessPort>);
    ctx2.cancel.cancel();
    let stage_build2 = make_stage_build(&ctx2);
    let turn2 = make_turn_input(
        Arc::new(MockEventSink::new()) as Arc<dyn EventSink>,
        MessageContent::text(""),
        false,
        history,
        stage_build2,
    );
    let _ = run_session_loop(ctx2, turn2).await;

    // Assert 2：keepgoing 会 push 一条 Prompt（空 human 由 stages 跳过转录）
    let drained = sm
        .get_session(session_id)
        .expect("session 应存在")
        .v2_message_queue
        .clone()
        .drain_all();
    assert_eq!(drained.len(), 1, "keepgoing 应 push 一条空 Prompt 消息");
    assert_eq!(
        drained[0].kind,
        peri_agent::session::queue::MessageKind::Prompt,
        "keepgoing push 的消息应为 Prompt kind"
    );
}

// ── FrozenSessionData 渲染测试（L5：渲染面留 ACP，经 build_frozen_data）───

/// 构造带 SkillsProvider 的 SessionManager（frozen 渲染输入）。
fn make_manager(tmp: &tempfile::TempDir) -> SessionManager {
    let thread_store = Arc::new(FilesystemThreadStore::new(tmp.path().join("threads")));
    let mut peri_config = PeriConfig::default();
    peri_config.config.active_alias = "sonnet".to_string();
    peri_config.config.providers = vec![ProviderConfig {
        id: "a".to_string(),
        provider_type: "openai".to_string(),
        api_key: "sk-test".to_string(),
        models: ProviderModels {
            sonnet: "gpt-4o".to_string(),
            ..Default::default()
        },
        ..Default::default()
    }];
    peri_config.config.profiles = Profiles {
        sonnet: ProfileConfig {
            provider: "a".to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    SessionManager::new(
        thread_store,
        LlmProvider::from_config(&peri_config).unwrap(),
        Arc::new(peri_config),
        SharedPermissionMode::new(PermissionMode::Bypass),
        None,
        None,
        None,
        None,
        None,
        Arc::new(SkillsProvider),
        Vec::new(), // plugin 命令条目（Phase 6 B2；测试无）
        Vec::new(), // plugin skill roots（C1；测试无）
    )
}

/// [回归测试] 同一 frozen 输入必须产生字节相同的 system prompt。
///
/// 历史背景（ARC-FROZEN-001）：system prompt 在 session/new 时一次性冻结；
/// 相同会话输入（cwd/language/skill roots/date/permission mode）若因调用方
/// 上下文差异产生不同前缀，会破坏 Anthropic 前缀缓存，并使主 agent 与
/// subagent 看到不一致的策略。本测试固定全部输入，验证
/// `build_frozen_data` 是确定性的。
///
/// `#[serial]`：`build_frozen_data` 扫描用户级 `~/.claude/skills`（HOME），
/// 与 requests_test 的 `#[serial]` 组（B3 用例重定向 HOME）互斥，防止两次
/// build 间读到不同 HOME 快照。
#[tokio::test]
#[serial]
async fn test_frozen_session_data_build_is_deterministic() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_manager(&tmp);
    let cwd = "/tmp";

    let a = mgr.build_frozen_data(cwd, &[], &[]);
    let b = mgr.build_frozen_data(cwd, &[], &[]);

    assert_eq!(
        a.system_prompt(),
        b.system_prompt(),
        "相同 frozen 输入两次 build 应产生相同 system prompt"
    );
    assert_eq!(
        a.skill_summary(),
        b.skill_summary(),
        "相同 frozen 输入两次 build 应产生相同 skill 摘要"
    );
}

/// [回归测试] 已冻结的 system prompt 与 skill 摘要不受会话中途磁盘变化影响。
///
/// 历史背景（ARC-FROZEN-001 / 审计 prompt-sections-audit.md P2-11）：skill
/// 摘要与 system prompt 在 session/new 冻结；会话内磁盘 skill 增删不得改变
/// 已冻结产物（冻结是前缀缓存稳定性的有意权衡，不能按需重扫）。
///
/// `#[serial]`：与 requests_test 的 `#[serial]` 组（B3 用例重定向 HOME）
/// 互斥，防止冻结前后两次扫描读到不同用户级 skills 快照。
#[tokio::test]
#[serial]
async fn test_frozen_system_prompt_immune_to_disk_changes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_manager(&tmp);
    let cwd = tmp.path().to_str().unwrap();

    // 冻结前：cwd 含 skill-a
    let skills_dir_a = tmp.path().join(".claude").join("skills").join("skill-a");
    std::fs::create_dir_all(&skills_dir_a).unwrap();
    std::fs::write(
        skills_dir_a.join("SKILL.md"),
        "---\nname: 'skill-a'\ndescription: 'A test skill'\n---\n\nbody",
    )
    .unwrap();

    let frozen = mgr.build_frozen_data(cwd, &[], &[]);

    let frozen_prompt = frozen.system_prompt().to_string();
    let frozen_summary = frozen.skill_summary().map(|s| s.to_string());
    assert!(
        frozen_summary.as_deref().unwrap_or("").contains("skill-a"),
        "冻结摘要应包含冻结时的 skill-a"
    );

    // 会话中途：删除 skill-a，新增 skill-b 与 CLAUDE.md
    std::fs::remove_dir_all(&skills_dir_a).unwrap();
    let skills_dir_b = tmp.path().join(".claude").join("skills").join("skill-b");
    std::fs::create_dir_all(&skills_dir_b).unwrap();
    std::fs::write(
        skills_dir_b.join("SKILL.md"),
        "---\nname: 'skill-b'\ndescription: 'B test skill'\n---\n\nbody",
    )
    .unwrap();
    std::fs::write(tmp.path().join("CLAUDE.md"), "# New CLAUDE.md").unwrap();

    // 已冻结产物不变（不按需重读磁盘）
    assert_eq!(
        frozen.system_prompt(),
        frozen_prompt,
        "已冻结 system prompt 不应受会话中途磁盘变化影响"
    );
    assert_eq!(
        frozen.skill_summary().map(|s| s.to_string()),
        frozen_summary,
        "已冻结 skill 摘要不应随磁盘重扫"
    );
}

/// [回归测试] 16_workflow 已整段删除（波 4 演进 C2）：冻结 prompt 恒不
/// 声明 Workflow（ultracode skill 完整承载 WorkflowTool 指引，设计 §3.1.2）；
/// `build_frozen_data` 的 workflow_enabled 参数随 gate 清理删除。
#[tokio::test]
async fn test_frozen_prompt_never_claims_workflow() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_manager(&tmp);
    let cwd = "/tmp";

    let frozen = mgr.build_frozen_data(cwd, &[], &[]);

    assert!(
        !frozen.system_prompt().contains("Workflow Orchestration"),
        "16_workflow 段落已删除：冻结 prompt 不得声明 Workflow"
    );
}

/// [回归测试] 子 agent / fork / workflow agent 复用的冻结 prompt 与主
/// prompt 字节相同（16_workflow 已删除，无子面向 feature 差异）。
#[tokio::test]
async fn test_frozen_subagent_prompt_identical_to_main() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_manager(&tmp);
    let cwd = "/tmp";

    let frozen = mgr.build_frozen_data(cwd, &[], &[]);

    assert!(
        !frozen.system_prompt().contains("Workflow Orchestration"),
        "16_workflow 段落已删除：冻结 prompt 不得声明 Workflow"
    );
    // 子面向 prompt 字段已随 C5 移除：子 agent / fork / workflow agent
    // 直接复用主冻结 prompt（16_workflow 删除后两版字节相同的语义固化）。
    assert!(
        !frozen.system_prompt().is_empty(),
        "主冻结 prompt 非空（子面向唯一复用来源）"
    );
}

/// [回归测试] advisor 裁决 B（2026-08-14）：workflow agent 链不装配审批
/// middleware（broker: None → PermissionMiddleware::disabled()），10_hitl
/// 描述的是主会话审批机制，对 workflow 模型是误导性指令——workflow 渲染
/// 路径（fallback + agentType builder）必须排除 10_hitl，兑现
/// presence-is-the-gate 契约（C3 D5 决策修订，design §3.1.1 契约 3 /
/// §3.5 语义边界；2026-08-15 拆分：10_hitl 持有者改为 PermissionMiddleware，
/// 过滤目标段落不变）。
#[tokio::test]
async fn test_workflow_prompt_excludes_hitl_section() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_manager(&tmp);
    let frozen = mgr.build_frozen_data("/tmp", &[], &[]);

    // 主链冻结 prompt 保留 10_hitl（PermissionMiddleware 默认装配）
    assert!(
        frozen.system_prompt().contains("Human-in-the-Loop (HITL)"),
        "主链冻结 prompt 应保留 10_hitl（PermissionMiddleware 默认装配）"
    );

    let skills: Arc<dyn peri_acp_types::ports::SkillsPort> = Arc::new(SkillsProvider);
    let fallback = crate::host::workflow_agent::build_workflow_system_prompt_fallback(
        Arc::clone(&skills),
        frozen.meta_harness().clone(),
    );
    let prompt = fallback("/tmp", Some("2026-01-01"), frozen.language());
    assert!(
        !prompt.contains("Human-in-the-Loop (HITL)"),
        "workflow 链无审批 middleware：提示词不得包含 10_hitl"
    );

    // agentType builder（workflow 子 agent）同样排除
    let builder = crate::host::workflow_agent::build_workflow_agent_prompt_builder(
        Arc::clone(&skills),
        frozen.meta_harness().clone(),
    );
    let agent_prompt = builder(None, "/tmp", Some("2026-01-01"), frozen.language());
    assert!(
        !agent_prompt.contains("Human-in-the-Loop (HITL)"),
        "workflow agentType builder 同样不得包含 10_hitl"
    );
}

// ── P2-1（实施质量审查）：链收集 vs 渲染面静态声明直接对拍 ─────────────────

/// 最小 ReactLLM fake（装配路径不调用 LLM）。
struct ParityFakeLlm;

#[async_trait]
impl peri_agent::agent::react::ReactLLM for ParityFakeLlm {
    async fn generate_reasoning(
        &self,
        _messages: &[peri_agent::messages::BaseMessage],
        _tools: &[&dyn peri_agent::tools::BaseTool],
        _streaming: Option<peri_agent::agent::react::StreamingContext>,
    ) -> peri_agent::error::AgentResult<peri_agent::agent::react::Reasoning> {
        unimplemented!("对拍测试不调用 LLM")
    }
}

/// 最小 Model fake（HITL auto-classifier 构造消费，不调用）。
struct ParityFakeModel;

#[async_trait]
impl peri_model::Model for ParityFakeModel {
    fn capabilities(&self) -> peri_model::ModelCapabilities {
        peri_model::ModelCapabilities {
            supports_tools: false,
            supports_reasoning: false,
            supports_vision: false,
            supports_streaming: true,
        }
    }

    async fn stream(
        &self,
        _request: peri_model::ModelRequest,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> peri_model::ModelResult<peri_model::ModelStream> {
        unimplemented!("对拍测试不调用模型")
    }
}

/// 最小 AgentEventHandler fake。
struct ParityFakeEventHandler;

impl peri_agent::agent::events::AgentEventHandler for ParityFakeEventHandler {
    fn on_event(&self, _event: peri_agent::agent::events::ExecutorEvent) {}
}

/// 构造最小 AssemblyContext（复刻 peri-middlewares assembly_test base_context
/// 的段落持有者相关字段；条件注册字段全部关闭——对拍只关心持有者槽位）。
fn make_parity_context(
    disabled: &[&str],
    overrides: Option<peri_acp_types::agents::AgentOverrides>,
    language: Option<String>,
) -> peri_agent::session::factory::AssemblyContext {
    use std::collections::BTreeMap;

    use parking_lot::RwLock;
    use peri_acp_types::tools::TodoItem;
    use peri_agent::{
        agent::{async_tasks::TaskManager, AgentCancellationToken},
        tools::BaseTool,
    };

    let (todo_tx, _todo_rx) = tokio::sync::mpsc::channel::<Vec<TodoItem>>(8);
    let (bg_event_tx, _bg_rx) = tokio::sync::mpsc::unbounded_channel::<ExecutorEvent>();
    let shared_tools: Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>> =
        Arc::new(RwLock::new(BTreeMap::new()));
    let llm_factory = Arc::new(|_model_alias: Option<&str>| {
        Box::new(ParityFakeLlm) as Box<dyn peri_agent::agent::react::ReactLLM + Send + Sync>
    });

    peri_agent::session::factory::AssemblyContext {
        cwd: "/tmp/parity-test".to_string(),
        cancel: AgentCancellationToken::new(),
        broker: Arc::new(NoopBroker),
        permission_mode: SharedPermissionMode::new(PermissionMode::Default),
        model_name: "parity-model".to_string(),
        provider_name: "parity-provider".to_string(),
        auxiliary_model: None,
        auto_classifier_model: Arc::new(tokio::sync::Mutex::new(
            Box::new(ParityFakeModel) as Box<dyn peri_model::Model>
        )),
        claude_md_excludes: Vec::new(),
        preload_skills: Vec::new(),
        plugin_skill_roots: Vec::new(),
        plugin_loaded: Vec::new(),
        hook_groups: Vec::new(),
        session_start_source: None,
        mcp_skill_registry: None,
        command_registry: None,
        cron_scheduler: None,
        mcp_pool: None,
        dynamic_mcp: None,
        dynamic_mcp_projection: Arc::new(parking_lot::Mutex::new(None)),
        session_id: "session-parity-test".to_string(),
        channel_state: None,
        tool_search_index: Arc::new(ToolSearchIndex::new()),
        shared_tools,
        lsp_servers: Vec::new(),
        lsp_pool: None,
        workflow_executor: None,
        workflow_middleware: None,
        event_handler: Arc::new(ParityFakeEventHandler),
        task_manager: Arc::new(TaskManager::new()),
        bg_event_tx,
        on_bg_complete: None,
        langfuse_bridge: None,
        thread_store: None,
        parent_thread_id: None,
        register_runtime: None,
        deregister_runtime: None,
        child_handler_factory: None,
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        system_prompt_for_sub: String::new(),
        llm_factory,
        system_builder: Arc::new(
            |_ov: Option<&peri_acp_types::agents::AgentOverrides>, _cwd: &str| String::new(),
        ),
        todo_tx,
        goal_controller: None,
        meta_harness_disabled: disabled.iter().map(|s| s.to_string()).collect(),
        agent_overrides: overrides,
        language,
    }
}

/// P2-1（实施质量审查）：链收集与渲染面静态声明**直接对拍**——同一 disabled
/// 状态 / overrides / language 下，真实装配链的 `collect_prompt_sections` 与
/// 渲染面 `build_collected_sections` 段落 (id, zone, order, 内容) 集合相等。
///
/// 锁定不变式：「5 个段落持有者的装配条件全部只按 disabled 集合过滤」。
/// 若未来装配条件因非 disabled 原因排除某持有者（链收集少段而静态声明仍
/// 收集），本测试立即失败——冻结提示词与链状态静默失同步的前哨。
#[test]
fn chain_collection_parity_with_build_collected_sections() {
    use peri_acp_types::agents::AgentOverrides;
    use peri_acp_types::meta_harness::MetaHarnessState;
    use peri_agent::session::factory::build_middleware_chain;
    use peri_middlewares::assembly::ProductionChainAssembler;

    let cases = &[
        ("默认装配", None, None, &[] as &[&str]),
        (
            "关闭 gated 持有者",
            None,
            None,
            &[
                "PermissionMiddleware",
                "HumanInTheLoopMiddleware",
                "SubAgentMiddleware",
                "SkillsMiddleware",
            ],
        ),
        (
            "关闭基础持有者",
            None,
            None,
            &["DefaultSystemPromptMiddleware", "LangMiddleware"],
        ),
        (
            "关闭全部持有者",
            None,
            None,
            &[
                "DefaultSystemPromptMiddleware",
                "LangMiddleware",
                "PermissionMiddleware",
                "HumanInTheLoopMiddleware",
                "SubAgentMiddleware",
                "SkillsMiddleware",
            ],
        ),
        (
            "persona + 语言注入",
            Some(AgentOverrides {
                persona: Some("parity persona".into()),
                tone: Some("parity tone".into()),
                proactiveness: None,
                mode: None,
            }),
            Some("zh-CN".to_string()),
            &[],
        ),
    ];

    for (name, overrides, language, disabled) in cases {
        let ctx = make_parity_context(disabled, overrides.clone(), language.clone());
        let out = build_middleware_chain(&ProductionChainAssembler, &ctx);
        let mut chain_sections: Vec<(String, u16, u16, String)> = out
            .chain
            .collect_prompt_sections()
            .into_iter()
            .map(|s| {
                (
                    s.id.to_string(),
                    s.zone as u16,
                    s.order,
                    s.content.as_str().to_string(),
                )
            })
            .collect();

        let state = MetaHarnessState {
            disabled_middlewares: disabled.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        let mut declared: Vec<(String, u16, u16, String)> =
            crate::session::build_collected_sections(
                &state,
                overrides.as_ref(),
                language.as_deref(),
            )
            .into_iter()
            .map(|s| {
                (
                    s.id.to_string(),
                    s.zone as u16,
                    s.order,
                    s.content.as_str().to_string(),
                )
            })
            .collect();

        // 集合对拍（契约 2：收集不承诺顺序——链收集按 middleware 链序、
        // 静态声明按持有者声明序，渲染面统一按 (zone, order) 排序；此处
        // 只锁定「同状态下收集到的段落集合相等」，顺序由渲染面位置属性
        // 测试独立锁定）。
        chain_sections.sort();
        declared.sort();
        assert_eq!(
            chain_sections, declared,
            "case [{name}]：链收集与静态声明必须一致（同一 disabled 状态）"
        );
    }
}

// ── PTC production-path E2E ────────────────────────────────────────────────

#[cfg(not(windows))]
struct PtcScriptedModel {
    calls: AtomicUsize,
    visible_tools: Arc<Mutex<Vec<String>>>,
    source: String,
}

#[cfg(not(windows))]
#[async_trait]
impl Model for PtcScriptedModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_tools: true,
            supports_reasoning: false,
            supports_vision: false,
            supports_streaming: true,
        }
    }

    async fn stream(
        &self,
        request: ModelRequest,
        cancellation: AgentCancellationToken,
    ) -> ModelResult<ModelStream> {
        *self.visible_tools.lock().unwrap() =
            request.tools.iter().map(|tool| tool.name.clone()).collect();
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let (message, stop_reason, mut events) = if call < 2 {
            let (name, arguments) = if call == 0 {
                (
                    "SearchExtraTools",
                    serde_json::json!({ "query": "ptc run code 程序化 批量" }),
                )
            } else {
                (
                    "ExecuteExtraTool",
                    serde_json::json!({
                        "tool_name": "RunPtcCode",
                        "params": { "source": self.source }
                    }),
                )
            };
            let tool_call = ToolCall::new(
                "ptc-e2e-outer",
                name,
                JsonObject::from_value(arguments.clone()).unwrap(),
            );
            (
                ModelMessage::assistant(vec![], vec![tool_call]),
                StopReason::ToolUse,
                vec![ModelStreamEvent::ToolCallDelta {
                    index: 0,
                    id: Some("ptc-e2e-outer".into()),
                    name: Some(name.into()),
                    arguments_delta: arguments.to_string(),
                }],
            )
        } else {
            (
                ModelMessage::assistant_text("PTC E2E complete"),
                StopReason::EndTurn,
                vec![ModelStreamEvent::TextDelta {
                    text: "PTC E2E complete".into(),
                }],
            )
        };
        let response = ModelResponse::new(message, stop_reason, None, None)?;
        events.push(ModelStreamEvent::Completed(response));
        Ok(ModelStream::with_parent_cancellation(
            stream::iter(events.into_iter().map(Ok)),
            cancellation,
        ))
    }
}

#[cfg(not(windows))]
struct RecordingApproveBroker {
    approvals: Arc<Mutex<Vec<String>>>,
}

#[cfg(not(windows))]
#[async_trait]
impl UserInteractionBroker for RecordingApproveBroker {
    async fn request(&self, ctx: InteractionContext) -> InteractionResponse {
        match ctx {
            InteractionContext::Approval { items } => {
                self.approvals
                    .lock()
                    .unwrap()
                    .extend(items.iter().map(|item| item.tool_name.clone()));
                InteractionResponse::Decisions(
                    items
                        .iter()
                        .map(|_| peri_acp_types::interaction::ApprovalDecision::Approve {
                            source: None,
                        })
                        .collect(),
                )
            }
            _ => InteractionResponse::Rejected,
        }
    }
}

#[cfg(not(windows))]
fn write_ptc_cache_fixture(root: &std::path::Path) {
    let package = root.join(".peri/ptc/0.2.3/node_modules/@peri-code/ptc");
    std::fs::create_dir_all(package.join("dist")).unwrap();
    std::fs::write(
        package.join("package.json"),
        r#"{"name":"@peri-code/ptc","version":"0.2.3","type":"module","main":"dist/index.js","bin":{"peri-ptc":"dist/peri-ptc.js"},"periProtocolVersion":1,"periBuildId":"@peri-code/ptc@0.2.3"}"#,
    )
    .unwrap();
    std::fs::write(package.join("dist/index.js"), "export {};\n").unwrap();
    std::fs::write(
        package.join("dist/peri-ptc.js"),
        r#"import readline from 'node:readline';
const pending = new Map();
let nextId = 100;
function send(message) { process.stdout.write(JSON.stringify(message) + '\n'); }
function callTool(toolName, input, options = {}) {
  if (options.signal?.aborted) return Promise.reject(Object.assign(new Error('cancelled'), { name: 'AbortError' }));
  const id = nextId++;
  const invocationId = `ptc-${id}`;
  send({ jsonrpc: '2.0', id, method: 'tool/call', params: { invocationId, toolName, input } });
  return new Promise((resolve, reject) => pending.set(id, { resolve, reject }));
}
const tools = new Proxy({}, { get: (_, toolName) => (input, options) => callTool(toolName, input, options) });
const AsyncFunction = Object.getPrototypeOf(async function() {}).constructor;
const rl = readline.createInterface({ input: process.stdin });
rl.on('line', async line => {
  const request = JSON.parse(line);
  if (request.method === 'ptc/start') {
    send({ jsonrpc: '2.0', id: request.id, result: { ok: true, protocolVersion: 1, buildId: '@peri-code/ptc@0.2.3' } });
  } else if (request.method === 'execute') {
    try {
      const logs = [];
      const console = { log: (...values) => logs.push(values.join(' ')) };
      const result = await new AsyncFunction('tools', 'input', 'console', request.params.source)(tools, request.params.input, console);
      send({ jsonrpc: '2.0', id: request.id, result: { value: result, logs } });
    } catch (error) {
      send({ jsonrpc: '2.0', id: request.id, error: { code: -32001, message: 'JavaScript execution failed', data: { code: error.code ?? 'EXECUTION_FAILED' } } });
    }
  } else if (Object.hasOwn(request, 'id')) {
    const waiter = pending.get(request.id);
    if (!waiter) return;
    pending.delete(request.id);
    if (request.error) {
      const error = Object.assign(new Error(request.error.message), request.error.data ?? {});
      error.name = 'ToolCallError';
      waiter.reject(error);
    } else waiter.resolve(request.result);
  }
});
"#,
    )
    .unwrap();
}

#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn test_ptc_runs_through_acp_session_agent_production_path() {
    let tmp = tempfile::tempdir().unwrap();
    let ptc_cache = tempfile::tempdir().unwrap();
    write_ptc_cache_fixture(ptc_cache.path());
    let _home = HomeGuard::set(ptc_cache.path());
    std::fs::write(tmp.path().join("a.txt"), "alpha").unwrap();
    std::fs::write(tmp.path().join("b.txt"), "beta").unwrap();
    let a = serde_json::to_string(&tmp.path().join("a.txt").to_string_lossy()).unwrap();
    let b = serde_json::to_string(&tmp.path().join("b.txt").to_string_lossy()).unwrap();
    let source = format!(
        r#"const [a, b] = await Promise.all([
            tools.Read({{ file_path: {a} }}),
            tools.Read({{ file_path: {b} }})
        ]);
        let structured;
        try {{ await tools.NoSuchPtcTool({{}}); }}
        catch (error) {{ structured = {{ name: error.name, code: error.code }}; }}
        const controller = new AbortController(); controller.abort();
        let cancelled;
        try {{ await tools.Read({{ file_path: {a} }}, {{ signal: controller.signal }}); }}
        catch (error) {{ cancelled = error.name; }}
        return {{ a, b, structured, cancelled }};"#
    );
    let visible_tools = Arc::new(Mutex::new(Vec::new()));
    let model = Arc::new(PtcScriptedModel {
        calls: AtomicUsize::new(0),
        visible_tools: Arc::clone(&visible_tools),
        source,
    }) as Arc<dyn Model>;
    let approvals = Arc::new(Mutex::new(Vec::new()));
    let mut ctx = make_session_context("ptc-production-e2e");
    ctx.cwd = tmp.path().to_string_lossy().into_owned();
    ctx.permission_mode = SharedPermissionMode::new(PermissionMode::Default);
    ctx.broker = Arc::new(RecordingApproveBroker {
        approvals: Arc::clone(&approvals),
    });
    ctx.primary_llm_factory = Some(Arc::new(move || Arc::clone(&model)));
    let stage_build = make_stage_build(&ctx);
    let sink = Arc::new(MockEventSink::new());
    let turn = make_turn_input(
        Arc::clone(&sink) as Arc<dyn EventSink>,
        MessageContent::text("run the scripted PTC scenario"),
        false,
        vec![],
        stage_build,
    );

    let result = run_session_loop(ctx, turn).await;

    assert!(
        result.ok,
        "PTC production path failed: stop_reason={:?}",
        result.stop_reason
    );
    let tools = visible_tools.lock().unwrap();
    assert!(tools.iter().any(|name| name == "ExecuteExtraTool"));
    assert!(tools.iter().any(|name| name == "SearchExtraTools"));
    assert!(!tools.iter().any(|name| name == "RunPtcCode"));
    assert!(!tools.iter().any(|name| name == "run_code"));
    assert_eq!(approvals.lock().unwrap().as_slice(), ["RunPtcCode"]);
    let events = sink.pushed_events.lock().unwrap().join("\n");
    assert!(events.contains("ptc-e2e-outer/ptc-"), "{events}");
    assert!(events.contains("RunPtcCode"), "{events}");
    assert!(events.contains("UNKNOWN_TOOL"), "{events}");
    assert!(
        events.contains("alpha") && events.contains("beta"),
        "{events}"
    );
}
