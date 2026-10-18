//! ACP Prompt execution — builds and executes the agent via crate::executor.
//! Extracted from original acp_server.rs (2026-05-20 split).

use std::sync::Arc;

use crate::{
    broker::AcpTransportBroker,
    session::{event_sink::TransportEventSink, executor},
    transport::types::AcpError,
};
use agent_client_protocol::schema::v1::{Meta, PromptResponse, StopReason};
use peri_acp_types::interaction::{
    ApprovalDecision, ApprovalItem, InteractionContext, InteractionResponse,
    TrustedInteractionOwner, UserInteractionBroker,
};
use peri_acp_types::permission::{PermissionMode, SharedPermissionMode};
use peri_acp_types::session::{ExecutionFailure, ExecutionFailureKind};
use serde_json::Value;
use tracing::info;

use peri_agent::session::exec::executor_helpers::CommandLookupFn;

mod models;
mod stage;
mod telemetry;
pub(crate) use stage::build_compact_hooks;

use super::SharedSessions;

// ── Prompt execution (spawned into background task) ──────────────────────────

// ── ACP 结果投影（spec/history/2026-08.md 2026-08-18 条目 D2）─────────────

/// fatal turn failure 的稳定 JSON-RPC server error code。
///
/// 取自 JSON-RPC 2.0 保留段 server error（`-32000..=-32099`）的首值，语义为
/// "agent turn execution failed"。`data` 保留稳定分类与完整诊断。
pub const ACP_TURN_EXECUTION_FAILED_CODE: i64 = -32000;

/// [`ExecutionFailureKind`] → JSON-RPC server error code 的穷尽映射。
///
/// 只映射稳定内部类别；新增类别时编译器强制在此显式补充映射，禁止
/// fallthrough 默认码。
const fn execution_failure_kind_code(kind: ExecutionFailureKind) -> i64 {
    match kind {
        ExecutionFailureKind::Internal
        | ExecutionFailureKind::Llm
        | ExecutionFailureKind::LlmHttp => ACP_TURN_EXECUTION_FAILED_CODE,
    }
}

/// Agent→ACP 结果边界的窄映射：`ExecutionFailure` → 传输层 `AcpError`。
///
/// - `code`：按 [`execution_failure_kind_code`] 穷尽映射；
/// - `message`：直接使用 failure 的错误信息（非空由
///   [`ExecutionFailure::internal`] 保证，空输入回落稳定 fallback）；
/// - `data`：稳定 `kind`、HTTP `status` 与完整 `diagnostic`。
pub(crate) fn execution_failure_to_acp_error(failure: &ExecutionFailure) -> AcpError {
    let mut data = serde_json::Map::new();
    data.insert(
        "kind".to_string(),
        Value::String(failure.kind.wire_name().to_string()),
    );
    if failure.kind == ExecutionFailureKind::LlmHttp {
        if let Some(status) = failure.http_status {
            data.insert("status".to_string(), Value::from(status));
        }
    }
    if let Some(category) = &failure.error_category {
        data.insert(
            "error_category".to_string(),
            Value::String(category.clone()),
        );
    }
    if !failure.causes.is_empty() {
        data.insert("causes".to_string(), serde_json::json!(failure.causes));
    }
    if let Some(diagnostic) = &failure.diagnostic {
        data.insert("diagnostic".to_string(), serde_json::json!(diagnostic));
    }
    AcpError {
        code: execution_failure_kind_code(failure.kind),
        message: failure.public_message.clone(),
        data: Some(Value::Object(data)),
    }
}

/// `PromptResult` 终止语义 → ACP 响应（`run_prompt` 尾部投影的窄 seam）。
///
/// - `failure=Some` → `Err(AcpError)`：fatal turn failure 由统一 host 与
///   transport 生成标准 JSON-RPC error response（mpsc 与 stdio 共用同一路径）；
/// - `failure=None` → 现有成功 `PromptResponse`（cancel / max-iterations /
///   end-turn 各有对应 `StopReason`，不得升级为请求失败）。
///
/// 调用方（`run_prompt`）必须完成全部后处理（历史保存 / session state /
/// cancel token 清理 / recall 回写）后再调用本函数——wire 形态决定不得先于
/// 失败路径清理（D2 顺序不变量）。
fn prompt_wire_response(
    failure: Option<&ExecutionFailure>,
    stop_reason: executor::PromptStopReason,
    pending_tasks: u32,
) -> Result<Value, AcpError> {
    if let Some(failure) = failure {
        return Err(execution_failure_to_acp_error(failure));
    }
    let acp_stop_reason = match stop_reason {
        executor::PromptStopReason::Cancelled => StopReason::Cancelled,
        executor::PromptStopReason::MaxTurnRequests => StopReason::MaxTurnRequests,
        executor::PromptStopReason::MaxTokens => StopReason::MaxTokens,
        executor::PromptStopReason::EndTurn => StopReason::EndTurn,
    };
    let mut resp = PromptResponse::new(acp_stop_reason);
    // §7.3 有界等待摘要：turn 退出时仍有未结算任务才附加标记，
    // 无未结算任务时响应保持原形（不产生 `_meta`）。
    if pending_tasks > 0 {
        let mut meta = Meta::new();
        meta.insert(
            "peri".to_owned(),
            serde_json::json!({ "pendingTasks": pending_tasks }),
        );
        resp.meta = Some(meta);
    }
    serde_json::to_value(resp).map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
}

/// stdio 部署是否过滤指定命令（按解析后的 `fullname` 判定，含别名）。
///
/// 仅 stdio 部署单元（`stdio_command_filter=true`）过滤 `rewind` / `clear`；
/// 命中即 fall-through 进 agent 管线（当作普通文本发给模型，IDE 客户端
/// 自管理这两个命令）。TUI / print（`false`）恒不过滤。
fn stdio_filters_command(fullname: &str, stdio_command_filter: bool) -> bool {
    stdio_command_filter && matches!(fullname, "core:rewind" | "core:clear")
}

/// **仅用于没有 session 环境的会话**（无工作区装配：测试 / 退化路径）的兜底
/// broker：那种会话没有 session 级 endpoint 表可查，因此按本轮的 transport 直接
/// 投递（与接线前的行为一致）。
///
/// 生产会话一律经 [`super::interaction_router::SessionInteractionRouter`]：
/// endpoint 选择是 session 级事实（在线表 / attachment 纪元 / 请求作用域），
/// 不得把某一轮的固定 broker 当成 session 级权威。
pub(crate) fn build_transport_broker(
    transport: &Arc<dyn crate::transport::AcpTransport>,
    session_id: &str,
    interaction_gate: Arc<tokio::sync::Mutex<()>>,
) -> Arc<dyn UserInteractionBroker> {
    Arc::new(
        AcpTransportBroker::new(
            Arc::new(crate::transport::AcpRequestBridge(Arc::clone(transport))),
            session_id.to_string().into(),
            interaction_gate,
        )
        .with_timeout(crate::broker::ask_user_timeout()),
    )
}

/// Cron/loop 是被持久注册的代理执行权：每次触发在进入模型前重新审批。
/// Bypass 按既有模式契约直接允许；其他模式必须有 broker 并明确 Approve。
///
/// `owner` 是本次 Host 程序化审批的归属（[`TrustedInteractionOwner::Host`]）：
/// 调度触发发生在模型 turn 之前，没有 turn 事实可绑定，不得伪造成 Model 归属。
pub(crate) async fn approve_scheduled_trigger(
    permission_mode: &SharedPermissionMode,
    broker: Option<&Arc<dyn UserInteractionBroker>>,
    owner: TrustedInteractionOwner,
    task_id: &str,
    prompt: &str,
) -> bool {
    if permission_mode.load() == PermissionMode::Bypass {
        return true;
    }
    let Some(broker) = broker else {
        return false;
    };
    let response = broker
        .request(
            owner,
            InteractionContext::Approval {
                items: vec![ApprovalItem {
                    tool_call_id: task_id.to_string(),
                    tool_name: "cron_trigger".to_string(),
                    tool_input: serde_json::json!({ "taskId": task_id, "prompt": prompt }),
                }],
            },
        )
        .await;
    matches!(
        response,
        InteractionResponse::Decisions(decisions)
            if matches!(decisions.as_slice(), [ApprovalDecision::Approve { .. }])
    )
}

#[allow(clippy::too_many_arguments)] // Shared host execution wiring plus optional Agent-owned input ticket.
pub(crate) async fn run_prompt(
    mut params: Value,
    sessions: &SharedSessions,
    deployment: &super::AcpServerConfig,
    transport: &Arc<dyn crate::transport::AcpTransport>,
    pool: Arc<parking_lot::Mutex<crate::session::agent_pool::AgentPool>>,
    cont_tx: Option<tokio::sync::mpsc::UnboundedSender<executor::ContinuationRequest>>,
    continuation: bool,
    input_ticket: Option<super::user_input::UserInputRun>,
) -> Result<Value, AcpError> {
    let notifications = super::execution::ExecutionNotifications::from_params(&mut params)?;
    let session_id = super::extract_session_id(&params, "").to_owned();
    let attempt_activation = deployment
        .session_manager
        .get_session(&session_id)
        .map(|runtime| {
            (
                Arc::clone(&runtime.activation),
                runtime.v2_message_queue.admission_watermark(),
            )
        });
    let result = run_prompt_attempt(
        params,
        sessions,
        deployment,
        transport,
        pool,
        cont_tx,
        continuation,
        input_ticket,
        &notifications,
        attempt_activation.as_ref(),
    )
    .await;
    if result.is_err() {
        if let Some((activation, watermark)) = &attempt_activation {
            activation.record_failed_attempt(*watermark);
        }
        notifications
            .finish_early(&session_id, "error", deployment, transport)
            .await;
    }
    result
}

#[allow(clippy::too_many_arguments)]
async fn run_prompt_attempt(
    params: Value,
    sessions: &SharedSessions,
    deployment: &super::AcpServerConfig,
    transport: &Arc<dyn crate::transport::AcpTransport>,
    pool: Arc<parking_lot::Mutex<crate::session::agent_pool::AgentPool>>,
    cont_tx: Option<tokio::sync::mpsc::UnboundedSender<executor::ContinuationRequest>>,
    continuation: bool,
    input_ticket: Option<super::user_input::UserInputRun>,
    notifications: &super::execution::ExecutionNotifications,
    attempt_activation: Option<&(Arc<crate::session::SessionActivation>, u64)>,
) -> Result<Value, AcpError> {
    // Borrow deployment services; turn-owned callbacks clone only their existing handles.
    // Provider/config snapshots remain below, after the session snapshot is captured.
    let provider = &deployment.provider;
    let peri_config = &deployment.peri_config;
    let permission_mode = &deployment.permission_mode;
    let cron_scheduler = deployment.cron_scheduler.clone();
    let plugin_skill_roots = deployment.plugin_skill_roots.as_slice();
    let plugin_loaded = deployment.plugin_loaded.as_slice();
    let hook_groups = deployment.hook_groups.as_slice();
    let mcp_pool = deployment.mcp_pool.clone();
    let dynamic_mcp = deployment.dynamic_mcp.clone();
    let tool_search_index = deployment.tool_search_index.clone();
    let agent_catalog = deployment.agent_catalog.clone();
    let shared_tools = deployment.shared_tools.clone();
    let session_resources = deployment.session_resources.clone();
    let controller = &deployment.controller;
    let langfuse_session = deployment.langfuse_session.clone();
    let session_manager = deployment.session_manager.clone();
    let stdio_command_filter = deployment.stdio_command_filter;
    let (session_id, content, _attachments) =
        crate::dispatch::prompt::extract_and_validate_run_prompt_params(&params)?;
    // v2 路径下 MessageQueue 由 run_session_loop 从 session_manager.v2_message_queue
    // 解析（executor.rs:368），不再作为 PromptExecutionContext 字段传入。

    // Parse optional background task results for synthetic tool_use + tool_result injection
    let bg_results: Vec<peri_acp_types::event::BackgroundTaskResult> = params
        .get("bgResults")
        .map(|v| serde_json::from_value(v.clone()).unwrap_or_default())
        .unwrap_or_default();

    // Issue 2026-08-05 返工：requestId 透传——TUI 提交时生成、随 prompt RPC 到达，
    // 服务器随 turn 结束事件（peri/agent_event_done）原样回带，供 TUI 侧 stale
    // TurnInterrupted 的 request_id 配对判定。缺失路径（stdio / continuation 等）为 None。
    let request_id = params
        .get("requestId")
        .and_then(|v| v.as_str())
        .map(String::from);

    let user_input_mailbox = super::user_input::ensure_mailbox(&session_id, deployment, transport)?;

    // Create cancel token and register in sessions.
    // `AgentCancellationToken` 即 `tokio_util::sync::CancellationToken` 别名
    // （peri-agent re-export；ACP 协议面直接使用底层类型，不经业务 crate）。
    let cancel = {
        let sessions = sessions.lock().await;
        sessions
            .get(&session_id)
            .filter(|state| continuation && state.continuation_in_flight)
            .and_then(|state| state.cancel_token.clone())
            .unwrap_or_default()
    };
    let managed_input = input_ticket.is_some();
    let input_attempt = match &input_ticket {
        Some(run) => {
            if !user_input_mailbox.attach_attempt(&run.ticket, cancel.clone()) {
                return Ok(Value::Null);
            }
            run.ticket.clone()
        }
        None => user_input_mailbox
            .attach_external_attempt(cancel.clone(), !continuation && content.is_empty())
            .ok_or_else(|| AcpError::new(-32800, "user input attempt superseded"))?,
    };
    let mut input_attempt_guard = super::user_input::InputAttemptGuard::new(
        Arc::clone(&user_input_mailbox),
        input_attempt.clone(),
    );
    if managed_input {
        super::user_input::publish_run_started(
            &session_id,
            &user_input_mailbox,
            &input_attempt,
            deployment,
            transport,
        )
        .await?;
    } else {
        notifications
            .start(
                &session_id,
                user_input_mailbox.generation(),
                deployment,
                transport,
            )
            .await?;
    }
    // 上一轮提交结果不确定时，热态只有会话身份。下一次使用前从权威存储恢复
    // 有效上下文；恢复失败保持 ReloadRequired，不用空集合冒充“空会话”。
    //
    // 放在 start/run-started 之后：续跑没有请求作用域响应，提前失败也必须由
    // `finish_early` 发出终止通知，否则调度器与 TUI 永远等不到本轮结束。
    ensure_resident_context(sessions, &session_id, controller).await?;
    // activation 监听器必须在移交正文**之前**建立：它是可失败的启动步骤，
    // 失败时 Host 仍持有有效上下文，下一次使用不受影响。
    if let Some(sender) = &cont_tx {
        if !continuation || managed_input {
            if let Some(runtime) = session_manager.get_session(&session_id) {
                runtime.activation.allow();
            }
        }
        super::activation::ensure_listener(&session_id, sessions, deployment, sender)?;
    }
    // Read session data under lock, then release immediately.
    let (
        cwd,
        history_payloads,
        _handoff_guard,
        thread_id,
        frozen,
        incoming_recalls,
        workflow_notifications,
        interaction_gate,
        interaction_router,
    ) = {
        let mut states = sessions.lock().await;
        let state = states
            .get_mut(&session_id)
            .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
        // 冻结输入是执行前置条件，必须在移交正文所有权**之前**验证：
        // 否则缺少 frozen 的会话会停在 Host 空手、执行器未启动的状态。
        if state.frozen.is_none() {
            return Err(AcpError::new(
                -32603,
                "Session has no frozen snapshot; refusing to rebuild it from the current state",
            ));
        }
        state.cancel_token = Some(cancel.clone());
        // 正文所有权在短锁内移交给执行器：Host 不再保留一份“以防万一”的全文。
        // 期间其他请求必须把 HandedOff 视为“上下文在途”，不能把空集合当空会话。
        let (history_payloads, handoff_guard) =
            super::ownership::begin_handoff(state, sessions, &session_id);
        (
            state.cwd.clone(),
            history_payloads,
            handoff_guard,
            state.thread_id.clone(),
            state.frozen.clone(),
            // 后台 continuation 保留 recall；队列承载的新用户输入仍消费 recall。
            take_recall_for_turn(&mut state.recall_items, continuation && !managed_input),
            state.workflow_notifications.clone(),
            // session-owned 交互互斥门：本轮 broker 借用，不重建（跨轮互斥）。
            Arc::clone(&state.interaction_gate),
            // session-owned 交互路由（同一门）：本轮只发布 / 清除 endpoint，
            // 不缓存某一轮的固定 broker（设计 §3.3 / §7）。
            state
                .environment
                .as_ref()
                .map(|env| Arc::clone(env.interaction_router())),
        )
    };
    #[cfg(test)]
    super::prompt_dispatch::admission_gate::wait(&format!("{session_id}:handed-off")).await;
    // 不再预构整段 `BaseMessage` 投影：执行器只在 keepgoing 短路或命中
    // slash 命令时按需派生（S2 命令识别前置）。
    let is_empty = history_payloads.is_empty();
    // 交互 endpoint：session 级路由按**本轮正在使用的 transport** 发布当前轮
    // endpoint（guard 在轮末 / 提前返回时清除）；没有 session 环境（无工作区
    // 装配的退化会话）时才回落到本轮固定 broker。
    let (broker, _model_turn_guard): (
        Arc<dyn UserInteractionBroker>,
        Option<super::interaction_router::ModelTurnGuard>,
    ) = match interaction_router.as_ref() {
        Some(router) => (
            Arc::clone(router) as Arc<dyn UserInteractionBroker>,
            router.publish_model_turn(transport),
        ),
        None => (
            build_transport_broker(transport, &session_id, interaction_gate),
            None,
        ),
    };
    let event_sink = Arc::new(TransportEventSink::new(
        Arc::clone(transport),
        session_manager.caps_registry(),
    ));

    let provider_snapshot = provider.read().clone();
    let peri_config_snapshot = Arc::new(peri_config.read().clone());

    // 同源收口（ARC-FROZEN-001）：可执行会话必然
    // 带有创建/恢复时定格的 frozen。缺失时 **fail-closed** —— 既不按当前配置/目录重冻
    // （那会把本轮的目录状态冒充成历史冻结输入），也不带着空冻结继续跑；宿主必须显式
    // 修复这条会话（重新 load 或删除），而不是让一次静默降级改掉系统提示词。
    //
    // 正常路径已在移交正文前验证过 frozen；这里保留防御分支，并保证即使
    // 状态在锁外被改坏也不会把已移走的正文当作仍驻留 Host。
    let Some(frozen) = frozen else {
        mark_reload_required(sessions, &session_id).await;
        return Err(AcpError::new(
            -32603,
            "Session has no frozen snapshot; refusing to rebuild it from the current state",
        ));
    };

    // ── L5：SessionContext 投影（provider / peri_config / pool / SessionManager /
    //    Controller 端口化——执行体迁入 peri-agent 后由本宿主构造注入面）──
    let provider_name = provider_snapshot.display_name().to_string();
    let provider_model_name = provider_snapshot.model_name().to_string();
    let provider_fp = crate::session::agent_pool::fingerprint(&provider_snapshot);
    let effective_context_window = provider_snapshot.effective_context_window();
    let language = peri_config_snapshot.config.language.clone();
    let mut compact_config = peri_config_snapshot
        .config
        .compact
        .clone()
        .unwrap_or_default();
    compact_config.apply_env_overrides();
    let retry_events = pool.lock().retry_events.clone();

    let models::ModelFactories {
        get_cached_llm,
        fresh_auxiliary_model,
        store_llm,
        primary_llm_factory,
        auto_classifier_factory,
        subagent_llm_factory,
    } = models::build_model_factories(
        &provider_snapshot,
        &peri_config_snapshot,
        &pool,
        &retry_events,
        &session_id,
    );

    // 事件端口（Controller 适配）
    let event_publisher: Arc<dyn peri_acp_types::event::EventPublisher> = Arc::new(
        crate::host::controller_ports::ControllerEventPublisher(Arc::clone(controller)),
    );
    let subscribe: Arc<dyn Fn() -> Box<dyn peri_acp_types::event::EventSubscriber> + Send + Sync> = {
        let controller = Arc::clone(controller);
        Arc::new(move || {
            Box::new(
                crate::host::controller_ports::ControllerSubscriptionAdapter(
                    controller.subscribe(),
                ),
            )
        })
    };

    // 命令拦截注入面（ACP 协议面注册表 / compact 配置 / /bg fork 装配）
    // Phase 2 常驻化：捕获会话级注册表（随 session 创建注册内置命令，跨轮
    // 常驻，动态注入条目不因轮次丢失），不再每轮 new 默认注册表。
    let command_registry = session_manager.command_registry_for(&session_id);
    let command_lookup: CommandLookupFn = Arc::new(move |text: &str| {
        let reg = command_registry.as_ref()?;
        let resolved = reg.resolve(text)?;
        // stdio 部署过滤：rewind / clear（含别名 cls/reset）不作为命令拦截，
        // fall-through 进 agent 管线（当普通文本发给模型，IDE 客户端自管理）。
        // 仅 stdio_command_filter 为 true 时生效，其余命令与 TUI 完全一致。
        if stdio_filters_command(resolved.entry.fullname.as_str(), stdio_command_filter) {
            return None;
        }
        Some(resolved)
    });
    let compact_config_loader: Arc<
        dyn Fn() -> peri_acp_types::compact::CompactConfig + Send + Sync,
    > = {
        let peri_config = Arc::clone(&peri_config_snapshot);
        Arc::new(move || crate::host::compact_config::load_compact_config(&peri_config))
    };
    let tool_invocation_resolver: Arc<dyn peri_agent::tools::ToolInvocationResolver> =
        Arc::new(peri_middlewares::tool_search::ExecuteExtraToolResolver::default());

    // 同源收口（§6.4）：宿主**不**注入 frozen 构建器。曾经的 `FrozenFallbackBuilder`
    // 会在 `turn.frozen=None` 时按当前目录/配置重冻一份，那是同源规则的第三入口；
    // 现在本入口在此前已对缺 frozen 的会话 fail-closed（见上文 `let Some(frozen)`），
    // 因此不存在「静默重冻」这条路径。
    let frozen_fallback_builder: Option<executor::FrozenFallbackBuilder> = None;

    let session_mcp_capability = dynamic_mcp
        .as_ref()
        .map(|deployment| deployment.capability(&session_id));
    let dynamic_mcp_projection = session_manager
        .get_session(&session_id)
        .map(|session| Arc::clone(&session.dynamic_mcp_projection))
        .unwrap_or_else(|| Arc::new(parking_lot::Mutex::new(None)));

    let ctx = executor::SessionContext {
        cwd,
        provider_name,
        provider_model_name,
        provider_fp,
        effective_context_window,
        language,
        compact_config,
        get_cached_llm,
        fresh_auxiliary_model,
        store_llm,
        retry_events: Some(Arc::new(retry_events)),
        primary_llm_factory,
        auto_classifier_factory,
        subagent_llm_factory,
        session_id: session_id.clone(),
        cancel,
        broker,
        permission_mode: permission_mode.clone(),
        session_access: Some(Arc::new(session_manager.clone())
            as Arc<dyn peri_acp_types::session::SessionAccessPort>),
        session_resources: Some(session_resources.clone()),
        thread_id: Some(thread_id.clone()),
        plugin_skill_roots: plugin_skill_roots.to_vec(),
        plugin_loaded: plugin_loaded.to_vec(),
        hook_groups: hook_groups.to_vec(),
        cron_scheduler,
        mcp_pool,
        dynamic_mcp,
        session_mcp_capability,
        dynamic_mcp_projection,
        tool_search_index,
        agent_catalog,
        shared_tools,
        workflow_notifications,
        event_publisher,
        subscribe,
        command_lookup,
        compact_config_loader,
        tool_invocation_resolver,
        session_start_source: if (!continuation || managed_input) && is_empty {
            Some("startup".to_string())
        } else {
            None
        },
        request_id,
        allow_await_wake: true,
        continuation_notify: cont_tx,
        user_input_mailbox: Some(Arc::clone(&user_input_mailbox)),
        frozen_fallback_builder,
    };

    // ── L5：TurnInput 注入面（Langfuse hooks / stage 装配桥 / forwarder）──
    // Langfuse hooks：从 session 级 LangfuseSession 构造 tracer 并烘焙三个闭包
    //（turn 开始/结束 trace + 观测旁路 bridge 工厂）。
    let langfuse_hooks = telemetry::build_langfuse_hooks(langfuse_session.as_ref(), &session_id);

    // stage 装配桥：从 SessionContext 投影 StageBuildInput 并补齐注入面
    //（Langfuse bridge factory 经 turn 级 hooks 构造），再调用 ACP 装配桥。
    let stage_build = stage::build_stage_bridge(
        &ctx,
        langfuse_hooks.as_ref(),
        deployment.host_task_spawner.clone(),
    );

    // EventBus forwarder 启动器（Langfuse bridge 构造留在 ACP——观测旁路；
    // biased select 顺序不变量单点保持在 crate::event::spawn_eventbus_forwarder）。
    let forwarder_launcher =
        telemetry::build_forwarder_launcher(&ctx.provider_name, langfuse_hooks.as_ref());

    let turn = executor::TurnInput {
        event_sink,
        content,
        continuation,
        frozen: Some(frozen),
        history_payloads,
        incoming_recalls,
        bg_results,
        langfuse: langfuse_hooks,
        stage_build,
        forwarder_launcher,
    };

    // 3.0 批 2：执行发起经 Controller（控制面第四步 run Session）。
    // 本轮执行句柄（PromptHandle）注册进 Runtime 映射（注册或替换，
    // 不递增 epoch/seq）→ `Controller::run_session` 经 Runtime 查映射发起 →
    // `run_session_loop` 执行完成（返回时结果已就绪）→ take_result。
    // L5：执行体固定为 `run_session_loop`（句柄内部直接调用，无需 runner 注入）。
    let handle = Arc::new(crate::host::prompt_handle::PromptHandle::new(ctx, turn));
    controller.register_session(&session_id, Arc::clone(&handle));
    #[cfg(test)]
    super::prompt_dispatch::admission_gate::wait(&format!("{session_id}:registered")).await;
    if let Err(error) = controller.run_session(&session_id).await {
        // 执行发起失败时正文已经随 TurnInput 交给句柄，Host 无法确认是否产生
        // 持久化副作用；进入 ReloadRequired，下一次使用从存储恢复。
        mark_reload_required(sessions, &session_id).await;
        return Err(AcpError::new(
            -32603,
            format!("run_session failed: {error}"),
        ));
    }
    let result = handle.take_result();
    notifications.mark_terminal();
    if !result.ok || result.failure.is_some() {
        if let Some((activation, watermark)) = attempt_activation {
            if result.stop_reason == executor::PromptStopReason::Cancelled {
                activation.suppress();
            } else {
                activation.record_failed_attempt(*watermark);
            }
        }
    }
    if let Some(run) = &input_ticket {
        run.mark_terminal_delivered();
    }

    if result.persistence_inconsistent {
        deployment
            .session_manager
            .invalidate_user_input_mailbox(&session_id);
    }
    input_attempt_guard.finish(&result);

    let compact_committed =
        result.history_replaced_by_compaction && !result.persistence_inconsistent;
    let response = finish_prompt_turn(
        sessions,
        &session_id,
        continuation && !managed_input,
        result,
        Some(deployment.session_resources.clone()),
    )
    .await?;
    if compact_committed {
        // 幂等轻量通知：只发给仍有活动 V1 window ticket 的会话；失败不影响 durable commit。
        super::history_window::notify_commit_if_open(sessions, &session_id).await;
    }
    Ok(response)
}

/// ReloadRequired 热态在下次使用前从权威存储恢复有效上下文。
///
/// `HandedOff` 表示正文在在途执行器里，Host 不得把它当成空会话，也不能
/// 同时再装入一份副本；此路径按“会话忙”拒绝。
pub(super) async fn ensure_resident_context(
    sessions: &SharedSessions,
    session_id: &str,
    controller: &peri_controller::Controller,
) -> Result<(), AcpError> {
    let thread_id = {
        let mut sessions = sessions.lock().await;
        let state = sessions
            .get_mut(session_id)
            .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
        super::ownership::mark_stale_if_dead(state);
        match state.context_ownership {
            super::ContextOwnership::Resident => return Ok(()),
            super::ContextOwnership::ReloadRequired => state.thread_id.clone(),
            super::ContextOwnership::HandedOff => {
                return Err(AcpError::new(
                    -32010,
                    "Session context is owned by an in-flight prompt",
                ))
            }
        }
    };
    let payloads = crate::dispatch::load_active_session_payloads(controller, &thread_id).await?;
    let restored_memory = {
        let mut sessions = sessions.lock().await;
        let mut restored_memory = None;
        if let Some(state) = sessions.get_mut(session_id) {
            if state.context_ownership == super::ContextOwnership::ReloadRequired {
                if peri_agent::session::memory_probe::observations_enabled() {
                    restored_memory =
                        Some(peri_agent::session::memory_probe::payload_set_snapshot(
                            &payloads,
                            payloads.capacity(),
                        ));
                }
                state.history_payloads = payloads;
                state.context_ownership = super::ContextOwnership::Resident;
            }
        }
        restored_memory
    };
    if let Some(snapshot) = restored_memory {
        peri_agent::session::memory_probe::log_payload_set("host_restore", session_id, &snapshot);
    }
    Ok(())
}

/// 执行发起失败后把热态标记为不可继续；正文已被移交，不能凭旧副本伪造成结果。
async fn mark_reload_required(sessions: &SharedSessions, session_id: &str) {
    let mut sessions = sessions.lock().await;
    if let Some(state) = sessions.get_mut(session_id) {
        state.history_payloads.clear();
        state.context_ownership = super::ContextOwnership::ReloadRequired;
        state.cancel_token = None;
        super::history_window::invalidate(state);
    }
}

pub(super) async fn finish_prompt_turn(
    sessions: &SharedSessions,
    session_id: &str,
    continuation: bool,
    result: executor::PromptResult,
    session_resources: Option<Arc<dyn peri_acp_types::session_resources::SessionResources>>,
) -> Result<Value, AcpError> {
    let compact_committed =
        result.history_replaced_by_compaction && !result.persistence_inconsistent;
    let mut commit_thread = None;
    let mut commit_generation = None;
    let mut commit_structure = None;
    let adoption_memory = {
        let mut sessions = sessions.lock().await;
        let mut adoption_memory = None;
        if let Some(state) = sessions.get_mut(session_id) {
            if result.persistence_inconsistent {
                // 提交结果不确定：不保留任何可继续执行的旧正文，也不删除身份。
                // 下一次 prompt / restore 从权威存储恢复（ReloadRequired）。
                // 窗口票据立即撤销：旧 snapshot/ticket 不再可信。
                state.history_payloads.clear();
                state.context_ownership = super::ContextOwnership::ReloadRequired;
                state.cancel_token = None;
                super::history_window::invalidate(state);
            } else {
                info!(
                    session_id,
                    payloads = result.persisted_payloads.len(),
                    compact = result.history_replaced_by_compaction,
                    ok = result.ok,
                    "Adopting canonical transcript snapshot"
                );
                // The writer barrier completes before PromptResult is built, even on cancel
                // or model/forwarder failure. The snapshot carries the effective payload set
                // only: bodies released by a successful Full Compact are absent (excluded).
                // Full canonical history is reconstructed from storage.
                if peri_agent::session::memory_probe::observations_enabled() {
                    adoption_memory = Some((
                        state.history_memory_snapshot(),
                        peri_agent::session::memory_probe::payload_set_snapshot(
                            &result.persisted_payloads,
                            result.persisted_payloads.capacity(),
                        ),
                    ));
                }
                state.history_payloads = result.persisted_payloads;
                state.context_ownership = super::ContextOwnership::Resident;
                if recall_overwrite_allowed(continuation) {
                    state.recall_items = result.recall_items;
                }
                state.cancel_token = None;
                if compact_committed {
                    commit_thread = Some(state.thread_id.clone());
                    commit_generation = Some(state.session_generation);
                    commit_structure = Some(state.structure_version);
                }
            }
        }
        adoption_memory
    };
    if let Some((before, after)) = adoption_memory {
        peri_agent::session::memory_probe::log_payload_adoption(session_id, &before, &after);
    }
    // Full compact 的 durable 确认已经在 PromptResult 构造前完成；这里只读取**存储事实**
    // 更新轻量 checkpoint/tail，不由 UI 事件反推提交成功。失败只撤销窗口可信元数据，
    // 不把已经提交成功的 turn 改成失败。
    if let (Some(resources), Some(thread_id), Some(generation), Some(structure)) = (
        session_resources,
        commit_thread,
        commit_generation,
        commit_structure,
    ) {
        match resources
            .history_window_bounds(&peri_acp_types::thread::ThreadId::from(thread_id))
            .await
        {
            Ok(bounds) => {
                let mut sessions = sessions.lock().await;
                if let Some(state) = sessions.get_mut(session_id) {
                    if state.session_generation == generation
                        && state.structure_version == structure
                        && !state.history_window.rewind_in_progress
                    {
                        super::history_window::publish_commit(
                            state,
                            bounds.checkpoint.as_ref(),
                            bounds.own_latest.as_ref(),
                        );
                    } else {
                        tracing::debug!(
                            session_id,
                            "discarding compact metadata read across a concurrent rewind"
                        );
                    }
                }
            }
            Err(error) => {
                tracing::warn!(session_id, error = %error, "history window commit metadata read failed");
                let mut sessions = sessions.lock().await;
                if let Some(state) = sessions.get_mut(session_id) {
                    super::history_window::revoke(state);
                }
            }
        }
    }
    // Fatal failures still use the standard JSON-RPC error; cancellation/max-iterations
    // retain their existing PromptResponse stop reasons after state cleanup.
    prompt_wire_response(
        result.failure.as_ref(),
        result.stop_reason,
        result.pending_tasks,
    )
}

/// [AsyncContinuation] 读取本轮 recall 的策略：
///
/// - 用户 prompt：`mem::take`（消费上一轮 recall 注入本轮的 system-reminder，
///   结束时由 `recall_overwrite_allowed` 回写新 recall）；
/// - 内部续跑：**clone 而非 take**——上一轮留给用户 prompt 的 recall 必须
///   保留在 `SessionState`（续跑自身也不注入，见 executor 的 continuation
///   分支），避免续跑"吞掉"用户 prompt 应得的 recall。
fn take_recall_for_turn(recall_items: &mut Vec<String>, continuation: bool) -> Vec<String> {
    if continuation {
        recall_items.clone()
    } else {
        std::mem::take(recall_items)
    }
}

/// 本轮结束时是否允许用 `result.recall_items` 覆盖 `SessionState.recall_items`。
///
/// 续跑结束时**不改变** SessionState 中的 recall（保留续跑开始前的值给后续
/// 用户 prompt）；用户 prompt 正常回写本轮产生的 recall。
fn recall_overwrite_allowed(continuation: bool) -> bool {
    !continuation
}

#[cfg(test)]
#[path = "prompt_test.rs"]
mod tests;
