//! ACP Prompt execution — builds and executes the agent via crate::executor.
//! Extracted from original acp_server.rs (2026-05-20 split).

use std::{collections::BTreeMap, sync::Arc};

use crate::{
    broker::AcpTransportBroker,
    session::{event_sink::TransportEventSink, executor},
    transport::types::AcpError,
};
use agent_client_protocol::schema::v1::{PromptResponse, StopReason};
use parking_lot::RwLock;
use peri_acp_types::cron::CronSchedulerPort;
use peri_acp_types::hooks::RegisteredHook;
use peri_acp_types::interaction::{
    ApprovalDecision, ApprovalItem, ChannelState, InteractionContext, InteractionResponse,
    UserInteractionBroker,
};
use peri_acp_types::permission::{PermissionMode, SharedPermissionMode};
use peri_acp_types::ports::{McpPoolPort, ToolSearchPort};
use peri_acp_types::session::{ExecutionFailure, ExecutionFailureKind};
use peri_controller::langfuse::bridge::LangfuseBridge;
use peri_controller::langfuse::tracer::LangfuseTracer;
use peri_controller::langfuse::LangfuseSession;
use serde_json::Value;
use tracing::info;

use peri_agent::session::exec::executor_helpers::{
    CommandLookupFn, ForwarderLauncherFn, StageBuildFn,
};
use peri_agent::session::exec::stage_builder::CachedLlmInstances;

use super::SharedSessions;
use crate::provider::{LlmProvider, PeriConfig};

#[cfg(test)]
fn rebuild_compacted_payloads(
    previous: &[peri_acp_types::store::PersistedPayload],
    projected: &[peri_acp_types::messages::BaseMessage],
) -> Vec<peri_acp_types::store::PersistedPayload> {
    use peri_acp_types::store::PersistedPayload;

    let reminders = previous
        .iter()
        .filter_map(|payload| match payload {
            PersistedPayload::SystemReminder { id, reminder } => Some((*id, reminder.clone())),
            PersistedPayload::Message(_) => None,
        })
        .collect::<std::collections::HashMap<_, _>>();
    projected
        .iter()
        .cloned()
        .map(|message| match reminders.get(&message.id()) {
            Some(reminder) => PersistedPayload::SystemReminder {
                id: message.id(),
                reminder: reminder.clone(),
            },
            None => PersistedPayload::Message(message),
        })
        .collect()
}

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

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_prompt(
    params: Value,
    sessions: &SharedSessions,
    provider: &Arc<RwLock<LlmProvider>>,
    peri_config: &Arc<RwLock<PeriConfig>>,
    permission_mode: &Arc<SharedPermissionMode>,
    cron_scheduler: Option<Arc<dyn CronSchedulerPort>>,
    plugin_skill_roots: &[peri_acp_types::skills::SkillRoot],
    plugin_agent_dirs: &[std::path::PathBuf],
    plugin_loaded: &[peri_acp_types::plugin::LoadedPlugin],
    hook_groups: &[Vec<peri_acp_types::hooks::RegisteredHook>],
    mcp_pool: Option<Arc<dyn McpPoolPort>>,
    dynamic_mcp: Option<Arc<dyn peri_acp_types::ports::DynamicMcpDeploymentPort>>,
    channel_state: Option<Arc<ChannelState>>,
    tool_search_index: Arc<dyn ToolSearchPort>,
    skills: Arc<dyn peri_acp_types::ports::SkillsPort>,
    shared_tools: Arc<RwLock<BTreeMap<String, Arc<dyn peri_agent::tools::BaseTool>>>>,
    plugin_lsp_servers: &[peri_acp_types::lsp::LspServerConfig],
    transport: &Arc<dyn crate::transport::AcpTransport>,
    thread_store: &Arc<dyn peri_acp_types::store::ThreadStore>,
    controller: &Arc<peri_controller::Controller>,
    langfuse_session: Option<Arc<LangfuseSession>>,
    pool: Arc<parking_lot::Mutex<crate::session::agent_pool::AgentPool>>,
    session_manager: crate::session::SessionManager,
    // p1-wa：workflow agent 装配端口（宿主装配点注入，ACP 侧只持端口）。
    workflow_middleware_factory: &Arc<dyn peri_agent::agent::workflow::WorkflowMiddlewareFactory>,
    // 内部 continuation 通知通道（注入 SessionContext，供 on_bg_complete
    // 闭包通知 server 的 continuation scheduler）。stdio 等无 scheduler 场景为 None。
    cont_tx: Option<
        tokio::sync::mpsc::UnboundedSender<crate::session::executor::ContinuationRequest>,
    >,
    // 内部 AsyncContinuation（bg 完成唤醒被取消的 turn）：不 push 空 user
    // prompt、不触发 keepgoing 语义。仅由 continuation scheduler 调用。
    continuation: bool,
    // stdio 部署过滤 rewind/clear（仅 stdio 置 true；TUI/print 恒 false）。
    stdio_command_filter: bool,
) -> Result<Value, AcpError> {
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
    // Every canonical payload projects to exactly one model message. This is the only
    // safe prefix boundary when reminders are interleaved with ordinary messages.
    let _projected_history_len = history_payloads.len();
    // Compact replacement must delete every canonical row, including reminder rows.
    let _history_ids: Vec<peri_acp_types::messages::MessageId> = history_payloads
        .iter()
        .map(|payload| payload.id())
        .collect();

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

    // Track first history message ID for cancel-with-progress path (history is moved below)
    // Uses Option<MessageId> (16 bytes) instead of cloning the entire history.
    let _first_history_id = history.first().map(|m| m.id());

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
        let sid = session_id.clone();
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
    let langfuse_hooks: Option<executor::LangfuseHooks> = langfuse_session.as_ref().map(|s| {
        let session_clone = Arc::clone(s);
        let config = session_clone.config.clone();
        let session: std::sync::Arc<dyn peri_controller::langfuse::LangfuseSessionLike> =
            session_clone;
        let tracer = Arc::new(parking_lot::Mutex::new(LangfuseTracer::new(
            session,
            session_id.clone(),
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
                Arc::new(
                    move |outcome: peri_acp_types::session::TurnTelemetryOutcome| {
                        tracer.lock().on_turn_end(outcome).into()
                    },
                )
                    as Arc<
                        dyn Fn(
                                peri_acp_types::session::TurnTelemetryOutcome,
                            ) -> Option<tokio::task::JoinHandle<()>>
                            + Send
                            + Sync,
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
    let ctx_for_stage = ctx.clone();
    let bridge_factory_for_stage: Option<
        Arc<dyn Fn() -> Arc<dyn peri_agent::agent::LangfuseBridgeLike> + Send + Sync>,
    > = langfuse_hooks.as_ref().map(|h| {
        let bf = Arc::clone(&h.bridge_factory);
        let provider_display = ctx_for_stage.provider_name.clone();
        Arc::new(move || {
            bf(provider_display.clone(), None)
                .expect("stage bridge_factory: hooks 存在时 bridge 构造必须成功")
        }) as Arc<dyn Fn() -> Arc<dyn peri_agent::agent::LangfuseBridgeLike> + Send + Sync>
    });
    let stage_build: StageBuildFn = Arc::new(move |sbr| {
        // compact hook 闭包在每次装配时构造（hook_groups 非空才产生动作；
        // 与迁移前 stage_builder 内构造时机逐次一致）
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
            bridge_factory_for_stage.clone(),
        )
    });

    // EventBus forwarder 启动器（Langfuse bridge 构造留在 ACP——观测旁路；
    // biased select 顺序不变量单点保持在 crate::event::spawn_eventbus_forwarder）。
    let forwarder_launcher: ForwarderLauncherFn = {
        let provider_display = ctx.provider_name.clone();
        let bridge_factory = langfuse_hooks
            .as_ref()
            .map(|h| Arc::clone(&h.bridge_factory));
        Arc::new(move |handles, agent_id, on_event| {
            let bridge: Option<LangfuseBridge> = bridge_factory
                .as_ref()
                .and_then(|bf| bf(provider_display.clone(), Some(agent_id.clone())))
                .and_then(|b| {
                    // LangfuseBridgeLike: Any 上界（L5）——trait upcasting 还原具体类型
                    let any: Arc<dyn std::any::Any + Send + Sync> = b;
                    any.downcast::<LangfuseBridge>().ok().map(|b| (*b).clone())
                });
            crate::event::spawn_eventbus_forwarder(handles, on_event, bridge)
        })
    };

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

    finish_prompt_turn(sessions, &session_id, continuation, result).await
}

// Durable progress is independent of the terminal status. This boundary also owns wire
// projection so that cancellation/error responses can never bypass canonical state adoption.
pub(super) async fn finish_prompt_turn(
    sessions: &SharedSessions,
    session_id: &str,
    continuation: bool,
    result: executor::PromptResult,
) -> Result<Value, AcpError> {
    {
        let mut sessions = sessions.lock().await;
        if result.persistence_inconsistent {
            // A failed barrier leaves only the disk prefix authoritative. Do not allow
            // another prompt to continue from an unverified in-memory snapshot.
            sessions.remove(session_id);
        } else if let Some(state) = sessions.get_mut(session_id) {
            info!(
                session_id,
                payloads = result.persisted_payloads.len(),
                compact = result.history_replaced_by_compaction,
                ok = result.ok,
                "Adopting canonical transcript snapshot"
            );
            // The writer barrier completes before PromptResult is built, even on cancel
            // or model/forwarder failure. Early failures return the previous snapshot.
            state.history_payloads = result.persisted_payloads;
            state.history = state
                .history_payloads
                .iter()
                .filter_map(|payload| payload.as_message().cloned())
                .collect();
            if recall_overwrite_allowed(continuation) {
                state.recall_items = result.recall_items;
            }
            state.cancel_token = None;
        }
    }
    // Fatal failures still use the standard JSON-RPC error; cancellation/max-iterations
    // retain their existing PromptResponse stop reasons after state cleanup.
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

/// 构造 compact plugin hook 回调（宿主装配面职责，L5 归位自
/// host/stage_builder.rs：hook_groups 非空时构造 `fire_pre_compact` /
/// `fire_post_compact` 转发闭包；语义同迁移前——tokio::spawn 转发、不阻塞
/// 管线；hook_groups 为空返回 `(None, None)`）。
#[allow(clippy::type_complexity)]
pub(crate) fn build_compact_hooks(
    hook_groups: &[Vec<RegisteredHook>],
    cwd: &str,
    session_id: &str,
    model: &str,
) -> (
    Option<Arc<dyn Fn() + Send + Sync>>,
    Option<Arc<dyn Fn(bool, usize) + Send + Sync>>,
) {
    let hook_groups_flat: Vec<RegisteredHook> = hook_groups.iter().flatten().cloned().collect();
    if hook_groups_flat.is_empty() {
        return (None, None);
    }
    let cwd = cwd.to_string();
    let sid = session_id.to_string();
    let model = model.to_string();
    let pre: Arc<dyn Fn() + Send + Sync> = {
        let hooks = hook_groups_flat.clone();
        let cwd = cwd.clone();
        let sid = sid.clone();
        let model = model.clone();
        Arc::new(move || {
            let hooks = hooks.clone();
            let cwd = cwd.clone();
            let sid = sid.clone();
            let model = model.clone();
            tokio::spawn(async move {
                peri_middlewares::hooks::stage_firing::fire_pre_compact(
                    &hooks, &cwd, &sid, "", &model, 0,
                )
                .await;
            });
        })
    };
    let post: Arc<dyn Fn(bool, usize) + Send + Sync> = {
        let hooks = hook_groups_flat.clone();
        let cwd = cwd.clone();
        let sid = sid.clone();
        let model = model.clone();
        Arc::new(move |_compacted: bool, affected_count: usize| {
            let hooks = hooks.clone();
            let cwd = cwd.clone();
            let sid = sid.clone();
            let model = model.clone();
            tokio::spawn(async move {
                peri_middlewares::hooks::stage_firing::fire_post_compact(
                    &hooks,
                    &cwd,
                    &sid,
                    "",
                    &model,
                    affected_count,
                )
                .await;
            });
        })
    };
    (Some(pre), Some(post))
}

/// 本轮结束时是否允许用 `result.recall_items` 覆盖 `SessionState.recall_items`。
///
/// 续跑结束时**不改变** SessionState 中的 recall（保留续跑开始前的值给后续
/// 用户 prompt）；用户 prompt 正常回写本轮产生的 recall。
fn recall_overwrite_allowed(continuation: bool) -> bool {
    !continuation
}

/// Returns `None` when a partial result omits existing history. A committed Full Compact
/// explicitly replaces prior visible messages with its persisted summary, so it is accepted.
#[cfg(test)]
fn strip_leaked_prepends(
    result_messages: &[peri_acp_types::messages::BaseMessage],
    first_history_id: Option<peri_acp_types::messages::MessageId>,
    full_compaction_committed: bool,
) -> Option<Vec<peri_acp_types::messages::BaseMessage>> {
    match first_history_id {
        Some(first_id) => {
            // Find where original history starts in result (skip leaked prepends).
            if let Some(start) = result_messages.iter().position(|m| m.id() == first_id) {
                Some(result_messages[start..].to_vec())
            } else if full_compaction_committed {
                Some(
                    result_messages
                        .iter()
                        .skip_while(|m| m.is_system())
                        .cloned()
                        .collect(),
                )
            } else {
                None
            }
        }
        None => {
            // Original history was empty — strip leading system messages (all prepends).
            Some(
                result_messages
                    .iter()
                    .skip_while(|m| m.is_system())
                    .cloned()
                    .collect(),
            )
        }
    }
}

#[cfg(test)]
#[path = "prompt_test.rs"]
mod tests;
