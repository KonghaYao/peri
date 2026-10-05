use std::{collections::HashMap, sync::Arc, time::Duration};

use peri_acp_types::event::BackgroundTaskResult;
use peri_acp_types::tasks::{BgTaskKind, TaskManager};
use rmcp::{
    model::{
        CancelTaskParams, ClientRequest, CustomRequest, DetailedTask, GetTaskParams,
        RequestMetaObject, ServerResult, TaskPayload,
    },
    service::{Peer, RoleClient},
};
use serde::Deserialize;
use serde_json::json;

use super::super::{McpClientHandle, McpClientPool, McpInitStatus};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScopeTaskRow {
    task: DetailedTask,
    summary: String,
    initiator_session_id: Option<String>,
    terminal_transition_id: Option<String>,
}

impl ScopeTaskRow {
    fn initiator_for_scope(&self, session_id: &str) -> Result<&str, String> {
        match self.initiator_session_id.as_deref() {
            Some(initiator) if !initiator.is_empty() && initiator == session_id => Ok(initiator),
            Some(_) => {
                Err("Unroutable: workspace task initiator conflicts with discovery scope".into())
            }
            None => Err("Unroutable: workspace task initiator missing".into()),
        }
    }
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
    tasks: Vec<ScopeTaskRow>,
}

#[derive(Deserialize)]
struct ScopeTaskChanges {
    cursor: u64,
    changes: Vec<ScopeTaskRow>,
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
    Ok(snapshot
        .tasks
        .into_iter()
        .filter(|row| !row.task.status().is_terminal())
        .map(|row| row.task.task.task_id)
        .collect())
}

impl McpClientPool {
    /// Rebuild a trusted remote Workspace connection for an unloaded close.
    /// Endpoint must come from the deployment, never
    /// from a close request or the model's MCP server declaration.
    pub async fn connect_trusted_workspace_for_close(
        cwd: &std::path::Path,
        url: &str,
    ) -> Result<Arc<Self>, String> {
        let mut workspace: peri_acp_types::plugin::McpServerConfig =
            serde_json::from_value(json!({"url":url,"systemMcp":true}))
                .map_err(|error| format!("trusted Workspace config invalid: {error}"))?;
        workspace.source = Some(crate::mcp::config::ConfigSource::WorkspaceRemote);
        let pool = Arc::new(Self::new_pending());
        pool.set_session_servers(HashMap::from([("workspace".to_owned(), workspace)]))
            .map_err(|error| error.to_string())?;
        let (status, received) = tokio::sync::watch::channel(McpInitStatus::Pending);
        Self::run_initialize_bare(pool.clone(), cwd, status).await;
        if !matches!(&*received.borrow(), McpInitStatus::Ready { .. }) {
            return Err(format!(
                "trusted Workspace reconnect failed: {:?}",
                *received.borrow()
            ));
        }
        if pool
            .clients
            .read()
            .get("workspace")
            .and_then(|client| client.peer.as_ref())
            .is_none()
        {
            return Err("trusted Workspace owner disconnected".into());
        }
        Ok(pool)
    }
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

    fn workspace_task_clients(&self) -> Vec<Arc<McpClientHandle>> {
        self.clients.read().values().filter(|client| matches!(
            client.source.as_ref(),
            Some(crate::mcp::config::ConfigSource::Builtin { instance }) if instance == "workspace"
        ) || matches!(client.source.as_ref(),
            Some(crate::mcp::config::ConfigSource::WorkspaceRemote)))
            .cloned().collect()
    }

    /// Conclusive scope reconciliation clears the execution evidence recorded by
    /// cancelled or timed-out calls to that same owner, so a single interruption
    /// cannot lock the session permanently. Without evidence nothing is cleared.
    fn resolve_execution_evidence(&self, session_id: &str, scope: &str) {
        let Some(manager) = self.session_tasks.read().get(session_id).cloned() else {
            return;
        };
        let cleared = manager.resolve_external_execution_evidence(scope);
        if cleared > 0 {
            tracing::info!(session = %session_id, scope, cleared,
                "cleared external execution uncertainty after conclusive reconciliation");
        }
    }

    async fn apply_scope_row(
        self: &Arc<Self>,
        session_id: &str,
        client: &McpClientHandle,
        peer: &Peer<RoleClient>,
        row: ScopeTaskRow,
    ) -> Result<(), String> {
        let initiator = row.initiator_for_scope(session_id)?;
        let raw_id = row.task.task.task_id.clone();
        if row.task.status().is_terminal() {
            let (manager, registration) = self.external_task_registration(
                session_id,
                Some(initiator),
                None,
                &client.name,
                &raw_id,
                BgTaskKind::Shell,
                &row.summary,
                true,
                &row.task.task.created_at,
            )?;
            let transition_id = row
                .terminal_transition_id
                .as_deref()
                .ok_or_else(|| "workspace terminal transition ID missing".to_owned())?;
            manager
                .restore_external_terminal(
                    registration,
                    transition_id,
                    Self::map_task_result(&client.name, &raw_id, &row.task, true),
                )
                .await?;
        } else {
            let task_id = self.register_external_task(
                session_id,
                Some(initiator),
                None,
                &client.name,
                &raw_id,
                BgTaskKind::Shell,
                &row.summary,
                true,
                &row.task.task.created_at,
            )?;
            match self.spawn_managed_task_subscription(
                client.name.clone(),
                session_id.to_owned(),
                raw_id,
                task_id,
                true,
                peer.clone(),
            ) {
                Ok(()) | Err(crate::mcp::task_scope::TaskAdmissionError::DuplicateKey) => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        Ok(())
    }

    /// Keep a scoped cursor after the initial snapshot. Expired cursors and
    /// disconnected owners cause a fresh snapshot, so response-loss tasks are rediscovered.
    pub async fn watch_workspace_tasks(
        self: &Arc<Self>,
        session_id: &str,
        cancel: tokio_util::sync::CancellationToken,
    ) {
        let mut cursors: HashMap<String, u64> = HashMap::new();
        loop {
            if cancel.is_cancelled() {
                break;
            }
            let clients = self.workspace_task_clients();
            for client in clients {
                let Some(peer) = client.peer.clone() else {
                    cursors.remove(&client.name);
                    continue;
                };
                let Some(meta) = self.task_scope_meta_for(&client.name, session_id) else {
                    continue;
                };
                if let Some(cursor) = cursors.get(&client.name).copied() {
                    let response = peri_time::timeout(
                        Duration::from_secs(5),
                        peer.send_request(ClientRequest::CustomRequest(CustomRequest::new(
                            "workspace/taskChanges",
                            Some(json!({"_meta": meta, "cursor":cursor,"waitMs":1000})),
                        ))),
                    )
                    .await;
                    let changes = match response {
                        Ok(Ok(ServerResult::CustomResult(result))) => {
                            serde_json::from_value::<ScopeTaskChanges>(result.0).ok()
                        }
                        _ => None,
                    };
                    if let Some(changes) = changes {
                        let mut applied = true;
                        for row in changes.changes {
                            if let Err(error) =
                                self.apply_scope_row(session_id, &client, &peer, row).await
                            {
                                tracing::warn!(server = %client.name, %error, "Workspace task change pending reconciliation");
                                applied = false;
                                break;
                            }
                        }
                        if applied {
                            cursors.insert(client.name.clone(), changes.cursor);
                        } else {
                            cursors.remove(&client.name);
                        }
                    } else {
                        cursors.remove(&client.name);
                    }
                } else {
                    match Self::workspace_scope_snapshot(&peer, meta)
                        .await
                        .and_then(|value| {
                            serde_json::from_value::<ScopeTaskSnapshot>(value).map_err(|error| {
                                format!("invalid Workspace scope snapshot: {error}")
                            })
                        }) {
                        Ok(snapshot) => {
                            let mut applied = true;
                            for row in snapshot.tasks {
                                if let Err(error) =
                                    self.apply_scope_row(session_id, &client, &peer, row).await
                                {
                                    tracing::warn!(server = %client.name, %error, "Workspace task snapshot pending reconciliation");
                                    applied = false;
                                    break;
                                }
                            }
                            if applied {
                                cursors.insert(client.name.clone(), snapshot.cursor);
                                self.resolve_execution_evidence(session_id, &client.name);
                            }
                        }
                        Err(error) => tracing::debug!(server = %client.name, %error,
                            "Workspace task scope unavailable; retrying"),
                    }
                }
            }
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = peri_time::sleep(Duration::from_secs(2)) => {},
            }
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
        let manager = self.session_tasks.read().get(session_id).cloned();
        let Some(manager) = manager else {
            // A session may have no Agent task projection (for example a
            // disabled Workspace profile). The owner scope was already
            // reconciled above, and SessionManager closes its own tasks.
            return Ok(());
        };
        if !self.workspace_task_clients().is_empty() {
            self.recover_workspace_tasks(session_id).await?;
        }
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

    /// Resume an explicit close after the Agent process has restarted. The
    /// trusted host derives scope credentials from the session identity; this
    /// path does not require a live Agent projection or inbox, because the
    /// session is being deleted after the owner confirms every task terminal.
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
            if response
                .0
                .get("barrierCursor")
                .and_then(serde_json::Value::as_u64)
                .is_none()
            {
                return Err("workspace task close barrier missing".into());
            }
            let until = peri_time::monotonic_now() + Duration::from_secs(20);
            loop {
                let snapshot: ScopeTaskSnapshot = serde_json::from_value(
                    Self::workspace_scope_snapshot(&peer, meta.clone()).await?,
                )
                .map_err(|error| format!("invalid workspace task snapshot: {error}"))?;
                let pending = pending_tasks_for_closed_epoch(&server, initial.epoch, snapshot)?;
                if pending.is_empty() {
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
    /// the durable closing intent. Epoch CAS fences delayed taskClose replays.
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

    /// Rebuild the disposable Agent projection from the Workspace owner.
    pub async fn recover_workspace_tasks(self: &Arc<Self>, session_id: &str) -> Result<(), String> {
        let clients = self.workspace_task_clients();
        if clients.is_empty() {
            return Err("workspace task owner unavailable".into());
        }
        for client in clients {
            let meta = self
                .task_scope_meta_for(&client.name, session_id)
                .ok_or_else(|| "workspace task scope unavailable".to_owned())?;
            let peer = client
                .peer
                .clone()
                .ok_or_else(|| "workspace disconnected".to_owned())?;
            let snapshot: ScopeTaskSnapshot =
                serde_json::from_value(Self::workspace_scope_snapshot(&peer, meta).await?)
                    .map_err(|error| format!("invalid workspace task snapshot: {error}"))?;
            for row in snapshot.tasks {
                self.apply_scope_row(session_id, &client, &peer, row)
                    .await?;
            }
            self.resolve_execution_evidence(session_id, &client.name);
        }
        Ok(())
    }
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
                let Some(manager) = pool
                    .session_tasks
                    .read()
                    .get(&session_id)
                    .cloned()
                else {
                    break;
                };
                let mut params = GetTaskParams::new(&raw_task_id);
                if is_workspace_shell {
                    params.meta = pool.task_scope_meta_for(&server, &session_id);
                }
                match peer.get_task(params).await {
                    Ok(snapshot) if snapshot.task.status().is_terminal() => {
                        let transition_id = if is_workspace_shell {
                            match pool.task_scope_meta_for(&server, &session_id) {
                                Some(meta) => Self::workspace_scope_snapshot(&peer, meta)
                                    .await
                                    .ok()
                                    .and_then(|value| {
                                        value
                                            .get("tasks")?
                                            .as_array()?
                                            .iter()
                                            .find(|row| {
                                                row.pointer("/task/taskId")
                                                    .and_then(serde_json::Value::as_str)
                                                    == Some(raw_task_id.as_str())
                                            })
                                            .and_then(|row| serde_json::from_value::<ScopeTaskRow>(row.clone()).ok())
                                            .and_then(|row| {
                                                row.initiator_for_scope(&session_id).ok()?;
                                                row.terminal_transition_id
                                            })
                                    }),
                                None => None,
                            }
                        } else {
                            Some(format!("{raw_task_id}:terminal"))
                        };
                        let result = match transition_id.as_deref() {
                            Some(transition_id) => {
                                pool.deliver_managed_task_status(
                                    &server,
                                    &task_id,
                                    transition_id,
                                    &snapshot.task,
                                    is_workspace_shell,
                                    manager.as_ref(),
                                )
                                .await
                            }
                            None => Err("workspace terminal transition ID unavailable".to_owned()),
                        };
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
                drop(manager);
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

#[cfg(test)]
#[path = "subscription_task_recovery_test.rs"]
mod task_recovery_tests;
