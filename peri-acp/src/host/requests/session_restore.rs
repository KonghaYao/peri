use std::collections::HashMap;
use std::sync::Arc;

use agent_client_protocol::schema::v1::{
    LoadSessionResponse, ResumeSessionResponse, SessionNotification,
};
use peri_acp_types::session_resources::{BindingRecheck, BindingState, FrozenState};
use peri_acp_types::thread::ThreadId;
use serde_json::Value;

use crate::dispatch::config_update::make_config_options;
use crate::dispatch::ReplaySender;
use crate::host::notify::{send_available_commands_update, send_config_option_update};
use crate::host::prepared::PreparedSessionInputs;
use crate::host::workspace::{workspace_error, BindingCheck};
use crate::host::{build_mode_state, AcpServerConfig, SessionState};
use crate::session::frozen_snapshot::decode_frozen_snapshot;
use crate::{dispatch, transport::types::AcpError};

use super::{create_session_workflow_middleware, prewarm_session_mcp_discovery};

#[path = "legacy_session.rs"]
mod legacy_session;

async fn reject_closing_session(params: &Value, cfg: &AcpServerConfig) -> Result<(), AcpError> {
    let id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
    if cfg
        .session_resources
        .is_session_closing(&id.to_owned())
        .await
        .map_err(crate::host::workspace::resource_error)?
    {
        return Err(AcpError::new(
            -32010,
            "Session close incomplete: resume the close before loading this session",
        ));
    }
    Ok(())
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
        .map_err(crate::host::workspace::resource_error)?;
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
pub(crate) struct PreparedSession {
    pub(super) id: String,
    pub(super) identity: Option<Value>,
}

pub(crate) async fn prepare_existing(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
) -> Result<PreparedSession, AcpError> {
    reject_closing_session(params, cfg).await?;
    let id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
    let availability = cfg
        .session_resources
        .inspect_availability(Some(&id.to_owned()))
        .await
        .map_err(crate::host::workspace::resource_error)?;
    use peri_acp_types::session_resources::{
        AccessMode, ExecutionAvailability, SessionResourceError, SessionResourceErrorKind,
    };
    if availability.access == AccessMode::ReadOnly {
        return Err(crate::host::workspace::resource_error(
            SessionResourceError::new(SessionResourceErrorKind::ReadOnlyStore),
        ));
    }
    match availability.execution {
        Some(ExecutionAvailability::Available | ExecutionAvailability::BindingMissing) => {}
        reason => {
            return Err(AcpError::new(
                -32010,
                format!("Session execution unavailable: {reason:?}"),
            ))
        }
    }
    cfg.session_resources
        .drain_persistence(&id.to_owned())
        .await
        .map_err(crate::host::workspace::resource_error)?;
    // ── 有效 servers 候选（只读）：持久 owner 声明 + 本次请求声明的一致性校验 ──
    // legacy 首次接纳的 bootstrap 会建立会话 MCP 池，而池只在装配时消费一次
    // servers（OnceLock）；因此候选必须在接纳/装配之前解析并交给 prepared，
    // 不能事后写入（那样环境里的 MCP 面会缺失）。
    // 但**持久绑定不能在这里发生**：本请求随后还要过 cwd/frozen 校验，失败请求
    // 不得把不可变 owner 声明写进会话事实（否则该会话后续不同的合法声明会被
    // 误判为篡改）。候选先只在内存里消费，校验全部通过后才定稿为持久声明。
    let owner_state = cfg
        .session_resources
        .load_session_work(&peri_acp_types::session_resources::work::WorkQuery {
            session_id: id.to_owned(),
            limit: 1,
        })
        .await
        .map_err(crate::host::workspace::resource_error)?;
    let owner_known = owner_state
        .state
        .resource_owners
        .contains_key(&owner_state.control.lifecycle);
    let mut effective_servers = super::super::resource_owners::load_for_restore(cfg, id).await?;
    let supplied = super::session_mcp_servers(params)?;
    // 既有声明已经存在时，本次声明只是复核（相等才算合法）；无声明时候选为
    // 请求声明本身，等到校验通过再绑定。
    let commit_owner_declaration = !supplied.is_empty() && !owner_known;
    if !supplied.is_empty() {
        effective_servers = if owner_known {
            super::super::resource_owners::resolve_without_binding(cfg, id, &supplied).await?
        } else {
            supplied
        };
    }

    let legacy_adoption =
        legacy_session::prepare_for_restore(cfg, id, None, effective_servers.clone()).await?;
    // 已激活的候选环境由本作用域唯一持有：成功路径把所有权交给 SessionState，
    // 之后的任何失败都必须排空（保留原错误原因，清理失败显式上报，不吞）。
    let mut environment_slot = legacy_adoption
        .as_ref()
        .and_then(|adoption| adoption.environment.clone());

    let outcome: Result<RestoreOutcome, AcpError> = async {
        let workspace = crate::host::workspace::validate_expected(
            cfg,
            id,
            params.get("cwd").and_then(Value::as_str),
        )
        .await?;
        let persisted = load_frozen_bytes(cfg, id).await?;
        decode_frozen_snapshot(&persisted).map_err(workspace_error)?;
        let identity = response_identity(cfg, id).await?;
        if let Some(state) = sessions.get_mut(id) {
            if state.closing {
                return Err(AcpError::new(-32010, "Session is closing"));
            }
            if state.history_payloads.is_empty() {
                let payloads = dispatch::load_session_payloads(cfg.controller.as_ref(), id).await?;
                state.history = payloads
                    .iter()
                    .filter_map(|payload| payload.as_message().cloned())
                    .collect();
                state.history_payloads = payloads;
            }
            return Ok(RestoreOutcome::AlreadyLive { identity });
        }
        // cwd/frozen/identity 校验已全部通过，且本次请求将发布会话（AlreadyLive
        // 不在此列：live 环境已经定稿，声明不再回写）：把请求声明的有效 servers
        // 候选定稿为持久不可变声明。此前任何一步失败都不会走到这里，失败请求
        // （错误的 cwd、读不懂的 frozen 等）不写 owner。
        if commit_owner_declaration {
            super::super::resource_owners::bind(cfg, id, &effective_servers).await?;
        }
        let payloads = dispatch::load_session_payloads(cfg.controller.as_ref(), id).await?;
        let cwd = workspace
            .cwd
            .to_str()
            .ok_or_else(|| AcpError::new(-32602, "Execution directory is not UTF-8"))?
            .to_owned();
        // M5：legacy 首次接纳已在准备阶段以定稿 frozen 完成原子接纳；这里只消费
        // 其结果（已按接纳/winner 字节定格），不再二次注入或重建。
        let mut prepared = match legacy_adoption {
            Some(adoption) => adoption.prepared,
            None => PreparedSessionInputs::prepare_restore(cfg, &cwd, &persisted)?,
        };
        prepared.session_mcp_servers = effective_servers;
        let frozen = prepared
            .frozen
            .clone()
            .ok_or_else(|| AcpError::new(-32603, "Restored frozen snapshot is missing"))?;
        // 环境定稿是最后一步可失败操作（take 已有候选或现场装配）；此后只做
        // 不可失败的登记与投影，成功路径才 move 环境进 SessionState。
        let environment = match environment_slot.take() {
            // 阶段一已装配（bootstrap）且与接纳字节同源：复用，不第二次装配。
            Some(environment) => Some(environment),
            None => {
                crate::host::workspace::SessionEnvironment::assemble_prepared(cfg, &prepared, id)
                    .await?
            }
        };
        let frozen = Some(frozen);
        let local = environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg);
        // AW3-11：登记会话时交出**装配时已送进 builtin 上下文的那一份** manager
        // （`environment` 为 `None` 时传 `None`，走工厂 / Noop fallback）。
        local.session_manager.ensure_session_with_task_manager(
            id,
            &cwd,
            environment.as_ref().map(|env| env.task_manager()),
        );
        local.session_manager.ensure_session_caps(id);
        // W4b（F4）：workflow middleware 的构造点后移到**会话登记之后**——
        // workflow agent 与主链共用会话级 MCP skill registry，而该 registry 在
        // 会话构造（`ensure_session_with_task_manager` → `build_session`）时才
        // 产生；先构造会让 workflow 链永久拿不到技能面（J5：不回退磁盘）。
        let workflow_middleware = match frozen.as_ref() {
            Some(frozen) => create_session_workflow_middleware(local, &cwd, id, frozen),
            None => None,
        };
        Ok(RestoreOutcome::Publish {
            state: Box::new(SessionState {
                session_id: id.to_owned(),
                thread_id: id.to_owned(),
                cwd,
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
                title: None,
                tags: Vec::new(),
            }),
            identity,
        })
    }
    .await;

    match outcome {
        Ok(RestoreOutcome::Publish { state, identity }) => {
            let state = *state;
            if let Some(environment) = &state.environment {
                environment.activate();
            }
            sessions.insert(id.to_owned(), state);
            Ok(PreparedSession {
                id: id.to_owned(),
                identity,
            })
        }
        Ok(RestoreOutcome::AlreadyLive { identity }) => {
            // 采纳的候选环境不会被使用：显式排空；清理失败必须上报，不静默泄漏。
            if let Some(environment) = environment_slot.take() {
                if !environment.shutdown().await {
                    return Err(AcpError::new(
                        -32603,
                        "unused legacy bootstrap environment did not confirm shutdown",
                    ));
                }
            }
            Ok(PreparedSession {
                id: id.to_owned(),
                identity,
            })
        }
        Err(error) => Err(shutdown_bootstrapped_environment(environment_slot.take(), error).await),
    }
}

/// 恢复准备的结果：既可能发布新会话状态，也可能是会话已在 live 表内（补历史）。
enum RestoreOutcome {
    AlreadyLive {
        identity: Option<Value>,
    },
    Publish {
        state: Box<SessionState>,
        identity: Option<Value>,
    },
}

/// 失败路径统一排空已激活的候选环境；保留原始原因，清理失败显式并入错误。
async fn shutdown_bootstrapped_environment(
    environment: Option<Arc<crate::host::workspace::SessionEnvironment>>,
    error: AcpError,
) -> AcpError {
    if let Some(environment) = environment {
        if !environment.shutdown().await {
            return AcpError::new(
                -32603,
                format!(
                    "{}; additionally the bootstrapped session environment did not confirm shutdown",
                    error.message
                ),
            );
        }
    }
    error
}

pub(super) fn identity_response(
    mut response: Value,
    identity: Option<Value>,
) -> Result<Value, AcpError> {
    if let Some(identity) = identity {
        response["_meta"]["peri.sessionWorkspaceV1"] = identity;
    }
    Ok(response)
}

pub(super) async fn response_identity(
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

/// 绑定复核强度见 [`BindingCheck`](crate::host::workspace::BindingCheck)：
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
        .map_err(crate::host::workspace::resource_error)?;
    let meta = resources
        .load_session_meta(&id)
        .await
        .map_err(crate::host::workspace::resource_error)?;
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
            .map_err(crate::host::workspace::resource_error)?
    } else {
        // Resolve the saved location for a restore request; context reads never adopt it.
        legacy_session::resolve_saved_workspace(cfg, &meta).await?
    };
    Ok(
        serde_json::json!({ "version": 1, "workspace": workspace, "binding": binding, "title": meta.title, "execution_environment_id": resources.session_environment_id(&id).await.map_err(crate::host::workspace::resource_error)? }),
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
                .map_err(crate::host::workspace::resource_error)?;
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
        .map_err(crate::host::workspace::resource_error)?;
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
            .map_err(crate::host::workspace::resource_error)?;
        response["payloads"] = Value::Array(
            payloads
                .iter()
                .map(|payload| {
                    let encoded = peri_acp_types::store::serialize_persisted_payload(payload)?;
                    Ok::<Value, anyhow::Error>(serde_json::from_str(&encoded)?)
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(crate::host::workspace::workspace_error)?,
        );
        let state = cfg
            .controller
            .sessions()
            .load_session_binding(&id)
            .await
            .map_err(crate::host::workspace::resource_error)?;
        let binding = match &state {
            BindingState::Bound(binding) => Some(binding.clone()),
            _ => None,
        };
        response["binding"] =
            serde_json::to_value(binding).map_err(|e| AcpError::new(-32603, e.to_string()))?;
    }
    Ok(response)
}

pub(crate) async fn handle_load(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
    transport: &Arc<dyn crate::transport::AcpTransport>,
) -> Result<Value, AcpError> {
    reject_incomplete_local_close(params, sessions)?;
    let already_loaded = params
        .get("sessionId")
        .and_then(Value::as_str)
        .and_then(|id| sessions.get(id))
        .is_some();
    let prepared = prepare_existing(params, cfg, sessions).await?;
    reopen_workspace_scope_after_restore(cfg, sessions, &prepared, already_loaded).await?;
    let PreparedSession { id, identity } = prepared;
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
    )
}

pub(crate) async fn handle_resume(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
    transport: &Arc<dyn crate::transport::AcpTransport>,
) -> Result<Value, AcpError> {
    reject_incomplete_local_close(params, sessions)?;
    let already_loaded = params
        .get("sessionId")
        .and_then(Value::as_str)
        .and_then(|id| sessions.get(id))
        .is_some();
    let prepared = prepare_existing(params, cfg, sessions).await?;
    reopen_workspace_scope_after_restore(cfg, sessions, &prepared, already_loaded).await?;
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
    )
}

fn reject_incomplete_local_close(
    params: &Value,
    sessions: &HashMap<String, SessionState>,
) -> Result<(), AcpError> {
    if params
        .get("sessionId")
        .and_then(Value::as_str)
        .and_then(|id| sessions.get(id))
        .is_some_and(|state| state.closing)
    {
        return Err(AcpError::new(
            -32010,
            "Session restore incomplete: prior runtime cleanup is pending",
        ));
    }
    Ok(())
}

async fn reopen_workspace_scope_after_restore(
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
    prepared: &PreparedSession,
    already_loaded: bool,
) -> Result<(), AcpError> {
    if already_loaded {
        return Ok(());
    }
    let id = prepared.id.as_str();
    let state = sessions.get(id).expect("prepared session");
    let environment = state.environment.clone();
    let local = environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg);
    let pool = local.mcp_pool.clone();
    let scope_result = async {
        if let Some(pool) = pool {
            pool.open_workspace_task_scope(id).await?;
        }
        Ok::<(), String>(())
    }
    .await;
    if let Err(error) = scope_result {
        // The restored runtime has not been reported to the client. Keep it
        // inaccessible while draining its runtime resources.
        sessions.get_mut(id).expect("prepared session").closing = true;
        let cleanup = async {
            local
                .session_manager
                .close_session(id)
                .await
                .map_err(|cause| cause.to_string())?;
            if let Some(environment) = environment.as_ref() {
                if !environment.shutdown().await {
                    return Err("restored environment remains active".to_owned());
                }
            }
            Ok::<(), String>(())
        }
        .await;
        if cleanup.is_ok() {
            sessions.remove(id);
        }
        return Err(AcpError::new(-32010, match cleanup {
            Ok(()) => format!("Session restore incomplete: Workspace task scope unavailable: {error}"),
            Err(cause) => format!("Session restore incomplete: Workspace task scope unavailable: {error}; cleanup incomplete: {cause}"),
        }));
    }
    Ok(())
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
