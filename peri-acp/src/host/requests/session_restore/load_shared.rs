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

use serde_json::Value;

use peri_acp_types::recovery::{
    RecoveryErrorKind, RecoveryFailureView, RecoveryView, RecoveryViewStatus,
};
use peri_acp_types::thread::ThreadId;

use crate::host::connection::ConnectionFacts;
use crate::host::{AcpServerConfig, SharedSessions};
use crate::{dispatch, transport::types::AcpError};

use super::*;

/// `session/load` 的生产入口：调用方**不得**持有 sessions 全局锁。
pub(crate) async fn handle_load_shared(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    transport: &Arc<dyn crate::transport::AcpTransport>,
    connection: ConnectionFacts,
) -> Result<Value, AcpError> {
    let generation = crate::host::history_page::initial_load_generation(params)?
        .unwrap_or_else(LoadGeneration::generate);
    let PreparedShared {
        prepared,
        publication,
    } = match prepare_shared(params, cfg, sessions).await {
        Ok(prepared) => prepared,
        Err(error) => {
            if let Some(kind) = view_only_kind(&error) {
                return serve_view_only_load(
                    params,
                    cfg,
                    sessions,
                    transport,
                    &connection,
                    &generation,
                    kind,
                    &error,
                )
                .await;
            }
            return Err(error);
        }
    };
    let outcome: Result<Value, AcpError> = async {
        // 窗口连接的首屏不是 load 响应里的回放：声明 `peri.historyWindowV1` 后，
        // `session/load` 只装配会话，历史由随后的 `history.open` 提供。
        let environment = replay_after_load(
            cfg,
            sessions,
            &prepared.id,
            transport,
            &connection,
            &generation,
            connection.history_window_v1,
        )
        .await?;
        let local = environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg);
        let context_usage = saved_context_usage(local, sessions, &prepared.id).await;
        let mut response = finish_load_response(
            local,
            transport,
            &prepared.id,
            prepared.identity,
            &connection,
            &generation,
            context_usage,
        )
        .await?;
        if connection.history_pagination {
            crate::host::history_page::attach_initial_cursor(
                cfg,
                sessions,
                &prepared.id,
                &mut response,
            )
            .await?;
        }
        Ok(response)
    }
    .await;
    match outcome {
        Ok(response) => Ok(response),
        Err(error) => {
            Err(rollback_after_restore_error(cfg, sessions, publication.as_ref(), error).await)
        }
    }
}

/// `threads.context_usage_tokens` 与当前有效 context window 的只读投影。
async fn saved_context_usage(
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    session_id: &str,
) -> Option<Value> {
    let thread_id = {
        let states = sessions.lock().await;
        states
            .get(session_id)
            .map(|state| ThreadId::from(state.thread_id.clone()))
    }?;
    let tokens = cfg
        .session_resources
        .load_session_meta(&thread_id)
        .await
        .ok()
        .and_then(|meta| meta.context_usage_tokens);
    let window = u64::from(cfg.provider.read().effective_context_window());
    Some(serde_json::json!({
        "tokens": tokens,
        "window": window,
    }))
}

/// 仅测试：在 cold publication 发布后、handler 后处理前注入一次状态变更。
#[cfg(test)]
pub(crate) mod publication_test_hook {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};

    use crate::host::{SessionState, SharedSessions};

    type Hook = Box<dyn FnOnce(&mut SessionState) + Send>;
    static HOOKS: LazyLock<Mutex<HashMap<String, Hook>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    pub(crate) fn install(session_id: &str, hook: impl FnOnce(&mut SessionState) + Send + 'static) {
        HOOKS
            .lock()
            .unwrap()
            .insert(session_id.to_owned(), Box::new(hook));
    }

    pub(super) async fn apply(session_id: &str, sessions: &SharedSessions) {
        let hook = HOOKS.lock().unwrap().remove(session_id);
        if let Some(hook) = hook {
            let mut states = sessions.lock().await;
            if let Some(state) = states.get_mut(session_id) {
                hook(state);
            }
        }
    }
}

/// 冷恢复发布的唯一身份；只有同一 generation 仍占据 map 时才有清理权。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RestorePublication {
    pub(crate) session_id: String,
    pub(crate) session_generation: u64,
}

impl RestorePublication {
    fn matches(&self, state: &SessionState) -> bool {
        state.session_id == self.session_id && state.session_generation == self.session_generation
    }
}

/// 共享准备结果：live 复用没有 publication，cold 发布携带精确 generation。
pub(super) struct PreparedShared {
    pub(super) prepared: PreparedSession,
    pub(super) publication: Option<RestorePublication>,
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
    let (prepared, publication) = if already_loaded {
        (prepare_live_shared(params, cfg, sessions).await?, None)
    } else {
        let (prepared, publication) = prepare_cold_shared(params, cfg, sessions).await?;
        (prepared, Some(publication))
    };
    #[cfg(test)]
    if publication.is_some() {
        publication_test_hook::apply(&prepared.id, sessions).await;
    }
    Ok(PreparedShared {
        prepared,
        publication,
    })
}

/// 冷恢复：复用 `prepare_existing` 完成装配，但只在**局部 scratch map** 上操作；
/// 装配完成后短锁发布到真实 map。
async fn prepare_cold_shared(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
) -> Result<(PreparedSession, RestorePublication), AcpError> {
    let mut scratch = HashMap::new();
    let prepared = super::prepare_existing(params, cfg, &mut scratch).await?;
    let PreparedSession { id, identity } = prepared;
    let state = scratch
        .remove(&id)
        .ok_or_else(|| AcpError::new(-32603, "cold restore produced no session state"))?;
    let publication = RestorePublication {
        session_id: id.clone(),
        session_generation: state.session_generation,
    };
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

    Ok((PreparedSession { id, identity }, publication))
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
        return Err(super::view_only_error(
            "read_only",
            crate::host::workspace::resource_error(SessionResourceError::new(
                SessionResourceErrorKind::ReadOnlyStore,
            )),
        ));
    }
    match availability.execution {
        Some(ExecutionAvailability::Available | ExecutionAvailability::BindingMissing) => {}
        reason => {
            return Err(super::view_only_error(
                "unavailable",
                AcpError::new(-32010, format!("Session execution unavailable: {reason:?}")),
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

/// 回滚已经发布到共享 map 的 cold restore。
///
/// 清理期间 state 保持 `closing` 并继续持有 environment；只有 manager 与 environment
/// 都确认排空后，才按 publication generation 删除。旧 generation 永不删除新 state。
pub(crate) async fn rollback_restore_publication(
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    publication: &RestorePublication,
) -> Result<(), AcpError> {
    let environment = {
        let mut states = sessions.lock().await;
        let Some(state) = states.get_mut(&publication.session_id) else {
            return Ok(());
        };
        if !publication.matches(state) {
            return Ok(());
        }
        state.closing = true;
        state.environment.clone()
    };
    let local = environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg);
    let manager_error = local
        .session_manager
        .close_session(&publication.session_id)
        .await
        .err()
        .map(|error| error.to_string());
    let environment_drained = match environment.as_ref() {
        Some(environment) => environment.shutdown().await,
        None => true,
    };
    if manager_error.is_none() && environment_drained {
        let mut states = sessions.lock().await;
        if states
            .get(&publication.session_id)
            .is_some_and(|state| publication.matches(state))
        {
            states.remove(&publication.session_id);
        }
        return Ok(());
    }
    let manager_detail = manager_error.as_deref().unwrap_or("complete");
    tracing::error!(
        session_id = %publication.session_id,
        session_generation = publication.session_generation,
        manager = manager_detail,
        environment_drained,
        "cold restore cleanup remains pending"
    );
    Err(AcpError::new(
        -32010,
        format!(
            "Session {} cleanup incomplete: manager={manager_detail}, environment_drained={environment_drained}",
            publication.session_id
        ),
    )
    .with_data(serde_json::json!({
        "reason": "session_cleanup_incomplete",
        "sessionId": publication.session_id,
    })))
}

pub(super) async fn rollback_after_restore_error(
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    publication: Option<&RestorePublication>,
    error: AcpError,
) -> AcpError {
    let Some(publication) = publication else {
        return error;
    };
    match rollback_restore_publication(cfg, sessions, publication).await {
        Ok(()) => error,
        Err(cleanup) => {
            let mut combined = AcpError::new(
                cleanup.code,
                format!("{}; {}", error.message, cleanup.message),
            );
            combined.data = cleanup.data;
            combined
        }
    }
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

/// 只读/不可执行装配错误的视图态标记（由 `session_restore::view_only_error` 写入）。
fn view_only_kind(error: &AcpError) -> Option<&'static str> {
    let data = error.data.as_ref()?;
    if data.get("reason").and_then(Value::as_str) != Some("view_only") {
        return None;
    }
    Some(match data.get("kind").and_then(Value::as_str) {
        Some("read_only") => "read_only",
        _ => "unavailable",
    })
}

/// 建立只读视图态：发布无执行环境的 `SessionState`，在 runtime 上留下执行阻断。
///
/// 已 live 的会话不降级为视图态——那会把正在执行的实例替换成不可执行视图。
async fn ensure_view_only_state(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    kind: &'static str,
    error: &AcpError,
) -> Result<
    (
        String,
        ThreadId,
        RecoveryFailureView,
        Option<RestorePublication>,
    ),
    AcpError,
> {
    let session_id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?
        .to_owned();
    let thread = ThreadId::from(session_id.clone());
    let meta = match cfg.session_resources.load_session_meta(&thread).await {
        Ok(meta) => meta,
        // 连元数据都读不到：保持原始装配错误，不伪装成可浏览历史。
        Err(_) => return Err(error.clone()),
    };
    let publication = {
        let mut states = sessions.lock().await;
        if let Some(state) = states.get(&session_id) {
            // 已有 live 会话不降级；已发布的视图态可重复加载（历史/占用重放）。
            if !state.is_view_only() {
                return Err(error.clone());
            }
            None
        } else {
            let state = crate::host::SessionState::view_only(
                session_id.clone(),
                thread.as_str().to_owned(),
                meta.cwd.clone(),
            );
            let publication = RestorePublication {
                session_id: session_id.clone(),
                session_generation: state.session_generation,
            };
            states.insert(session_id.clone(), state);
            Some(publication)
        }
    };
    cfg.session_manager.ensure_session(&session_id, &meta.cwd);
    let kind = if kind == "read_only" {
        RecoveryErrorKind::ReadOnly
    } else {
        RecoveryErrorKind::Unavailable
    };
    let failure = RecoveryFailureView {
        thread_id: thread.clone(),
        kind,
        detail: error.message.clone(),
        execution_blocked: true,
    };
    if let Some(runtime) = cfg.session_manager.get_session(&session_id) {
        runtime.set_recovery_execution_block(failure.clone());
    }
    Ok((session_id, thread, failure, publication))
}

fn view_only_recovery_view(
    thread: &ThreadId,
    failure: RecoveryFailureView,
) -> Result<Value, AcpError> {
    serde_json::to_value(RecoveryView {
        status: RecoveryViewStatus::Failed,
        root_thread_id: Some(thread.clone()),
        entered_thread_id: thread.clone(),
        recovered: Vec::new(),
        reused: Vec::new(),
        skipped: Vec::new(),
        failures: Vec::new(),
        error: Some(failure),
        execution_blocked: true,
    })
    .map_err(|error| AcpError::new(-32603, error.to_string()))
}

/// 只读/不可执行 store 的 load：装配失败但历史与占用仍可展示，执行被阻断。
#[allow(clippy::too_many_arguments)]
async fn serve_view_only_load(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    transport: &Arc<dyn crate::transport::AcpTransport>,
    connection: &ConnectionFacts,
    generation: &LoadGeneration,
    kind: &'static str,
    error: &AcpError,
) -> Result<Value, AcpError> {
    let (session_id, thread, failure, publication) =
        ensure_view_only_state(params, cfg, sessions, kind, error).await?;
    let outcome: Result<Value, AcpError> = async {
        replay_after_load(
            cfg,
            sessions,
            &session_id,
            transport,
            connection,
            generation,
            connection.history_window_v1,
        )
        .await?;
        let identity = response_identity(cfg, &session_id).await.ok().flatten();
        let context_usage = saved_context_usage(cfg, sessions, &session_id).await;
        let mut response = finish_load_response(
            cfg,
            transport,
            &session_id,
            identity,
            connection,
            generation,
            context_usage,
        )
        .await?;
        if connection.history_pagination {
            crate::host::history_page::attach_initial_cursor(
                cfg,
                sessions,
                &session_id,
                &mut response,
            )
            .await?;
        }
        response["_meta"]["peri.recovery"] = view_only_recovery_view(&thread, failure)?;
        Ok(response)
    }
    .await;
    match outcome {
        Ok(response) => Ok(response),
        Err(error) => {
            Err(rollback_after_restore_error(cfg, sessions, publication.as_ref(), error).await)
        }
    }
}

/// 只读/不可执行 store 的 resume：不取执行环境，只回身份、占用与阻断结果。
async fn serve_view_only_resume(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    transport: &Arc<dyn crate::transport::AcpTransport>,
    kind: &'static str,
    error: &AcpError,
) -> Result<Value, AcpError> {
    let (session_id, thread, failure, _publication) =
        ensure_view_only_state(params, cfg, sessions, kind, error).await?;
    let context_usage = saved_context_usage(cfg, sessions, &session_id).await;
    let caps = cfg.session_manager.ensure_session_caps(&session_id);
    send_available_commands_update(
        transport,
        &session_id,
        &caps,
        cfg.session_manager.command_registry_for(&session_id),
        cfg.stdio_command_filter,
    )
    .await;
    prewarm_session_mcp_discovery(cfg, &session_id);
    let identity = response_identity(cfg, &session_id).await.ok().flatten();
    let mut response = identity_response(
        serde_json::to_value(ResumeSessionResponse::new())
            .map_err(|error| AcpError::new(-32603, error.to_string()))?,
        identity,
    )?;
    if let Some(usage) = context_usage {
        response["_meta"]["peri.contextUsage"] = usage;
    }
    response["_meta"]["peri.recovery"] = view_only_recovery_view(&thread, failure)?;
    Ok(response)
}

async fn finish_load_response(
    cfg: &AcpServerConfig,
    transport: &Arc<dyn crate::transport::AcpTransport>,
    session_id: &str,
    identity: Option<Value>,
    connection: &ConnectionFacts,
    generation: &LoadGeneration,
    context_usage: Option<Value>,
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
    if let Some(usage) = context_usage {
        response["_meta"]["peri.contextUsage"] = usage;
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
    let PreparedShared {
        prepared,
        publication,
    } = match prepare_shared(params, cfg, sessions).await {
        Ok(prepared) => prepared,
        Err(error) => {
            if let Some(kind) = view_only_kind(&error) {
                return serve_view_only_resume(params, cfg, sessions, transport, kind, &error)
                    .await;
            }
            return Err(error);
        }
    };
    let outcome: Result<Value, AcpError> = async {
        let environment = {
            let states = sessions.lock().await;
            states
                .get(&prepared.id)
                .ok_or_else(|| AcpError::new(-32602, "session not found"))?
                .environment
                .clone()
        };
        let local = environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg);
        let context_usage = saved_context_usage(local, sessions, &prepared.id).await;
        let caps = local.session_manager.ensure_session_caps(&prepared.id);
        send_available_commands_update(
            transport,
            &prepared.id,
            &caps,
            local.session_manager.command_registry_for(&prepared.id),
            local.stdio_command_filter,
        )
        .await;
        prewarm_session_mcp_discovery(local, &prepared.id);
        let resp = ResumeSessionResponse::new();
        let mut response = identity_response(
            serde_json::to_value(resp).map_err(|e| AcpError::new(-32603, e.to_string()))?,
            prepared.identity,
        )?;
        if let Some(usage) = context_usage {
            response["_meta"]["peri.contextUsage"] = usage;
        }
        Ok(response)
    }
    .await;
    match outcome {
        Ok(response) => Ok(response),
        Err(error) => {
            Err(rollback_after_restore_error(cfg, sessions, publication.as_ref(), error).await)
        }
    }
}
