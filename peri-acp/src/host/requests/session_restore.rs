use std::collections::HashMap;
use std::sync::Arc;

use agent_client_protocol::schema::v1::{
    LoadSessionResponse, ResumeSessionResponse, SessionNotification,
};
use peri_acp_types::session_resources::{BindingRecheck, BindingState, FrozenState};
use peri_acp_types::thread::ThreadId;
use serde_json::Value;

use crate::dispatch::config_update::make_config_options;
use crate::dispatch::{LoadGeneration, ReplaySender};
use crate::host::connection::ConnectionFacts;
use crate::host::notify::{send_available_commands_update, send_config_option_update};
use crate::host::prepared::PreparedSessionInputs;
use crate::host::workspace::{workspace_error, BindingCheck};
use crate::host::{build_mode_state, AcpServerConfig, SessionState};
use crate::session::frozen_snapshot::decode_frozen_snapshot;
use crate::{dispatch, transport::types::AcpError};

use super::prewarm_session_mcp_discovery;
// 本地 workflow 运行时不在 emscripten 构建面内（wasm 产物不打包 workflow）。
#[cfg(not(target_os = "emscripten"))]
use super::{attach_session_workflow, create_session_workflow_chain, workflow_face_closed};

#[path = "legacy_session.rs"]
mod legacy_session;

#[path = "session_restore/fork_shared.rs"]
mod fork_shared;
#[path = "session_restore/load_shared.rs"]
mod load_shared;
pub(crate) use fork_shared::handle_fork_shared;
#[cfg(test)]
pub(crate) use load_shared::cleanup_unpublished_restore;
pub(crate) use load_shared::{handle_load_shared, handle_resume_shared};

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
    // 恢复准入只消费 frozen：单列读取，不把 own 正文、flags 或 inherited 带进内存。
    let frozen = cfg
        .session_resources
        .load_frozen_state(&session_id.to_owned())
        .await
        .map_err(crate::host::workspace::resource_error)?;
    match frozen {
        FrozenState::Present(bytes) => Ok(bytes.into_string()),
        FrozenState::LegacyAbsent => Err(AcpError::new(
            -32603,
            "Bound session has no frozen snapshot",
        )),
    }
}

/// 一次恢复准入的结果。
pub(crate) struct PreparedSession {
    pub(super) id: String,
    pub(super) identity: Option<Value>,
}

/// 进程内/测试入口：调用方持有的是本地 map 引用，不是生产 `SharedSessions` 锁。
/// 生产 `ServerLoop` 的 `session/load` 走 [`load_shared::handle_load_shared`]
/// （短锁读取 → 锁外 I/O → 短锁写回），不得从生产路径调用本函数。
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
    // ── 有效 servers（只读候选）──
    // 本分支不再有持久 owner 声明（随工作账本移除）：请求声明即本次候选，且
    // **接纳前只作内存候选**，不写任何会话事实。
    // 候选必须在 legacy 首次接纳的 bootstrap 之前定格：会话 MCP 池只在装配时
    // 消费一次 servers（OnceLock），事后写入 prepared 不会再生效。
    let effective_servers = super::session_mcp_servers(params)?;

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
            crate::host::ownership::mark_stale_if_dead(state);
            match state.context_ownership {
                // 内存会话状态只发布有效集合：已排除正文只留在存储侧。
                crate::host::ContextOwnership::ReloadRequired => {
                    let payloads =
                        dispatch::load_active_session_payloads(cfg.controller.as_ref(), id).await?;
                    state.history_payloads = payloads;
                    state.context_ownership = crate::host::ContextOwnership::Resident;
                }
                // 空驻留会话是唯一允许与存储重新对齐的 Resident 状态：新建会话
                // 可能尚未观察到后来写入的 canonical 历史。非空 Resident 已有本轮
                // 事实，不用存储覆盖；HandedOff 的正文在在途执行器里，不能再装入
                // 第二份副本，也不能据此判断会话为空。
                crate::host::ContextOwnership::Resident if state.history_payloads.is_empty() => {
                    let payloads =
                        dispatch::load_active_session_payloads(cfg.controller.as_ref(), id).await?;
                    state.history_payloads = payloads;
                }
                crate::host::ContextOwnership::Resident
                | crate::host::ContextOwnership::HandedOff => {}
            }
            return Ok(RestoreOutcome::AlreadyLive { identity });
        }
        // 冷恢复同样只发布有效集合：完整 canonical 历史按需从存储读取。
        let payloads = dispatch::load_active_session_payloads(cfg.controller.as_ref(), id).await?;
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
        // 环境定稿（take 已有候选或现场装配）之后只剩登记、绑定与投影：登记与投影
        // 不可失败；成功路径才 move 环境进 SessionState（绑定失败见下方排空处理）。
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
        // P5：关闭集命中 workflow 时不创建运行时、不绑定、不启动消费者（实例也不会 spawn）。
        // Emscripten：本地 workflow 运行时不在构建面内，无链可建（通知句柄随之缺省）。
        #[cfg(not(target_os = "emscripten"))]
        let workflow_chain = match frozen.as_ref() {
            Some(frozen) if !workflow_face_closed(frozen) => {
                Some(create_session_workflow_chain(local, &cwd, id, frozen))
            }
            _ => None,
        };
        // 恢复路径同样绑定 builtin `workflow` 实例槽位（同一份运行时）。
        // 绑定失败按会话初始化失败处理（design §6.2）：环境已定稿且由本作用域唯一持有，
        // 失败必须排空它（`shutdown_bootstrapped_environment` 保留原错误原因），不发布
        // 半可用会话。
        #[cfg(not(target_os = "emscripten"))]
        if let Some(chain) = workflow_chain.as_ref() {
            if let Err(error) = attach_session_workflow(environment.as_ref(), chain) {
                return Err(shutdown_bootstrapped_environment(environment, error).await);
            }
        }
        Ok(RestoreOutcome::Publish {
            state: Box::new(SessionState {
                session_id: id.to_owned(),
                thread_id: id.to_owned(),
                cwd,
                // session-owned 交互门：环境自带（router 借用同一 Arc，跨轮共享）——
                // 必须在 move 环境之前取用。
                interaction_gate: crate::host::workspace_interaction::session_interaction_gate(
                    &environment,
                ),
                environment,
                closing: false,
                context_ownership: crate::host::ContextOwnership::Resident,
                history_payloads: payloads,
                structure_version: 0,
                session_generation: crate::host::next_session_generation(),
                history_window: Default::default(),
                event_health: Default::default(),
                cancel_token: None,
                // 本分支无持久执行恢复：恢复准入一律以「无续跑」状态发布。
                continuation_armed: false,
                host_request_seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                continuation_epoch: 0,
                continuation_in_flight: false,
                continuation_mq_steering_pending: false,
                frozen,
                recall_items: Vec::new(),
                agent_pool: crate::session::agent_pool::AgentPool::new(),
                // Emscripten：无本地 workflow 运行时 ⇒ 无完成通知源。
                #[cfg(not(target_os = "emscripten"))]
                workflow_notifications: workflow_chain
                    .as_ref()
                    .map(|chain| Arc::clone(&chain.notifications)),
                #[cfg(target_os = "emscripten")]
                workflow_notifications: None,
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

/// 初始 load。`connection` 是**当前连接**的事实快照
/// （[`crate::host::connection::ConnectionFacts`]），两个字段各管一处：
///
/// - `history_pagination`：本次交付的**分流判据**，决定拿哪一种历史、打不打条目位置戳
///   （REQ §5.2、REQ §5.3、REQ-H01~H04）。
///   - 协商（`true`）：行为完全不变——热态**有效集合**回放 + 条目身份 / 位置戳
///     （`entryId`/`position`）+ 响应里的代次与首游标。
///   - 未协商（`false`）：canonical **完整历史**的流式交付
///     （[`dispatch::replay_canonical_history_streamed`]）：从存储按向前段逐段读取，逐段投影成
///     标准 `session/update`。**不读** `state.history_payloads`（那只是有效集合，会丢 Full
///     归档原文），也不引入任何分页私有字段。会话已在内存（`AlreadyLive`）时同样从存储流式
///     读取：交付来源由连接事实决定，不由热态是否恰好有一份快照决定。
/// - `caps`：本次回放的**标记门**（`periReplay` / `periLoadGeneration`，两者同一门
///   `caps.replay`，见 [`dispatch::replay_meta`]）。判据只能是本连接 `initialize` 的协商
///   结果：会话级 `caps_registry`（[`crate::session::SessionManager::ensure_session_caps`]）
///   会把先前 peri 连接的能力留给重连的同一会话，未协商的标准连接因此会继承到 Peri 私有
///   字段（REQ §5.3、验收 A25）。
///
/// 进程内/测试入口；生产 `ServerLoop` 使用 [`load_shared::handle_load_shared`]，
/// 后者不在 `SharedSessions` 全局锁内做 DB/资源 I/O。
pub(crate) async fn handle_load(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
    transport: &Arc<dyn crate::transport::AcpTransport>,
    connection: ConnectionFacts,
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
    // 回放标记 / 代次戳的门是**当前连接**的协商结果（REQ §5.3）：本连接没声明 `peri.replay`
    // 时，本次交付的响应与全部回放通知都不带 Peri 私有字段——即使会话级注册表里存着先前
    // peri 连接写下的全能力值（`ensure_session_caps` 命中旧值就直接返回，见 session/caps.rs）。
    let connection_caps = &connection.caps;
    // 命令表广播仍走**会话级**注册表（`ui_commands` 明细在会话创建时已注册进命令表，
    // 见 `send_available_commands_update` / `register_ui_entries`）：那是另一个功能面，
    // 与回放标记刻意不同源，不得为了让两者一致而改成连接 caps。
    let session_caps = cfg.session_manager.ensure_session_caps(req_session_id);
    // 本次 load 的代次（REQ §5.2 `loadGeneration`、REQ-H04）：客户端可显式指定（重试复用
    // 同一代次），缺省由服务端生成。形状在**任何状态被改动之前**就已校验
    // （`history_page::classify` 与本行共用同一 helper）。
    let generation = crate::host::history_page::initial_load_generation(params)?
        .unwrap_or_else(LoadGeneration::generate);

    // ── ACP v1 spec: replay history via session/update BEFORE responding ──
    let replay_sender = TuiReplaySender {
        transport: transport.as_ref(),
    };
    // 条目位置戳（REQ §5.2 的 `entryId`/`position` 行）：只对**本连接**协商过分页的初始 load
    // 打——准入判据与 `history_page::classify` 同源。位置来自一次「只读 id + 存储序键」的
    // 有界读取（不读正文）；继承区条目在本线程没有行，因此只拿身份、没有位置。
    //
    // 读失败即本次 load 失败：位置戳是分页扩展契约的一部分，静默降级会让客户端按一份缺
    // 位置的稀疏首屏去合并，比一次可见的失败更难查（REQ §5.1 的失败关闭）。
    let positions = if connection.history_pagination {
        Some(
            cfg.session_resources
                .load_entry_positions(&ThreadId::from(req_session_id.to_owned()))
                .await
                .map_err(crate::host::workspace::resource_error)?,
        )
    } else {
        None
    };
    let replay_marks = match positions.as_ref() {
        Some(positions) => dispatch::ReplayMarks::new(&generation, req_session_id, positions),
        None => dispatch::ReplayMarks::without_entry_marks(&generation, req_session_id),
    };
    // 回放失败**必须**让这次 load 失败（REQ-H04）：只告警再回一个成功响应，客户端会把
    // 半份首屏当成完整恢复，然后带着一个假 cursor 继续。
    //
    // 失败时**不拆除**已发布的会话状态：那一份是完整的（装配、绑定、frozen 与有效集合
    // 都已定稿），失败的只是「本次交付」。下一次 load 命中 `AlreadyLive` 分支，重新交付
    // 同一份事实（协商连接重放有效集合；未协商连接重新从存储流式读取 canonical 历史）
    // 即修复；重试不会重复安装，本代次也没有任何东西落到客户端（通知没有完整边界，见
    // REQ-H04 的客户端侧）。
    let replay = if connection.history_pagination {
        // 分页回放读热态前必须看所有权：`HandedOff` 的空 `history_payloads` 是
        // 「正文在在途执行器里」的哨兵，不是空会话；回放它等于向客户端交付一份
        // 假历史并返回成功（A09）。`ReloadRequired` 正常已由 `prepare_existing`
        // 转为 `Resident`，这里仍按只读存储快照交付本次回放，不回填热态。
        let ownership = sessions
            .get(req_session_id)
            .expect("prepared session")
            .context_ownership;
        let history_payloads = match ownership {
            crate::host::ContextOwnership::Resident => sessions
                .get(req_session_id)
                .expect("prepared session")
                .history_payloads
                .clone(),
            crate::host::ContextOwnership::HandedOff => {
                return Err(AcpError::new(
                    -32010,
                    "Session context is owned by an in-flight prompt",
                ));
            }
            crate::host::ContextOwnership::ReloadRequired => {
                // 防御性分支（契约上不可达）：只读存储的有效集合，绝不修改热态字段。
                dispatch::load_active_session_payloads(cfg.controller.as_ref(), req_session_id)
                    .await?
            }
        };
        dispatch::replay_persisted_session_history(
            req_session_id,
            &history_payloads,
            &replay_sender,
            connection_caps,
            &replay_marks,
        )
        .await
    } else {
        // 标准 ACP 完整回放：存储是唯一来源（热态只有有效集合，用它会把归档原文漏掉）。
        dispatch::replay_canonical_history_streamed(
            cfg.controller.as_ref(),
            req_session_id,
            &replay_sender,
            connection_caps,
            &generation,
        )
        .await
    };
    replay.map_err(|error| {
        tracing::error!(
            session_id = %req_session_id,
            error = %error,
            "session/load: history replay failed"
        );
        AcpError::new(
            -32603,
            format!("session/load: history replay failed: {error}"),
        )
    })?;
    // 回放成功也不刷新共享 session generation：warm load 只推进本连接自己的
    // loadGeneration，另一连接的固定读取视图不能被重新 attach 撤销。真实
    // rewind/delete 才使受影响结构失效。

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
    //
    // 这里的 caps 刻意仍是**会话级**（`session_caps`）：命令表是会话级注册表语义
    // （`ui_commands` 明细在会话创建时按当时声明的能力注册进表，之后按会话复用），
    // 与「本次回放标记按当前连接门控」不是同一个功能面。不要为了让两处一致而改成连接 caps：
    // 那会改变命令表在重连 / 换连接时的既有语义（本需求范围之外）。
    send_available_commands_update(
        transport,
        req_session_id,
        &session_caps,
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
    let mut response = identity_response(
        serde_json::to_value(resp).map_err(|e| AcpError::new(-32603, e.to_string()))?,
        identity,
    )?;
    // 代次回带（与回放通知上的 `periLoadGeneration` 同名，见 `dispatch::replay_meta`）：
    // 客户端按它确认「这批通知属于我刚拿到的那次响应」（REQ-H04）。门与回放通知同一个
    // ——**当前连接**的 `caps.replay`：未协商回放标记的标准客户端不引入 Peri 私有字段。
    if connection_caps.replay {
        response["_meta"]["periLoadGeneration"] = Value::String(generation.as_str().to_owned());
    }
    Ok(response)
}

/// 进程内/测试入口；生产 `ServerLoop` 使用 [`load_shared::handle_resume_shared`]
/// （短锁准备 + 锁外 I/O）。
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
        marks: &crate::dispatch::EntryMarks<'_>,
    ) -> Result<(), crate::dispatch::ReplayError> {
        // H8：replay 与 live 共用同一条受众投递规则；未声明客户端受众的
        // reminder（例如 Model-only recall）不得因回放而出现在 wire 上。
        // 判据与历史页出口同源（`dispatch::reminder_visible_to_client`）。
        if !crate::dispatch::reminder_visible_to_client(reminder) {
            tracing::debug!(
                session_id = %session_id,
                source = %reminder.source,
                kind = %reminder.kind,
                delivery = ?reminder.delivery,
                "replay: system reminder is not addressed to the client audience; nothing is sent"
            );
            return Ok(());
        }
        let (event, mut data) = if caps.system_reminder {
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
        // 代次与 `session/update` 出口**同一个门**（协商过回放标记的连接，见
        // `dispatch::replay_meta`）：客户端按它把本次 load 的通知与那次成功响应配对
        // （REQ-H04）。未协商的标准客户端不引入 Peri 私有字段。
        //
        // 条目位置戳（`periEntryId` / `periPosition`）与 `session/update` 出口**同一份拼装**
        // （`dispatch::write_entry_marks`）：客户端要能用同一套身份把 reminder 与页/首屏合并。
        let mut stamp = serde_json::Map::new();
        if caps.replay {
            stamp.insert(
                "periLoadGeneration".to_owned(),
                serde_json::json!(marks.generation().as_str()),
            );
        }
        crate::dispatch::write_entry_marks(marks, &mut stamp);
        if let Some(data) = data.as_object_mut() {
            data.extend(stamp);
        }
        self.transport
            .send_notification(
                "peri/unstable_event",
                serde_json::json!({ "sessionId": session_id, "event": event, "data": data }),
            )
            .await
            .map_err(|e| crate::dispatch::ReplayError::SendFailed(e.to_string()))
    }
}
