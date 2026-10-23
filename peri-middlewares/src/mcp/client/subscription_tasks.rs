use std::{sync::Arc, time::Duration};

use peri_acp_types::event::BackgroundTaskResult;
use peri_acp_types::tasks::TaskManager;
use rmcp::{
    model::{DetailedTask, GetTaskParams, TaskPayload},
    service::{Peer, RoleClient},
};

use super::super::McpClientPool;

/// After this many consecutive failed status queries, attempt to settle an
/// unobservable owner as unresolved. Settlement itself can still fail, and a
/// known terminal result must never be replaced by an unresolved reminder.
const LOST_ABANDON_ATTEMPTS: usize = 300;

fn lost_owner_retry_due(attempts: &mut usize) -> bool {
    *attempts = attempts.saturating_add(1);
    *attempts >= LOST_ABANDON_ATTEMPTS
}

impl McpClientPool {
    /// Cancel every external task the session TaskManager knows about and wait
    /// for their processes to settle.
    ///
    /// There is no snapshot/close barrier: unused owner state cannot be
    /// discovered, and a session only owns the tasks its runtime registered.
    pub async fn cancel_session_external_tasks(&self, session_id: &str) -> Result<(), String> {
        let manager = self.session_bindings.read().manager(session_id);
        let Some(manager) = manager else {
            return Ok(());
        };
        let mut failures = Vec::new();
        for task_id in manager.external_task_ids() {
            if let Err(err) = manager.cancel_async(&task_id).await {
                failures.push(format!("{task_id}: {err}"));
            }
        }
        let until = peri_time::monotonic_now() + Duration::from_secs(20);
        while manager.has_unsettled_external() {
            if peri_time::monotonic_now() >= until {
                failures.push("external task close incomplete".into());
                break;
            }
            peri_time::sleep(Duration::from_millis(200)).await;
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
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
            let mut lost_owner_attempts: usize = 0;
            let mut terminal_delivery_failures: usize = 0;
            let mut known_terminal: Option<DetailedTask> = None;
            loop {
                let Some(pool) = weak_pool.upgrade() else {
                    break;
                };
                let Some(manager) = manager.as_ref() else {
                    break;
                };
                let mut params = GetTaskParams::new(&raw_task_id);
                if is_workspace_shell {
                    params.meta = pool.task_owner_meta_for(&server, &session_id);
                }
                let observed = match known_terminal.as_ref() {
                    Some(task) => Ok(task.clone()),
                    None => peer.get_task(params).await.map(|snapshot| snapshot.task),
                };
                match observed {
                    Ok(task) if task.status().is_terminal() => {
                        known_terminal = Some(task.clone());
                        let transition_id = format!("{raw_task_id}:terminal");
                        let result = pool
                            .deliver_managed_task_status(
                                &server,
                                &task_id,
                                &transition_id,
                                &task,
                                is_workspace_shell,
                                manager.as_ref(),
                            )
                            .await;
                        if let Err(error) = result {
                            terminal_delivery_failures =
                                terminal_delivery_failures.saturating_add(1);
                            tracing::warn!(server = %server, session_id = %session_id,
                                task_id = %raw_task_id, terminal_delivery_failures, %error,
                                "MCP task terminal delivery pending reconciliation");
                        } else {
                            break;
                        }
                    }
                    Ok(_) => {
                        lost_owner_attempts = 0;
                        manager.mark_external_running(&task_id);
                    }
                    Err(error) => {
                        manager.mark_external_lost(&task_id);
                        if lost_owner_retry_due(&mut lost_owner_attempts) {
                            match manager
                                .abandon_external(
                                    &task_id,
                                    &format!("task status unavailable: {error}"),
                                )
                                .await
                            {
                                Ok(true) => break,
                                Ok(false) => {
                                    tracing::warn!(server = %server, session_id = %session_id,
                                        task_id = %raw_task_id, lost_owner_attempts,
                                        "MCP task abandon not yet committed");
                                }
                                Err(abandon_error) => {
                                    tracing::warn!(server = %server, session_id = %session_id,
                                        task_id = %raw_task_id, lost_owner_attempts,
                                        %abandon_error, "MCP task abandon pending reconciliation");
                                }
                            }
                        }
                        tracing::warn!(server = %server, session_id = %session_id,
                            task_id = %raw_task_id, lost_owner_attempts, %error,
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
mod tests;
