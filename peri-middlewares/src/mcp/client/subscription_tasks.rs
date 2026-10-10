use std::{sync::Arc, time::Duration};

use peri_acp_types::event::BackgroundTaskResult;
use peri_acp_types::tasks::TaskManager;
use rmcp::{
    model::{
        CancelTaskParams, ClientRequest, CustomRequest, DetailedTask, GetTaskParams,
        RequestMetaObject, ServerResult, TaskPayload,
    },
    service::{Peer, RoleClient},
};
use serde::Deserialize;
use serde_json::json;

use super::super::McpClientPool;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScopeTaskRow {
    task: DetailedTask,
}

/// Bounded give-up for an unobservable task owner: after this many consecutive
/// failed 2-second polls (about 10 minutes) the projection is settled as
/// unresolved instead of staying active forever. The final reminder states that
/// the remote side effects are unknown.
const LOST_ABANDON_ATTEMPTS: usize = 300;

#[derive(Deserialize)]
struct ScopeTaskSnapshot {
    cursor: u64,
    epoch: u64,
    closing: bool,
    #[serde(default, rename = "resourcesSettled")]
    resources_settled: Option<bool>,
    tasks: Vec<ScopeTaskRow>,
}

fn pending_tasks_for_closed_epoch(
    server: &str,
    expected_epoch: u64,
    snapshot: ScopeTaskSnapshot,
) -> Result<Vec<String>, String> {
    if snapshot.epoch != expected_epoch || !snapshot.closing {
        return Err(format!(
            "workspace task owner {server} close epoch changed during reconciliation"
        ));
    }
    if snapshot.resources_settled.is_none() {
        return Err(format!(
            "Incomplete: workspace task owner {server} resource settlement evidence missing"
        ));
    }
    Ok(snapshot
        .tasks
        .into_iter()
        .filter(|row| !row.task.status().is_terminal())
        .map(|row| row.task.task.task_id)
        .collect())
}

impl McpClientPool {
    async fn wait_for_task_owner_catalog(&self) -> Result<(), String> {
        let until = peri_time::monotonic_now() + Duration::from_secs(10);
        while self.configs.read().is_empty()
            && !self.initialized.load(std::sync::atomic::Ordering::Acquire)
        {
            if peri_time::monotonic_now() >= until {
                return Err("workspace task owner catalog unavailable".into());
            }
            peri_time::sleep(Duration::from_millis(50)).await;
        }
        Ok(())
    }

    async fn wait_for_workspace_peer(&self, server: &str) -> Result<Peer<RoleClient>, String> {
        let until = peri_time::monotonic_now() + Duration::from_secs(10);
        loop {
            if let Some(peer) = self
                .clients
                .read()
                .get(server)
                .and_then(|client| client.peer.clone())
            {
                return Ok(peer);
            }
            if peri_time::monotonic_now() >= until {
                return Err(format!("workspace task owner {server} disconnected"));
            }
            peri_time::sleep(Duration::from_millis(50)).await;
        }
    }

    fn configured_workspace_task_owners(&self) -> Vec<String> {
        self.configs
            .read()
            .iter()
            .filter_map(|(name, config)| match config.source.as_ref() {
                Some(crate::mcp::config::ConfigSource::Builtin { instance })
                    if instance == "workspace" =>
                {
                    Some(name.clone())
                }
                Some(crate::mcp::config::ConfigSource::WorkspaceRemote) => Some(name.clone()),
                _ => None,
            })
            .collect()
    }

    /// Conclusive scope reconciliation clears the execution evidence recorded by
    /// cancelled or timed-out calls to that same owner, so a single interruption
    /// cannot lock the session permanently. Without evidence nothing is cleared.
    fn resolve_execution_evidence(&self, session_id: &str, scope: &str) {
        let Some(manager) = self.session_bindings.read().manager(session_id) else {
            return;
        };
        let cleared = manager.resolve_external_execution_evidence(scope);
        if cleared > 0 {
            tracing::info!(session = %session_id, scope, cleared,
                "cleared external execution uncertainty after conclusive reconciliation");
        }
    }

    /// Explicit session close: gate new Workspace work, discover all admitted
    /// tasks, then route cancellation through the session TaskManager.
    pub async fn close_workspace_task_scope(
        self: &Arc<Self>,
        session_id: &str,
    ) -> Result<(), String> {
        self.reconcile_closing_workspace_scope(session_id).await?;
        for server in self.configured_workspace_task_owners() {
            self.resolve_execution_evidence(session_id, &server);
        }
        let manager = self.session_bindings.read().manager(session_id);
        let Some(manager) = manager else {
            // A session may have no Agent task projection (for example a
            // disabled Workspace profile). The owner scope was already
            // reconciled above, and SessionManager closes its own tasks.
            return Ok(());
        };
        for task_id in manager.external_task_ids() {
            manager.cancel_async(&task_id).await?;
        }
        let until = peri_time::monotonic_now() + Duration::from_secs(20);
        while manager.has_unsettled_external() {
            if peri_time::monotonic_now() >= until {
                return Err("external task close incomplete".into());
            }
            peri_time::sleep(Duration::from_millis(200)).await;
        }
        Ok(())
    }

    /// Close resources on the currently connected, trusted Workspace scope.
    pub async fn reconcile_closing_workspace_scope(&self, session_id: &str) -> Result<(), String> {
        self.wait_for_task_owner_catalog().await?;
        for server in self.configured_workspace_task_owners() {
            let peer = self.wait_for_workspace_peer(&server).await?;
            let meta = self
                .task_scope_meta_for(&server, session_id)
                .ok_or_else(|| format!("workspace task scope for {server} unavailable"))?;
            let initial: ScopeTaskSnapshot =
                serde_json::from_value(Self::workspace_scope_snapshot(&peer, meta.clone()).await?)
                    .map_err(|error| format!("invalid workspace task snapshot: {error}"))?;
            let response = peri_time::timeout(
                Duration::from_secs(10),
                peer.send_request(ClientRequest::CustomRequest(CustomRequest::new(
                    "workspace/taskClose",
                    Some(json!({"_meta": meta, "epoch": initial.epoch})),
                ))),
            )
            .await
            .map_err(|_| "workspace task close timed out".to_owned())?;
            let response = match response {
                Ok(response) => response,
                Err(error) => {
                    let current: ScopeTaskSnapshot = serde_json::from_value(
                        Self::workspace_scope_snapshot(&peer, meta.clone()).await?,
                    )
                    .map_err(|cause| format!("invalid workspace task snapshot: {cause}"))?;
                    if current.epoch != initial.epoch || !current.closing {
                        return Err(format!(
                            "workspace task close epoch changed; scope must be reviewed before retry: {error}"
                        ));
                    }
                    return Err(format!("workspace task close rejected: {error}"));
                }
            };
            let ServerResult::CustomResult(response) = response else {
                return Err("workspace task close returned unexpected response".into());
            };
            let barrier_cursor = response
                .0
                .get("barrierCursor")
                .and_then(serde_json::Value::as_u64)
                .ok_or("Incomplete: workspace task close barrier missing")?;
            if response.0.get("epoch").and_then(serde_json::Value::as_u64) != Some(initial.epoch) {
                return Err("Incomplete: workspace task close response epoch mismatch".into());
            }
            let until = peri_time::monotonic_now() + Duration::from_secs(20);
            loop {
                let snapshot: ScopeTaskSnapshot = serde_json::from_value(
                    Self::workspace_scope_snapshot(&peer, meta.clone()).await?,
                )
                .map_err(|error| format!("invalid workspace task snapshot: {error}"))?;
                if snapshot.cursor < barrier_cursor {
                    return Err("Incomplete: workspace task snapshot precedes close barrier".into());
                }
                let resources_settled = snapshot.resources_settled == Some(true);
                let pending = pending_tasks_for_closed_epoch(&server, initial.epoch, snapshot)?;
                if pending.is_empty() && resources_settled {
                    break;
                }
                for raw_id in pending {
                    let mut params = CancelTaskParams::new(raw_id);
                    params.meta = Some(meta.clone());
                    // A concurrent terminal transition can race cancellation.
                    // The next snapshot is the authoritative completion check.
                    match peri_time::timeout(Duration::from_secs(5), peer.cancel_task(params)).await
                    {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => tracing::debug!(server = %server, %error,
                            "Workspace close cancel failed; reconciling"),
                        Err(error) => tracing::debug!(server = %server, %error,
                            "Workspace close cancel timed out; reconciling"),
                    }
                }
                if peri_time::monotonic_now() >= until {
                    return Err(format!("workspace task owner {server} close incomplete"));
                }
                peri_time::sleep(Duration::from_millis(200)).await;
            }
        }
        Ok(())
    }

    /// Admit work into a previously closed scope only after Store has cleared
    /// closing the owner scope. Epoch CAS fences delayed taskClose replays.
    pub async fn open_workspace_task_scope(&self, session_id: &str) -> Result<(), String> {
        self.wait_for_task_owner_catalog().await?;
        for server in self.configured_workspace_task_owners() {
            let peer = self.wait_for_workspace_peer(&server).await?;
            let meta = self
                .task_scope_meta_for(&server, session_id)
                .ok_or_else(|| format!("workspace task scope for {server} unavailable"))?;
            let snapshot: ScopeTaskSnapshot =
                serde_json::from_value(Self::workspace_scope_snapshot(&peer, meta.clone()).await?)
                    .map_err(|error| format!("invalid workspace task snapshot: {error}"))?;
            if !snapshot.closing {
                continue;
            }
            if snapshot.resources_settled != Some(true)
                || snapshot
                    .tasks
                    .iter()
                    .any(|row| !row.task.task.status.is_terminal())
            {
                return Err("Incomplete: previous Workspace scope resources unsettled".into());
            }
            let response = peri_time::timeout(
                Duration::from_secs(10),
                peer.send_request(ClientRequest::CustomRequest(CustomRequest::new(
                    "workspace/taskOpen",
                    Some(json!({"_meta":meta,"epoch":snapshot.epoch})),
                ))),
            )
            .await
            .map_err(|_| "workspace task open timed out".to_owned())?
            .map_err(|error| format!("workspace task open rejected: {error}"))?;
            let ServerResult::CustomResult(result) = response else {
                return Err("workspace task open returned unexpected response".into());
            };
            let next_epoch = result
                .0
                .get("epoch")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| "workspace task open epoch missing".to_owned())?;
            if next_epoch != snapshot.epoch.saturating_add(1) {
                return Err("workspace task open epoch mismatch".into());
            }
        }
        Ok(())
    }

    async fn workspace_scope_snapshot(
        peer: &Peer<RoleClient>,
        meta: RequestMetaObject,
    ) -> Result<serde_json::Value, String> {
        let result = peri_time::timeout(
            Duration::from_secs(10),
            peer.send_request(ClientRequest::CustomRequest(CustomRequest::new(
                "workspace/taskSnapshot",
                Some(json!({"_meta": meta})),
            ))),
        )
        .await
        .map_err(|_| "workspace task snapshot timed out".to_owned())?
        .map_err(|error| error.to_string())?;
        match result {
            ServerResult::CustomResult(result) => Ok(result.0),
            _ => Err("workspace task snapshot returned unexpected response".into()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn spawn_managed_task_subscription(
        self: &Arc<Self>,
        server: String,
        session_id: String,
        raw_task_id: String,
        task_id: String,
        is_workspace_shell: bool,
        initial_peer: Peer<RoleClient>,
    ) -> Result<(), crate::mcp::task_scope::TaskAdmissionError> {
        let weak_pool = Arc::downgrade(self);
        let manager = self.session_bindings.read().manager(&session_id);
        let key = crate::mcp::McpTaskKey::TaskStatus {
            server: server.clone(),
            task_id: task_id.clone(),
        };
        self.task_spawner.spawn(key, async move {
            let mut peer = initial_peer;
            // Consecutive polls without an observable owner or confirmed
            // terminal delivery. Reset whenever the owner answers.
            let mut attempts: usize = 0;
            loop {
                let Some(pool) = weak_pool.upgrade() else {
                    break;
                };
                let Some(manager) = manager.as_ref() else {
                    break;
                };
                let mut params = GetTaskParams::new(&raw_task_id);
                if is_workspace_shell {
                    params.meta = pool.task_scope_meta_for(&server, &session_id);
                }
                match peer.get_task(params).await {
                    Ok(snapshot) if snapshot.task.status().is_terminal() => {
                        let transition_id = format!("{raw_task_id}:terminal");
                        let result = pool.deliver_managed_task_status(
                            &server,
                            &task_id,
                            &transition_id,
                            &snapshot.task,
                            is_workspace_shell,
                            manager.as_ref(),
                        ).await;
                        if let Err(error) = result {
                            // Bounded give-up: an owner that stays unobservable
                            // must not keep the task active forever.
                            let attempts = attempts.saturating_add(1);
                            if attempts >= LOST_ABANDON_ATTEMPTS {
                                match manager
                                    .abandon_external(&task_id, &error)
                                    .await
                                {
                                    Ok(true) => break,
                                    Ok(false) => {}
                                    Err(abandon_error) => {
                                        tracing::warn!(server = %server, task_id = %raw_task_id,
                                            %abandon_error, "MCP task abandon pending reconciliation");
                                    }
                                }
                            }
                            tracing::warn!(server = %server, task_id = %raw_task_id, %error,
                                "MCP task terminal delivery pending reconciliation");
                        } else {
                            break;
                        }
                    }
                    Ok(_) => {
                        attempts = 0;
                        manager.mark_external_running(&task_id);
                    }
                    Err(error) => {
                        manager.mark_external_lost(&task_id);
                        let attempts = attempts.saturating_add(1);
                        if attempts >= LOST_ABANDON_ATTEMPTS {
                            match manager
                                .abandon_external(
                                    &task_id,
                                    &format!("task status unavailable: {error}"),
                                )
                                .await
                            {
                                Ok(true) => break,
                                Ok(false) => {}
                                Err(abandon_error) => {
                                    tracing::warn!(server = %server, task_id = %raw_task_id,
                                        %abandon_error, "MCP task abandon pending reconciliation");
                                }
                            }
                        }
                        tracing::warn!(server = %server, task_id = %raw_task_id, %error,
                            "MCP task status unavailable; retrying");
                    }
                }
                drop(pool);
                peri_time::sleep(Duration::from_secs(2)).await;
                if let Some(pool) = weak_pool.upgrade() {
                    if let Some(next) = pool
                        .clients
                        .read()
                        .get(&server)
                        .and_then(|client| client.peer.clone())
                    {
                        peer = next;
                    }
                }
            }
        })
    }

    async fn deliver_managed_task_status(
        &self,
        server: &str,
        task_id: &str,
        transition_id: &str,
        task: &DetailedTask,
        is_workspace_shell: bool,
        manager: &dyn TaskManager,
    ) -> Result<(), String> {
        let result = Self::map_task_result(server, task_id, task, is_workspace_shell);
        if manager
            .settle_external(task_id, transition_id, result)
            .await?
            || manager.snapshot().tasks.iter().any(|record| {
                record.task_id == task_id
                    && matches!(record.status.as_str(), "completed" | "failed" | "cancelled")
            })
        {
            Ok(())
        } else {
            Err(format!(
                "MCP task {task_id} terminal settlement is unconfirmed"
            ))
        }
    }

    fn map_task_result(
        server: &str,
        task_id: &str,
        task: &DetailedTask,
        is_workspace_shell: bool,
    ) -> BackgroundTaskResult {
        let shell_result = if is_workspace_shell {
            match &task.payload {
                TaskPayload::Completed { result } => {
                    result.get("structuredContent").and_then(|value| {
                        serde_json::from_value::<BackgroundTaskResult>(value.clone()).ok()
                    })
                }
                _ => None,
            }
        } else {
            None
        };
        let status = format!("{:?}", task.status());
        let output = match &task.payload {
            TaskPayload::Completed { result } => result
                .get("content")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|item| item.get("text").and_then(serde_json::Value::as_str))
                .collect::<Vec<_>>()
                .join("\n"),
            TaskPayload::Failed { error } => error
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Task failed")
                .to_owned(),
            _ => task.task.status_message.clone().unwrap_or_default(),
        };
        let result = shell_result.unwrap_or_else(|| BackgroundTaskResult {
            task_id: task_id.to_owned(),
            agent_name: "mcp".into(),
            prompt_summary: server.to_owned(),
            success: matches!(task.payload, TaskPayload::Completed { .. }),
            output: format!(
                "MCP task on {server} finished with status {status}.\n{}",
                output.chars().take(16_000).collect::<String>()
            ),
            tool_calls_count: 0,
            duration_ms: 0,
            timed_out: false,
            child_thread_id: None,
            subagent_failure: None,
            shell_output: None,
        });
        let mut result = result;
        result.task_id = task_id.to_owned();
        result
    }
}

#[path = "subscription_task_binding.rs"]
mod binding;

#[cfg(test)]
#[path = "subscription_tasks_test.rs"]
mod task_projection_tests;
