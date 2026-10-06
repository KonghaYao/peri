use std::collections::HashMap;

use peri_acp_types::session_resources::{
    CloseSettlement, ControlCommand, ControlState, FrozenState,
};

use crate::host::{
    prepared::PreparedSessionInputs, workspace::SessionEnvironment, AcpServerConfig, SessionState,
};
use crate::transport::types::AcpError;

pub(super) async fn apply(
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
    command: &ControlCommand,
    current: &ControlState,
) -> Result<(), AcpError> {
    let id = command.session_id.as_str();
    if cfg
        .session_resources
        .close_settlement(&command.session_id)
        .await
        .map_err(crate::host::workspace::resource_error)?
        != CloseSettlement::Finished
    {
        return Err(AcpError::new(
            -32010,
            "Reopen Blocked: previous resource close is not settled",
        ));
    }
    super::super::resource_owners::copy_for_reopen(
        cfg,
        id,
        command.expected_lifecycle,
        current.lifecycle,
    )
    .await?;
    if sessions.contains_key(id)
        && cfg
            .session_manager
            .get_session(id)
            .is_some_and(|session| session.recipient_lifecycle == current.lifecycle)
    {
        return Ok(());
    }
    let previous_environment = sessions.get(id).and_then(|state| state.environment.clone());
    if sessions
        .get(id)
        .is_some_and(|state| state.cancel_token.is_some())
        || cfg.session_manager.get_session(id).is_some_and(|session| {
            !session.active_agents.is_empty() || !session.task_manager.is_execution_idle()
        })
        || previous_environment
            .as_ref()
            .is_some_and(|environment| !environment.task_manager().is_execution_idle())
    {
        return Err(AcpError::new(
            -32010,
            "Reopen Blocked: previous live execution is not closed",
        ));
    }
    cfg.session_manager
        .close_session(id)
        .await
        .map_err(|error| {
            AcpError::new(
                -32010,
                format!("Reopen Blocked: idle view resource close remains incomplete: {error}"),
            )
        })?;
    if let Some(environment) = previous_environment {
        if !environment.shutdown().await {
            return Err(AcpError::new(
                -32010,
                "Reopen Blocked: restored idle environment shutdown is unconfirmed",
            ));
        }
    }
    let snapshot = cfg
        .session_resources
        .load_session_snapshot(&command.session_id)
        .await
        .map_err(crate::host::workspace::resource_error)?;
    let FrozenState::Present(frozen) = &snapshot.frozen else {
        return Err(AcpError::new(
            -32010,
            "Reopen Blocked: previous frozen context unavailable",
        ));
    };
    let mut prepared =
        PreparedSessionInputs::prepare_restore(cfg, &snapshot.meta.cwd, frozen.as_str())?;
    prepared.session_mcp_servers = super::super::resource_owners::load(cfg, id).await?;
    let environment = SessionEnvironment::assemble_prepared(cfg, &prepared, id).await?;
    if environment.is_none() && (cfg.mcp_pool.is_some() || !prepared.session_mcp_servers.is_empty())
    {
        return Err(AcpError::new(-32010, "Reopen Blocked: persisted owners require a new session environment, not the current root pool"));
    }
    let local = environment
        .as_ref()
        .map(|environment| &environment.cfg)
        .unwrap_or(cfg);
    let manager = environment
        .as_ref()
        .map(|environment| environment.task_manager())
        .unwrap_or_else(|| local.session_manager.new_session_task_manager());
    local
        .session_manager
        .ensure_session_for_lifecycle(id, current.lifecycle, &snapshot.meta.cwd, manager.clone())
        .map_err(|error| AcpError::new(-32010, format!("Reopen Blocked: {error}")))?;
    if let Some(pool) = &local.mcp_pool {
        let inbox = local.session_manager.session_inbox_for(id).ok_or_else(|| {
            AcpError::new(-32010, "Reopen Blocked: new lifecycle inbox unavailable")
        })?;
        pool.bind_agent_session_for_lifecycle(id, current.lifecycle, inbox.handle(), manager)
            .map_err(|error| AcpError::new(-32010, format!("Reopen Blocked: {error}")))?;
        pool.bind_agent_session_resources(id, current.lifecycle, cfg.session_resources.clone())
            .map_err(|error| AcpError::new(-32010, format!("Reopen Blocked: {error}")))?;
    }
    local.session_manager.ensure_session_caps(id);
    let frozen = prepared
        .frozen
        .ok_or_else(|| AcpError::new(-32010, "Reopen frozen context unavailable"))?;
    let workflow_middleware = super::super::session_lifecycle::create_session_workflow_middleware(
        local,
        &snapshot.meta.cwd,
        id,
        &frozen,
    );
    if let Some(environment) = &environment {
        environment.activate();
    }
    sessions.insert(
        id.to_owned(),
        SessionState {
            session_id: id.to_owned(),
            thread_id: id.to_owned(),
            cwd: snapshot.meta.cwd,
            environment,
            closing: false,
            history: snapshot
                .payloads
                .iter()
                .filter_map(|payload| payload.as_message().cloned())
                .collect(),
            history_payloads: snapshot.payloads,
            cancel_token: None,
            frozen: Some(frozen),
            recall_items: Vec::new(),
            agent_pool: crate::session::agent_pool::AgentPool::new(),
            workflow_middleware,
            title: snapshot.meta.title,
            tags: Vec::new(),
        },
    );
    Ok(())
}
