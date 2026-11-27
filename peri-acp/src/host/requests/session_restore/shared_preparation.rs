//! Admit a live or cold session, publish its generation, and clean up failed publications.
//! Storage and environment work happens outside the shared sessions lock.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

use crate::dispatch;
use crate::host::{AcpServerConfig, ContextOwnership, SessionState, SharedSessions};
use crate::transport::types::AcpError;

use super::session_restore_dispatch::{
    load_frozen_bytes, prepare_existing_with_context, reject_closing_session,
    reject_incomplete_local_close, shutdown_bootstrapped_environment,
};
use super::{PreparedSession, RestoreContext};

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
    context: RestoreContext,
) -> Result<PreparedShared, AcpError> {
    let already_loaded = {
        let states = sessions.lock().await;
        reject_incomplete_local_close(params, &states)?;
        params
            .get("sessionId")
            .and_then(Value::as_str)
            .and_then(|id| states.get(id))
            .is_some()
    };
    let (prepared, publication) = if already_loaded {
        (
            prepare_live_shared(params, cfg, sessions, context).await?,
            None,
        )
    } else {
        let (prepared, publication) = prepare_cold_shared(params, cfg, sessions, context).await?;
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
    context: RestoreContext,
) -> Result<(PreparedSession, RestorePublication), AcpError> {
    let mut scratch = HashMap::new();
    let prepared = prepare_existing_with_context(params, cfg, &mut scratch, context).await?;
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
    context: RestoreContext,
) -> Result<PreparedSession, AcpError> {
    reject_closing_session(params, cfg).await?;
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
    let effective_servers =
        crate::host::requests::session_lifecycle::session_mcp_setup::session_mcp_servers(params)?;
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
        let persisted = load_frozen_bytes(cfg, &id).await?;
        crate::session::frozen_snapshot::decode_frozen_snapshot(&persisted)
            .map_err(crate::host::workspace::workspace_error)?;
        let identity = super::response_identity(cfg, &id).await?;

        let read = {
            let mut states = sessions.lock().await;
            let state = states
                .get_mut(&id)
                .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
            if state.closing {
                return Err(AcpError::new(-32010, "Session is closing"));
            }
            crate::host::ownership::mark_stale_if_dead(state);
            context.prepare_live_context(state).then_some((
                state.session_generation,
                state.structure_version,
                state.context_ownership,
            ))
        };
        if let Some((generation, structure, ownership)) = read {
            let payloads =
                dispatch::load_active_session_payloads(cfg.controller.as_ref(), &id).await?;
            let mut states = sessions.lock().await;
            let state = states
                .get_mut(&id)
                .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
            if state.closing {
                return Err(AcpError::new(-32010, "Session is closing"));
            }
            if state.session_generation != generation
                || state.structure_version != structure
                || state.context_ownership != ownership
            {
                return Err(AcpError::new(
                    -32010,
                    "session changed while loading; retry the load",
                ));
            }
            if ownership != ContextOwnership::Resident || state.history_payloads.is_empty() {
                state.history_payloads = payloads;
                state.context_ownership = ContextOwnership::Resident;
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
        Err(error) => Err(shutdown_bootstrapped_environment(environment_slot.take(), error).await),
    }
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
    rollback_publication(cfg, sessions, publication, false).await
}

/// 回收已经发布、但尚无 turn 接管的 cold restore（恢复准备失败/跳过的路径）。
///
/// `closing` 只在该代次仍无 `cancel_token` 时设置：判定与 turn 准入共用
/// `SharedSessions` 锁（prompt 在锁内发布令牌并检查 `closing`），因此要么 turn
/// 先取得会话、回滚放弃删除，要么回滚先置 `closing`、新 turn 被拒绝。不能把
/// 已开始的 turn 连同环境一起拆掉。
pub(crate) async fn rollback_unused_restore_publication(
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    publication: &RestorePublication,
) -> Result<(), AcpError> {
    rollback_publication(cfg, sessions, publication, true).await
}

async fn rollback_publication(
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    publication: &RestorePublication,
    require_idle: bool,
) -> Result<(), AcpError> {
    let environment = {
        let mut states = sessions.lock().await;
        let Some(state) = states.get_mut(&publication.session_id) else {
            return Ok(());
        };
        if !publication.matches(state) {
            return Ok(());
        }
        // 本代次已有 turn 持有会话：会话归执行方，回滚必须放弃。
        if require_idle && state.cancel_token.is_some() {
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
