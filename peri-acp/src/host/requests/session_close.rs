//! Explicit session close and delete settlement.

use super::*;

struct CloseOwnerHeartbeat {
    stop: tokio_util::sync::CancellationToken,
    lost: tokio_util::sync::CancellationToken,
}

impl CloseOwnerHeartbeat {
    fn start(
        resources: Arc<dyn peri_acp_types::session_resources::SessionResources>,
        token: peri_acp_types::workspace::ExecutionOwnerToken,
    ) -> Self {
        let stop = tokio_util::sync::CancellationToken::new();
        let lost = tokio_util::sync::CancellationToken::new();
        let stop_for_task = stop.clone();
        let lost_for_task = lost.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = stop_for_task.cancelled() => break,
                    _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {}
                }
                if stop_for_task.is_cancelled() {
                    break;
                }
                if let Err(error) = resources.renew_execution_owner(&token).await {
                    tracing::error!(session_id = %token.root_id, %error,
                        "closing owner renewal failed");
                    lost_for_task.cancel();
                    break;
                }
            }
        });
        Self { stop, lost }
    }

    async fn guard<T>(
        &self,
        operation: impl std::future::Future<Output = Result<T, AcpError>>,
    ) -> Result<T, AcpError> {
        tokio::select! {
            _ = self.lost.cancelled() => Err(AcpError::new(-32010,
                "Session close incomplete: Store execution owner renewal failed")),
            result = operation => {
                if self.lost.is_cancelled() {
                    Err(AcpError::new(-32010,
                        "Session close incomplete: Store execution owner renewal failed"))
                } else { result }
            }
        }
    }
}

impl Drop for CloseOwnerHeartbeat {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

fn release_close_owner_scope(cfg: &AcpServerConfig, state: &SessionState, session_id: &str) {
    let Some(token) = state
        .execution_owner
        .as_ref()
        .and_then(|owner| owner.owner_token())
    else {
        return;
    };
    debug_assert_eq!(token.root_id.as_str(), session_id);
    let local = state
        .environment
        .as_ref()
        .map(|env| &env.cfg)
        .unwrap_or(cfg);
    if let Some(pool) = local.mcp_pool.clone().and_then(|port| {
        port.downcast_arc::<peri_middlewares::mcp::McpClientPool>()
            .ok()
    }) {
        pool.release_session_execution_owner(&token);
    }
}

pub(super) async fn close_owned_session(
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
    session_id: &str,
    delete: bool,
) -> Result<(), AcpError> {
    if sessions.get(session_id).is_some_and(|state| state.closing) {
        let resources = &cfg.session_resources;
        if delete {
            match resources.load_session_meta(&session_id.to_owned()).await {
                Err(error)
                    if matches!(
                        error.kind(),
                        peri_acp_types::session_resources::SessionResourceErrorKind::NotFound
                    ) =>
                {
                    if let Some(state) = sessions.get(session_id) {
                        release_close_owner_scope(cfg, state, session_id);
                    }
                    sessions.remove(session_id);
                    return Ok(());
                }
                Err(error) => return Err(super::super::super::workspace::resource_error(error)),
                Ok(_) => {}
            }
        } else if let Some(token) = sessions
            .get(session_id)
            .and_then(|state| state.execution_owner.as_ref())
            .and_then(|lease| lease.owner_token())
        {
            // The request dispatcher closes admission before this handler runs.
            // Only a persisted close intent makes this a settlement retry.
            if resources
                .is_session_closing(&session_id.to_owned())
                .await
                .map_err(super::super::super::workspace::resource_error)?
            {
                match resources
                    .close_settlement(&token)
                    .await
                    .map_err(super::super::super::workspace::resource_error)?
                {
                    peri_acp_types::session_resources::CloseSettlement::Finished => {
                        if let Some(state) = sessions.get(session_id) {
                            release_close_owner_scope(cfg, state, session_id);
                        }
                        sessions.remove(session_id);
                        return Ok(());
                    }
                    peri_acp_types::session_resources::CloseSettlement::ChangedOwner => {
                        return Err(AcpError::new(
                            -32010,
                            "Session close incomplete: execution owner changed during settlement",
                        ));
                    }
                    peri_acp_types::session_resources::CloseSettlement::Pending => {}
                }
            }
        }
    }
    if let Some(state) = sessions.get_mut(session_id) {
        let was_closing = state.closing;
        state.closing = true;
        let environment = state.environment.clone();
        let local = environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg);
        if state.execution_owner.is_none() {
            if delete {
                return Err(AcpError::new(
                    -32010,
                    "Session delete incomplete: read-only attachment has no execution owner",
                ));
            }
            // A read-only attachment never owns the session's running tasks.
            // Closing this view must not close another Agent's Workspace scope.
            local
                .session_manager
                .close_session(session_id)
                .await
                .map_err(super::super::super::workspace::workspace_error)?;
            sessions.remove(session_id);
            return Ok(());
        }
        if state.execution_owner.is_some() {
            let target = session_id.to_owned();
            if let Err(error) = cfg.session_resources.mark_session_closing(&target).await {
                match cfg.session_resources.is_session_closing(&target).await {
                    Ok(true) => {}
                    Ok(false) if !error.is_persistence_uncertain() => {
                        if !was_closing {
                            state.closing = false;
                        }
                        return Err(super::super::super::workspace::resource_error(error));
                    }
                    _ => {
                        return Err(AcpError::new(
                            -32010,
                            "Session close incomplete: closing intent is unconfirmed",
                        ));
                    }
                }
            }
        }
        let close_heartbeat = state
            .execution_owner
            .as_ref()
            .and_then(|owner| owner.owner_token())
            .map(|token| CloseOwnerHeartbeat::start(Arc::clone(&cfg.session_resources), token));
        state.continuation_armed = false;
        if let Some(token) = state.cancel_token.as_ref() {
            token.cancel();
        }
        cfg.session_manager.pre_close_session(session_id);
        if state.cancel_token.is_some() {
            return Err(AcpError::new(
                -32010,
                "Session close incomplete: prompt is still active",
            ));
        }
        if let Some(pool) = local.mcp_pool.clone().and_then(|port| {
            port.downcast_arc::<peri_middlewares::mcp::McpClientPool>()
                .ok()
        }) {
            let close_scope = async {
                pool.close_workspace_task_scope(session_id)
                    .await
                    .map_err(|error| {
                        AcpError::new(-32010, format!("Session close incomplete: {error}"))
                    })
            };
            if let Some(heartbeat) = &close_heartbeat {
                heartbeat.guard(close_scope).await?;
            } else {
                close_scope.await?;
            }
        }
        local
            .session_manager
            .close_session(session_id)
            .await
            .map_err(super::super::super::workspace::workspace_error)?;
        // A11/A22：`session/delete` **不**关闭 LSP pool——pool 归 host（同一 `Arc`
        // 被多 session 共享），关闭只发生在 host shutdown。此处若关闭，会把其它
        // 仍活跃 session 的 language server 一起掐掉。
        if let Some(environment) = environment.as_ref() {
            let shutdown = async { Ok::<_, AcpError>(environment.shutdown().await) };
            let drained = if let Some(heartbeat) = &close_heartbeat {
                heartbeat.guard(shutdown).await?
            } else {
                shutdown.await?
            };
            if !drained {
                return Err(AcpError::new(
                    -32010,
                    "Session close incomplete: resources are still active",
                ));
            }
        }
        // 只读会话没有执行所有权：关闭只需释放内存状态，不删除（删除会绕过他处的
        // 独占锁），也没有本节点持有的代际需要标 clean。
        let owner = state.execution_owner.clone();
        // 执行资源已排空（环境 shutdown 成功）后，才请求门面结清持久化并按需删除：
        // 排空确认在前、结清在最后，任一未完成都保持 Closing 与唯一 owner。
        let resources = &cfg.session_resources;
        let target = session_id.to_owned();
        let drain = async {
            resources
                .drain_persistence(&target)
                .await
                .map_err(super::super::super::workspace::resource_error)
        };
        if let Some(heartbeat) = &close_heartbeat {
            heartbeat.guard(drain).await?;
        } else {
            drain.await?;
        }
        if delete {
            // 删除是完整生命周期行为：数据、本机执行代际与本次持有都被门面在同一步
            // 结束（删除成功后 owner 已释放），因此这里不再重复收尾。
            if let Err(error) = resources.delete_session_tree(&target).await {
                match resources.load_session_meta(&target).await {
                    Err(readback)
                        if matches!(
                            readback.kind(),
                            peri_acp_types::session_resources::SessionResourceErrorKind::NotFound
                        ) => {}
                    _ => return Err(super::super::super::workspace::resource_error(error)),
                }
            }
        } else {
            if let Some(owner) = owner {
                let token = owner.owner_token().ok_or_else(|| {
                    AcpError::new(
                        -32010,
                        "Session close incomplete: Store execution owner token unavailable",
                    )
                })?;
                owner
                    .mark_clean()
                    .await
                    .map_err(super::super::super::workspace::workspace_error)?;
                if let Err(error) = resources.finish_close(&token).await {
                    match resources.close_settlement(&token).await {
                        Ok(peri_acp_types::session_resources::CloseSettlement::Finished) => {}
                        _ => return Err(super::super::super::workspace::resource_error(error)),
                    }
                }
            }
        }
        release_close_owner_scope(cfg, state, session_id);
        sessions.remove(session_id);
    } else {
        // 仅恢复已经持久接纳的关闭。无本地 SessionState 时没有执行所有权，
        // 不可通过新建关闭意图来关闭另一个 Agent 正在执行的会话。
        let resources = &cfg.session_resources;
        let target = session_id.to_owned();
        match resources.load_session_meta(&target).await {
            Ok(meta) => {
                if !resources
                    .is_session_closing(&target)
                    .await
                    .map_err(super::super::super::workspace::resource_error)?
                {
                    return Err(AcpError::new(
                        -32010,
                        "Session close incomplete: execution owner must initiate close",
                    ));
                }
                let recorded = resources
                    .read_execution_workspace_owner(&target)
                    .await
                    .map_err(super::super::super::workspace::resource_error)?
                    .ok_or_else(|| {
                        AcpError::new(
                            -32010,
                            "Session close incomplete: execution owner catalog unavailable",
                        )
                    })?;
                let trusted = super::super::owner_catalog::trusted_workspace_identity()
                    .map_err(|error| {
                        AcpError::new(-32010, format!("Session close incomplete: {error}"))
                    })?
                    .ok_or_else(|| {
                        AcpError::new(
                            -32010,
                            "Session close incomplete: trusted Workspace owner unavailable",
                        )
                    })?;
                super::super::owner_catalog::verify_recoverable_owner(&recorded, &trusted)
                    .map_err(|error| {
                        AcpError::new(-32010, format!("Session close incomplete: {error}"))
                    })?;
                let lease = resources
                    .claim_closing_execution(&target, recorded.current_epoch)
                    .await
                    .map_err(super::super::super::workspace::resource_error)?;
                let token = lease.owner_token().ok_or_else(|| {
                    AcpError::new(
                        -32010,
                        "Session close incomplete: Store takeover token unavailable",
                    )
                })?;
                let fenced_record = resources.read_execution_workspace_owner(&target).await
                    .map_err(super::super::super::workspace::resource_error)?
                    .ok_or_else(|| AcpError::new(-32010,
                        "Session close incomplete: execution owner catalog unavailable after takeover"))?;
                if fenced_record.current_epoch != token.epoch
                    || fenced_record.descriptor_epoch != recorded.descriptor_epoch
                    || fenced_record.descriptor != recorded.descriptor
                {
                    return Err(AcpError::new(
                        -32010,
                        "Session close incomplete: task owner catalog changed during takeover",
                    ));
                }
                resources
                    .renew_execution_owner(&token)
                    .await
                    .map_err(super::super::super::workspace::resource_error)?;
                let heartbeat = CloseOwnerHeartbeat::start(Arc::clone(resources), token.clone());
                // Only the SDK's private supervisor can attest the exact
                // former ACP generation and its registered process groups.
                heartbeat
                    .guard(async {
                        super::super::super::supervisor::previous_generation_stopped(
                            session_id,
                            &recorded.descriptor.agent_generation_id,
                        )
                        .await
                        .map_err(|error| {
                            AcpError::new(-32010, format!("Session close incomplete: {error}"))
                        })
                    })
                    .await?;
                let url = trusted.endpoint;
                let secret =
                    std::env::var("PERI_TRUSTED_WORKSPACE_SCOPE_SECRET_FILE").map_err(|_| {
                        AcpError::new(-32010,
                        "Session close incomplete: trusted Workspace scope authority unavailable")
                    })?;
                let pool = heartbeat
                    .guard(async {
                        peri_middlewares::mcp::McpClientPool::connect_trusted_workspace_for_close(
                            std::path::Path::new(&meta.cwd),
                            &url,
                            &secret,
                        )
                        .await
                        .map_err(|error| {
                            AcpError::new(-32010, format!("Session close incomplete: {error}"))
                        })
                    })
                    .await?;
                pool.bind_session_execution_owner(session_id, token.clone())
                    .map_err(|error| {
                        AcpError::new(-32010, format!("Session close incomplete: {error}"))
                    })?;
                heartbeat
                    .guard(async {
                        pool.fence_workspace_task_scope(session_id)
                            .await
                            .map_err(|error| {
                                AcpError::new(-32010, format!("Session close incomplete: {error}"))
                            })
                    })
                    .await?;
                heartbeat
                    .guard(async {
                        pool.reconcile_closing_workspace_scope(session_id)
                            .await
                            .map_err(|error| {
                                AcpError::new(-32010, format!("Session close incomplete: {error}"))
                            })
                    })
                    .await?;
                heartbeat
                    .guard(async {
                        resources
                            .drain_persistence(&target)
                            .await
                            .map_err(super::super::super::workspace::resource_error)
                    })
                    .await?;
                if delete {
                    if let Err(error) = resources.delete_session_tree(&target).await {
                        match resources.load_session_meta(&target).await {
                            Err(readback) if matches!(readback.kind(),
                                peri_acp_types::session_resources::SessionResourceErrorKind::NotFound) => {}
                            _ => return Err(super::super::super::workspace::resource_error(error)),
                        }
                    }
                } else {
                    lease
                        .mark_clean()
                        .await
                        .map_err(super::super::super::workspace::workspace_error)?;
                    if let Err(error) = resources.finish_close(&token).await {
                        match resources.close_settlement(&token).await {
                            Ok(peri_acp_types::session_resources::CloseSettlement::Finished) => {}
                            _ => return Err(super::super::super::workspace::resource_error(error)),
                        }
                    }
                }
                return Ok(());
            }
            Err(error)
                if matches!(
                    error.kind(),
                    peri_acp_types::session_resources::SessionResourceErrorKind::NotFound
                ) =>
            {
                return Ok(());
            }
            Err(error) => return Err(super::super::super::workspace::resource_error(error)),
        }
    }
    Ok(())
}
