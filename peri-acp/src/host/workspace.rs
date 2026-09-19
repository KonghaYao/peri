//! A session owns one verified execution environment and its resource lifetime.

use std::{path::Path, sync::Arc};

use super::{assemble, task_scope, AcpServerConfig, SessionState};
use crate::transport::types::AcpError;
use peri_acp_types::workspace::{ResolvedWorkspace, SessionExecutionLease, WorkspaceError};

enum SessionEndState {
    Pending,
    Running(tokio::task::JoinHandle<()>),
    Finished,
    Skipped,
    Failed,
}

pub(crate) struct SessionEnvironment {
    pub(crate) cfg: AcpServerConfig,
    activation: tokio_util::sync::CancellationToken,
    task_owner: tokio::sync::Mutex<task_scope::HostTaskOwner>,
    mcp_owner: tokio::sync::Mutex<Box<dyn peri_acp_types::ports::McpTaskOwnerPort>>,
    session_id: String,
    cwd: String,
    end_hooks: tokio::sync::Mutex<SessionEndState>,
    cleanup_tasks: Arc<dyn peri_acp_types::tasks::TaskManager>,
}

impl SessionEnvironment {
    pub(crate) async fn assemble(
        host: &AcpServerConfig,
        cwd: &str,
        session_id: &str,
    ) -> Result<Option<Arc<Self>>, AcpError> {
        let Some(source) = host.workspace_assembly.as_ref() else {
            return Ok(None);
        };
        let same_directory =
            std::fs::canonicalize(&source.startup_cwd).ok().as_deref() == Some(Path::new(cwd));
        let (config_source, peri_config, provider) = if same_directory {
            (
                host.config_source.clone(),
                Arc::new(parking_lot::RwLock::new(host.peri_config.read().clone())),
                host.provider.read().clone(),
            )
        } else {
            let source = Arc::new(
                crate::provider::ConfigSource::load_at(
                    Path::new(cwd),
                    host.config_source.global_path().to_owned(),
                )
                .map_err(workspace_error)?,
            );
            let config = source.loaded_merged();
            let provider = crate::provider::LlmProvider::from_config(&config)
                .or_else(crate::provider::LlmProvider::from_env)
                .ok_or_else(|| {
                    AcpError::new(-32603, "No provider configured for session workspace")
                })?;
            (source, Arc::new(parking_lot::RwLock::new(config)), provider)
        };
        let input = assemble::HostAssemblyInput {
            provider,
            peri_config,
            config_source,
            permission_mode: peri_acp_types::permission::SharedPermissionMode::new(
                host.permission_mode.load(),
            ),
            thread_store: host.thread_store.clone(),
            cwd: cwd.to_owned(),
            bare: source.bare,
            drive_cron_tick: false,
        };
        let activation = tokio_util::sync::CancellationToken::new();
        let mut cfg = assemble::assemble_server_config_with_mcp_profile(
            input,
            source.mcp_profile.clone(),
            true,
            Some(activation.clone()),
        )
        .await;
        cfg.session_manager
            .share_registry_with(&host.session_manager);
        cfg.cron_scheduler = host.cron_scheduler.clone();
        cfg.controller = host.controller.clone();
        cfg.langfuse_session = host.langfuse_session.clone();
        cfg.stdio_command_filter = host.stdio_command_filter;
        let task_owner = cfg.host_task_owner.take().expect("session resource owner");
        let mcp_owner = cfg
            .mcp_task_owner
            .take()
            .expect("session MCP resource owner");
        // Session OAuth keeps the existing host event transport, while callbacks remain
        // attached to this workspace's MCP pool.
        if let (Some(mut events), Some(host_tx)) =
            (cfg.oauth_event_rx.take(), host.oauth_event_tx.clone())
        {
            let shutdown = cfg.host_task_spawner.shutdown_token();
            let session_id = session_id.to_owned();
            let _ = cfg.host_task_spawner.spawn(task_scope::HostTaskOwnerKind::Session, task_scope::HostTaskKind::OAuthConsumer, async move {
                loop { tokio::select! {
                    _ = shutdown.cancelled() => break,
                    event = events.recv() => match event {
                        Some(event) => { if host_tx.send(crate::event::oauth::HostOAuthEvent::Session { session_id: session_id.clone(), event: Box::new(event) }).is_err() { break; } }
                        None => break,
                    }
                }}
            });
        }
        Ok(Some(Arc::new(Self {
            cfg,
            activation,
            task_owner: tokio::sync::Mutex::new(task_owner),
            mcp_owner: tokio::sync::Mutex::new(mcp_owner),
            session_id: session_id.to_owned(),
            cwd: cwd.to_owned(),
            end_hooks: tokio::sync::Mutex::new(SessionEndState::Pending),
            cleanup_tasks: Arc::new(peri_agent::agent::async_tasks::TaskManager::new()),
        })))
    }

    pub(crate) fn activate(&self) {
        self.activation.cancel();
    }

    pub(crate) async fn shutdown(&self) -> bool {
        if !self.finish_session_end().await {
            return false;
        }
        let mut tasks = self.task_owner.lock().await;
        tasks.begin_shutdown();
        if let Some(pool) = self.cfg.mcp_pool.as_ref() {
            pool.begin_shutdown();
        }
        if let Some(dynamic) = self.cfg.dynamic_mcp.as_ref() {
            dynamic.begin_shutdown();
        }
        let host = tasks.shutdown().await;
        let dynamic = match self.cfg.dynamic_mcp.as_ref() {
            Some(dynamic) => dynamic.shutdown().await,
            None => peri_acp_types::dynamic_mcp::DynamicMcpShutdownReport::Complete,
        };
        let mut mcp_tasks = self.mcp_owner.lock().await;
        mcp_tasks.begin_shutdown();
        let _ = mcp_tasks.shutdown().await;
        let pool = match self.cfg.mcp_pool.as_ref() {
            Some(pool) => pool.shutdown().await,
            None => peri_acp_types::ports::McpPoolShutdownReport::Complete {
                settled_services: 0,
                failed_services: 0,
            },
        };
        matches!(
            task_scope::HostTerminalShutdownReport::aggregate(host, dynamic, pool, 0),
            task_scope::HostTerminalShutdownReport::Complete { .. }
        )
    }

    /// Keep one terminal hook execution across close retries. Its cleanup scope is
    /// separate because ordinary session task admission has already closed.
    async fn finish_session_end(&self) -> bool {
        let mut state = self.end_hooks.lock().await;
        if matches!(*state, SessionEndState::Pending) {
            let hooks = self
                .cfg
                .hook_groups
                .iter()
                .flatten()
                .filter(|hook| hook.event == peri_acp_types::hooks::HookEvent::SessionEnd)
                .cloned()
                .collect::<Vec<_>>();
            if !self.activation.is_cancelled() || hooks.is_empty() {
                *state = SessionEndState::Finished;
            } else if let Err(error) =
                validate_expected(&self.cfg, &self.session_id, Some(&self.cwd)).await
            {
                // This hook never started. Invalid execution context must not
                // prevent draining resources that the session already owns.
                tracing::warn!(error = %error.message, "SessionEnd skipped: workspace binding is no longer valid");
                *state = SessionEndState::Skipped;
            } else {
                let cwd = self.cwd.clone();
                let session_id = self.session_id.clone();
                let model = self.cfg.provider.read().model_name().to_owned();
                let tasks = self.cleanup_tasks.clone();
                match self
                    .cleanup_tasks
                    .spawn_owned(assemble::build_session_end_task(
                        hooks, cwd, session_id, model, tasks,
                    )) {
                    Ok(handle) => *state = SessionEndState::Running(handle),
                    Err(error) => {
                        tracing::warn!(%error, "SessionEnd hook admission failed");
                        *state = SessionEndState::Failed;
                    }
                }
            }
        }
        if let SessionEndState::Running(handle) = &mut *state {
            // Timeout leaves the task and its process ownership in this environment;
            // the next close/EOF attempt joins the same invocation.
            match tokio::time::timeout(std::time::Duration::from_secs(5), handle).await {
                Ok(Ok(())) => *state = SessionEndState::Finished,
                Ok(Err(error)) => {
                    tracing::warn!(%error, "SessionEnd hook task failed");
                    *state = SessionEndState::Failed;
                }
                Err(_) => return false,
            }
        }
        let joined = matches!(*state, SessionEndState::Finished | SessionEndState::Skipped);
        let cleanup = self.cleanup_tasks.shutdown().await;
        joined && cleanup == peri_acp_types::tasks::TaskShutdownReport::Complete
    }
}

pub(crate) fn workspace_error(error: impl Into<anyhow::Error>) -> AcpError {
    let error = error.into();
    let mut response = AcpError::new(-32010, error.to_string());
    if let Some(WorkspaceError::RecoveryRequired(details)) = error.downcast_ref::<WorkspaceError>()
    {
        response.data = Some(
            serde_json::to_value(
                peri_acp_types::workspace::WorkspaceErrorData::RecoveryRequired(details.clone()),
            )
            .expect("recovery details serialize"),
        );
    }
    response
}

pub(crate) fn require_owner(state: &SessionState) -> Result<(), AcpError> {
    if state.closing {
        return Err(AcpError::new(-32010, "Session is closing"));
    }
    if state.execution_owner.is_none() {
        return Err(workspace_error(WorkspaceError::ExecutionLeaseRequired));
    }
    Ok(())
}

pub(crate) async fn validate_expected(
    cfg: &AcpServerConfig,
    session_id: &str,
    expected: Option<&str>,
) -> Result<ResolvedWorkspace, AcpError> {
    let store = cfg.controller.sessions();
    let workspace = store
        .validate_session_binding(&session_id.to_owned())
        .await
        .map_err(workspace_error)?;
    if let Some(expected) = expected {
        let expected = store
            .resolve_workspace(Path::new(expected))
            .await
            .map_err(workspace_error)?;
        if expected != workspace {
            return Err(workspace_error(WorkspaceError::ExecutionBindingMismatch));
        }
    }
    Ok(workspace)
}

pub(crate) async fn acquire_for_load(
    cfg: &AcpServerConfig,
    sessions: &std::collections::HashMap<String, SessionState>,
    session_id: &str,
    expected: Option<&str>,
) -> Result<(ResolvedWorkspace, Arc<dyn SessionExecutionLease>), AcpError> {
    validate_expected(cfg, session_id, expected).await?;
    let owner = if let Some(state) = sessions.get(session_id) {
        require_owner(state)?;
        state.execution_owner.clone().expect("owner checked")
    } else {
        cfg.controller
            .sessions()
            .acquire_execution_lease(&session_id.to_owned())
            .await
            .map_err(workspace_error)?
    };
    let workspace = match validate_expected(cfg, session_id, expected).await {
        Ok(workspace) => workspace,
        Err(error) => {
            if !sessions.contains_key(session_id) {
                let _ = owner.mark_clean().await;
            }
            return Err(error);
        }
    };
    if let Some(state) = sessions.get(session_id) {
        if Path::new(&state.cwd) != workspace.cwd {
            return Err(workspace_error(WorkspaceError::ExecutionBindingMismatch));
        }
    }
    Ok((workspace, owner))
}
