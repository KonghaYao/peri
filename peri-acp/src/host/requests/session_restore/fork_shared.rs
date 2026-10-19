//! `session/fork` 的生产入口：源会话准备走短锁，fork 自身的 DB/资源 I/O 全部在锁外。
//!
//! 与 `session/load` 的差异保留在这里：源会话身份、继承边界、目标新身份和响应语义都是
//! fork 自己的；共享的只是“确保源会话 live”的准备阶段。

use std::sync::Arc;

use serde_json::Value;

use super::super::{
    fork_source_error, prewarm_session_mcp_discovery, session_mcp_servers, ForkSessionResponse,
    SessionId,
};
// 本地 workflow 运行时不在 emscripten 构建面内（wasm 产物不打包 workflow），
// 对应名称在该目标不存在；调用处已有同条件 cfg。
#[cfg(not(target_os = "emscripten"))]
use super::super::{attach_session_workflow, create_session_workflow_chain, workflow_face_closed};
use super::*;
use crate::host::notify::send_available_commands_update;
use crate::host::workspace::SessionEnvironment;
use crate::host::SharedSessions;
use crate::transport::types::AcpError;

pub(crate) async fn handle_fork_shared(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    transport: &Arc<dyn crate::transport::AcpTransport>,
) -> Result<Value, AcpError> {
    let source_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?
        .to_owned();
    // 与 load/resume 共用：短锁判断 + 锁外 preflight/DB + 短锁复核发布。
    super::load_shared::prepare_shared(params, cfg, sessions).await?;
    let workspace = crate::host::workspace::reassert_expected(
        cfg,
        &source_id,
        params.get("cwd").and_then(Value::as_str),
    )
    .await?;
    let (execution_active, frozen_missing) = {
        let states = sessions.lock().await;
        let source = states
            .get(&source_id)
            .ok_or_else(|| AcpError::new(-32602, "Load the source session before forking"))?;
        (source.cancel_token.is_some(), source.frozen.is_none())
    };
    if execution_active
        || cfg
            .session_manager
            .get_session(&source_id)
            .is_some_and(|s| !s.active_agents.is_empty() || !s.task_manager.is_execution_idle())
    {
        return Err(AcpError::new(
            -32010,
            "Cannot fork while source execution is active",
        ));
    }
    if frozen_missing {
        return Err(AcpError::new(-32603, "Source frozen snapshot is missing"));
    }
    let cwd_owned = workspace
        .cwd
        .to_str()
        .ok_or_else(|| AcpError::new(-32602, "Execution directory is not UTF-8"))?
        .to_owned();
    let cwd = cwd_owned.as_str();
    let fork_source = dispatch::load_fork_source(&cfg.session_resources, &source_id)
        .await
        .map_err(fork_source_error)?;
    let mut prepared = PreparedSessionInputs::prepare_fork(cfg, cwd, fork_source.frozen.as_str())?;
    prepared.session_mcp_servers = session_mcp_servers(params)?;
    let frozen_data = prepared
        .frozen
        .clone()
        .ok_or_else(|| AcpError::new(-32603, "Fork frozen snapshot is missing"))?;
    let (new_thread_id, published_payloads) = dispatch::fork_bound_session(
        &cfg.session_resources,
        &fork_source,
        &workspace,
        peri_time::now_utc_rfc3339(),
    )
    .await
    .map_err(fork_source_error)?;

    let identity = match response_identity(cfg, &new_thread_id).await {
        Ok(identity) => identity,
        Err(error) => {
            cfg.session_resources
                .abandon_initialization(&new_thread_id)
                .await
                .map_err(crate::host::workspace::resource_error)?;
            return Err(error);
        }
    };
    let environment =
        match SessionEnvironment::assemble_prepared(cfg, &prepared, &new_thread_id).await {
            Ok(environment) => environment,
            Err(error) => {
                cfg.session_resources
                    .abandon_initialization(&new_thread_id)
                    .await
                    .map_err(crate::host::workspace::resource_error)?;
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

    #[cfg(not(target_os = "emscripten"))]
    let workflow_chain = if workflow_face_closed(&frozen_data) {
        None
    } else {
        Some(create_session_workflow_chain(
            cfg,
            cwd,
            &new_session_id,
            &frozen_data,
        ))
    };
    #[cfg(not(target_os = "emscripten"))]
    if let Some(chain) = workflow_chain.as_ref() {
        if let Err(error) = attach_session_workflow(environment.as_ref(), chain) {
            if let Some(environment) = &environment {
                if !environment.shutdown().await {
                    return Err(AcpError::new(
                        -32603,
                        "workflow runtime binding failed and the unpublished fork environment \
                         did not confirm shutdown",
                    ));
                }
            }
            cfg.session_resources
                .abandon_initialization(&new_thread_id)
                .await
                .map_err(crate::host::workspace::resource_error)?;
            return Err(error);
        }
    }

    let state = SessionState {
        session_id: new_session_id.clone(),
        thread_id: new_thread_id.clone(),
        cwd: cwd.to_string(),
        environment: environment.clone(),
        closing: false,
        context_ownership: crate::host::ContextOwnership::Resident,
        history_payloads: published_payloads,
        structure_version: 0,
        session_generation: crate::host::next_session_generation(),
        history_window: Default::default(),
        cancel_token: None,
        interaction_gate: crate::host::workspace_interaction::session_interaction_gate(
            &environment,
        ),
        host_request_seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        continuation_armed: false,
        continuation_epoch: 0,
        continuation_in_flight: false,
        continuation_mq_steering_pending: false,
        frozen: Some(frozen_data),
        recall_items: Vec::new(),
        agent_pool: crate::session::agent_pool::AgentPool::new(),
        title: None,
        tags: Vec::new(),
    };
    let conflict = {
        let mut states = sessions.lock().await;
        if states.contains_key(&new_session_id) {
            Some(state)
        } else {
            states.insert(new_session_id.clone(), state);
            None
        }
    };
    if let Some(mut state) = conflict {
        super::load_shared::cleanup_unpublished_restore(
            cfg,
            &new_session_id,
            state.environment.take(),
        )
        .await;
        cfg.session_resources
            .abandon_initialization(&new_thread_id)
            .await
            .map_err(crate::host::workspace::resource_error)?;
        return Err(AcpError::new(
            -32010,
            "fork target became live concurrently; retry the fork",
        ));
    }

    if let Some(environment) = &environment {
        environment.activate();
    }
    tracing::info!(source = %source_id, new = %new_session_id, "Session forked");
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
        serde_json::to_value(resp).map_err(crate::host::workspace::workspace_error)?,
        identity,
    )
}
