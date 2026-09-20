//! Session 生命周期命令 handler：initialize / new / load / list /
//! cancel-bg-task / close / delete / resume / fork / rename（自 requests.rs
//! 拆出，请求分发见 `host/requests.rs`）。

use std::collections::HashMap;
use std::sync::Arc;

use agent_client_protocol::schema::v1::{
    CloseSessionResponse, DeleteSessionResponse, ForkSessionResponse, ListSessionsResponse,
    LoadSessionResponse, NewSessionResponse, ResumeSessionResponse, SessionId, SessionNotification,
};
use peri_acp_types::ports::WorkflowMiddlewarePort;
use peri_acp_types::thread::ThreadMeta;
use peri_acp_types::workspace::ReadOnlyAdmission;
use peri_acp_types::PeriCaps;
use serde_json::Value;
use tracing::{info, warn};

use super::super::notify::{send_available_commands_update, send_config_option_update};
use super::super::workspace::BindingCheck;
use super::super::{build_mode_state, AcpServerConfig, SessionState};
use crate::dispatch::config_update::make_config_options;
use crate::dispatch::ReplaySender;
use crate::session::frozen_snapshot::{decode_frozen_snapshot, encode_frozen_snapshot};
use crate::{dispatch, transport::types::AcpError};

#[path = "legacy_session.rs"]
mod legacy_session;

async fn store_frozen_snapshot(
    cfg: &AcpServerConfig,
    session_id: &str,
    frozen_data: &crate::session::executor::FrozenSessionData,
) -> Result<bool, AcpError> {
    let snapshot = encode_frozen_snapshot(frozen_data).map_err(|error| {
        AcpError::new(-32603, format!("Frozen snapshot encode failed: {error}"))
    })?;
    cfg.thread_store
        .store_frozen_snapshot_if_absent(&session_id.to_string(), &snapshot)
        .await
        .map_err(|error| AcpError::new(-32603, format!("Frozen snapshot store failed: {error}")))
}

async fn store_new_frozen_snapshot_or_compensate(
    cfg: &AcpServerConfig,
    session_id: &str,
    frozen_data: &crate::session::executor::FrozenSessionData,
) -> Result<(), AcpError> {
    let stored = store_frozen_snapshot(cfg, session_id, frozen_data).await;
    if !matches!(stored, Ok(true)) {
        let error = match stored {
            Ok(false) => AcpError::new(
                -32603,
                format!("Frozen snapshot already exists for new session: {session_id}"),
            ),
            Err(error) => error,
            Ok(true) => unreachable!(),
        };
        if let Err(cleanup_error) = cfg
            .thread_store
            .delete_thread(&session_id.to_string())
            .await
        {
            warn!(
                session_id,
                error = %cleanup_error,
                "failed to compensate thread after frozen snapshot store failure"
            );
        }
        return Err(error);
    }
    Ok(())
}

async fn load_frozen_data(
    cfg: &AcpServerConfig,
    session_id: &str,
) -> Result<crate::session::executor::FrozenSessionData, AcpError> {
    let snapshot = cfg
        .controller
        .sessions()
        .load_frozen_snapshot(&session_id.to_owned())
        .await
        .map_err(super::super::workspace::workspace_error)?
        .ok_or_else(|| AcpError::new(-32603, "Bound session has no frozen snapshot"))?;
    decode_frozen_snapshot(&snapshot).map_err(super::super::workspace::workspace_error)
}

/// 一次恢复准入的结果。
pub(super) struct PreparedSession {
    pub(super) id: String,
    pub(super) identity: Option<Value>,
    /// 只读准入原因：`Some` 表示本次没有取得执行所有权，历史可读、执行与写入仍被
    /// `require_owner` 挡住。
    pub(super) read_only: Option<ReadOnlyAdmission>,
}

async fn prepare_existing(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
) -> Result<PreparedSession, AcpError> {
    let id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
    legacy_session::prepare_for_restore(cfg, id, params.get("cwd").and_then(Value::as_str)).await?;
    let admission = super::super::workspace::acquire_for_load(
        cfg,
        sessions,
        id,
        params.get("cwd").and_then(Value::as_str),
    )
    .await?;
    let workspace = admission.workspace;
    let (owner, read_only) = match admission.execution {
        super::super::workspace::ExecutionAdmission::Owned(owner) => (Some(owner), None),
        // 执行所有权不可得（他处持有 / 待恢复 / 本节点只读）：不再用错误信息挡住进入，
        // 改为只读进入并记 warning。独占语义不变——写入与执行仍要所有权。
        super::super::workspace::ExecutionAdmission::Unavailable(reason) => {
            if !cfg
                .session_manager
                .effective_host_caps()
                .session_workspace_v1
            {
                // 未协商只读标记的客户端无法得知本次准入只读，仍按原语义失败。
                return Err(super::super::workspace::read_only_error(reason));
            }
            warn!(
                session_id = %id,
                reason = ?reason,
                "session admitted read-only: execution ownership is unavailable"
            );
            (None, Some(reason))
        }
    };
    let identity = match response_identity(cfg, id).await {
        Ok(identity) => identity,
        Err(error) => {
            if !sessions.contains_key(id) {
                if let Some(owner) = owner.as_ref() {
                    owner
                        .mark_clean()
                        .await
                        .map_err(super::super::workspace::workspace_error)?;
                }
            }
            return Err(error);
        }
    };
    if let Some(state) = sessions.get_mut(id) {
        if state.history_payloads.is_empty() {
            let payloads = dispatch::load_session_payloads(cfg.controller.as_ref(), id).await?;
            state.history = payloads
                .iter()
                .filter_map(|payload| payload.as_message().cloned())
                .collect();
            state.history_payloads = payloads;
        }
        // 只读会话刚取回执行所有权：既有的只读状态没有执行环境，按新会话重建。
        let upgrading = state.execution_owner.is_none() && read_only.is_none();
        if !upgrading {
            return Ok(PreparedSession {
                id: id.to_owned(),
                identity,
                read_only,
            });
        }
        sessions.remove(id);
    }
    let prepared = async {
        let payloads = dispatch::load_session_payloads(cfg.controller.as_ref(), id).await?;
        let cwd = workspace
            .cwd
            .to_str()
            .ok_or_else(|| AcpError::new(-32602, "Execution directory is not UTF-8"))?
            .to_owned();
        // 只读准入不建执行环境：不要求 frozen 快照存在，也不启动 workflow / LSP。
        let (frozen, environment, workflow_middleware, lsp_pool) = match owner.as_ref() {
            Some(_) => {
                let frozen = load_frozen_data(cfg, id).await?;
                let environment =
                    super::super::workspace::SessionEnvironment::assemble(cfg, &cwd, id).await?;
                let local = environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg);
                let workflow_middleware =
                    create_session_workflow_middleware(local, &cwd, id, &frozen);
                let lsp_pool = create_session_lsp_pool(local, &cwd);
                (Some(frozen), environment, workflow_middleware, lsp_pool)
            }
            None => (None, None, None, None),
        };
        let local = environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg);
        local.session_manager.ensure_session(id, &cwd);
        local.session_manager.ensure_session_caps(id);
        Ok::<_, AcpError>(SessionState {
            session_id: id.to_owned(),
            thread_id: id.to_owned(),
            cwd,
            execution_owner: owner.clone(),
            environment,
            closing: false,
            history: payloads
                .iter()
                .filter_map(|p| p.as_message().cloned())
                .collect(),
            history_payloads: payloads,
            cancel_token: None,
            frozen,
            recall_items: Vec::new(),
            agent_pool: crate::session::agent_pool::AgentPool::new(),
            workflow_middleware,
            lsp_pool,
            title: None,
            tags: Vec::new(),
            continuation_armed: false,
            continuation_epoch: 0,
            continuation_in_flight: false,
            continuation_mq_steering_pending: false,
            lease: super::super::lease::WriterLease::acquired("default"),
        })
    }
    .await;
    match prepared {
        Ok(state) => {
            if let Some(environment) = &state.environment {
                environment.activate();
            }
            sessions.insert(id.to_owned(), state);
        }
        Err(error) => {
            if let Some(owner) = owner {
                owner
                    .mark_clean()
                    .await
                    .map_err(super::super::workspace::workspace_error)?;
            }
            return Err(error);
        }
    }
    Ok(PreparedSession {
        id: id.to_owned(),
        identity,
        read_only,
    })
}

/// 装配准入响应：会话身份载荷 + 本次准入是否只读（只读标记只在协商了
/// `sessionWorkspaceV1` 的客户端上出现，见 `prepare_existing` 的降级前置条件）。
fn identity_response(
    mut response: Value,
    identity: Option<Value>,
    read_only: Option<ReadOnlyAdmission>,
) -> Result<Value, AcpError> {
    if let Some(identity) = identity {
        response["_meta"]["peri.sessionWorkspaceV1"] = identity;
        if let Some(reason) = read_only {
            response["_meta"]["peri.sessionWorkspaceV1"]["read_only"] =
                serde_json::to_value(reason).map_err(|e| AcpError::new(-32603, e.to_string()))?;
        }
    }
    Ok(response)
}

async fn response_identity(
    cfg: &AcpServerConfig,
    session_id: &str,
) -> Result<Option<Value>, AcpError> {
    if cfg
        .session_manager
        .effective_host_caps()
        .session_workspace_v1
    {
        admission_identity(cfg, session_id).await.map(Some)
    } else {
        Ok(None)
    }
}

pub(super) fn retain_failed_assembly(
    sessions: &mut HashMap<String, SessionState>,
    id: &str,
    cwd: &str,
    owner: Arc<dyn peri_acp_types::workspace::SessionExecutionLease>,
    environment: Arc<super::super::workspace::SessionEnvironment>,
) {
    sessions.insert(
        id.to_owned(),
        SessionState {
            session_id: id.to_owned(),
            thread_id: id.to_owned(),
            cwd: cwd.to_owned(),
            execution_owner: Some(owner),
            environment: Some(environment),
            closing: true,
            history: Vec::new(),
            history_payloads: Vec::new(),
            cancel_token: None,
            frozen: None,
            recall_items: Vec::new(),
            agent_pool: crate::session::agent_pool::AgentPool::new(),
            workflow_middleware: None,
            lsp_pool: None,
            title: None,
            tags: Vec::new(),
            continuation_armed: false,
            continuation_epoch: 0,
            continuation_in_flight: false,
            continuation_mq_steering_pending: false,
            lease: super::super::lease::WriterLease::acquired("default"),
        },
    );
}

/// 绑定复核强度见 [`BindingCheck`](super::super::workspace::BindingCheck)：
/// 协议读请求复核完整发现快照，同一次准入内的身份读取只复核已记录证据。
async fn context_for_session(cfg: &AcpServerConfig, session_id: &str) -> Result<Value, AcpError> {
    session_context_payload(cfg, session_id, BindingCheck::Full).await
}

/// 准入内的身份读取（`session/new` | `load` | `resume` | `fork` 的响应装配）。
async fn admission_identity(cfg: &AcpServerConfig, session_id: &str) -> Result<Value, AcpError> {
    session_context_payload(cfg, session_id, BindingCheck::Recorded).await
}

async fn session_context_payload(
    cfg: &AcpServerConfig,
    session_id: &str,
    check: BindingCheck,
) -> Result<Value, AcpError> {
    let store = cfg.controller.sessions();
    let binding = store
        .load_session_binding(&session_id.to_owned())
        .await
        .map_err(super::super::workspace::workspace_error)?;
    let meta = store
        .load_meta(&session_id.to_owned())
        .await
        .map_err(super::super::workspace::workspace_error)?;
    let workspace = if binding.is_some() {
        let id = session_id.to_owned();
        match check {
            BindingCheck::Full => store.validate_session_binding(&id).await,
            BindingCheck::Recorded => store.reassert_session_binding(&id).await,
        }
        .map_err(super::super::workspace::workspace_error)?
    } else {
        // Resolve the saved location for a restore request; context reads never adopt it.
        legacy_session::resolve_saved_workspace(cfg, &meta).await?
    };
    Ok(
        serde_json::json!({ "version": 1, "workspace": workspace, "binding": binding, "title": meta.title }),
    )
}

pub(crate) async fn handle_context(
    params: &Value,
    cfg: &AcpServerConfig,
) -> Result<Value, AcpError> {
    if !cfg
        .session_manager
        .effective_host_caps()
        .session_workspace_v1
    {
        return Err(AcpError::new(
            -32601,
            "Session workspace capability was not negotiated",
        ));
    }
    match (
        params.get("sessionId").and_then(Value::as_str),
        params.get("cwd").and_then(Value::as_str),
    ) {
        (Some(id), None) => context_for_session(cfg, id).await,
        (None, Some(cwd)) => {
            let workspace = cfg
                .controller
                .sessions()
                .resolve_workspace(std::path::Path::new(cwd))
                .await
                .map_err(super::super::workspace::workspace_error)?;
            Ok(serde_json::json!({ "version": 1, "workspace": workspace }))
        }
        _ => Err(AcpError::new(
            -32602,
            "Specify exactly one of sessionId or cwd",
        )),
    }
}

pub(crate) async fn handle_metadata(
    params: &Value,
    cfg: &AcpServerConfig,
    history: bool,
) -> Result<Value, AcpError> {
    if !cfg
        .session_manager
        .effective_host_caps()
        .session_workspace_v1
    {
        return Err(AcpError::new(
            -32601,
            "Session workspace capability was not negotiated",
        ));
    }
    let id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?
        .to_owned();
    let store = cfg.controller.sessions();
    let meta = store
        .load_meta(&id)
        .await
        .map_err(super::super::workspace::workspace_error)?;
    let mut response = serde_json::json!({ "sessionId": id, "title": meta.title, "cwd": meta.cwd, "permissionMode": build_mode_state(&cfg.permission_mode).current_mode_id.to_string(), "modelAlias": cfg.peri_config.read().config.active_alias });
    {
        let provider = cfg.provider.read();
        response["modelName"] = Value::String(provider.model_name().to_owned());
        let effort = match &*provider {
            crate::provider::LlmProvider::OpenAi { effort, .. }
            | crate::provider::LlmProvider::Anthropic { effort, .. } => effort.clone(),
        };
        response["effort"] = serde_json::json!(effort);
        let config = cfg.peri_config.read();
        response["providerName"] = serde_json::json!(config
            .config
            .profiles
            .get(&config.config.active_alias)
            .map(|profile| profile.provider.clone()));
    }
    if history {
        let payloads = store
            .load_context_payloads(&id)
            .await
            .map_err(super::super::workspace::workspace_error)?;
        response["payloads"] = Value::Array(
            payloads
                .iter()
                .map(|payload| {
                    let encoded = peri_acp_types::store::serialize_persisted_payload(payload)?;
                    Ok::<Value, anyhow::Error>(serde_json::from_str(&encoded)?)
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(super::super::workspace::workspace_error)?,
        );
        response["binding"] = serde_json::to_value(
            store
                .load_session_binding(&id)
                .await
                .map_err(super::super::workspace::workspace_error)?,
        )
        .map_err(super::super::workspace::workspace_error)?;
    }
    Ok(response)
}

/// 创建 session 级 WorkflowMiddleware（session/new / load / resume 共用，GAP-05）。
///
/// 构造收拢在 host 装配面（`host/workflow_agent.rs` 薄壳：executor 注入面 +
/// 端口装配），命令面只持 `Arc<dyn WorkflowMiddlewarePort>`（3.0 批 2
/// 波 2 装配边界收口；p1-wa：执行体在 peri-agent，装配经
/// `workflow_middleware_factory` 端口）。
fn create_session_workflow_middleware(
    cfg: &AcpServerConfig,
    cwd: &str,
    session_id: &str,
    frozen_data: &crate::session::executor::FrozenSessionData,
) -> Option<Arc<dyn WorkflowMiddlewarePort>> {
    let middleware = crate::host::workflow_agent::create_session_workflow_middleware(
        Arc::clone(&cfg.provider),
        &cfg.peri_config,
        cwd,
        session_id,
        frozen_data,
        Arc::clone(&cfg.workflow_middleware_factory),
        // session 级路径与迁移前一致，不启用事件发布（workflow 事件仅由
        // 内部 handler 消费：usage/progress）；统一发射接线留待单独裁定。
        None,
        Arc::clone(&cfg.skills),
    );
    if let (Some(middleware), Some(session)) =
        (&middleware, cfg.session_manager.get_session(session_id))
    {
        middleware.set_bg_registry(session.task_manager.clone());
    }
    middleware
}

/// 创建 session 级 LSP 服务器池（session/new / load / resume / fork 共用，H1）。
///
/// 会话级实例跨 turn 复用（服务器进程 / initialized / 诊断状态不丢），
/// 宿主退出（`run_acp_server` 返回）时经端口 `shutdown` 优雅关闭。
/// 无 LSP 配置时返回 None（不注册 LSP 中间件）。
fn create_session_lsp_pool(
    cfg: &AcpServerConfig,
    cwd: &str,
) -> Option<Arc<dyn peri_acp_types::ports::LspPoolPort>> {
    peri_middlewares::assembly::create_session_lsp_pool(cwd, &cfg.plugin_lsp_servers)
}

pub(crate) fn handle_initialize(params: &Value, cfg: &AcpServerConfig) -> Result<Value, AcpError> {
    let version = params
        .get("protocolVersion")
        .and_then(|v| v.as_u64())
        .unwrap_or(1);
    info!(protocol_version = %version, "ACP initialize");

    // 解析 clientCapabilities._meta 中的 peri 自定义 flag
    let peri_caps = params
        .get("clientCapabilities")
        .and_then(|c| c.get("_meta"))
        .and_then(|m| m.as_object())
        .map(PeriCaps::from_client_meta)
        .unwrap_or_default();

    // 暂存 caps，session/new 时 consume
    cfg.session_manager.set_pending_caps(peri_caps.clone());

    let resp = dispatch::build_initialize_response(&peri_caps);
    serde_json::to_value(resp).map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
}

pub(crate) async fn handle_new(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
) -> Result<Value, AcpError> {
    let store = cfg.controller.sessions();
    let requested = params.get("cwd").and_then(Value::as_str).unwrap_or(".");
    let workspace = store
        .resolve_workspace(std::path::Path::new(requested))
        .await
        .map_err(super::super::workspace::workspace_error)?;
    let cwd = workspace
        .cwd
        .to_str()
        .ok_or_else(|| AcpError::new(-32602, "Execution directory is not UTF-8"))?
        .to_owned();
    let thread_id = store
        .create_bound_thread(ThreadMeta::new(&cwd), &workspace)
        .await
        .map_err(super::super::workspace::workspace_error)?;
    let session_id = thread_id.clone();
    let owner = store
        .acquire_execution_lease(&thread_id)
        .await
        .map_err(super::super::workspace::workspace_error)?;
    if let Err(error) =
        super::super::workspace::reassert_expected(cfg, &thread_id, Some(&cwd)).await
    {
        store
            .delete_thread(&thread_id)
            .await
            .map_err(super::super::workspace::workspace_error)?;
        owner
            .mark_clean()
            .await
            .map_err(super::super::workspace::workspace_error)?;
        return Err(error);
    }
    let identity = match response_identity(cfg, &session_id).await {
        Ok(identity) => identity,
        Err(error) => {
            store
                .delete_thread(&thread_id)
                .await
                .map_err(super::super::workspace::workspace_error)?;
            owner
                .mark_clean()
                .await
                .map_err(super::super::workspace::workspace_error)?;
            return Err(error);
        }
    };
    let environment =
        match super::super::workspace::SessionEnvironment::assemble(cfg, &cwd, &session_id).await {
            Ok(environment) => environment,
            Err(error) => {
                store
                    .delete_thread(&thread_id)
                    .await
                    .map_err(super::super::workspace::workspace_error)?;
                owner
                    .mark_clean()
                    .await
                    .map_err(super::super::workspace::workspace_error)?;
                return Err(error);
            }
        };
    let cfg = environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg);

    // ── Freeze system prompt data at session creation ──
    // 通过 SessionManager 统一构造路径，并登记 AcpSession 记录以支撑
    // cascade cancel 子 agent 与 goal_state（见 SessionManager::ensure_session）。
    // GAP-05: frozen data 在 WorkflowMiddleware 创建前构建，注入到 executor。
    let frozen_data = cfg.session_manager.build_frozen_data(
        &cwd,
        &cfg.plugin_skill_roots,
        &cfg.plugin_agent_dirs,
    );
    if let Err(error) =
        store_new_frozen_snapshot_or_compensate(cfg, &session_id, &frozen_data).await
    {
        if let Some(environment) = environment.as_ref() {
            if !environment.shutdown().await {
                retain_failed_assembly(
                    sessions,
                    &session_id,
                    &cwd,
                    owner.clone(),
                    environment.clone(),
                );
                return Err(AcpError::new(
                    -32010,
                    "Session assembly cleanup incomplete; resources retained for shutdown retry",
                ));
            }
        }
        owner
            .mark_clean()
            .await
            .map_err(super::super::workspace::workspace_error)?;
        return Err(error);
    }
    cfg.session_manager.ensure_session(&session_id, &cwd);

    // Create session-scoped WorkflowMiddleware at session/new (GAP-05: inject frozen data)
    let workflow_middleware =
        create_session_workflow_middleware(cfg, &cwd, &session_id, &frozen_data);
    // Create session-scoped LspServerPool at session/new（H1：跨 turn 复用）
    let lsp_pool = create_session_lsp_pool(cfg, &cwd);

    sessions.insert(
        session_id.clone(),
        SessionState {
            session_id: session_id.clone(),
            thread_id: thread_id.clone(),
            cwd: cwd.clone(),
            execution_owner: Some(owner),
            environment: environment.clone(),
            closing: false,
            history: Vec::new(),
            history_payloads: Vec::new(),
            cancel_token: None,
            frozen: Some(frozen_data),
            recall_items: Vec::new(),
            agent_pool: crate::session::agent_pool::AgentPool::new(),
            workflow_middleware,
            lsp_pool,
            title: None,
            tags: Vec::new(),
            continuation_armed: false,
            continuation_epoch: 0,
            continuation_in_flight: false,
            continuation_mq_steering_pending: false,
            lease: super::super::lease::WriterLease::acquired("default"),
        },
    );

    if let Some(environment) = &environment {
        environment.activate();
    }
    info!(session_id = %session_id, "ACP session created with ThreadStore");
    let modes = build_mode_state(&cfg.permission_mode);
    let config_options = {
        let c = cfg.peri_config.read();
        let p = cfg.provider.read();
        make_config_options(&c, &p, cfg.permission_mode.load())
    };
    let resp = NewSessionResponse::new(SessionId::new(&*session_id))
        .modes(modes)
        .config_options(config_options);
    // 将暂存的 peri caps 关联到新 session（MpscTransport 路径：若未
    // 显式调用 initialize（TUI 内部连接），默认全部 cap=true）。首次
    // AvailableCommandsUpdate 必须由 host 在 session/new response 成功发送后
    // 推送，确保客户端已能按 response 中的 sessionId 建立通知路由。
    cfg.session_manager.ensure_session_caps(&session_id);

    // BRIDGE_RESET_COUNTER handles stale committed cleanup; no explicit clear needed
    identity_response(
        serde_json::to_value(resp).map_err(|e| AcpError::new(-32603, e.to_string()))?,
        identity,
        None,
    )
}

/// `session/new` response 成功写入 transport 后执行的初始化通知。
///
/// commands 首发与 MCP 预热必须保持此顺序：先挂载命令注册表的 on_change
/// 回调并发送 snapshot，再启动 MCP 发现，避免发现结果抢在首次 snapshot 前推送。
pub(crate) async fn after_new_response(
    cfg: &AcpServerConfig,
    transport: &Arc<dyn crate::transport::AcpTransport>,
    session_id: &str,
) {
    let peri_caps = cfg.session_manager.ensure_session_caps(session_id);
    send_available_commands_update(
        transport,
        session_id,
        &peri_caps,
        cfg.session_manager.command_registry_for(session_id),
        cfg.stdio_command_filter,
    )
    .await;
    prewarm_session_mcp_discovery(cfg, session_id);
}

pub(crate) async fn handle_reset_dirty(
    params: &Value,
    cfg: &AcpServerConfig,
) -> Result<Value, AcpError> {
    // 显式协商才放行：`negotiated_caps` 在未 initialize 时为默认（全 false），
    // 不能用 `effective_host_caps` 的 MPSC 全能力兜底放开这条写入路径。
    if !cfg.session_manager.negotiated_caps().session_recovery_v1 {
        return Err(AcpError::new(
            -32601,
            "Session recovery capability was not negotiated",
        ));
    }
    let request: peri_acp_types::workspace::ResetDirtyRequest =
        serde_json::from_value(params.clone())
            .map_err(|_| AcpError::new(-32602, "invalid dirty reset request"))?;
    if !request.accept_risk {
        return Err(AcpError::new(-32602, "explicit risk acceptance required"));
    }
    cfg.thread_store
        .reset_dirty_execution(&request.target)
        .await
        .map_err(super::super::workspace::workspace_error)?;
    Ok(serde_json::json!({}))
}

pub(crate) async fn handle_load(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
    transport: &Arc<dyn crate::transport::AcpTransport>,
) -> Result<Value, AcpError> {
    let prepared = prepare_existing(params, cfg, sessions).await?;
    let PreparedSession {
        id,
        identity,
        read_only,
    } = prepared;
    let req_session_id = id.as_str();
    let state = sessions.get(req_session_id).expect("prepared session");
    let environment = state.environment.clone();
    let cfg = environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg);
    let history_payloads = state.history_payloads.clone();
    let caps = cfg.session_manager.ensure_session_caps(req_session_id);

    // ── ACP v1 spec: replay history via session/update BEFORE responding ──
    let replay_sender = TuiReplaySender {
        transport: transport.as_ref(),
    };
    if let Err(e) = dispatch::replay_persisted_session_history(
        req_session_id,
        &history_payloads,
        &replay_sender,
        &caps,
    )
    .await
    {
        tracing::warn!(session_id = %req_session_id, error = %e, "session/load: history replay failed, continuing");
    }

    // modes/configOptions sent both via notification AND in response body
    // (notification for async update, response body for immediate availability)
    send_config_option_update(transport.as_ref(), req_session_id, cfg).await;

    let modes = build_mode_state(&cfg.permission_mode);
    let config_options = {
        let c = cfg.peri_config.read();
        let p = cfg.provider.read();
        make_config_options(&c, &p, cfg.permission_mode.load())
    };
    let resp = LoadSessionResponse::new()
        .modes(modes)
        .config_options(config_options);
    // Push AvailableCommandsUpdate notification（Phase 6 A4：投影 =
    // 注册表 snapshot；本地 skills / ui / 插件条目已在会话创建时注册）
    send_available_commands_update(
        transport,
        req_session_id,
        &caps,
        cfg.session_manager.command_registry_for(req_session_id),
        cfg.stdio_command_filter,
    )
    .await;
    // 与 session/new 同构（决策 B 扩展）：恢复会话同样预热 MCP skill
    // 发现——stdio 宿主 session/load 后无需等首 turn before_agent
    // 装配即有 mcp 命令（广播首发无 mcp 条目属预期，发现完成经注册表
    // on_change 重发）。幂等（Started 去重）；pool/registry 缺失或
    // 连接中 → 空跑，由首 turn 装配与连接完成事件兜底。
    prewarm_session_mcp_discovery(cfg, req_session_id);
    identity_response(
        serde_json::to_value(resp).map_err(|e| AcpError::new(-32603, e.to_string()))?,
        identity,
        read_only,
    )
}

pub(crate) async fn handle_list(params: &Value, cfg: &AcpServerConfig) -> Result<Value, AcpError> {
    if let Some(extension) = params
        .get("_meta")
        .and_then(|meta| meta.get("peri.sessionWorkspaceV1"))
    {
        if !cfg
            .session_manager
            .effective_host_caps()
            .session_workspace_v1
        {
            return Err(AcpError::new(
                -32602,
                "Session workspace capability was not negotiated",
            ));
        }
        let scope = serde_json::from_value(
            extension
                .get("scope")
                .cloned()
                .ok_or_else(|| AcpError::new(-32602, "missing scope"))?,
        )
        .map_err(|e| AcpError::new(-32602, format!("Invalid scope: {e}")))?;
        let cursor = extension
            .get("cursor")
            .filter(|v| !v.is_null())
            .map(|v| serde_json::from_value(v.clone()))
            .transpose()
            .map_err(|e| AcpError::new(-32602, format!("Invalid cursor: {e}")))?;
        let limit = extension
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(100)
            .clamp(1, 500) as u32;
        let page = cfg
            .controller
            .sessions()
            .list_scoped_threads(&peri_acp_types::workspace::ScopedThreadQuery {
                scope,
                cursor,
                limit,
            })
            .await
            .map_err(super::super::workspace::workspace_error)?;
        let entries = page
            .entries
            .iter()
            .map(|entry| {
                agent_client_protocol::schema::v1::SessionInfo::new(
                    SessionId::new(entry.thread.id.clone()),
                    entry.effective_cwd.clone(),
                )
                .title(entry.thread.title.clone())
            })
            .collect::<Vec<_>>();
        let mut response = serde_json::to_value(ListSessionsResponse::new(entries))
            .map_err(super::super::workspace::workspace_error)?;
        response["_meta"]["peri.sessionWorkspaceV1"] =
            serde_json::json!({ "threads": page.entries, "nextCursor": page.next_cursor });
        return Ok(response);
    }
    let cwd_filter = params.get("cwd").and_then(|v| v.as_str());
    let entries = dispatch::list_sessions_as_info(cfg.controller.as_ref(), cwd_filter)
        .await
        .map_err(|e| AcpError::new(-32603, format!("Failed to list sessions: {e}")))?;

    let resp = ListSessionsResponse::new(entries);
    serde_json::to_value(resp).map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
}

pub(super) fn handle_cancel_bg_task(
    params: &Value,
    cfg: &AcpServerConfig,
) -> Result<Value, AcpError> {
    let req_session_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
    let task_id = params
        .get("taskId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing taskId"))?;

    // 会话不存在时如实报错（此前静默返回 success，掩盖取消未生效）
    let session = cfg
        .session_manager
        .get_session(req_session_id)
        .ok_or_else(|| AcpError::new(-32602, format!("session not found: {req_session_id}")))?;
    session
        .task_manager
        .cancel(task_id)
        .map_err(|e| AcpError::new(-32603, e.to_string()))?;
    info!(session_id = %req_session_id, task_id = %task_id, "Background task cancelled via ACP");
    Ok(serde_json::json!({ "success": true }))
}

async fn close_owned_session(
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
    session_id: &str,
    delete: bool,
) -> Result<(), AcpError> {
    if let Some(state) = sessions.get_mut(session_id) {
        state.closing = true;
        state.continuation_armed = false;
        if let Some(token) = state.cancel_token.as_ref() {
            token.cancel();
        }
        cfg.session_manager.pre_close_session(session_id);
        if state.cancel_token.is_some() {
            return Err(AcpError::new(
                -32010,
                "Session close incomplete: prompt is still active",
            ));
        }
        let environment = state.environment.clone();
        let local = environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg);
        local
            .session_manager
            .close_session(session_id)
            .await
            .map_err(super::super::workspace::workspace_error)?;
        if let Some(pool) = state.lsp_pool.as_ref() {
            pool.shutdown().await;
        }
        if let Some(environment) = environment.as_ref() {
            if !environment.shutdown().await {
                return Err(AcpError::new(
                    -32010,
                    "Session close incomplete: resources are still active",
                ));
            }
        }
        // 只读会话没有执行所有权：关闭只需释放内存状态，不删除（删除会绕过他处的
        // 独占锁），也没有本节点持有的代际需要标 clean。
        let Some(owner) = state.execution_owner.clone() else {
            if delete {
                return Err(super::super::workspace::workspace_error(
                    peri_acp_types::workspace::WorkspaceError::ExecutionLeaseRequired,
                ));
            }
            sessions.remove(session_id);
            return Ok(());
        };
        if delete {
            cfg.controller
                .sessions()
                .delete_thread(&session_id.to_owned())
                .await
                .map_err(super::super::workspace::workspace_error)?;
        }
        owner
            .mark_clean()
            .await
            .map_err(super::super::workspace::workspace_error)?;
        sessions.remove(session_id);
    } else if delete {
        let store = cfg.controller.sessions();
        if store
            .load_session_binding(&session_id.to_owned())
            .await
            .map_err(super::super::workspace::workspace_error)?
            .is_none()
        {
            // Missing delete remains idempotent; unresolved rows are never mutated.
            let exists = store
                .list_threads()
                .await
                .map_err(super::super::workspace::workspace_error)?
                .iter()
                .any(|thread| thread.id == session_id);
            if !exists {
                return Ok(());
            }
        }
        let owner = cfg
            .controller
            .sessions()
            .acquire_execution_lease(&session_id.to_owned())
            .await
            .map_err(super::super::workspace::workspace_error)?;
        let result = cfg
            .controller
            .sessions()
            .delete_thread(&session_id.to_owned())
            .await;
        owner
            .mark_clean()
            .await
            .map_err(super::super::workspace::workspace_error)?;
        result.map_err(super::super::workspace::workspace_error)?;
    }
    Ok(())
}

pub(crate) async fn handle_close(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
) -> Result<Value, AcpError> {
    let id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
    close_owned_session(cfg, sessions, id, false).await?;
    serde_json::to_value(CloseSessionResponse::new())
        .map_err(super::super::workspace::workspace_error)
}

pub(crate) async fn handle_delete(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
) -> Result<Value, AcpError> {
    let id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
    close_owned_session(cfg, sessions, id, true).await?;
    serde_json::to_value(DeleteSessionResponse::new())
        .map_err(super::super::workspace::workspace_error)
}

pub(crate) async fn handle_resume(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
    transport: &Arc<dyn crate::transport::AcpTransport>,
) -> Result<Value, AcpError> {
    let prepared = prepare_existing(params, cfg, sessions).await?;
    let req_session_id = prepared.id.as_str();
    let environment = sessions
        .get(req_session_id)
        .and_then(|state| state.environment.clone());
    let cfg = environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg);
    let caps = cfg.session_manager.ensure_session_caps(req_session_id);

    // Push AvailableCommandsUpdate notification + 预热 MCP skill 发现
    // （决策 B 扩展，与 session/load 同构；stdio 装配面同款行为——恢复会话
    // 后无需等首 turn before_agent 装配即有 mcp 命令）。幂等（Started 去重）；
    // pool/registry 缺失或连接中 → 空跑，由首 turn 装配兜底。
    send_available_commands_update(
        transport,
        req_session_id,
        &caps,
        cfg.session_manager.command_registry_for(req_session_id),
        cfg.stdio_command_filter,
    )
    .await;
    prewarm_session_mcp_discovery(cfg, req_session_id);

    let resp = ResumeSessionResponse::new();
    identity_response(
        serde_json::to_value(resp).map_err(|e| AcpError::new(-32603, e.to_string()))?,
        prepared.identity,
        prepared.read_only,
    )
}

pub(crate) async fn handle_fork(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
    transport: &Arc<dyn crate::transport::AcpTransport>,
) -> Result<Value, AcpError> {
    let source_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
    // prepare_existing 已在本准入里完整复核过源会话，这里只复核已记录证据。
    prepare_existing(params, cfg, sessions).await?;
    let (workspace, _source_owner) = super::super::workspace::reacquire_for_load(
        cfg,
        sessions,
        source_id,
        params.get("cwd").and_then(Value::as_str),
    )
    .await?;
    let source = sessions
        .get(source_id)
        .ok_or_else(|| AcpError::new(-32602, "Load the source session before forking"))?;
    if source.cancel_token.is_some()
        || source.continuation_in_flight
        || cfg
            .session_manager
            .get_session(source_id)
            .is_some_and(|s| !s.active_agents.is_empty() || !s.task_manager.is_execution_idle())
    {
        return Err(AcpError::new(
            -32010,
            "Cannot fork while source execution is active",
        ));
    }
    let source_frozen = source
        .frozen
        .clone()
        .ok_or_else(|| AcpError::new(-32603, "Source frozen snapshot is missing"))?;
    let cwd_owned = workspace
        .cwd
        .to_str()
        .ok_or_else(|| AcpError::new(-32602, "Execution directory is not UTF-8"))?
        .to_owned();
    let cwd = cwd_owned.as_str();
    let (new_thread_id, copied_payloads, owner) =
        dispatch::fork_bound_session(cfg.controller.as_ref(), source_id, &workspace)
            .await
            .map_err(super::super::workspace::workspace_error)?;
    let identity = match response_identity(cfg, &new_thread_id).await {
        Ok(identity) => identity,
        Err(error) => {
            cfg.thread_store
                .delete_thread(&new_thread_id)
                .await
                .map_err(super::super::workspace::workspace_error)?;
            owner
                .mark_clean()
                .await
                .map_err(super::super::workspace::workspace_error)?;
            return Err(error);
        }
    };
    let environment =
        match super::super::workspace::SessionEnvironment::assemble(cfg, cwd, &new_thread_id).await
        {
            Ok(environment) => environment,
            Err(error) => {
                cfg.thread_store
                    .delete_thread(&new_thread_id)
                    .await
                    .map_err(super::super::workspace::workspace_error)?;
                owner
                    .mark_clean()
                    .await
                    .map_err(super::super::workspace::workspace_error)?;
                return Err(error);
            }
        };
    let cfg = environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg);

    let new_session_id = new_thread_id.clone();

    // Fork inherits the source session's exact frozen prefix. Rebuilding from the
    // current environment would invalidate the provider cache on its first turn.
    let frozen_data = source_frozen;
    if let Err(error) =
        store_new_frozen_snapshot_or_compensate(cfg, &new_session_id, &frozen_data).await
    {
        if let Some(environment) = environment.as_ref() {
            if !environment.shutdown().await {
                retain_failed_assembly(
                    sessions,
                    &new_session_id,
                    cwd,
                    owner.clone(),
                    environment.clone(),
                );
                return Err(AcpError::new(
                    -32010,
                    "Fork cleanup incomplete; resources retained for shutdown retry",
                ));
            }
        }
        owner
            .mark_clean()
            .await
            .map_err(super::super::workspace::workspace_error)?;
        return Err(error);
    }
    cfg.session_manager.ensure_session(&new_session_id, cwd);
    let caps = cfg.session_manager.ensure_session_caps(&new_session_id);
    let workflow_middleware =
        create_session_workflow_middleware(cfg, cwd, &new_session_id, &frozen_data);
    let lsp_pool = create_session_lsp_pool(cfg, cwd);

    sessions.insert(
        new_session_id.clone(),
        SessionState {
            session_id: new_session_id.clone(),
            thread_id: new_thread_id.clone(),
            cwd: cwd.to_string(),
            execution_owner: Some(owner),
            environment: environment.clone(),
            closing: false,
            history: copied_payloads
                .iter()
                .filter_map(|payload| payload.as_message().cloned())
                .collect(),
            history_payloads: copied_payloads,
            cancel_token: None,
            frozen: Some(frozen_data),
            recall_items: Vec::new(),
            agent_pool: crate::session::agent_pool::AgentPool::new(),
            workflow_middleware,
            lsp_pool,
            title: None,
            tags: Vec::new(),
            continuation_armed: false,
            continuation_epoch: 0,
            continuation_in_flight: false,
            continuation_mq_steering_pending: false,
            lease: super::super::lease::WriterLease::acquired("default"),
        },
    );

    if let Some(environment) = &environment {
        environment.activate();
    }
    info!(source = %source_id, new = %new_session_id, "Session forked");
    // Push AvailableCommandsUpdate notification + 预热 MCP skill 发现
    // （决策 B 扩展，与 session/new 同构；stdio 装配面同款行为——fork 产生
    // 新 session 后无需等首 turn before_agent 装配即有 mcp 命令）。
    send_available_commands_update(
        transport,
        &new_session_id,
        &caps,
        cfg.session_manager.command_registry_for(&new_session_id),
        cfg.stdio_command_filter,
    )
    .await;
    prewarm_session_mcp_discovery(cfg, &new_session_id);
    let resp = ForkSessionResponse::new(SessionId::new(new_session_id.clone()));
    identity_response(
        serde_json::to_value(resp).map_err(super::super::workspace::workspace_error)?,
        identity,
        None,
    )
}

pub(super) async fn handle_rename(
    params: &Value,
    cfg: &AcpServerConfig,
    transport: &Arc<dyn crate::transport::AcpTransport>,
) -> Result<Value, AcpError> {
    let session_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
    let title = params
        .get("title")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing title"))?;

    cfg.thread_store
        .update_title(&session_id.to_string(), title)
        .await
        .map_err(|e| AcpError::new(-32603, format!("Failed to rename session: {e}")))?;

    // 通过 session/update 通知推送新的标题给外部客户端
    super::super::notify::send_session_info_update_with_title(
        transport.as_ref(),
        session_id,
        Some(title),
    )
    .await;

    info!(session_id = %session_id, title = %title, "Session renamed");

    Ok(serde_json::json!({
        "sessionId": session_id,
        "title": title,
    }))
}

/// 新会话 MCP skill 发现预热（决策 B 扩展）：session/new 完成时挂接连接
/// 事件 notifier + 触发幂等发现，chain 首 turn 装配前即可开始。任何组件
/// 缺失（pool 未装配 / registry 缺失）→ 空跑返回，由首 turn 装配兜底；
/// cancel 持 session token，会话关闭即早退。notifier 无 ExecutorEvent 通道
/// （通知展示由首 turn 装配覆盖为完整版），连接完成事件在此即触发发现。
fn prewarm_session_mcp_discovery(cfg: &AcpServerConfig, session_id: &str) {
    let Some(pool) = cfg.mcp_pool.clone() else {
        return;
    };
    let Ok(pool) = pool.downcast_arc::<peri_middlewares::mcp::McpClientPool>() else {
        return;
    };
    let Some(registry) = cfg.session_manager.mcp_skill_registry_for(session_id) else {
        return;
    };
    let Some(command_registry) = cfg.session_manager.command_registry_for(session_id) else {
        return;
    };
    let Some(cancel) = cfg
        .session_manager
        .inner_sessions()
        .get(session_id)
        .map(|s| s.cancel_token.clone())
    else {
        return;
    };
    peri_middlewares::mcp::middleware::attach_connection_notifier(
        &pool,
        Some(&registry),
        Some(&command_registry),
        &cancel,
        None,
    );
    peri_middlewares::mcp::middleware::prewarm_discovery(
        &pool,
        &registry,
        &command_registry,
        &cancel,
    );
}

/// Adapts `&dyn AcpTransport` into a `ReplaySender` for the TUI path.
struct TuiReplaySender<'a> {
    transport: &'a dyn crate::transport::AcpTransport,
}

#[cfg(test)]
#[path = "session_lifecycle_replay_test.rs"]
mod replay_tests;

#[async_trait::async_trait]
impl ReplaySender for TuiReplaySender<'_> {
    async fn send(&self, notif: SessionNotification) -> Result<(), crate::dispatch::ReplayError> {
        let payload = serde_json::to_value(&notif)
            .map_err(|e| crate::dispatch::ReplayError::SendFailed(e.to_string()))?;
        self.transport
            .send_notification("session/update", payload)
            .await
            .map_err(|e| crate::dispatch::ReplayError::SendFailed(e.to_string()))
    }

    async fn send_system_reminder(
        &self,
        session_id: &str,
        reminder: &peri_acp_types::system_reminder::SystemReminder,
        caps: &peri_acp_types::PeriCaps,
    ) -> Result<(), crate::dispatch::ReplayError> {
        let (event, data) = if caps.system_reminder {
            (
                "system-reminder",
                serde_json::json!({ "reminder": reminder, "replay": true }),
            )
        } else {
            (
                "system-reminder-fallback",
                serde_json::json!({
                    "text": reminder.summary.as_deref().unwrap_or(&reminder.body),
                    "replay": true,
                    "legacy": true
                }),
            )
        };
        self.transport
            .send_notification(
                "peri/unstable_event",
                serde_json::json!({ "sessionId": session_id, "event": event, "data": data }),
            )
            .await
            .map_err(|e| crate::dispatch::ReplayError::SendFailed(e.to_string()))
    }
}
