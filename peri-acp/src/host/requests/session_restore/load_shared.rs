//! `session/load` 的分阶段（短锁）实现。
//!
//! 生产路径不持有 `SharedSessions` 全局锁做 DB I/O：
//! 1. 短锁读取 live 事实 / 清理拒绝条件；
//! 2. 释放锁，完成 availability / drain / workspace / frozen / history 读取；
//! 3. 短锁写回并复核 generation/ownership/closing；不匹配显式失败；
//! 4. 交付当前连接的历史视图；warm load 保留同一实例的 generation。
//!
//! 旧的 `prepare_existing`/`handle_load` 仍服务进程内测试入口；生产 `ServerLoop` 只走本模块。

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use peri_acp_types::history_v2::{
    DecimalU64, HistoryLiveBoundary, HistoryV2Backend, HISTORY_META_KEY,
};
use peri_acp_types::recovery::{
    RecoveryErrorKind, RecoveryFailureView, RecoveryView, RecoveryViewStatus,
};
use peri_acp_types::thread::ThreadId;

use crate::dispatch;
use crate::host::connection::ConnectionFacts;
use crate::host::history_service::{self, HistoryInitialContext};
use crate::host::history_v2_adapter::HistoryV2BackendAdapter;
use crate::host::history_v2_load::LoadInitial;
use crate::host::{AcpServerConfig, ContextOwnership, SharedSessions};
use crate::transport::types::AcpError;

use super::shared_preparation::{
    prepare_shared, rollback_after_restore_error, PreparedShared, RestorePublication,
};
use super::{
    LoadGeneration, LoadSessionResponse, RestoreContext, ResumeSessionResponse, SessionState,
    TuiReplaySender,
};

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
    let skip_replay = connection.history_window_v1 || history.is_some();
    let context = if skip_replay {
        RestoreContext::OnFirstPrompt
    } else {
        RestoreContext::Immediate
    };
    let PreparedShared {
        prepared,
        publication,
    } = match prepare_shared(params, cfg, sessions, context).await {
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
                    history,
                    kind,
                    &error,
                )
                .await;
            }
            return Err(error);
        }
    };
    let mut restored = false;
    let outcome: Result<Value, AcpError> = async {
        // 窗口连接的首屏不是 load 响应里的回放：声明 `peri.historyWindowV1` 后，
        // `session/load` 只装配会话，历史由随后的 `history.open` 提供。判据取本连接
        // 的协商事实，不能用 `history_pagination` 代替——两者独立。
        // History V2 initial 同样自行交付首屏，不再执行 V1 回放；所有权检查仍保留。
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
        let local = environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg);
        // 进入会话的占用初值：已保存快照 + 当前有效 window。读取失败不阻断 load，
        // 缺失字段由客户端按未知展示（设计 §2.2）。
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
        // 会话已恢复：之后 initial 交付失败只回领域错误、保留会话（同 requestKey 可重试），
        // 不能再走 cold publication 回滚把已装配的会话拆掉。
        restored = true;
        attach_load_history(
            local,
            cfg,
            sessions,
            &prepared.id,
            &connection,
            history,
            &mut response,
        )
        .await?;
        Ok(response)
    }
    .await;
    match outcome {
        Ok(response) => Ok(response),
        Err(error) => Err(rollback_after_restore_error(
            cfg,
            sessions,
            publication_for(restored, &publication),
            error,
        )
        .await),
    }
}

/// 只有恢复尚未完成时才允许回滚 cold publication。
fn publication_for(
    restored: bool,
    publication: &Option<RestorePublication>,
) -> Option<&RestorePublication> {
    if restored {
        None
    } else {
        publication.as_ref()
    }
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
        .saturating_sub(2) as u64
        // result 之外的 JSON-RPC 包络也必须计入预算，页面才能用满剩余额度。
        + initial.envelope_bytes;

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
    // 与响应发布之间不存在「sink 拿不到流」的窗口。绑定前复核代次：initial 期间
    // 另一连接可能关闭并重建会话，旧代次的流与元数据不得挂到新代次上。
    let (previous, generation_changed) = {
        let mut states = sessions.lock().await;
        let state = states.get_mut(session_id).ok_or_else(|| {
            AcpError::new(-32603, "session disappeared while loading history initial")
        })?;
        if runtime_generation != format!("runtime-{}", state.session_generation) {
            (None, true)
        } else {
            (state.live_delivery.replace(Arc::clone(&live)), false)
        }
    };
    if generation_changed {
        live.close_if_paused(session_id);
        return Err(AcpError::new(
            -32010,
            "session changed while loading history initial; retry the load",
        ));
    }
    if let Some(previous) = previous {
        if !Arc::ptr_eq(&previous, &live) {
            // A44：新 initial 原子替换旧 delivery，旧流关闭且不再投递。
            previous.close(session_id);
        }
    }
    response_meta(response)?.insert(HISTORY_META_KEY.to_owned(), value);
    Ok(())
}

/// 恢复完成后交付历史：V2 initial 优先，否则按协商挂 V1 cursor 并撤销旧实时投递。
///
/// 非 V2 连接成功接管会话后，事件回到 V1 wire 形状；装配/回放拒绝不破坏既有交付，
/// cursor 失败同样保留旧流，因此 detach 只能放在 cursor 之后。两条 load 路径对 V2
/// initial 与 V1 cursor 取用的配置来源不同，故分别传入。
async fn attach_load_history(
    initial_cfg: &AcpServerConfig,
    cursor_cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    session_id: &str,
    connection: &ConnectionFacts,
    history: Option<LoadInitial>,
    response: &mut Value,
) -> Result<(), AcpError> {
    let Some(initial) = history else {
        if connection.history_pagination {
            crate::host::history_page::attach_initial_cursor(
                cursor_cfg, sessions, session_id, response,
            )
            .await?;
        }
        crate::host::live_delivery::detach_session(sessions, session_id).await;
        return Ok(());
    };
    attach_history_initial(
        initial_cfg,
        sessions,
        session_id,
        connection,
        initial,
        response,
    )
    .await
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

/// 执行准入由实例状态决定；可从存储按需取得上下文的窗口会话也能接受 prompt。
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
        !state.is_view_only()
            && matches!(
                state.context_ownership,
                ContextOwnership::Resident | ContextOwnership::Deferred
            ),
    ))
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
            if connection.history_pagination {
                state.history_payloads.clone()
            } else {
                Vec::new()
            },
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
            ContextOwnership::Deferred | ContextOwnership::ReloadRequired => {
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
            let state = SessionState::view_only(
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
    history: Option<LoadInitial>,
    kind: &'static str,
    error: &AcpError,
) -> Result<Value, AcpError> {
    let (session_id, thread, failure, publication) =
        ensure_view_only_state(params, cfg, sessions, kind, error).await?;
    let mut restored = false;
    let outcome: Result<Value, AcpError> = async {
        // 与正常 load 同一条回放路径：窗口连接跳过首屏回放，其余连接读存储回放。
        replay_after_load(
            cfg,
            sessions,
            &session_id,
            transport,
            connection,
            generation,
            connection.history_window_v1 || history.is_some(),
        )
        .await?;
        let identity = super::response_identity(cfg, &session_id)
            .await
            .ok()
            .flatten();
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
        // 只读会话同样在恢复完成后才交付历史；initial 失败不得回滚已发布状态。
        restored = true;
        response["_meta"]["peri.recovery"] = view_only_recovery_view(&thread, failure)?;
        // 历史可读与执行可用是独立事实：只读进入也遵守已协商的 initial 契约。
        attach_load_history(
            cfg,
            cfg,
            sessions,
            &session_id,
            connection,
            history,
            &mut response,
        )
        .await?;
        Ok(response)
    }
    .await;
    match outcome {
        Ok(response) => Ok(response),
        Err(error) => Err(rollback_after_restore_error(
            cfg,
            sessions,
            publication_for(restored, &publication),
            error,
        )
        .await),
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
    crate::host::notify::send_available_commands_update(
        transport,
        &session_id,
        &caps,
        cfg.session_manager.command_registry_for(&session_id),
        cfg.stdio_command_filter,
    )
    .await;
    crate::host::requests::session_lifecycle::prewarm_session_mcp_discovery(cfg, &session_id);
    let identity = super::response_identity(cfg, &session_id)
        .await
        .ok()
        .flatten();
    let mut response = super::identity_response(
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
    crate::host::notify::send_config_option_update(transport.as_ref(), session_id, cfg).await;
    let modes = crate::session::state_builders::build_mode_state(&cfg.permission_mode);
    let config_options = {
        let c = cfg.peri_config.read();
        let p = cfg.provider.read();
        crate::host::requests::session_lifecycle::config_update::make_config_options(
            &c,
            &p,
            cfg.permission_mode.load(),
        )
    };
    let resp = LoadSessionResponse::new()
        .modes(modes)
        .config_options(config_options);
    crate::host::notify::send_available_commands_update(
        transport,
        session_id,
        &session_caps,
        cfg.session_manager.command_registry_for(session_id),
        cfg.stdio_command_filter,
    )
    .await;
    crate::host::requests::session_lifecycle::prewarm_session_mcp_discovery(cfg, session_id);
    let mut response = super::identity_response(
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
    } = match prepare_shared(params, cfg, sessions, RestoreContext::Immediate).await {
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
        crate::host::notify::send_available_commands_update(
            transport,
            &prepared.id,
            &caps,
            local.session_manager.command_registry_for(&prepared.id),
            local.stdio_command_filter,
        )
        .await;
        crate::host::requests::session_lifecycle::prewarm_session_mcp_discovery(
            local,
            &prepared.id,
        );
        let resp = ResumeSessionResponse::new();
        let mut response = super::identity_response(
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
