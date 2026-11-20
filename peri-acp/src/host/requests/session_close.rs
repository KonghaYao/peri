//! Explicit session close and delete settlement.

use crate::host::workspace;
use crate::host::AcpServerConfig;
use crate::host::SessionState;
use crate::transport::types::AcpError;

use peri_acp_types::session_resources::{CloseSettlement, SessionResourceErrorKind};

use std::collections::HashMap;

/// 关闭成功后回收 Controller/Runtime 登记的本轮执行句柄。
///
/// 句柄按轮 `register_or_replace`，轮次结束不会自行注销；只有会话确定性关闭时
/// 才能按 §9 销毁并移除映射，否则同连接轮换 ID 时 Runtime 表只增不减（#465）。
/// 关闭失败路径不调用，重试仍可续聊。
async fn release_runtime_handle(cfg: &AcpServerConfig, session_id: &str) -> Result<(), AcpError> {
    if !cfg.controller.contains_session(session_id) {
        return Ok(());
    }
    cfg.controller
        .destroy_session(session_id, std::time::Duration::from_secs(1))
        .await
        .map_err(|error| {
            AcpError::new(
                -32010,
                format!("Session close incomplete: runtime handle release failed: {error}"),
            )
        })?;
    Ok(())
}

pub(in crate::host::requests) async fn close_session(
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
    session_id: &str,
    delete: bool,
) -> Result<(), AcpError> {
    let resources = &cfg.session_resources;
    let target = session_id.to_owned();
    match resources.load_session_meta(&target).await {
        Err(error) if matches!(error.kind(), SessionResourceErrorKind::NotFound) => {
            if sessions.get(session_id).is_some_and(|state| !state.closing) {
                return Err(workspace::resource_error(error));
            }
            release_runtime_handle(cfg, session_id).await?;
            sessions.remove(session_id);
            return Ok(());
        }
        Err(error) => return Err(workspace::resource_error(error)),
        Ok(_) => {}
    }
    // 只看历史没有执行资源/持久关闭意图。离开它只回收本进程视图，不能向只读库
    // 写 closing，也不能把原本不可执行的会话标为关闭中的工作。delete 仍走写权限门。
    let view_only = sessions
        .get(session_id)
        .is_some_and(SessionState::is_view_only);
    let closed_read_only = !sessions.contains_key(session_id)
        && resources
            .inspect_availability(None)
            .await
            .map_err(workspace::resource_error)?
            .access
            == peri_acp_types::session_resources::AccessMode::ReadOnly;
    if !delete && (view_only || closed_read_only) {
        cfg.session_manager
            .close_session(session_id)
            .await
            .map_err(workspace::workspace_error)?;
        release_runtime_handle(cfg, session_id).await?;
        sessions.remove(session_id);
        return Ok(());
    }
    if sessions.get(session_id).is_some_and(|state| state.closing)
        && !delete
        && resources
            .is_session_closing(&target)
            .await
            .map_err(workspace::resource_error)?
    {
        match resources
            .close_settlement(&target)
            .await
            .map_err(workspace::resource_error)?
        {
            CloseSettlement::Finished => {
                release_runtime_handle(cfg, session_id).await?;
                sessions.remove(session_id);
                return Ok(());
            }
            CloseSettlement::Unknown => {
                return Err(AcpError::new(
                    -32010,
                    "Session close incomplete: settlement is unknown",
                ))
            }
            CloseSettlement::Pending => {}
        }
    }
    let environment = sessions
        .get(session_id)
        .and_then(|state| state.environment.clone());
    let local = environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg);
    // 本函数是 `SessionState.closing` 的唯一写入方（`server_loop` 不再预置）：
    // 这里读到的就是本次尝试开始前的准入状态。持久化意图确认未成立时回滚到它，
    // 否则关闭失败会把会话永久挡在续聊之外（#196）。
    let was_closing = sessions.get(session_id).is_some_and(|state| state.closing);
    if let Some(state) = sessions.get_mut(session_id) {
        state.closing = true;
    }
    if let Err(error) = resources.mark_session_closing(&target).await {
        match resources.is_session_closing(&target).await {
            Ok(true) => {}
            Ok(false) if !error.is_persistence_uncertain() => {
                if let Some(state) = sessions.get_mut(session_id) {
                    state.closing = was_closing;
                }
                return Err(workspace::resource_error(error));
            }
            _ => {
                return Err(AcpError::new(
                    -32010,
                    "Session close incomplete: closing intent is unconfirmed",
                ))
            }
        }
    }
    if let Some(state) = sessions.get_mut(session_id) {
        if local
            .session_manager
            .get_session(session_id)
            .is_some_and(|session| {
                session.active_agents.values().any(|agent| {
                    agent.cancel_policy == peri_acp_types::thread::CancelPolicy::Independent
                })
            })
        {
            return Err(AcpError::new(
                -32010,
                "Session close incomplete: Independent child needs an explicit handoff",
            ));
        }
        if let Some(token) = state.cancel_token.as_ref() {
            token.cancel();
        }
        local.session_manager.pre_close_session(session_id);
        if state.cancel_token.is_some() {
            return Err(AcpError::new(
                -32010,
                "Session close incomplete: prompt is still active",
            ));
        }
    }
    let child_manager = local
        .session_manager
        .get_session(session_id)
        .map(|session| session.task_manager.clone());
    if let Some(manager) = child_manager {
        manager
            .close_child_resources()
            .await
            .map_err(|error| AcpError::new(-32010, format!("Session close incomplete: {error}")))?;
    }
    if let Some(pool) = local.mcp_pool.as_ref() {
        // Only tasks this session's runtime registered are known; there is no
        // owner snapshot/close barrier and no cross-connection recovery.
        pool.cancel_session_external_tasks(session_id)
            .await
            .map_err(|error| AcpError::new(-32010, format!("Session close incomplete: {error}")))?;
    }
    local
        .session_manager
        .close_session(session_id)
        .await
        .map_err(workspace::workspace_error)?;
    if let Some(environment) = environment.as_ref() {
        if let Some(pool) = environment.cfg.mcp_pool.as_ref() {
            pool.verify_shared_environment_close(session_id)
                .await
                .map_err(|error| {
                    AcpError::new(-32010, format!("Session close incomplete: {error}"))
                })?;
        }
        if !environment.shutdown().await {
            return Err(AcpError::new(
                -32010,
                "Session close incomplete: resources are still active",
            ));
        }
    }
    resources
        .drain_persistence(&target)
        .await
        .map_err(workspace::resource_error)?;
    if delete {
        if let Err(error) = resources.delete_session_tree(&target).await {
            match resources.load_session_meta(&target).await {
                Err(readback) if matches!(readback.kind(), SessionResourceErrorKind::NotFound) => {}
                _ => return Err(workspace::resource_error(error)),
            }
        }
        // 持久树已整树删除：恢复路径冷装配的 child 没有客户端会话绑定，不会再收到
        // 一次自己的 close；不在这里按父树回收，它们会常驻到整连接 shutdown。
        reclaim_recovered_children(cfg, sessions, &target).await;
    } else if let Err(error) = resources.finish_close(&target).await {
        match resources.close_settlement(&target).await {
            Ok(CloseSettlement::Finished) => {}
            _ => return Err(workspace::resource_error(error)),
        }
    }
    release_runtime_handle(cfg, session_id).await?;
    sessions.remove(session_id);
    Ok(())
}

/// 父树整树删除后，回收该树内由恢复动作冷装配的 child。
///
/// 只按「树根 + 记录代次」匹配：显式 load 推进过代次的实例属于别的生命周期，
/// 不在这里拆；其他树的登记不参与。回收不依赖请求级 close（存储里已没有它的行，
/// 那条路径会在清本地状态前以 `NotFound` 早退）。
async fn reclaim_recovered_children(
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
    root_thread_id: &str,
) {
    for registration in cfg
        .session_manager
        .recovered_children_of_tree(root_thread_id)
    {
        let child_session_id = registration.child_session_id.clone();
        let live_generation = sessions
            .get(&child_session_id)
            .map(|state| state.session_generation);
        if live_generation != Some(registration.session_generation) {
            // 实例已被关闭或推进到新代次：本记录不再代表那次恢复发布。
            cfg.session_manager
                .forget_recovered_child(&child_session_id);
            continue;
        }
        if reclaim_recovered_child(cfg, sessions, &child_session_id).await {
            cfg.session_manager
                .forget_recovered_child(&child_session_id);
        }
    }
}

/// 单个恢复 child 的收口顺序与 cold restore 回滚一致：manager → 环境 → Controller。
///
/// 返回是否已完全释放；未释放时保留 `closing` 与登记，由整连接 shutdown 兜底重试。
async fn reclaim_recovered_child(
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
    session_id: &str,
) -> bool {
    let environment = {
        let Some(state) = sessions.get_mut(session_id) else {
            return true;
        };
        // 已删除的树不再接受任何执行：先封闭入口，再取消在飞 turn。
        state.closing = true;
        if let Some(token) = state.cancel_token.as_ref() {
            token.cancel();
        }
        state.environment.clone()
    };
    let local = environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg);
    let manager_settled = match local.session_manager.close_session(session_id).await {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(
                session_id,
                error = %error,
                "recovered child manager settlement incomplete after parent tree delete"
            );
            false
        }
    };
    let environment_settled = match environment.as_ref() {
        Some(environment) => environment.shutdown().await,
        None => true,
    };
    // Controller 按轮登记执行句柄，删除后不会再有 close 请求替它释放。
    if let Err(error) = release_runtime_handle(local, session_id).await {
        tracing::warn!(
            session_id,
            code = error.code,
            error = %error.message,
            "recovered child runtime handle release incomplete after parent tree delete"
        );
    }
    if manager_settled && environment_settled {
        sessions.remove(session_id);
        true
    } else {
        tracing::warn!(
            session_id,
            manager_settled,
            environment_settled,
            "recovered child reclaim incomplete; retained for host shutdown"
        );
        false
    }
}
