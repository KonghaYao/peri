//! A session owns one verified execution environment and its resource lifetime.

use std::{path::Path, sync::Arc};

use super::{assemble, task_scope, AcpServerConfig, SessionState};
use crate::transport::types::AcpError;
use peri_acp_types::workspace::{
    ReadOnlyAdmission, ResolvedWorkspace, SessionExecutionLease, WorkspaceError,
};

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

/// 绑定复核强度。
///
/// 准入边界（协议读请求、`session/new` | `load` | `resume` | `fork` 的入口）复核完整
/// 发现快照；同一次准入内的后续检查只复核已记录证据——准入已经观测过完整快照，为
/// 同一件事再跑一轮 Git 发现不会带来新证据，只会把 Git 的等待叠加到这次准入的每一步。
#[derive(Clone, Copy)]
pub(crate) enum BindingCheck {
    Full,
    Recorded,
}

/// 一次准入的权威检查：复核完整发现快照，并比对调用方给出的执行目录。
///
/// 一次准入只应调用一次（准入以本次复核为判定依据）；准入内的后续检查用
/// [`reassert_expected`]。
pub(crate) async fn validate_expected(
    cfg: &AcpServerConfig,
    session_id: &str,
    expected: Option<&str>,
) -> Result<ResolvedWorkspace, AcpError> {
    check_expected(cfg, session_id, expected, BindingCheck::Full).await
}

/// 同一次准入内的检查：只复核已记录证据，不重新执行 Git 发现。
///
/// 绑定不存在、关系不一致、目录被替换或换位仍然失败；省去的只有「重新执行 Git
/// 发现」——准入已经观测过完整快照，重复发现只是把 Git 的等待叠加到同一次准入。
pub(crate) async fn reassert_expected(
    cfg: &AcpServerConfig,
    session_id: &str,
    expected: Option<&str>,
) -> Result<ResolvedWorkspace, AcpError> {
    check_expected(cfg, session_id, expected, BindingCheck::Recorded).await
}

async fn check_expected(
    cfg: &AcpServerConfig,
    session_id: &str,
    expected: Option<&str>,
    check: BindingCheck,
) -> Result<ResolvedWorkspace, AcpError> {
    let store = cfg.controller.sessions();
    let id = session_id.to_owned();
    let workspace = match check {
        BindingCheck::Full => store.validate_session_binding(&id).await,
        BindingCheck::Recorded => store.reassert_session_binding(&id).await,
    }
    .map_err(workspace_error)?;
    if let Some(expected) = expected {
        expect_directory(expected, &workspace).await?;
    }
    Ok(workspace)
}

/// 调用方给出的执行目录必须与绑定指向同一目录。
///
/// 绑定的 cwd 在登记时已规范化，因此比对规范化后的路径即可：同一目录的等价路径
/// （符号链接、`/var` 与 `/private/var`）仍然一致，别的目录与绑定不符。比较不再
/// 解析登记——解析会为比较再跑一轮完整发现，也顺带登记一个与本次执行无关的目录。
pub(crate) async fn expect_directory(
    expected: &str,
    workspace: &ResolvedWorkspace,
) -> Result<(), AcpError> {
    let expected = tokio::fs::canonicalize(expected)
        .await
        .map_err(|_| workspace_error(WorkspaceError::Unavailable))?;
    if expected != workspace.cwd {
        return Err(workspace_error(WorkspaceError::ExecutionBindingMismatch));
    }
    Ok(())
}

/// 一次加载准入的结果：绑定与执行目录已复核，执行所有权可能不可得。
///
/// 所有权不可得（`ExecutionBusy` / `RecoveryRequired` / `ExecutionLeaseRequired`）时
/// 仍返回已复核的 `workspace`，由调用方决定是降级为只读会话还是原样上报——绑定复核
/// 已经跑过一次完整发现，降级路径不能为了拿到同一个 `workspace` 再跑一轮。
pub(crate) struct LoadAdmission {
    pub(crate) workspace: ResolvedWorkspace,
    pub(crate) execution: ExecutionAdmission,
}

/// 执行所有权判定结果。
pub(crate) enum ExecutionAdmission {
    /// 本次准入持有执行所有权。
    Owned(Arc<dyn SessionExecutionLease>),
    /// 执行所有权不可得，但会话历史仍可只读访问；携带原因供调用方上报与降级。
    Unavailable(ReadOnlyAdmission),
}

/// 加载/恢复/克隆准入的第一步：复核执行目录并取得执行所有权。
///
/// 这是准入入口，因此做完整复核；同一次准入内再取一次（如 `session/fork` 在
/// `prepare_existing` 之后）用 [`reacquire_for_load`]，不重复跑 Git 发现。
pub(crate) async fn acquire_for_load(
    cfg: &AcpServerConfig,
    sessions: &std::collections::HashMap<String, SessionState>,
    session_id: &str,
    expected: Option<&str>,
) -> Result<LoadAdmission, AcpError> {
    acquire_for_load_with(cfg, sessions, session_id, expected, BindingCheck::Full).await
}

/// 同一次准入内再次取得执行目录与所有权：只复核已记录证据。
///
/// 这里不接受只读降级：调用方（`session/fork`）必须有执行所有权才能继续。
pub(crate) async fn reacquire_for_load(
    cfg: &AcpServerConfig,
    sessions: &std::collections::HashMap<String, SessionState>,
    session_id: &str,
    expected: Option<&str>,
) -> Result<(ResolvedWorkspace, Arc<dyn SessionExecutionLease>), AcpError> {
    let admission =
        acquire_for_load_with(cfg, sessions, session_id, expected, BindingCheck::Recorded).await?;
    match admission.execution {
        ExecutionAdmission::Owned(owner) => Ok((admission.workspace, owner)),
        ExecutionAdmission::Unavailable(reason) => Err(read_only_error(reason)),
    }
}

/// 只读降级原因还原为错误：不接受降级的调用方（如 `session/fork`）按原语义上报。
pub(crate) fn read_only_error(reason: ReadOnlyAdmission) -> AcpError {
    let error = match reason {
        ReadOnlyAdmission::ExecutionBusy => WorkspaceError::ExecutionBusy,
        ReadOnlyAdmission::RecoveryRequired(details) => WorkspaceError::RecoveryRequired(details),
        ReadOnlyAdmission::ExecutionLeaseRequired => WorkspaceError::ExecutionLeaseRequired,
    };
    workspace_error(error)
}

async fn acquire_for_load_with(
    cfg: &AcpServerConfig,
    sessions: &std::collections::HashMap<String, SessionState>,
    session_id: &str,
    expected: Option<&str>,
    check: BindingCheck,
) -> Result<LoadAdmission, AcpError> {
    // 绑定与执行目录不符直接失败：只读降级只针对执行所有权不可得，不掩盖身份问题。
    let workspace = check_expected(cfg, session_id, expected, check).await?;
    // 已持有所有权直接复用；只读会话与冷会话都重新尝试取得——他处释放后再次准入
    // 才有机会升级回可执行，而不是一次只读就永久只读。
    let held = match sessions.get(session_id) {
        Some(state) if state.closing => return Err(AcpError::new(-32010, "Session is closing")),
        Some(state) => state.execution_owner.clone(),
        None => None,
    };
    let (owner, acquired_here) = match held {
        Some(owner) => (owner, false),
        None => {
            match cfg
                .controller
                .sessions()
                .acquire_execution_lease(&session_id.to_owned())
                .await
            {
                Ok(owner) => (owner, true),
                Err(error) => match read_only_reason(&error) {
                    Some(reason) => {
                        return Ok(LoadAdmission {
                            workspace,
                            execution: ExecutionAdmission::Unavailable(reason),
                        });
                    }
                    None => return Err(workspace_error(error)),
                },
            }
        }
    };
    // 取得所有权后复核的是同一件事：重新发现的证据与首次复核相同，这里只复核已记录证据。
    let workspace = match check_expected(cfg, session_id, expected, BindingCheck::Recorded).await {
        Ok(workspace) => workspace,
        Err(error) => {
            // 本次准入自己取得的代际必须收尾，否则会留下既无持有者又未标 clean 的运行。
            if acquired_here {
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
    Ok(LoadAdmission {
        workspace,
        execution: ExecutionAdmission::Owned(owner),
    })
}

/// 取不到执行所有权的原因是否属于「所有权不可得、历史仍可读」。
///
/// 只有存储层明确给出的三类才降级：其他失败（IO、绑定复核、schema 不支持）仍旧
/// 原样上报，避免把「读不了」伪装成「可以只读进入」。
fn read_only_reason(error: &anyhow::Error) -> Option<ReadOnlyAdmission> {
    error
        .downcast_ref::<WorkspaceError>()
        .and_then(ReadOnlyAdmission::from_workspace_error)
}
