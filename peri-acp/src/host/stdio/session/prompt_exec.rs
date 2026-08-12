//! Prompt 执行管线：executor → 持久化 → 响应。

use std::sync::Arc;

use crate::session::{event_sink::StdioEventSink, executor};
use agent_client_protocol::{
    schema::v1::{PromptResponse, SessionId, SessionInfoUpdate, SessionUpdate, StopReason},
    Client, ConnectionTo, Responder,
};
use peri_acp_types::messages::MessageContent;
use peri_acp_types::PeriCaps;
use tokio_util::sync::CancellationToken;

use peri_agent::session::exec::executor_helpers::{
    CommandLookupFn, ForwarderLauncherFn, ParentToolsFactory, StageBuildFn,
};
use peri_agent::session::exec::stage_builder::CachedLlmInstances;
use peri_controller::langfuse::bridge::LangfuseBridge;
use peri_controller::langfuse::tracer::LangfuseTracer;

use super::super::context::StdioContext;
use crate::provider::LlmProvider;

/// Prompt 执行的完整参数集合。
pub(crate) struct PromptExecParams {
    pub ctx: Arc<StdioContext>,
    pub cx: ConnectionTo<Client>,
    pub session_id: SessionId,
    pub sid: String,
    pub agent_cwd: String,
    pub content: MessageContent,
    pub frozen: Option<executor::FrozenSessionData>,
    pub history: Vec<peri_acp_types::messages::BaseMessage>,
    pub session_start_source: Option<String>,
    pub history_len: usize,
    pub cancel: CancellationToken,
    pub pool: Arc<parking_lot::Mutex<crate::session::agent_pool::AgentPool>>,
    pub thread_id: String,
    pub responder: Responder<PromptResponse>,
    pub peri_caps: PeriCaps,
}

/// 执行 agent 管线：executor → pool 恢复 → 持久化 → 内存更新 → 响应。
pub(crate) async fn run(params: PromptExecParams) {
    let PromptExecParams {
        ctx,
        cx,
        session_id,
        sid,
        agent_cwd,
        content,
        frozen,
        history,
        session_start_source,
        history_len,
        cancel,
        pool,
        thread_id,
        responder,
        peri_caps,
    } = params;

    let broker: Arc<dyn peri_acp_types::interaction::UserInteractionBroker> =
        Arc::new(super::super::context::StdioBroker::new());

    let event_sink = Arc::new(StdioEventSink::new(
        cx.clone(),
        session_id.clone(),
        peri_caps,
    ));
    let event_sink_for_notif = Arc::clone(&event_sink);

    // Snapshot provider / config (release guards before await).
    let provider_snapshot = ctx.provider.read().clone();
    let peri_config_snapshot = Arc::new(ctx.peri_config.read().clone());

    // Create workflow executor (enables Workflow tool for multi-agent orchestration)
    // GAP-05: inject frozen data so workflow agents reuse SubAgent infra
    // p1-wa：执行体在 peri-agent（`agent::workflow`），ACP 侧构造注入面。
    let workflow_executor = peri_agent::agent::workflow::create_executor(
        peri_agent::agent::workflow::WorkflowAgentContext {
            cwd: agent_cwd.clone(),
            frozen_claude_md: frozen
                .as_ref()
                .and_then(|f| f.claude_md().map(|s| s.to_string())),
            frozen_claude_local_md: frozen
                .as_ref()
                .and_then(|f| f.claude_local_md().map(|s| s.to_string())),
            frozen_skill_summary: frozen
                .as_ref()
                .and_then(|f| f.skill_summary().map(|s| s.to_string())),
            session_id: Some(sid.clone()),
            compact_config: {
                let mut cc = peri_config_snapshot
                    .config
                    .compact
                    .clone()
                    .unwrap_or_default();
                cc.apply_env_overrides();
                Some(cc)
            },
            cancel: Some(cancel.clone()),
            // 无 16_workflow 版本（P2-2026-08-02）：workflow agent 链不
            // 注册 WorkflowTool，不得复用带 workflow 声明的主 prompt。
            system_prompt: frozen
                .as_ref()
                .map(|f| f.subagent_system_prompt().to_string()),
            broker: None,
            permission_mode: None,
            frozen_date: frozen.as_ref().map(|f| f.date().to_string()),
            frozen_language: frozen
                .as_ref()
                .and_then(|f| f.language().map(|s| s.to_string())),
            thread_store: None,
            progress_tx: None,
            subagent_ctx_builder: None,
            agent_prompt_builder: crate::host::workflow_agent::build_workflow_agent_prompt_builder(
                Arc::clone(&ctx.skills),
            ),
            model_factory: crate::host::workflow_agent::build_model_factory(
                &ctx.provider,
                &ctx.peri_config,
            ),
            middleware_factory: Arc::clone(&ctx.workflow_middleware_factory),
            system_prompt_fallback:
                crate::host::workflow_agent::build_workflow_system_prompt_fallback(Arc::clone(
                    &ctx.skills,
                )),
            forwarder_launcher: crate::host::workflow_agent::build_workflow_forwarder_launcher(),
            publish_hook: Some(crate::host::workflow_agent::build_publish_hook(
                &ctx.controller,
            )),
            // Langfuse 观测：与迁移前一致（workflow agent 路径未启用遥测）。
            langfuse_hooks: None,
            langfuse_event_handler: None,
        },
    );

    // Read session-scoped workflow_middleware from SessionInfo
    let (workflow_middleware, lsp_pool) = {
        let sessions = ctx.sessions.read();
        (
            sessions
                .get(&sid)
                .and_then(|s| s.workflow_middleware.clone()),
            sessions.get(&sid).and_then(|s| s.lsp_pool.clone()),
        )
    };

    // v2 路径下 MessageQueue 由 run_session_loop 从 session_access.v2_message_queue
    // 解析（executor.rs），不再作为 PromptExecutionContext 字段传入。

    // ── L5：SessionContext 投影（provider / peri_config / pool / SessionManager /
    //    Controller 端口化——与 host/prompt.rs 同模式，stdio 宿主构造注入面）──
    let provider_name = provider_snapshot.display_name().to_string();
    let provider_model_name = provider_snapshot.model_name().to_string();
    let provider_fp = crate::session::agent_pool::fingerprint(&provider_snapshot);
    let effective_context_window = if provider_snapshot.context_1m() {
        1_000_000
    } else {
        provider_snapshot.context_window()
    };
    let claude_md_excludes = peri_config_snapshot.config.claude_md_excludes.clone();
    let language = peri_config_snapshot.config.language.clone();
    let mut compact_config = peri_config_snapshot
        .config
        .compact
        .clone()
        .unwrap_or_default();
    compact_config.apply_env_overrides();
    let retry_events = pool.lock().retry_events.clone();

    // /bg fork LLM 构造（LlmProvider::from_config 语义，惰性构造仅 /bg 触发）
    let bg_llm_factory: Arc<
        dyn Fn() -> Result<Box<dyn peri_agent::agent::react::ReactLLM + Send + Sync>, String>
            + Send
            + Sync,
    > = {
        let peri_config = Arc::clone(&peri_config_snapshot);
        Arc::new(move || match LlmProvider::from_config(&peri_config) {
            Some(provider) => Ok(Box::new(
                peri_agent::agent::model_bridge::AgentModelBridge::new(Arc::from(
                    provider.into_model(),
                )),
            )),
            None => {
                Err("无法构造 LLM 实例（请检查 peri-config.toml 的 Provider 配置）".to_string())
            }
        })
    };
    // 主 LLM 缓存读取（AgentPool has_valid_cache + get_cached_llm 语义）
    let get_cached_llm: Option<Arc<dyn Fn() -> Option<CachedLlmInstances> + Send + Sync>> = {
        let pool = Arc::clone(&pool);
        let provider = provider_snapshot.clone();
        Some(Arc::new(move || {
            let guard = pool.lock();
            if guard.has_valid_cache(&provider) {
                guard.get_cached_llm().cloned()
            } else {
                None
            }
        }))
    };
    // fresh auxiliary model（缓存缺失时；retry observer 烘焙）
    let fresh_auxiliary_model: Option<Arc<dyn Fn() -> Arc<dyn peri_model::Model> + Send + Sync>> = {
        let pool = Arc::clone(&pool);
        let provider = provider_snapshot.clone();
        Some(Arc::new(move || {
            let provider = provider
                .clone()
                .with_retry_observer(Some(pool.lock().retry_events.as_retry_observer()));
            provider.into_model().into()
        }))
    };
    // LLM 缓存回写（AgentPool store_llm 语义）
    let store_llm: Option<Arc<dyn Fn(CachedLlmInstances) + Send + Sync>> = {
        let pool = Arc::clone(&pool);
        Some(Arc::new(move |cache: CachedLlmInstances| {
            pool.lock().store_llm(cache);
        }))
    };
    // stage 装配 LLM 工厂（主 LLM / auto-classifier / 子 agent；与迁移前
    // stage_builder 桥内构造同源——AgentPool 缓存 + RetryObserver 烘焙）
    let primary_llm_factory: Option<Arc<dyn Fn() -> Arc<dyn peri_model::Model> + Send + Sync>> = {
        let pool = Arc::clone(&pool);
        let provider = provider_snapshot.clone();
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
    let auto_classifier_factory: Option<executor::AutoClassifierFactory> = {
        let provider = provider_snapshot.clone();
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
    let subagent_llm_factory: Option<executor::SubagentLlmFactory> = {
        let provider = provider_snapshot.clone();
        let peri_config = Arc::clone(&peri_config_snapshot);
        let pool = Arc::clone(&pool);
        let retry_events = retry_events.clone();
        let sid = sid.clone();
        Some(Arc::new(move |model_alias: Option<&str>| {
            // 解析 provider 并构建 fingerprint
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
            // 尝试 SubAgent 缓存
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

    // 事件端口（Controller 适配）
    let event_publisher: Arc<dyn peri_acp_types::event::EventPublisher> = Arc::new(
        crate::host::controller_ports::ControllerEventPublisher(Arc::clone(&ctx.controller)),
    );
    let subscribe: Arc<dyn Fn() -> Box<dyn peri_acp_types::event::EventSubscriber> + Send + Sync> = {
        let controller = Arc::clone(&ctx.controller);
        Arc::new(move || {
            Box::new(
                crate::host::controller_ports::ControllerSubscriptionAdapter(
                    controller.subscribe(),
                ),
            )
        })
    };

    // 命令拦截注入面（ACP 协议面注册表 / compact 配置 / /bg fork 装配）
    let command_lookup: CommandLookupFn = Arc::new(|text: &str| {
        crate::session::command::default_prompt_command_registry().find_arc(text)
    });
    let compact_config_loader: Arc<
        dyn Fn() -> peri_acp_types::compact::CompactConfig + Send + Sync,
    > = {
        let peri_config = Arc::clone(&peri_config_snapshot);
        Arc::new(move || crate::host::compact_config::load_compact_config(&peri_config))
    };
    let parent_tools_factory: ParentToolsFactory = {
        let bg_cwd = agent_cwd.clone();
        Arc::new(move || {
            let mut tools: Vec<Box<dyn peri_agent::tools::BaseTool>> =
                peri_middlewares::middleware::FilesystemMiddleware::build_tools(&bg_cwd);
            tools.extend(peri_middlewares::middleware::TerminalMiddleware::build_tools(&bg_cwd));
            tools.extend(peri_middlewares::middleware::WebMiddleware::build_tools());
            Arc::new(
                tools
                    .into_iter()
                    .map(|t| {
                        Arc::new(peri_middlewares::tools::BoxToolWrapper(t))
                            as Arc<dyn peri_agent::tools::BaseTool>
                    })
                    .collect(),
            )
        })
    };
    let chain_assembler: Arc<dyn peri_agent::session::subagent::SubagentChainAssembler> =
        Arc::new(peri_middlewares::subagent::SubagentChainAssemblerImpl);
    let tool_invocation_resolver: Arc<dyn peri_agent::tools::ToolInvocationResolver> =
        Arc::new(peri_middlewares::tool_search::ExecuteExtraToolResolver::default());

    // 防御性 frozen 构建器（turn.frozen=None 回落；生产不可达）
    let frozen_fallback_builder: Option<executor::FrozenFallbackBuilder> = {
        let sm = ctx.session_manager.clone();
        let roots = ctx.plugin_skill_roots.clone();
        let dirs = ctx.plugin_agent_dirs.clone();
        let wf = true; // create_executor 返回 Arc（非 Option），原 ctx.workflow_executor.is_some() 恒真
        Some(Arc::new(move |cwd, _language| {
            sm.build_frozen_data(cwd, &roots, &dirs, wf)
        }))
    };

    let cx = executor::SessionContext {
        cwd: agent_cwd,
        provider_name,
        provider_model_name,
        provider_fp,
        effective_context_window,
        claude_md_excludes,
        language,
        compact_config,
        bg_llm_factory,
        get_cached_llm,
        fresh_auxiliary_model,
        store_llm,
        retry_events: Some(Arc::new(retry_events)),
        primary_llm_factory,
        auto_classifier_factory,
        subagent_llm_factory,
        session_id: sid.clone(),
        cancel,
        broker,
        permission_mode: ctx.permission_mode.clone(),
        session_access: Some(Arc::new(ctx.session_manager.clone())
            as Arc<dyn peri_acp_types::session::SessionAccessPort>),
        thread_store: Some(Arc::clone(&ctx.thread_store)),
        thread_id: Some(thread_id.clone()),
        plugin_skill_roots: ctx.plugin_skill_roots.clone(),
        plugin_agent_dirs: ctx.plugin_agent_dirs.clone(),
        plugin_loaded: ctx.plugin_loaded.clone(),
        hook_groups: ctx.hook_groups.clone(),
        cron_scheduler: Some(ctx.cron_scheduler.clone()),
        mcp_pool: ctx.mcp_pool.clone(),
        channel_state: ctx.channel_state.clone(),
        tool_search_index: ctx.tool_search_index.clone(),
        skills: ctx.skills.clone(),
        shared_tools: ctx.shared_tools.clone(),
        lsp_servers: ctx.plugin_lsp_servers.clone(),
        lsp_pool,
        workflow_executor: Some(workflow_executor),
        workflow_middleware,
        event_publisher,
        subscribe,
        command_lookup,
        compact_config_loader,
        parent_tools_factory,
        chain_assembler,
        tool_invocation_resolver,
        session_start_source,
        request_id: None, // stdio 无 requestId 配对（TUI 专用）
        allow_await_wake: false,
        continuation_notify: None, // stdio 无 continuation scheduler
        frozen_fallback_builder,
    };

    // ── L5：TurnInput 注入面（Langfuse hooks / stage 装配桥 / forwarder）──
    let langfuse_hooks: Option<executor::LangfuseHooks> = ctx.langfuse_session.as_ref().map(|s| {
        let session_clone = Arc::clone(s);
        let config = session_clone.config.clone();
        let session: std::sync::Arc<dyn peri_controller::langfuse::LangfuseSessionLike> =
            session_clone;
        let tracer = Arc::new(parking_lot::Mutex::new(LangfuseTracer::new(
            session,
            sid.clone(),
            config,
        )));
        executor::LangfuseHooks {
            on_turn_start: {
                let tracer = Arc::clone(&tracer);
                Arc::new(move |input: &str| {
                    tracer.lock().on_turn_start(input);
                }) as Arc<dyn Fn(&str) + Send + Sync>
            },
            on_turn_end: {
                let tracer = Arc::clone(&tracer);
                Arc::new(move |err: Option<String>| {
                    tracer.lock().on_turn_end(err.as_deref()).into()
                })
                    as Arc<
                        dyn Fn(Option<String>) -> Option<tokio::task::JoinHandle<()>> + Send + Sync,
                    >
            },
            bridge_factory: {
                let tracer = Arc::clone(&tracer);
                Arc::new(move |name: String, agent_id: Option<String>| {
                    Some(
                        Arc::new(LangfuseBridge::new(Arc::clone(&tracer), name, agent_id))
                            as Arc<dyn peri_agent::agent::LangfuseBridgeLike>,
                    )
                })
                    as Arc<
                        dyn Fn(
                                String,
                                Option<String>,
                            )
                                -> Option<Arc<dyn peri_agent::agent::LangfuseBridgeLike>>
                            + Send
                            + Sync,
                    >
            },
        }
    });

    // stage 装配桥：从 SessionContext 投影 StageBuildInput 并补齐注入面
    //（Langfuse bridge factory 经 turn 级 hooks 构造），再调用 ACP 装配桥。
    let cx_for_stage = cx.clone();
    let bridge_factory_for_stage: Option<
        Arc<dyn Fn() -> Arc<dyn peri_agent::agent::LangfuseBridgeLike> + Send + Sync>,
    > = langfuse_hooks.as_ref().map(|h| {
        let bf = Arc::clone(&h.bridge_factory);
        let provider_display = cx_for_stage.provider_name.clone();
        Arc::new(move || {
            bf(provider_display.clone(), None)
                .expect("stage bridge_factory: hooks 存在时 bridge 构造必须成功")
        }) as Arc<dyn Fn() -> Arc<dyn peri_agent::agent::LangfuseBridgeLike> + Send + Sync>
    });
    let stage_build: StageBuildFn = Arc::new(move |sbr| {
        // compact hook 闭包在每次装配时构造（hook_groups 非空才产生动作；
        // 与迁移前 stage_builder 内构造时机逐次一致）
        let (compact_pre_hook, compact_post_hook) = crate::host::prompt::build_compact_hooks(
            &cx_for_stage.hook_groups,
            &cx_for_stage.cwd,
            &cx_for_stage.session_id,
            &cx_for_stage.provider_model_name,
        );
        crate::host::stage_builder::build_stage_context(
            &cx_for_stage,
            &peri_middlewares::assembly::ProductionChainAssembler, // ZST 装配器
            compact_pre_hook,
            compact_post_hook,
            sbr.cached_llm.as_ref(),
            sbr.system_prompt,
            sbr.subagent_system_prompt,
            sbr.frozen,
            sbr.event_handler,
            sbr.agent_overrides,
            sbr.preload_skills,
            sbr.child_handler_factory,
            sbr.auxiliary_model,
            sbr.thread_persistence,
            sbr.goal_controller,
            sbr.task_manager,
            sbr.on_bg_complete,
            bridge_factory_for_stage.clone(),
        )
    });

    // EventBus forwarder 启动器（Langfuse bridge 构造留在 ACP——观测旁路）
    let forwarder_launcher: ForwarderLauncherFn = {
        let provider_display = cx.provider_name.clone();
        let bridge_factory = langfuse_hooks
            .as_ref()
            .map(|h| Arc::clone(&h.bridge_factory));
        Arc::new(move |handles, agent_id, on_event| {
            let bridge: Option<LangfuseBridge> = bridge_factory
                .as_ref()
                .and_then(|bf| bf(provider_display.clone(), Some(agent_id)))
                .and_then(|b| {
                    // LangfuseBridgeLike: Any 上界（L5）——trait upcasting 还原具体类型
                    let any: Arc<dyn std::any::Any + Send + Sync> = b;
                    any.downcast::<LangfuseBridge>().ok().map(|b| (*b).clone())
                });
            crate::event::spawn_eventbus_forwarder(handles, on_event, bridge);
        })
    };

    let turn = executor::TurnInput {
        event_sink,
        content,
        continuation: false,
        frozen,
        history,
        incoming_recalls: vec![],
        bg_results: vec![], // stdio 无后台任务
        langfuse: langfuse_hooks,
        stage_build,
        forwarder_launcher,
    };

    // 3.0 批 2：执行发起经 Controller（控制面第四步 run Session）。
    // 本轮执行句柄（PromptHandle）注册进 Runtime 映射 → run_session 发起 →
    // 返回时结果已就绪 → take_result。
    // L5：执行体固定为 `run_session_loop`（句柄内部直接调用，无需 runner 注入）。
    let handle = Arc::new(crate::host::prompt_handle::PromptHandle::new(cx, turn));
    ctx.controller.register_session(&sid, Arc::clone(&handle));
    if let Err(e) = ctx.controller.run_session(&sid).await {
        tracing::error!(session_id = %sid, error = %e, "run_session failed");
        let _ = responder.respond(PromptResponse::new(StopReason::Cancelled));
        return;
    }
    let result = handle.take_result();

    // Restore AgentPool back into session
    if let Ok(mutex) = Arc::try_unwrap(pool) {
        let mut sessions = ctx.sessions.write();
        if let Some(s) = sessions.get_mut(&sid) {
            s.agent_pool = mutex.into_inner();
        }
    }

    // Persist new messages to ThreadStore.
    if result.ok && history_len < result.messages.len() {
        let new_msgs = &result.messages[history_len..];
        if let Err(e) = ctx.thread_store.append_messages(&thread_id, new_msgs).await {
            tracing::warn!(error = %e, "Failed to persist messages to ThreadStore");
        }
    }
    // Update in-memory state.
    {
        let mut sessions = ctx.sessions.write();
        if let Some(s) = sessions.get_mut(&sid) {
            s.history = result.messages;
            s.cancel_token = None;
        }
    }

    let acp_stop_reason = match result.stop_reason {
        executor::PromptStopReason::Cancelled => StopReason::Cancelled,
        executor::PromptStopReason::MaxTurnRequests => StopReason::MaxTurnRequests,
        executor::PromptStopReason::EndTurn => StopReason::EndTurn,
    };
    let _ = responder.respond(PromptResponse::new(acp_stop_reason));

    // Send SessionInfoUpdate after prompt completes.
    let info = SessionInfoUpdate::new().updated_at(chrono::Utc::now().to_rfc3339());
    event_sink_for_notif.send_update(SessionUpdate::SessionInfoUpdate(info));
}
