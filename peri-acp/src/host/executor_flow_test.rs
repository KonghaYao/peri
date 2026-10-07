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
};
use peri_agent::session::exec::executor_helpers::{
    ForwarderLauncherFn, StageBuildFn, StageBuildRequest,
};
use serial_test::serial;
use tokio_util::sync::CancellationToken as AgentCancellationToken;

use crate::session::executor::{
    AutoClassifierFactory, FrozenSessionData, PromptStopReason, SessionContext, SubagentLlmFactory,
    TurnInput,
};
#[path = "execution_fixture_test.rs"]
pub(super) mod execution_fixture;
use crate::{
    provider::{LlmProvider, PeriConfig, ProfileConfig, Profiles, ProviderConfig, ProviderModels},
    session::{agent_pool::AgentPool, event_sink::EventSink, SessionManager},
};
pub(super) use execution_fixture::run_session_loop;
use peri_middlewares::{host_ports::AgentCatalogProvider, tool_search::ToolSearchIndex};
#[cfg(not(windows))]
use peri_model::{
    Model, ModelCapabilities, ModelMessage, ModelRequest, ModelResponse, ModelResult, ModelStream,
    ModelStreamEvent, StopReason, TokenUsage,
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
        let lock = HOME_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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
///
/// `pub(super)`：`host::mcp_v4_startup_tests`（B-07 的 MCP 启动准入用例）复用同一
/// 观测面，避免测试各自维护一套事件顺序断言（字段仍私有，只经访问器暴露快照）。
pub(super) struct MockEventSink {
    push_done_count: Mutex<usize>,
    push_done_stop_reasons: Mutex<Vec<String>>,
    pushed_events: Mutex<Vec<String>>,
    operations: Mutex<Vec<String>>,
}

impl MockEventSink {
    pub(super) fn new() -> Self {
        Self {
            push_done_count: Mutex::new(0),
            push_done_stop_reasons: Mutex::new(Vec::new()),
            pushed_events: Mutex::new(Vec::new()),
            operations: Mutex::new(Vec::new()),
        }
    }

    pub(super) fn push_done_count(&self) -> usize {
        *self.push_done_count.lock().unwrap()
    }

    /// 事件流快照（与 `push_done` 交错记录，用于断言 terminal 顺序）。
    /// 调用方都在 `host::mcp_v4_startup_tests`（unix 用例），Windows 上没有使用者。
    #[cfg_attr(windows, allow(dead_code))]
    pub(super) fn operations(&self) -> Vec<String> {
        self.operations.lock().unwrap().clone()
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
struct UsageModel;

#[cfg(not(windows))]
#[async_trait]
impl Model for UsageModel {
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
        let response = ModelResponse::new(
            ModelMessage::assistant_text("done"),
            StopReason::EndTurn,
            Some(TokenUsage {
                input_tokens: 104_478,
                output_tokens: 1,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: Some(70_000),
            }),
            Some("chatcmpl-barrier".into()),
        )?;
        Ok(ModelStream::with_parent_cancellation(
            stream::iter(vec![Ok(ModelStreamEvent::Completed(response))]),
            cancellation,
        ))
    }
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

/// 构造可靠 Store 与当前运行态的 SessionContext（stage 装配桥经
/// 真实 ACP 桥注入——与生产 host/prompt.rs 同模式；LLM 工厂从测试
/// LlmProvider + AgentPool 烘焙，装配路径实际调用）。
///
/// `pub(super)`：`host::mcp_v4_startup_tests` 复用同一装配面后按需注入 MCP pool。
pub(super) async fn make_session_context(session_id: &str) -> SessionContext {
    // 事件广播宿主：发射端（EventPublisher 适配）与订阅端（subscribe 工厂）
    // 共享同一 Controller 实例，保持迁移前「publish/subscribe 同一广播」语义。
    // Controller 与历史持久化共享同一次打开的真实 SQLite 门面。
    let (resources, directory) = execution_fixture::new_resources(session_id).await;
    let controller = Arc::new(peri_controller::Controller::new(resources.clone()));
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

    let mut context = SessionContext {
        cwd: "/tmp".to_string(),
        provider_name: "OpenAI:gpt-4o".to_string(),
        provider_model_name: "gpt-4o".to_string(),
        provider_fp: "openai:gpt-4o".to_string(),
        effective_context_window: 200_000,
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
        session_resources: Some(resources),
        thread_id: Some(session_id.into()),
        plugin_skill_roots: vec![],
        plugin_loaded: vec![],
        hook_groups: vec![],
        cron_scheduler: None,
        mcp_pool: None,
        dynamic_mcp: None,
        session_mcp_capability: None,
        dynamic_mcp_projection: Arc::new(parking_lot::Mutex::new(None)),
        tool_search_index: Arc::new(ToolSearchIndex::default()),
        shared_tools: Arc::new(parking_lot::RwLock::new(Default::default())),
        workflow_executor: None,
        agent_catalog: Arc::new(AgentCatalogProvider::new()),
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
        user_input_mailbox: None,
        frozen_fallback_builder: None,
    };
    execution_fixture::initialize_runtime(&mut context, Some(directory));
    context
}

/// 夹具的会话任务管理器登记：生产由 host 请求路径（`bind_session_tasks`，
/// `peri-acp/src/host/requests.rs`）在 `session/new` 等入口把会话 `TaskManager`
/// 绑到 pool；直连 `run_session_loop` 的夹具绕过了该入口，必须在此复刻同一步——
/// 否则每个 MCP 工具调用都会被 `begin_external_task_execution` 以
/// "session task manager unavailable" 拒绝（调用必须先进入会话任务范围）。
///
/// pool 只存 `Weak`，因此强引用由夹具持有到用例结束（随夹具析构释放）；
/// 未绑定时 `begin_external_execution` 的守卫语义不变。
#[derive(Default)]
pub(super) struct SessionTaskBindings {
    managers: Mutex<Vec<Arc<dyn peri_acp_types::tasks::TaskManager>>>,
}

impl SessionTaskBindings {
    /// 把 executor 持有的真实 `TaskManager` 绑定到 `pool`。
    pub(super) fn bind(&self, pool: &peri_middlewares::mcp::McpClientPool, ctx: &SessionContext) {
        let manager = ctx
            .session_access
            .as_ref()
            .and_then(|access| access.task_manager(&ctx.session_id))
            .expect("fixture must bind the actual executor task owner");
        pool.bind_session_task_manager(&ctx.session_id, &manager);
        self.managers
            .lock()
            .expect("夹具任务管理器登记不得中毒")
            .push(manager);
    }
}

/// 构造带真实 SessionManager + 已登记 session 的 SessionContext
///（可观察 v2 MessageQueue；stage 装配桥 + forwarder 真实注入）。
async fn make_session_context_with_manager(
    session_id: &str,
    tmp: &tempfile::TempDir,
) -> (SessionContext, SessionManager) {
    let mut ctx = make_session_context(session_id).await;
    let session_resources =
        peri_agent::resources::open_session_resources_with(Some(tmp.path().join("threads.db")))
            .await
            .unwrap();
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
        session_resources,
        LlmProvider::from_config(&peri_config).unwrap(),
        Arc::new(peri_config),
        SharedPermissionMode::new(PermissionMode::Bypass),
        None,
        None,
        None, // MCP 订阅端口（测试无）
        None, // Dynamic MCP（测试无）
        None, // 无 bg 场景：fallback NoopTaskManager
        Arc::new(AgentCatalogProvider::new()),
        Vec::new(), // plugin 命令条目（Phase 6 B2；测试无）
    );
    sm.new_session_with_id(session_id, "/tmp")
        .await
        .expect("session 登记失败");
    ctx.session_access =
        Some(Arc::new(sm.clone()) as Arc<dyn peri_acp_types::session::SessionAccessPort>);
    ctx.session_resources = Some(sm.session_resources().clone());
    execution_fixture::initialize_runtime(&mut ctx, None);
    (ctx, sm)
}

/// 构造 stage 装配桥（真实 ACP 桥，与生产 host/prompt.rs 同模式：ZST
/// ProductionChainAssembler + build_compact_hooks（测试 ctx hook_groups 为空
/// → (None, None)）；测试无 Langfuse → bridge factory None）。
pub(super) fn make_stage_build(ctx: &SessionContext) -> StageBuildFn {
    let ctx_for_stage = ctx.clone();
    Arc::new(move |sbr| {
        let (compact_pre_hook, compact_post_hook) = crate::host::prompt::build_compact_hooks(
            &ctx_for_stage.hook_groups,
            &ctx_for_stage.cwd,
            &ctx_for_stage.session_id,
            &ctx_for_stage.provider_model_name,
            None,
            None,
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
        crate::event::spawn_eventbus_forwarder(handles, on_event, None)
    })
}

#[cfg(not(windows))]
fn make_gated_forwarder_launcher(
    reached: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
) -> ForwarderLauncherFn {
    let reached = Arc::new(Mutex::new(Some(reached)));
    let release = Arc::new(Mutex::new(Some(release)));
    Arc::new(move |mut handles, _agent_id, on_event| {
        let reached = reached.lock().unwrap().take();
        let release = release.lock().unwrap().take();
        tokio::spawn(async move {
            let mut reached = reached;
            let mut release = release;
            loop {
                tokio::select! {
                    biased;
                    Some(event) = handles.render_rx.recv() => {
                        if let Some(exec) = peri_acp_types::event_v2::render_event_to_executor(event.clone()) {
                            on_event(
                                peri_acp_types::runtime::UnstampedEvent::new(
                                    event.turn_id().to_string(),
                                    event.agent_id().to_string(),
                                    None,
                                    peri_acp_types::identity::EventDeliveryClass::Critical,
                                ),
                                exec,
                            );
                        }
                    }
                    Some(event) = handles.state_rx.recv() => {
                        if let Some(exec) = peri_acp_types::event_v2::state_event_to_executor(event.clone()) {
                            on_event(
                                peri_acp_types::runtime::UnstampedEvent::new(
                                    event.turn_id().to_string(),
                                    event.agent_id().to_string(),
                                    None,
                                    peri_acp_types::identity::EventDeliveryClass::Critical,
                                ),
                                exec,
                            );
                        }
                    }
                    result = handles.observe_rx.recv() => match result {
                        Ok(event) => {
                            if matches!(event, peri_acp_types::event_v2::ObserveEvent::LlmCallEnd { .. }) {
                                if let Some(reached) = reached.take() {
                                    let _ = reached.send(());
                                }
                                if let Some(gate) = release.take() {
                                    let _ = gate.await;
                                }
                            }
                            if let Some(exec) = peri_acp_types::event_v2::observe_event_to_executor(event.clone()) {
                                on_event(
                                    peri_acp_types::runtime::UnstampedEvent::new(
                                        event.turn_id().to_string(),
                                        event.agent_id().to_string(),
                                        None,
                                        peri_acp_types::identity::EventDeliveryClass::Broadcast,
                                    ),
                                    exec,
                                );
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    },
                    else => break,
                }
            }
        })
    })
}

#[cfg(not(windows))]
fn make_aborting_forwarder_launcher() -> ForwarderLauncherFn {
    Arc::new(|_handles, _agent_id, _on_event| {
        let handle = tokio::spawn(std::future::pending());
        handle.abort();
        handle
    })
}

pub(super) fn make_turn_input(
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
        history_payloads: history
            .iter()
            .cloned()
            .map(peri_acp_types::store::PersistedPayload::Message)
            .collect(),
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

/// 装配测试共用的最小事件处理器。
struct ParityFakeEventHandler;

impl peri_agent::agent::events::AgentEventHandler for ParityFakeEventHandler {
    fn on_event(&self, _event: peri_agent::agent::events::ExecutorEvent) {}
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

#[path = "executor_flow_dynamic_test.rs"]
mod dynamic_tests;

#[path = "executor_flow_continuation_test.rs"]
mod continuation_tests;

#[path = "executor_flow_frozen_test.rs"]
mod frozen_tests;

#[path = "executor_flow_parity_test.rs"]
mod parity_tests;

#[cfg(not(windows))]
#[path = "compact_history_fixture_test.rs"]
mod compact_history_tests;
