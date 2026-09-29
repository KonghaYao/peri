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
use peri_acp_types::session_resources::{
    BindingRecheck, BindingState, FrozenSnapshotBytes, FrozenState, NewSessionDraft,
    NewSessionMeta, SessionInitialization, SessionMetaPatch,
};
use peri_acp_types::thread::{CancelPolicy, ThreadId};
use peri_acp_types::workspace::{ReadOnlyAdmission, ResolvedWorkspace, SessionBinding};
use peri_acp_types::PeriCaps;
use serde_json::Value;
use tracing::{info, warn};

use super::super::notify::{send_available_commands_update, send_config_option_update};
use super::super::prepared::PreparedSessionInputs;
use super::super::workspace::{workspace_error, BindingCheck};
use super::super::{build_mode_state, AcpServerConfig, SessionState};
use crate::dispatch::config_update::make_config_options;
use crate::dispatch::ReplaySender;
use crate::session::frozen_snapshot::decode_frozen_snapshot;
use crate::{dispatch, transport::types::AcpError};

#[path = "legacy_session.rs"]
mod legacy_session;

/// fork source 读取/保存失败 → ACP 错误。
///
/// 门面失败保留领域分类（含只读准入与未决持久化载荷）；领域与 IO 失败按
/// workspace 语义上报，不把门面错误降级成「存储不可用」。
fn fork_source_error(error: anyhow::Error) -> AcpError {
    match error.downcast::<peri_acp_types::session_resources::SessionResourceError>() {
        Ok(error) => super::super::workspace::resource_error(error),
        Err(error) => super::super::workspace::workspace_error(error),
    }
}

/// 读取 bound 会话的持久 frozen **字节**（唯一事实源）：恢复路径的装配输入由它定格。
///
/// 快照缺 frozen 是错误（bound 会话必须有），本构建读不懂也是错误——两者都不是
/// 「没有 frozen，可以重建」：重建会把已发布的冻结输入换成当前目录/日期。
async fn load_frozen_bytes(cfg: &AcpServerConfig, session_id: &str) -> Result<String, AcpError> {
    let snapshot = cfg
        .session_resources
        .load_session_snapshot(&session_id.to_owned())
        .await
        .map_err(super::super::workspace::resource_error)?;
    match snapshot.frozen {
        FrozenState::Present(bytes) => Ok(bytes.into_string()),
        FrozenState::LegacyAbsent => Err(AcpError::new(
            -32603,
            "Bound session has no frozen snapshot",
        )),
        FrozenState::Unsupported => Err(AcpError::new(
            -32603,
            "Session frozen snapshot is not readable by this build",
        )),
    }
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
    // legacy 无 frozen 的恢复：准备阶段按保存的 cwd 只读定格（严格只读插件、
    // 不生成合成清单），装配消费同一份输入；其余情况返回 None，装配按既有
    // 入口准备（下一批统一为单一 prepared 路径）。
    let legacy_prepared =
        legacy_session::prepare_for_restore(cfg, id, params.get("cwd").and_then(Value::as_str))
            .await?;
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
        // 执行所有权不可得（他处持有 / 待恢复 / 本节点只读）：不用错误信息挡住进入，
        // 一律改为只读进入并记 warning——未协商 `sessionWorkspaceV1` 的客户端同样进入，
        // 只是拿不到身份载荷里的只读标记（见 `identity_response`）。独占语义不变：
        // 写入与执行仍要所有权，`require_owner` 是唯一闸门。
        super::super::workspace::ExecutionAdmission::Unavailable(reason) => {
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
        let (frozen, environment, workflow_middleware) = match owner.as_ref() {
            Some(_) => {
                // 持久 blob 是唯一事实源：恢复路径不再按当前目录/配置构建第二份 frozen
                // （ARC-FROZEN-001 的同源收口）。
                let persisted = load_frozen_bytes(cfg, id).await?;
                let prepared = match legacy_prepared {
                    // legacy 竞争：adopt 是 write-once，本进程可能被竞争落下——装配必须
                    // 按**adopt 后重读的 winner**，候选只当过写入尝试；`legacy_prepared`
                    // 至此只剩非 frozen 事实（配置/插件/cwd）。
                    Some(mut inputs) => {
                        let winner = decode_frozen_snapshot(&persisted).map_err(workspace_error)?;
                        inputs.inject_frozen(winner, persisted)?;
                        inputs
                    }
                    None => PreparedSessionInputs::prepare_restore(cfg, &cwd, &persisted)?,
                };
                let frozen = prepared.frozen.clone();
                let environment = super::super::workspace::SessionEnvironment::assemble_prepared(
                    cfg, &prepared, id,
                )
                .await?;
                let local = environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg);
                let workflow_middleware =
                    create_session_workflow_middleware(local, &cwd, id, &frozen);
                (Some(frozen), environment, workflow_middleware)
            }
            None => (None, None, None),
        };
        let local = environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg);
        // A11/A22：session **不再**建池，只投影所属部署单元（有环境时即该环境的
        // 装配结果，否则宿主）的 host pool 同一 `Arc`；`prompt_dispatch` 用的
        // 正是同一个 `local`，故投影与执行面同源。
        let lsp_pool = local.lsp_pool.clone();
        // AW3-11：登记会话时交出**装配时已送进 builtin 上下文的那一份** manager
        // （`environment` 为 `None` 时传 `None`，走工厂 / Noop fallback）。
        local.session_manager.ensure_session_with_task_manager(
            id,
            &cwd,
            environment.as_ref().map(|env| env.task_manager()),
        );
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

/// 装配准入响应：会话身份载荷 + 本次准入是否只读。
///
/// 只读标记挂在身份载荷里，因此只有协商了 `sessionWorkspaceV1` 的客户端才看得到它；
/// 未协商的连接同样按只读准入进入（见 `prepare_existing`），只是拿不到这个标记——
/// 它无从得知本次准入只读，`require_owner` 在写入/执行时仍是确定拒绝。
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

/// 绑定复核强度见 [`BindingCheck`](super::super::workspace::BindingCheck)：
/// 协议读请求复核完整发现快照，同一次准入内的身份读取只复核已记录证据。
///
/// 绑定分类走门面的轻量投影（不拉全历史）；本机无法验证的绑定
/// （[`BindingState::ExternalOrUnregistered`]）不冒充「没有绑定」，其执行目录由
/// 保存路径解析给出，执行准入另经 `check_expected` 复核。
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
    let resources = cfg.controller.sessions();
    let id = ThreadId::from(session_id.to_owned());
    let state = resources
        .load_session_binding(&id)
        .await
        .map_err(super::super::workspace::resource_error)?;
    let meta = resources
        .load_session_meta(&id)
        .await
        .map_err(super::super::workspace::resource_error)?;
    let binding = match &state {
        BindingState::Bound(binding) => Some(binding.clone()),
        _ => None,
    };
    let workspace = if binding.is_some() {
        let recheck = match check {
            BindingCheck::Full => BindingRecheck::Full,
            BindingCheck::Recorded => BindingRecheck::Recorded,
        };
        resources
            .validate_bound_workspace(&id, recheck)
            .await
            .map_err(super::super::workspace::resource_error)?
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
                .session_resources
                .resolve_workspace(std::path::Path::new(cwd))
                .await
                .map_err(super::super::workspace::resource_error)?;
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
    // 轻量 metadata 投影走门面（不拉全历史）；`history=true` 的历史回放经门面的
    // 完整逻辑上下文读取（继承区 + 自有 payload），不再由协议面拼祖先链。
    let meta = cfg
        .session_resources
        .load_session_meta(&id)
        .await
        .map_err(super::super::workspace::resource_error)?;
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
        let payloads = cfg
            .controller
            .sessions()
            .load_session_history(&id)
            .await
            .map_err(super::super::workspace::resource_error)?;
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
        let state = cfg
            .controller
            .sessions()
            .load_session_binding(&id)
            .await
            .map_err(super::super::workspace::resource_error)?;
        let binding = match &state {
            BindingState::Bound(binding) => Some(binding.clone()),
            _ => None,
        };
        response["binding"] =
            serde_json::to_value(binding).map_err(|e| AcpError::new(-32603, e.to_string()))?;
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
    let requested = params.get("cwd").and_then(Value::as_str).unwrap_or(".");
    let workspace = cfg
        .session_resources
        .resolve_workspace(std::path::Path::new(requested))
        .await
        .map_err(super::super::workspace::resource_error)?;
    let cwd = workspace
        .cwd
        .to_str()
        .ok_or_else(|| AcpError::new(-32602, "Execution directory is not UTF-8"))?
        .to_owned();
    // 只读准备（lease 之前）：定格配置/插件/frozen，不创建 thread、不占 lease、
    // 不启动 MCP/LSP/hooks，也不写任何会话数据或本机登记。new 路径只在这里准备
    // 一次，发布段消费同一个准备对象。
    let prepared = super::super::prepared::PreparedSessionInputs::prepare_new(cfg, &cwd)?;
    new_session_from_prepared(cfg, &workspace, &prepared, sessions).await
}

/// `session/new` 的发布段：消费**已定格**的准备输入，一次写出 meta/binding/frozen
/// 并取得执行 owner，随后复核准入、装配环境、发布 live 状态。
///
/// 本函数不读配置、不加载插件、不重建 frozen：保存字节与 live 状态都取自调用方
/// 给定的 `prepared`。测试以自己定格的准备对象直接驱动本函数，因此「保存字节 ==
/// 给定的 frozen 字节」是可断言的；准备之后外部输入若被改写，任何在这里重读或
/// 重建的实现都会产出不同字节而使断言失败。
pub(crate) async fn new_session_from_prepared(
    cfg: &AcpServerConfig,
    workspace: &ResolvedWorkspace,
    prepared: &super::super::prepared::PreparedSessionInputs,
    sessions: &mut HashMap<String, SessionState>,
) -> Result<Value, AcpError> {
    let resources = cfg.session_resources.clone();
    let cwd = prepared.cwd.clone();
    // ── P1：草稿 + lease（frozen 暂空）──
    // 身份一次生成；「未发布创建」的 frozen 由 P5 一次性提交，因此内容准入（本次的
    // 准备产物）与发布之间不存在「保存了半份」的中间态。数据已保存但准入失败
    // （saved_but_not_admitted）原样上报，不谎称「确定未创建」。
    let session_id = uuid::Uuid::now_v7().to_string();
    let initialization = resources
        .begin_initialization(&NewSessionDraft {
            thread_id: session_id.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
            meta: NewSessionMeta {
                title: None,
                cwd: cwd.clone(),
                parent_thread_id: None,
                hidden: false,
                cancel_policy: CancelPolicy::default(),
                snapshot_at_message_id: None,
            },
            binding: SessionBinding::from_workspace(workspace),
        })
        .await
        .map_err(super::super::workspace::resource_error)?;
    let thread_id = session_id.clone();
    // 创建后的同一次准入复核：与草稿事务写入的绑定比对已记录证据。
    if let Err(error) = resources.validate_session(&session_id, workspace).await {
        initialization
            .clone()
            .abandon()
            .await
            .map_err(super::super::workspace::resource_error)?;
        return Err(super::super::workspace::resource_error(error));
    }
    let identity = match response_identity(cfg, &session_id).await {
        Ok(identity) => identity,
        Err(error) => {
            initialization
                .clone()
                .abandon()
                .await
                .map_err(super::super::workspace::resource_error)?;
            return Err(error);
        }
    };
    // ── P2：装配（MCP pool 挂起；frozen 消费准备产物，不重建）──
    // 装配失败时环境尚未建立（没有对外资源需要排空），撤销未发布的创建即可。
    let environment = match super::super::workspace::SessionEnvironment::assemble_prepared(
        cfg,
        prepared,
        &session_id,
    )
    .await
    {
        Ok(environment) => environment,
        Err(error) => {
            initialization
                .clone()
                .abandon()
                .await
                .map_err(super::super::workspace::resource_error)?;
            return Err(error);
        }
    };
    // ── P3：activate（资源准入开始）──
    // B3 口径写死为「P0–P5 发布前」：发布点（P6 的 `sessions.insert`）之前可以发生
    // 准备、装配、activate 与资源读取，**不含**工具执行、模型请求与 hook；此处提前
    // activate 后会话仍不在 `sessions` 表里，`require_owner` 因此挡住任何执行。
    if let Some(environment) = &environment {
        environment.activate();
    }
    let cfg = environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg);

    // ── P4：frozen 字节（本次准备的唯一产物；W3a 无资源读取，内容仍在准备期定格）──
    let frozen_data = prepared.frozen.clone();
    // ── P5：commit_frozen（一次性 CAS）──
    // 失败时按效果结清纪律处理：先重读单条判据，只有确证未生效才撤销；判据不可得
    // 时保留草稿与 dirty 代际（由 `RecoveryRequired` 显式恢复），绝不删除。
    let frozen_bytes = FrozenSnapshotBytes::new(prepared.frozen_encoded.clone());
    if let Err(error) = initialization.commit_frozen(&frozen_bytes).await {
        match committed_frozen_state(&resources, &session_id).await {
            Ok(true) => {
                warn!(
                    session_id = %session_id,
                    "frozen commit reported an error but the snapshot is present; publishing"
                );
            }
            Ok(false) => {
                drain_and_abandon(environment.as_ref(), &initialization).await?;
                return Err(super::super::workspace::resource_error(error));
            }
            Err(criterion) => {
                // 判据不可得：不得删除。环境仍按「未发布」排空（尽力），草稿连同
                // 未结清的执行代际留给显式恢复。
                if let Some(environment) = &environment {
                    let _ = environment.shutdown().await;
                }
                return Err(AcpError::new(
                    -32603,
                    format!(
                        "frozen commit failed and its outcome cannot be proven ({}); \
                         the unpublished session was kept for explicit recovery",
                        criterion.message
                    ),
                ));
            }
        }
    }

    // ── P6 之前的同一任务内登记（非对外可见点）──
    // 通过 SessionManager 统一构造路径，并登记 AcpSession 记录以支撑
    // cascade cancel 子 agent 与 goal_state（见 SessionManager::ensure_session）。
    // frozen 与准备阶段同源：内容与字节都来自同一 PreparedSessionInputs
    // （日期/运行环境/配置/插件 roots 均为准备阶段定格的那一份），不二次构建；
    // 保存字节已由 P5 的 commit 一次写出，因此这里不存在「发布后需补偿」的中间态。
    cfg.session_manager.ensure_session_with_task_manager(
        &session_id,
        &cwd,
        environment.as_ref().map(|env| env.task_manager()),
    );

    // Create session-scoped WorkflowMiddleware at session/new (GAP-05: inject frozen data)
    let workflow_middleware =
        create_session_workflow_middleware(cfg, &cwd, &session_id, &frozen_data);
    // A11/A22：session 只投影部署单元（有环境时即该环境）的 host pool，不再按
    // session cwd 建池；`prompt` 每 turn 只 clone 同一 `Arc`。
    let lsp_pool = environment
        .as_ref()
        .map(|env| &env.cfg)
        .unwrap_or(cfg)
        .lsp_pool
        .clone();

    // ── P6：发布（`sessions.insert` 是唯一对外可见点）──
    // 发布之后才可能有 prompt/执行（`require_owner` 只查这张表）；activate 已在 P3
    // 完成，因此这里的两次动作之间没有 await，不存在「已可见但资源未起」的窗口。
    sessions.insert(
        session_id.clone(),
        SessionState {
            session_id: session_id.clone(),
            thread_id: thread_id.clone(),
            cwd: cwd.clone(),
            execution_owner: Some(initialization.execution_lease()),
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

/// P5 失败后的效果结清：单条判据 `threads.frozen_context`。
///
/// `Ok(true)` = 非 NULL ⇒ 提交实际生效（转发布）；`Ok(false)` = NULL 且读得到
/// ⇒ 确证未提交（可撤销）；`Err` = 判据不可得 ⇒ 不得删除。
async fn committed_frozen_state(
    resources: &Arc<dyn peri_acp_types::session_resources::SessionResources>,
    session_id: &str,
) -> Result<bool, AcpError> {
    let snapshot = resources
        .load_session_snapshot(&session_id.to_owned())
        .await
        .map_err(super::super::workspace::resource_error)?;
    match snapshot.frozen {
        FrozenState::Present(_) => Ok(true),
        FrozenState::LegacyAbsent => Ok(false),
        // 有字节但本构建读不懂：判据存在但不能证明「已提交的是本次内容」，
        // 按不可得处理（不删除、不冒充发布）。
        FrozenState::Unsupported => Err(AcpError::new(
            -32603,
            "session frozen snapshot is present but not readable by this build",
        )),
    }
}

/// 发布前失败的补偿：环境先排空，**排空确认之后**才撤销未发布的创建。
///
/// 顺序不变量（§5.1）：`abandon` 必须在环境 drain 完成之后——否则资源持有的是已删除
/// 会话的 lease/handle；排空未确认时不撤销（草稿与 dirty 代际保留，由显式恢复收敛）。
pub(super) async fn drain_and_abandon(
    environment: Option<&Arc<super::super::workspace::SessionEnvironment>>,
    initialization: &Arc<dyn SessionInitialization>,
) -> Result<(), AcpError> {
    if let Some(environment) = environment {
        if !environment.shutdown().await {
            return Err(AcpError::new(
                -32603,
                "session resources did not confirm shutdown; the unpublished session was kept",
            ));
        }
    }
    initialization
        .clone()
        .abandon()
        .await
        .map_err(super::super::workspace::resource_error)
}

/// `session/new` response 成功写入 transport 后执行的初始化通知。///
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
    // 解除精确代际由门面统一承担：只解除本机 dirty，永不解除未决持久化。
    cfg.session_resources
        .reset_dirty_execution(&request)
        .await
        .map_err(super::super::workspace::resource_error)?;
    Ok(serde_json::json!({}))
}

// 曾有的「会话存储登记」两条 RPC（`peri/session_store_status` /
// `peri/session_register_store`）按用户裁决撤销：不再有本机登记、准入裁决与跨安装
// 来源判定，配置里指到哪个 store 就直接用哪个。历史见
// `docs/design/peri-acp-protocol.md`。

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
            .session_resources
            .list_sessions(&peri_acp_types::workspace::ScopedThreadQuery {
                scope,
                cursor,
                limit,
            })
            .await
            .map_err(super::super::workspace::resource_error)?;
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
        // A11/A22：`session/delete` **不**关闭 LSP pool——pool 归 host（同一 `Arc`
        // 被多 session 共享），关闭只发生在 host shutdown。此处若关闭，会把其它
        // 仍活跃 session 的 language server 一起掐掉。
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
        // 执行资源已排空（环境 shutdown 成功）后，才请求门面结清持久化并按需删除：
        // 排空确认在前、结清在最后，任一未完成都保持 Closing 与唯一 owner。
        let resources = &cfg.session_resources;
        let target = session_id.to_owned();
        resources
            .drain_persistence(&target)
            .await
            .map_err(super::super::workspace::resource_error)?;
        if delete {
            // 删除是完整生命周期行为：数据、本机执行代际与本次持有都被门面在同一步
            // 结束（删除成功后 owner 已释放），因此这里不再重复收尾。
            resources
                .delete_session_tree(&target)
                .await
                .map_err(super::super::workspace::resource_error)?;
        } else {
            owner
                .mark_clean()
                .await
                .map_err(super::super::workspace::workspace_error)?;
        }
        sessions.remove(session_id);
    } else if delete {
        // Missing delete remains idempotent；未加载会话不因此变成可删除对象，
        // 删除仍要求本次取得执行所有权（短时准入，不抢夺活 owner）。
        let resources = &cfg.session_resources;
        let target = session_id.to_owned();
        match resources.load_session_meta(&target).await {
            Ok(_) => {
                let owner =
                    super::super::workspace::acquire_transient_owner(cfg, session_id).await?;
                let result = resources.delete_session_tree(&target).await;
                if result.is_err() {
                    // 删除失败：数据仍在，本次为删除临时取得的所有权按正常收尾放下
                    // （代际行还在，走 clean CAS）；成功时所有权已随删除结束。
                    owner
                        .mark_clean()
                        .await
                        .map_err(super::super::workspace::workspace_error)?;
                }
                result.map_err(super::super::workspace::resource_error)?;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    peri_acp_types::session_resources::SessionResourceErrorKind::NotFound
                ) =>
            {
                return Ok(());
            }
            Err(error) => return Err(super::super::workspace::resource_error(error)),
        }
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
    if source.frozen.is_none() {
        return Err(AcpError::new(-32603, "Source frozen snapshot is missing"));
    }
    let cwd_owned = workspace
        .cwd
        .to_str()
        .ok_or_else(|| AcpError::new(-32602, "Execution directory is not UTF-8"))?
        .to_owned();
    let cwd = cwd_owned.as_str();
    // 一致 source 快照：payload/flags/binding/frozen 一次读出，ID 重映射是领域纯函数，
    // 目标快照由门面一次保存（不逐条写 flags、不做存储补偿）。
    let fork_source = dispatch::load_fork_source(&cfg.session_resources, source_id)
        .await
        .map_err(fork_source_error)?;
    // 普通 fork 复用 source 已持久化的精确 frozen 字节（内存对象只是同一次保存的
    // 解码视图），不按当前日期/目录重冻。frozen 与本次保存同源：字节来自 source
    // 快照，装配消费 `prepared` 的解码视图，不二次构建。
    let prepared = super::super::prepared::PreparedSessionInputs::prepare_fork(
        cfg,
        cwd,
        fork_source.frozen.as_str(),
    )?;
    let frozen_data = prepared.frozen.clone();
    let (new_thread_id, copied_payloads, owner) = dispatch::fork_bound_session(
        &cfg.session_resources,
        &fork_source,
        &workspace,
        chrono::Utc::now().to_rfc3339(),
    )
    .await
    .map_err(fork_source_error)?;
    let identity = match response_identity(cfg, &new_thread_id).await {
        Ok(identity) => identity,
        Err(error) => {
            // identity 装配失败：环境尚未建立，撤销本次未发布的创建即可。
            cfg.session_resources
                .abandon_initialization(&new_thread_id, &owner)
                .await
                .map_err(super::super::workspace::resource_error)?;
            return Err(error);
        }
    };
    let environment = match super::super::workspace::SessionEnvironment::assemble_prepared(
        cfg,
        &prepared,
        &new_thread_id,
    )
    .await
    {
        Ok(environment) => environment,
        Err(error) => {
            // 装配失败时环境尚未建立（没有对外资源需要排空）：撤销未发布的创建。
            cfg.session_resources
                .abandon_initialization(&new_thread_id, &owner)
                .await
                .map_err(super::super::workspace::resource_error)?;
            return Err(error);
        }
    };
    let cfg = environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg);

    let new_session_id = new_thread_id.clone();
    cfg.session_manager.ensure_session_with_task_manager(
        &new_session_id,
        cwd,
        environment.as_ref().map(|env| env.task_manager()),
    );
    let caps = cfg.session_manager.ensure_session_caps(&new_session_id);
    let workflow_middleware =
        create_session_workflow_middleware(cfg, cwd, &new_session_id, &frozen_data);
    // 同上：fork 出的 session 也只投影所属部署单元的 host pool（此处 `cfg` 已
    // 收敛为 environment-or-host）。
    let lsp_pool = cfg.lsp_pool.clone();

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

    // 定向更新：只改标题，不整份覆盖 metadata（cwd/binding/计数/缓存的持有者是门面）。
    cfg.session_resources
        .update_session_meta(
            &session_id.to_owned(),
            &SessionMetaPatch {
                title: Some(Some(title.to_owned())),
                ..Default::default()
            },
        )
        .await
        .map_err(super::super::workspace::resource_error)?;

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
        session_id,
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
