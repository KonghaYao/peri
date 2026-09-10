//! ACP Prompt execution — builds and executes the agent via crate::executor.
//! Extracted from original acp_server.rs (2026-05-20 split).

use std::sync::Arc;

use crate::{
    broker::AcpTransportBroker,
    session::{event_sink::TransportEventSink, executor},
    transport::types::AcpError,
};
use agent_client_protocol::schema::v1::{PromptResponse, StopReason};
use peri_acp_types::interaction::{
    ApprovalDecision, ApprovalItem, InteractionContext, InteractionResponse, UserInteractionBroker,
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

// ── ACP 结果投影（spec/issues/2026-08-18-acp-error-handler.md D2）─────────────

/// fatal turn failure 的稳定 JSON-RPC server error code。
///
/// 取自 JSON-RPC 2.0 保留段 server error（`-32000..=-32099`）的首值，语义为
/// "agent turn execution failed"。具名常量替代调用点 magic number；`data`
/// 只携带稳定 allowlist 分类和可选 HTTP status，不包含 provider payload 或
/// 内部错误链（D2/D5）。
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
/// - `message`：直接使用 failure 的脱敏 public message（非空由
///   [`ExecutionFailure::internal`] 保证，空输入回落稳定 fallback）；
/// - `data`：稳定 allowlist `kind`，LLM HTTP 错误额外携带 `status`。
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
) -> Result<Value, AcpError> {
    if let Some(failure) = failure {
        return Err(execution_failure_to_acp_error(failure));
    }
    let acp_stop_reason = match stop_reason {
        executor::PromptStopReason::Cancelled => StopReason::Cancelled,
        executor::PromptStopReason::MaxTurnRequests => StopReason::MaxTurnRequests,
        executor::PromptStopReason::EndTurn => StopReason::EndTurn,
    };
    let resp = PromptResponse::new(acp_stop_reason);
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

pub(crate) fn build_transport_broker(
    transport: &Arc<dyn crate::transport::AcpTransport>,
    session_id: &str,
) -> Arc<dyn UserInteractionBroker> {
    Arc::new(
        AcpTransportBroker::new(
            Arc::new(crate::transport::AcpRequestBridge(Arc::clone(transport))),
            session_id.to_string().into(),
        )
        .with_timeout(crate::broker::ask_user_timeout()),
    )
}

/// Cron/loop 是被持久注册的代理执行权：每次触发在进入模型前重新审批。
/// Bypass 按既有模式契约直接允许；其他模式必须有 broker 并明确 Approve。
pub(crate) async fn approve_scheduled_trigger(
    permission_mode: &SharedPermissionMode,
    broker: Option<&Arc<dyn UserInteractionBroker>>,
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
        .request(InteractionContext::Approval {
            items: vec![ApprovalItem {
                tool_call_id: task_id.to_string(),
                tool_name: "cron_trigger".to_string(),
                tool_input: serde_json::json!({ "taskId": task_id, "prompt": prompt }),
            }],
        })
        .await;
    matches!(
        response,
        InteractionResponse::Decisions(decisions)
            if matches!(decisions.as_slice(), [ApprovalDecision::Approve { .. }])
    )
}

pub(crate) async fn run_prompt(
    params: Value,
    sessions: &SharedSessions,
    deployment: &super::AcpServerConfig,
    transport: &Arc<dyn crate::transport::AcpTransport>,
    pool: Arc<parking_lot::Mutex<crate::session::agent_pool::AgentPool>>,
    cont_tx: Option<tokio::sync::mpsc::UnboundedSender<executor::ContinuationRequest>>,
    continuation: bool,
) -> Result<Value, AcpError> {
    // Borrow deployment services; turn-owned callbacks clone only their existing handles.
    // Provider/config snapshots remain below, after the session snapshot is captured.
    let provider = &deployment.provider;
    let peri_config = &deployment.peri_config;
    let permission_mode = &deployment.permission_mode;
    let cron_scheduler = deployment.cron_scheduler.clone();
    let plugin_skill_roots = deployment.plugin_skill_roots.as_slice();
    let plugin_agent_dirs = deployment.plugin_agent_dirs.as_slice();
    let plugin_loaded = deployment.plugin_loaded.as_slice();
    let hook_groups = deployment.hook_groups.as_slice();
    let mcp_pool = deployment.mcp_pool.clone();
    let dynamic_mcp = deployment.dynamic_mcp.clone();
    let channel_state = deployment.channel_state.clone();
    let tool_search_index = deployment.tool_search_index.clone();
    let skills = deployment.skills.clone();
    let shared_tools = deployment.shared_tools.clone();
    let plugin_lsp_servers = deployment.plugin_lsp_servers.as_slice();
    let thread_store = &deployment.thread_store;
    let controller = &deployment.controller;
    let langfuse_session = deployment.langfuse_session.clone();
    let session_manager = deployment.session_manager.clone();
    let workflow_middleware_factory = &deployment.workflow_middleware_factory;
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

    // Create cancel token and register in sessions.
    // `AgentCancellationToken` 即 `tokio_util::sync::CancellationToken` 别名
    // （peri-agent re-export；ACP 协议面直接使用底层类型，不经业务 crate）。
    let cancel = tokio_util::sync::CancellationToken::new();
    {
        let mut sessions = sessions.lock().await;
        let state = sessions
            .get_mut(&session_id)
            .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
        state.cancel_token = Some(cancel.clone());
    }

    // Read session data under lock, then release immediately.
    let (
        cwd,
        history,
        history_payloads,
        is_empty,
        thread_id,
        frozen,
        incoming_recalls,
        workflow_middleware,
        lsp_pool,
    ) = {
        let mut sessions = sessions.lock().await;
        let state = sessions
            .get_mut(&session_id)
            .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
        (
            state.cwd.clone(),
            state
                .history_payloads
                .iter()
                .filter_map(|payload| payload.as_message().cloned())
                .collect::<Vec<_>>(),
            state.history_payloads.clone(),
            state.history_payloads.is_empty(),
            state.thread_id.clone(),
            state.frozen.clone(),
            // [AsyncContinuation] 续跑不 take recall：上一轮留给用户 prompt 的
            // recall 必须保留在 SessionState（后续用户 prompt 注入），续跑自身
            // 也不注入（见 executor::run_session_loop 的 continuation 分支）。
            take_recall_for_turn(&mut state.recall_items, continuation),
            state.workflow_middleware.clone(),
            state.lsp_pool.clone(),
        )
    };
    let history_len = history.len();
    let broker = build_transport_broker(transport, &session_id);
    let event_sink = Arc::new(TransportEventSink::new(
        Arc::clone(transport),
        session_manager.caps_registry(),
    ));

    let provider_snapshot = provider.read().clone();
    let peri_config_snapshot = Arc::new(peri_config.read().clone());

    // MetaHarness：从会话冻结数据投影（ARC-FROZEN-001——禁止从每 turn 的
    // 当前配置重建；frozen None（print mode 等防御路径）回落默认空状态）。
    // 设计 §2.5-2.6：装配与 workflow 渲染统一消费冻结状态。
    let meta_harness = frozen
        .as_ref()
        .map(|f| f.meta_harness().clone())
        .unwrap_or_default();

    // Create workflow executor (enables Workflow tool for multi-agent orchestration)
    // GAP-05: inject frozen data so workflow agents reuse SubAgent infra
    // p1-wa：执行体在 peri-agent（`agent::workflow`），ACP 侧构造注入面
    // （模型工厂 / 装配端口 / forwarder / publish hook / prompt fallback）。
    let workflow_executor = peri_agent::agent::workflow::create_executor(
        peri_agent::agent::workflow::WorkflowAgentContext {
            cwd: cwd.clone(),
            frozen_claude_md: frozen
                .as_ref()
                .and_then(|f| f.claude_md().map(|s| s.to_string())),
            frozen_claude_local_md: frozen
                .as_ref()
                .and_then(|f| f.claude_local_md().map(|s| s.to_string())),
            frozen_skill_summary: frozen
                .as_ref()
                .and_then(|f| f.skill_summary().map(|s| s.to_string())),
            session_id: Some(session_id.clone()),
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
            // （16_workflow 已删除（C2），主 prompt 即子面向唯一版本。）
            system_prompt: frozen.as_ref().map(|f| f.system_prompt().to_string()),
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
                Arc::clone(&skills),
                meta_harness.clone(),
            ),
            model_factory: crate::host::workflow_agent::build_model_factory(provider, peri_config),
            middleware_factory: Arc::clone(workflow_middleware_factory),
            system_prompt_fallback:
                crate::host::workflow_agent::build_workflow_system_prompt_fallback(
                    Arc::clone(&skills),
                    meta_harness.clone(),
                ),
            forwarder_launcher: crate::host::workflow_agent::build_workflow_forwarder_launcher(),
            publish_hook: Some(crate::host::workflow_agent::build_publish_hook(controller)),
            // Langfuse 观测：与迁移前一致（workflow agent 路径未启用遥测）。
            langfuse_hooks: None,
            langfuse_event_handler: None,
            // MetaHarness：装配期关闭集合（与段落覆盖同源，见上方 meta_harness
            // 投影——设计 §2.5）。
            meta_harness_disabled: meta_harness.disabled_middlewares.clone(),
        },
    );

    // ── L5：SessionContext 投影（provider / peri_config / pool / SessionManager /
    //    Controller 端口化——执行体迁入 peri-agent 后由本宿主构造注入面）──
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

    // 防御性 frozen 构建器（turn.frozen=None 回落；生产不可达）
    let frozen_fallback_builder: Option<executor::FrozenFallbackBuilder> = {
        let sm = session_manager.clone();
        let roots = plugin_skill_roots.to_vec();
        let dirs = plugin_agent_dirs.to_vec();
        Some(Arc::new(move |cwd, _language| {
            sm.build_frozen_data(cwd, &roots, &dirs)
        }))
    };

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
        claude_md_excludes,
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
        session_access: Some(
            Arc::new(session_manager) as Arc<dyn peri_acp_types::session::SessionAccessPort>
        ),
        thread_store: Some(Arc::clone(thread_store)),
        thread_id: Some(thread_id.clone()),
        plugin_skill_roots: plugin_skill_roots.to_vec(),
        plugin_agent_dirs: plugin_agent_dirs.to_vec(),
        plugin_loaded: plugin_loaded.to_vec(),
        hook_groups: hook_groups.to_vec(),
        cron_scheduler,
        mcp_pool,
        dynamic_mcp,
        session_mcp_capability,
        dynamic_mcp_projection,
        channel_state,
        tool_search_index,
        skills,
        shared_tools,
        lsp_servers: plugin_lsp_servers.to_vec(),
        lsp_pool,
        workflow_executor: Some(workflow_executor),
        workflow_middleware,
        event_publisher,
        subscribe,
        command_lookup,
        compact_config_loader,
        tool_invocation_resolver,
        session_start_source: if !continuation && is_empty {
            Some("startup".to_string())
        } else {
            None
        },
        request_id,
        allow_await_wake: true,
        continuation_notify: cont_tx,
        frozen_fallback_builder,
    };

    // ── L5：TurnInput 注入面（Langfuse hooks / stage 装配桥 / forwarder）──
    // Langfuse hooks：从 session 级 LangfuseSession 构造 tracer 并烘焙三个闭包
    //（turn 开始/结束 trace + 观测旁路 bridge 工厂）。
    let langfuse_hooks = telemetry::build_langfuse_hooks(langfuse_session.as_ref(), &session_id);

    // stage 装配桥：从 SessionContext 投影 StageBuildInput 并补齐注入面
    //（Langfuse bridge factory 经 turn 级 hooks 构造），再调用 ACP 装配桥。
    let stage_build = stage::build_stage_bridge(&ctx, langfuse_hooks.as_ref());

    // EventBus forwarder 启动器（Langfuse bridge 构造留在 ACP——观测旁路；
    // biased select 顺序不变量单点保持在 crate::event::spawn_eventbus_forwarder）。
    let forwarder_launcher =
        telemetry::build_forwarder_launcher(&ctx.provider_name, langfuse_hooks.as_ref());

    let turn = executor::TurnInput {
        event_sink,
        content,
        continuation,
        frozen,
        history,
        history_payloads: history_payloads.clone(),
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
    controller
        .run_session(&session_id)
        .await
        .map_err(|e| AcpError::new(-32603, format!("run_session failed: {e}")))?;
    let result = handle.take_result();

    // Persistence rollback/verification failure invalidates the in-memory session. Keeping the
    // old snapshot would allow a subsequent turn to fork from unknown durable state.
    let persistence_inconsistent = result.persistence_inconsistent;

    // Persist new messages to ThreadStore and update in-memory state.
    {
        let mut sessions = sessions.lock().await;
        if persistence_inconsistent {
            sessions.remove(&session_id);
        } else if let Some(state) = sessions.get_mut(&session_id) {
            if result.ok {
                info!(
                    session_id = %session_id,
                    payloads = result.persisted_payloads.len(),
                    compact = result.history_replaced_by_compaction,
                    "Agent execution completed with canonical transcript snapshot"
                );
                // The transcript writer is the sole execution-time store writer. Its barrier
                // completes before PromptResult is built; host only adopts that canonical
                // discriminated snapshot and never reconstructs persistence from model projection.
                state.history_payloads = result.persisted_payloads;
                state.history = state
                    .history_payloads
                    .iter()
                    .filter_map(|payload| payload.as_message().cloned())
                    .collect();
            } else {
                state.history.truncate(history_len);
                info!(session_id = %session_id, history_len, "Agent execution failed/cancelled without canonical progress");
            }
            // [AsyncContinuation] 续跑结束不回写 recall：保留续跑开始前
            // SessionState 中的 recall（上一轮留给用户 prompt 的），续跑自身
            // 产生的 recall 不覆盖它；用户 prompt 正常回写。
            if recall_overwrite_allowed(continuation) {
                state.recall_items = result.recall_items;
            }
            state.cancel_token = None;
        }
    }

    // ── ACP 结果投影（spec/issues/2026-08-18-acp-error-handler.md D2）──
    // 所有失败路径的历史保存 / session state / cancel token 清理 / recall 回写
    // 已在上方完成（result.messages / recall_items 已被消费）；此处只决定 wire
    // 形态：fatal failure → Err(AcpError)（统一 host 与 mpsc/stdio transport 共用
    // 同一标准 JSON-RPC error 路径），其余终止（cancel / max-iterations / end-turn）
    // → 现有成功 PromptResponse。
    prompt_wire_response(result.failure.as_ref(), result.stop_reason)
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
