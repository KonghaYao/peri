//! Explicit session close and delete settlement.

use super::*;
use crate::host::workspace::{resource_error, workspace_error};
use peri_acp_types::session_resources::{CloseSettlement, SessionResourceErrorKind};

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
                return Err(resource_error(error));
            }
            sessions.remove(session_id);
            return Ok(());
        }
        Err(error) => return Err(resource_error(error)),
        Ok(_) => {}
    }
    if sessions.get(session_id).is_some_and(|state| state.closing)
        && !delete
        && resources
            .is_session_closing(&target)
            .await
            .map_err(resource_error)?
    {
        match resources
            .close_settlement(&target)
            .await
            .map_err(resource_error)?
        {
            CloseSettlement::Finished => {
                sessions.remove(session_id);
                return Ok(());
            }
            CloseSettlement::Unknown => {
                return Err(AcpError::new(
                    -32010,
                    "Session close incomplete: settlement is unknown",
                ));
            }
            CloseSettlement::Pending => {}
        }
    }
    let mut environment = sessions
        .get(session_id)
        .and_then(|state| state.environment.clone());
    if environment.is_none() && cfg.workspace_assembly.is_some() {
        environment = super::super::resource_owners::cold_environment(cfg, session_id).await?;
    }
    let local = environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg);
    if environment.is_none() && local.workspace_assembly.is_some() && local.mcp_pool.is_none() {
        return Err(AcpError::new(
            -32010,
            "Session close incomplete: persisted resource owner connections are unavailable",
        ));
    }
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
                return Err(resource_error(error));
            }
            _ => {
                return Err(AcpError::new(
                    -32010,
                    "Session close incomplete: closing intent is unconfirmed",
                ));
            }
        }
    }
    let control = resources
        .load_session_control(&target)
        .await
        .map_err(resource_error)?;
    let work = crate::host::work_query::inspect(
        resources.as_ref(),
        &target,
        peri_acp_types::session_resources::work::WorkSelector::RecoveryDescriptor {
            lifecycle: control.lifecycle,
        },
    )
    .await?;
    let missing = crate::host::work_query::inspect(
        resources.as_ref(),
        &target,
        peri_acp_types::session_resources::work::WorkSelector::LegacyEvidence {
            record_id: format!("ownerMissing:{target}:{}", control.lifecycle),
        },
    )
    .await?;
    let peri_acp_types::session_resources::work::WorkPage::LegacyEvidence(evidence) = &missing.page
    else {
        return Err(crate::host::work_query::wrong_page());
    };
    if !crate::host::work_query::descriptor(&work, control.lifecycle)?
        .is_some_and(|descriptor| descriptor.resource_owners.is_some())
        && !evidence.is_empty()
    {
        return Err(AcpError::new(
            -32010,
            "Session close incomplete: historical resource owner is unknown",
        ));
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
    if let Some(pool) = local.mcp_pool.as_ref() {
        if sessions.contains_key(session_id) {
            Arc::clone(pool)
                .close_workspace_task_scope(session_id)
                .await
        } else {
            pool.reconcile_closing_workspace_scope(session_id).await
        }
        .map_err(|error| AcpError::new(-32010, format!("Session close incomplete: {error}")))?;
    }
    local
        .session_manager
        .close_session(session_id)
        .await
        .map_err(workspace_error)?;
    if let Some(environment) = environment.as_ref() {
        if let Some(pool) = environment.cfg.mcp_pool.as_ref() {
            pool.verify_shared_environment_close(session_id)
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
        .map_err(resource_error)?;
    if delete {
        if let Err(error) = resources.delete_session_tree(&target).await {
            match resources.load_session_meta(&target).await {
                Err(readback) if matches!(readback.kind(), SessionResourceErrorKind::NotFound) => {}
                _ => return Err(resource_error(error)),
            }
        }
    } else if let Err(error) = resources.finish_close(&target).await {
        match resources.close_settlement(&target).await {
            Ok(CloseSettlement::Finished) => {}
            _ => return Err(resource_error(error)),
        }
    }
    sessions.remove(session_id);
    Ok(())
}
