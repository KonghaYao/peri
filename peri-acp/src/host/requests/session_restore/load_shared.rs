//! `session/load` 的分阶段（短锁）实现。
//!
//! 生产路径不持有 `SharedSessions` 全局锁做 DB I/O：
//! 1. 短锁读取 live 事实 / 清理拒绝条件；
//! 2. 释放锁，完成 availability / drain / workspace / frozen / history 读取；
//! 3. 短锁写回并复核 generation/ownership/closing；不匹配显式失败；
//! 4. 回放成功后才刷新 generation（G3：被拒绝的 load 不得撤销旧窗口）。
//!
//! 旧的 `prepare_existing`/`handle_load` 仍服务进程内测试入口；生产 `ServerLoop` 只走本模块。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use peri_acp_types::history_v2::{
    DecimalU64, HistoryLiveBoundary, HistoryV2Backend, HISTORY_META_KEY,
};
use peri_acp_types::thread::ThreadId;

use crate::host::connection::ConnectionFacts;
use crate::host::history_service::{self, HistoryInitialContext};
use crate::host::history_v2_adapter::HistoryV2BackendAdapter;
use crate::host::history_v2_load::LoadInitial;
use crate::host::{AcpServerConfig, ContextOwnership, SharedSessions};
use crate::{dispatch, transport::types::AcpError};

use super::*;

/// initial 沿用独立的可取消恢复期限（设计 §12.4）：分页 10 秒总期限会误杀合法冷启动。
const LOAD_INITIAL_TIMEOUT: Duration = Duration::from_secs(60);

/// `session/load` 的生产入口：调用方**不得**持有 sessions 全局锁。
pub(crate) async fn handle_load_shared(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    transport: &Arc<dyn crate::transport::AcpTransport>,
    connection: ConnectionFacts,
    history: Option<LoadInitial>,
) -> Result<Value, AcpError> {
    let generation = crate::host::history_page::initial_load_generation(params)?
        .unwrap_or_else(LoadGeneration::generate);
    let prepared = prepare_shared(params, cfg, sessions).await?.prepared;
    // 窗口连接的首屏不是 load 响应里的回放：声明 `peri.historyWindowV1` 后，
    // `session/load` 只装配会话，历史由随后的 `history.open` 提供。判据取本连接
    // 的协商事实，不能用 `history_pagination` 代替——两者独立。
    // History V2 initial 同样自行交付首屏，不再执行 V1 回放；所有权检查仍保留。
    let skip_replay = connection.history_window_v1 || history.is_some();
    let environment = replay_after_load(
        cfg,
        sessions,
        &prepared.id,
        transport,
        &connection,
        &generation,
        skip_replay,
    )
    .await?;
    let cfg = environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg);
    let mut response = finish_load_response(
        cfg,
        transport,
        &prepared.id,
        prepared.identity,
        &connection,
        &generation,
    )
    .await?;
    // 非 V2 连接成功接管会话：撤销上一条连接留下的实时投递绑定，事件回到 V1 wire
    // 形状。失败路径（装配/回放拒绝）不破坏既有交付。
    if history.is_none() {
        crate::host::live_delivery::detach_session(sessions, &prepared.id).await;
    }
    if let Some(initial) = history {
        attach_history_initial(
            cfg,
            sessions,
            &prepared.id,
            &connection,
            initial,
            &mut response,
        )
        .await?;
    }
    Ok(response)
}

/// 在 standard `session/load` 响应上挂载 `_meta["peri.history"]`。
///
/// 顺序保证：
/// 1. 先量出完整响应的固定部分（配置、模式、身份与 JSON-RPC 包络），把剩余预算交给
///    initial；H4a 的页面字节复核因此覆盖整份 load 响应，而不只是页面本身。
/// 2. initial 成功后才写入 `_meta`；失败返回带 effect 的领域错误，会话保持已恢复状态，
///    同 requestKey 可重试。
async fn attach_history_initial(
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    session_id: &str,
    connection: &ConnectionFacts,
    initial: LoadInitial,
    response: &mut Value,
) -> Result<(), AcpError> {
    // V2 交付用 deliveryId/liveBoundary；V1 的 periLoadGeneration 不得混入。
    let meta = response_meta(response)?;
    meta.remove("periLoadGeneration");
    meta.insert(HISTORY_META_KEY.to_owned(), json!({}));
    let reserved_response_bytes = serde_json::to_vec(response)
        .map_err(|error| AcpError::new(-32603, error.to_string()))?
        .len()
        .saturating_sub(2) as u64;

    let (runtime_generation, execution_ready) = execution_facts(sessions, session_id).await?;
    let backend: Arc<dyn HistoryV2Backend> = Arc::new(HistoryV2BackendAdapter::new(
        Arc::clone(&cfg.session_resources),
        connection.caps.clone(),
    ));
    let live = Arc::clone(&initial.live);
    let context = HistoryInitialContext {
        session_id: session_id.to_owned(),
        backend: Some(backend),
        negotiated: true,
        limits: initial.limits,
        runtime_generation: runtime_generation.clone(),
        execution_ready,
        live: Some(Arc::clone(&live)),
        // 生产路径由 `begin` 生成边界；该字段只在无注册表的测试路径使用。
        live_boundary: HistoryLiveBoundary {
            stream_id: String::new(),
            after_seq: DecimalU64::new(0),
        },
        reserved_response_bytes,
    };
    let result = match tokio::time::timeout(
        LOAD_INITIAL_TIMEOUT,
        initial.service.initial(initial.request, context),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => {
            live.close_if_paused(session_id);
            return Err(history_service::initial_timeout_error(
                execution_ready,
                &runtime_generation,
            ));
        }
    }?;
    let value = serde_json::to_value(result).map_err(|error| {
        live.close_if_paused(session_id);
        AcpError::new(-32603, error.to_string())
    })?;
    // 成功交付之后才把注册表绑定到会话：load 期间没有可执行的 prompt，绑定点
    // 与响应发布之间不存在「sink 拿不到流」的窗口。
    let previous = {
        let mut states = sessions.lock().await;
        let state = states.get_mut(session_id).ok_or_else(|| {
            AcpError::new(-32603, "session disappeared while loading history initial")
        })?;
        state.live_delivery.replace(Arc::clone(&live))
    };
    if let Some(previous) = previous {
        if !Arc::ptr_eq(&previous, &live) {
            // A44：新 initial 原子替换旧 delivery，旧流关闭且不再投递。
            previous.close(session_id);
        }
    }
    response_meta(response)?.insert(HISTORY_META_KEY.to_owned(), value);
    Ok(())
}

fn response_meta(response: &mut Value) -> Result<&mut serde_json::Map<String, Value>, AcpError> {
    let object = response
        .as_object_mut()
        .ok_or_else(|| AcpError::new(-32603, "session/load response is not an object"))?;
    object
        .entry("_meta")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| AcpError::new(-32603, "session/load response _meta is not an object"))
}

/// 当前生命周期的执行事实：`runtimeGeneration` 取本会话实例代次；只有正文仍驻留
/// Host（`Resident`）且会话未关闭时，prompt 才可立即开始。
async fn execution_facts(
    sessions: &SharedSessions,
    session_id: &str,
) -> Result<(String, bool), AcpError> {
    let states = sessions.lock().await;
    let state = states
        .get(session_id)
        .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
    if state.closing {
        return Err(AcpError::new(-32010, "Session is closing"));
    }
    Ok((
        format!("runtime-{}", state.session_generation),
        state.context_ownership == ContextOwnership::Resident,
    ))
}

/// 共享的“确保会话已 live”阶段：load/resume/fork 复用，不包含回放或具体响应语义。
pub(super) struct PreparedShared {
    pub(super) prepared: PreparedSession,
}

pub(super) async fn prepare_shared(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
) -> Result<PreparedShared, AcpError> {
    let already_loaded = {
        let states = sessions.lock().await;
        super::reject_incomplete_local_close(params, &states)?;
        params
            .get("sessionId")
            .and_then(Value::as_str)
            .and_then(|id| states.get(id))
            .is_some()
    };
    let prepared = if already_loaded {
        prepare_live_shared(params, cfg, sessions).await?
    } else {
        prepare_cold_shared(params, cfg, sessions).await?
    };
    Ok(PreparedShared { prepared })
}

/// 冷恢复：复用 `prepare_existing` 完成装配，但只在**局部 scratch map** 上操作；
/// 装配完成后短锁发布到真实 map。
async fn prepare_cold_shared(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
) -> Result<PreparedSession, AcpError> {
    let mut scratch = HashMap::new();
    let prepared = super::prepare_existing(params, cfg, &mut scratch).await?;
    let PreparedSession { id, identity } = prepared;
    let state = scratch
        .remove(&id)
        .ok_or_else(|| AcpError::new(-32603, "cold restore produced no session state"))?;
    let conflict = {
        let mut states = sessions.lock().await;
        if states.contains_key(&id) {
            Some(state)
        } else {
            states.insert(id.clone(), state);
            None
        }
    };
    if let Some(mut state) = conflict {
        // 另一个 load 抢先发布：清理本次装配的环境 **和 session_manager 登记**，
        // 不覆盖既有 live 状态，也不留下幽灵 manager。
        cleanup_unpublished_restore(cfg, &id, state.environment.take()).await;
        return Err(AcpError::new(
            -32010,
            "session became live while loading; retry the load",
        ));
    }

    Ok(PreparedSession { id, identity })
}

/// 已 live：preflight 在锁外完成，payload 回填与 generation 刷新各自短锁。
async fn prepare_live_shared(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
) -> Result<PreparedSession, AcpError> {
    super::reject_closing_session(params, cfg).await?;
    let id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?
        .to_owned();
    let availability = cfg
        .session_resources
        .inspect_availability(Some(&id))
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
        .drain_persistence(&id)
        .await
        .map_err(crate::host::workspace::resource_error)?;
    let effective_servers = super::super::session_mcp_servers(params)?;
    let legacy_adoption =
        super::legacy_session::prepare_for_restore(cfg, &id, None, effective_servers).await?;
    let mut environment_slot = legacy_adoption
        .as_ref()
        .and_then(|adoption| adoption.environment.clone());
    // 失败路径统一排空 legacy 候选环境（与 prepare_existing 同规）。
    let outcome: Result<PreparedSession, AcpError> = async {
        // 与旧路径相同的准入校验；结果不参与 live 回放，但失败仍必须让 load 失败。
        let _workspace = crate::host::workspace::validate_expected(
            cfg,
            &id,
            params.get("cwd").and_then(Value::as_str),
        )
        .await?;
        let persisted = super::load_frozen_bytes(cfg, &id).await?;
        super::decode_frozen_snapshot(&persisted).map_err(workspace_error)?;
        let identity = super::response_identity(cfg, &id).await?;

        let live = {
            let mut states = sessions.lock().await;
            states.get_mut(&id).map(|state| {
                crate::host::ownership::mark_stale_if_dead(state);
                (state.session_generation, state.context_ownership)
            })
        };
        let Some((generation_before, ownership)) = live else {
            return Err(AcpError::new(-32602, "session not found"));
        };
        let needs_reload = matches!(ownership, crate::host::ContextOwnership::ReloadRequired);
        if needs_reload {
            let payloads =
                dispatch::load_active_session_payloads(cfg.controller.as_ref(), &id).await?;
            let mut states = sessions.lock().await;
            let state = states
                .get_mut(&id)
                .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
            if state.closing {
                return Err(AcpError::new(-32010, "Session is closing"));
            }
            if state.session_generation != generation_before
                || state.context_ownership != crate::host::ContextOwnership::ReloadRequired
            {
                return Err(AcpError::new(
                    -32010,
                    "session changed while loading; retry the load",
                ));
            }
            state.history_payloads = payloads;
            state.context_ownership = crate::host::ContextOwnership::Resident;
        } else if ownership == crate::host::ContextOwnership::Resident {
            // 空驻留会话允许与存储重新对齐；非空 Resident / HandedOff 不动热态。
            let empty = {
                let states = sessions.lock().await;
                states
                    .get(&id)
                    .is_some_and(|state| state.history_payloads.is_empty())
            };
            if empty {
                let payloads =
                    dispatch::load_active_session_payloads(cfg.controller.as_ref(), &id).await?;
                let mut states = sessions.lock().await;
                let state = states
                    .get_mut(&id)
                    .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
                if state.closing {
                    return Err(AcpError::new(-32010, "Session is closing"));
                }
                if state.session_generation != generation_before
                    || state.context_ownership != crate::host::ContextOwnership::Resident
                {
                    return Err(AcpError::new(
                        -32010,
                        "session changed while loading; retry the load",
                    ));
                }
                // 另一个路径可能已经完成回填；不要用较早的存储快照覆盖它。
                if state.history_payloads.is_empty() {
                    state.history_payloads = payloads;
                }
            }
        }

        // 采纳候选环境不会被 live 路径使用：显式排空，失败上报。
        if let Some(environment) = environment_slot.take() {
            if !environment.shutdown().await {
                return Err(AcpError::new(
                    -32603,
                    "unused legacy bootstrap environment did not confirm shutdown",
                ));
            }
        }

        Ok(PreparedSession {
            id: id.clone(),
            identity,
        })
    }
    .await;
    match outcome {
        Ok(prepared) => Ok(prepared),
        Err(error) => {
            Err(super::shutdown_bootstrapped_environment(environment_slot.take(), error).await)
        }
    }
}

/// 回放（锁外读热态快照）；成功后 live 路径才刷新 generation（G3）。
///
/// `skip_replay` 为窗口连接的协议契约：load 只装配会话、不交付历史，首屏由随后
/// 的 `session/load history.open` 窗口页提供。**声明能力后必须 open，否则首屏为空**；
/// 这不是可选优化。跳过回放不等于跳过所有权检查：`HandedOff` 仍必须拒绝。
async fn replay_after_load(
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    session_id: &str,
    transport: &Arc<dyn crate::transport::AcpTransport>,
    connection: &ConnectionFacts,
    generation: &LoadGeneration,
    skip_replay: bool,
) -> Result<Option<Arc<crate::host::workspace::SessionEnvironment>>, AcpError> {
    if skip_replay {
        // 所有权检查与回放内容无关，不能因跳过回放而绕过：
        // - `HandedOff`：正文在在途执行器里，load 必须失败（-32010）；
        // - `ReloadRequired`：窗口读取以存储为事实源，这里不回填热态，直接返回
        //   environment（与分页回放的防御性分支同口径）。
        let (environment, ownership) = {
            let states = sessions.lock().await;
            let state = states
                .get(session_id)
                .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
            (state.environment.clone(), state.context_ownership)
        };
        if ownership == crate::host::ContextOwnership::HandedOff {
            return Err(AcpError::new(
                -32010,
                "Session context is owned by an in-flight prompt",
            ));
        }
        return Ok(environment);
    }
    let replay_sender = TuiReplaySender {
        transport: transport.as_ref(),
    };
    let positions = if connection.history_pagination {
        Some(
            cfg.session_resources
                .load_entry_positions(&ThreadId::from(session_id.to_owned()))
                .await
                .map_err(crate::host::workspace::resource_error)?,
        )
    } else {
        None
    };
    let replay_marks = match positions.as_ref() {
        Some(positions) => dispatch::ReplayMarks::new(generation, session_id, positions),
        None => dispatch::ReplayMarks::without_entry_marks(generation, session_id),
    };
    let (environment, ownership, history_payloads) = {
        let states = sessions.lock().await;
        let state = states
            .get(session_id)
            .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
        (
            state.environment.clone(),
            state.context_ownership,
            state.history_payloads.clone(),
        )
    };
    let local = environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg);
    let replay = if connection.history_pagination {
        let history_payloads = match ownership {
            crate::host::ContextOwnership::Resident => history_payloads,
            crate::host::ContextOwnership::HandedOff => {
                return Err(AcpError::new(
                    -32010,
                    "Session context is owned by an in-flight prompt",
                ))
            }
            crate::host::ContextOwnership::ReloadRequired => {
                // 防御性分支：只读存储，不回填热态。
                dispatch::load_active_session_payloads(local.controller.as_ref(), session_id)
                    .await?
            }
        };
        dispatch::replay_persisted_session_history(
            session_id,
            &history_payloads,
            &replay_sender,
            &connection.caps,
            &replay_marks,
        )
        .await
    } else {
        dispatch::replay_canonical_history_streamed(
            local.controller.as_ref(),
            session_id,
            &replay_sender,
            &connection.caps,
            generation,
        )
        .await
    };
    replay.map_err(|error| {
        tracing::error!(
            session_id,
            error = %error,
            "session/load: history replay failed"
        );
        AcpError::new(
            -32603,
            format!("session/load: history replay failed: {error}"),
        )
    })?;
    // warm load 只推进该连接自己的 loadGeneration（由调用方持有），不得刷新共享
    // session generation：票据绑定的是 open 时的固定视图，另一连接的读取不能被
    // 一次重新 attach 撤销。只有真实 rewind/delete 才使受影响结构失效。
    Ok(environment)
}

/// 清理未发布的恢复装配：环境与 session_manager 登记都不能泄漏。
pub(crate) async fn cleanup_unpublished_restore(
    cfg: &AcpServerConfig,
    session_id: &str,
    environment: Option<Arc<crate::host::workspace::SessionEnvironment>>,
) {
    let local = environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg);
    let _ = local.session_manager.close_session(session_id).await;
    if let Some(environment) = environment {
        let _ = environment.shutdown().await;
    }
}

async fn finish_load_response(
    cfg: &AcpServerConfig,
    transport: &Arc<dyn crate::transport::AcpTransport>,
    session_id: &str,
    identity: Option<Value>,
    connection: &ConnectionFacts,
    generation: &LoadGeneration,
) -> Result<Value, AcpError> {
    let session_caps = cfg.session_manager.ensure_session_caps(session_id);
    send_config_option_update(transport.as_ref(), session_id, cfg).await;
    let modes = build_mode_state(&cfg.permission_mode);
    let config_options = {
        let c = cfg.peri_config.read();
        let p = cfg.provider.read();
        make_config_options(&c, &p, cfg.permission_mode.load())
    };
    let resp = LoadSessionResponse::new()
        .modes(modes)
        .config_options(config_options);
    send_available_commands_update(
        transport,
        session_id,
        &session_caps,
        cfg.session_manager.command_registry_for(session_id),
        cfg.stdio_command_filter,
    )
    .await;
    prewarm_session_mcp_discovery(cfg, session_id);
    let mut response = identity_response(
        serde_json::to_value(resp).map_err(|e| AcpError::new(-32603, e.to_string()))?,
        identity,
    )?;
    if connection.caps.replay {
        response["_meta"]["periLoadGeneration"] = Value::String(generation.as_str().to_owned());
    }
    Ok(response)
}

/// `session/resume` 的生产入口：与 load 共用“确保 live”阶段，但不回放、不刷新 generation。
pub(crate) async fn handle_resume_shared(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    transport: &Arc<dyn crate::transport::AcpTransport>,
) -> Result<Value, AcpError> {
    let PreparedShared { prepared, .. } = prepare_shared(params, cfg, sessions).await?;
    let environment = {
        let states = sessions.lock().await;
        states
            .get(&prepared.id)
            .ok_or_else(|| AcpError::new(-32602, "session not found"))?
            .environment
            .clone()
    };
    let cfg = environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg);
    let caps = cfg.session_manager.ensure_session_caps(&prepared.id);
    send_available_commands_update(
        transport,
        &prepared.id,
        &caps,
        cfg.session_manager.command_registry_for(&prepared.id),
        cfg.stdio_command_filter,
    )
    .await;
    prewarm_session_mcp_discovery(cfg, &prepared.id);
    let resp = ResumeSessionResponse::new();
    identity_response(
        serde_json::to_value(resp).map_err(|e| AcpError::new(-32603, e.to_string()))?,
        prepared.identity,
    )
}
