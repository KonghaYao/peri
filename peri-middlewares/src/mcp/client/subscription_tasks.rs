use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use peri_acp_types::event::BackgroundTaskResult;
use peri_acp_types::monitor::monitor_terminal_alert_note;
use peri_acp_types::shell_task::{ShellTaskResultMeta, SHELL_TASK_RESULT_META_KEY};
use peri_acp_types::tasks::{TaskManager, TaskRegistrationTicket};
use rmcp::{
    model::{DetailedTask, GetTaskParams, MetaObject, TaskPayload},
    service::{Peer, RoleClient},
};

use super::super::monitor_alert::{
    parse_monitor_meta, push_monitor_alert, MonitorAlertAction, MonitorAlertState,
};
use super::super::McpClientPool;

/// One `tasks/get` observation: the protocol task plus its result metadata.
///
/// The metadata is retained with the terminal cache so a terminal retry keeps
/// the final alert projection instead of re-fetching it.
#[derive(Clone)]
struct ObservedTask {
    task: DetailedTask,
    meta: Option<MetaObject>,
}

/// After this many consecutive failed status queries, attempt to settle an
/// unobservable owner as unresolved. Settlement itself can still fail, and a
/// known terminal result must never be replaced by an unresolved reminder.
const LOST_ABANDON_ATTEMPTS: usize = 300;

/// A terminal payload that cannot be validated is never cached as evidence.
/// Re-query a few times so a repaired owner snapshot can recover, then settle
/// explicitly unresolved rather than holding session shutdown forever.
const INVALID_TERMINAL_ATTEMPTS: usize = 3;

/// One settlement budget shared by every external task owned by a closing
/// session. Per-task waits consume the remaining budget; they never add up.
const EXTERNAL_TASK_CLOSE_BUDGET: Duration = Duration::from_secs(20);

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
        let deadline = peri_time::monotonic_now() + EXTERNAL_TASK_CLOSE_BUDGET;
        for task_id in manager.external_task_ids() {
            let remaining = deadline.saturating_duration_since(peri_time::monotonic_now());
            if remaining.is_zero() {
                failures.push(format!("{task_id}: cancellation budget exhausted"));
                continue;
            }
            match peri_time::timeout(remaining, manager.cancel_async(&task_id)).await {
                Ok(Ok(())) => {}
                Ok(Err(err)) => failures.push(format!("{task_id}: {err}")),
                Err(_) => failures.push(format!("{task_id}: cancellation timed out")),
            }
        }
        while manager.has_unsettled_external() {
            let remaining = deadline.saturating_duration_since(peri_time::monotonic_now());
            if remaining.is_zero() {
                failures.push("external task close incomplete".into());
                break;
            }
            peri_time::sleep(remaining.min(Duration::from_millis(200))).await;
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
        ticket: TaskRegistrationTicket,
        owner_scoped: bool,
        is_workspace_shell: bool,
        initial_peer: Peer<RoleClient>,
    ) -> Result<(), crate::mcp::task_scope::TaskAdmissionError> {
        let task_id = ticket.task_id().to_owned();
        let weak_pool = Arc::downgrade(self);
        let manager = self.session_bindings.read().manager(&session_id);
        let key = crate::mcp::McpTaskKey::TaskStatus {
            server: server.clone(),
            task_id: task_id.clone(),
        };
        self.task_spawner.spawn(key, async move {
            let mut peer = initial_peer;
            let mut lost_owner_attempts: usize = 0;
            let mut invalid_terminal_attempts: usize = 0;
            let mut invalid_terminal_reason: Option<String> = None;
            let mut terminal_delivery_failures: usize = 0;
            let mut known_terminal: Option<ObservedTask> = None;
            let mut monitor_alerts = MonitorAlertState::default();
            let mut monitor_meta_warned = false;
            let mut terminal_note: Option<String> = None;
            loop {
                let Some(pool) = weak_pool.upgrade() else {
                    break;
                };
                let Some(manager) = manager.as_ref() else {
                    break;
                };
                if invalid_terminal_reason.is_none() {
                    let mut params = GetTaskParams::new(&raw_task_id);
                    if owner_scoped {
                        params.meta = pool.task_owner_meta_for(&server, &session_id);
                    }
                    let observed = match known_terminal.as_ref() {
                        Some(observed) => Ok(observed.clone()),
                        None => peer.get_task(params).await.map(|snapshot| ObservedTask {
                            task: snapshot.task,
                            meta: snapshot.meta,
                        }),
                    };
                    match observed {
                        Ok(observed) if observed.task.status().is_terminal() => {
                            let mapped = if observed.task.task.task_id != raw_task_id {
                                Err(format!(
                                    "task result identity mismatch: requested {raw_task_id}, received {}",
                                    observed.task.task.task_id
                                ))
                            } else {
                                Self::map_task_result(
                                    &server,
                                    &task_id,
                                    &observed.task,
                                    observed.meta.as_ref(),
                                    is_workspace_shell,
                                )
                            };
                            match mapped {
                                Err(error) => {
                                    manager.mark_external_lost(&task_id);
                                    invalid_terminal_attempts = invalid_terminal_attempts.saturating_add(1);
                                    tracing::warn!(server = %server, session_id = %session_id,
                                        task_id = %raw_task_id, invalid_terminal_attempts, %error,
                                        "MCP task terminal evidence invalid; fetching a fresh snapshot");
                                    if invalid_terminal_attempts >= INVALID_TERMINAL_ATTEMPTS {
                                        invalid_terminal_reason = Some(format!(
                                            "task terminal evidence invalid after {invalid_terminal_attempts} observations: {error}"
                                        ));
                                    }
                                }
                                Ok(mapped) => {
                                    // A terminal task wakes its initiator exactly once.
                                    // Hits without an earlier alert reminder are merged
                                    // into that reminder, so the terminal round never
                                    // pushes a second alert. The note is cached with the
                                    // terminal observation: a settlement retry reuses the
                                    // same terminal delivery id and body.
                                    let note = if known_terminal.is_some() {
                                        terminal_note.clone()
                                    } else {
                                        let note = pool.consume_monitor_terminal_alert(
                                            &session_id,
                                            &server,
                                            &raw_task_id,
                                            observed.meta.as_ref(),
                                            &mut monitor_alerts,
                                            &mut monitor_meta_warned,
                                        );
                                        terminal_note = note.clone();
                                        note
                                    };
                                    known_terminal = Some(observed.clone());
                                    let transition_id = format!("{raw_task_id}:terminal");
                                    let result = pool
                                        .settle_managed_task_result(
                                            &ticket,
                                            &transition_id,
                                            mapped,
                                            note.as_deref(),
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
                            }
                        }
                        Ok(observed) => {
                            pool.observe_monitor_alert(
                                &session_id,
                                &server,
                                &raw_task_id,
                                observed.meta.as_ref(),
                                &mut monitor_alerts,
                                &mut monitor_meta_warned,
                            )
                            .await;
                            lost_owner_attempts = 0;
                            manager.mark_external_running(&task_id);
                        }
                        Err(error) => {
                            manager.mark_external_lost(&task_id);
                            if lost_owner_retry_due(&mut lost_owner_attempts) {
                                match manager
                                    .abandon_external(
                                        &ticket,
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
                }
                if let Some(reason) = invalid_terminal_reason.as_ref() {
                    match manager.abandon_external(&ticket, reason).await {
                        Ok(committed) if committed || ticket.is_settled() => break,
                        Ok(_) => {
                            tracing::warn!(server = %server, session_id = %session_id,
                                task_id = %raw_task_id,
                                "MCP task invalid terminal abandonment not yet committed");
                        }
                        Err(error) => {
                            tracing::warn!(server = %server, session_id = %session_id,
                                task_id = %raw_task_id, %error,
                                "MCP task invalid terminal abandonment pending reconciliation");
                        }
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

    /// Apply one running-task monitor alert observation to the per-task
    /// delivery state.
    ///
    /// A missing or malformed `tasks/get` projection warns once per task, since
    /// the observation cannot be turned into a delivery. That is the whole
    /// failure mode: the caller settles terminal tasks regardless, so a server
    /// that merely matches the monitor identity by name can never keep a task
    /// unsettled. Non-terminal observations are retried by the next poll, which
    /// keeps the processed count untouched.
    async fn observe_monitor_alert(
        &self,
        session_id: &str,
        server: &str,
        raw_task_id: &str,
        meta: Option<&MetaObject>,
        state: &mut MonitorAlertState,
        warned_meta: &mut bool,
    ) {
        if !self.is_monitor_server(server) {
            return;
        }
        let meta = match parse_monitor_meta(meta) {
            Ok(meta) => meta,
            Err(reason) => {
                if !*warned_meta {
                    *warned_meta = true;
                    tracing::warn!(
                        server = %server,
                        session_id = %session_id,
                        task_id = %raw_task_id,
                        reason,
                        "monitor alert projection unavailable; skipping reminder delivery"
                    );
                }
                return;
            }
        };
        let now = Instant::now();
        let has_inbox = self.owner_online(session_id);
        match state.decide(meta.alert_matched, now, has_inbox) {
            MonitorAlertAction::Nothing => {}
            MonitorAlertAction::Send { hits } => {
                push_monitor_alert(self, session_id, raw_task_id, &meta, hits).await;
            }
            MonitorAlertAction::DropOffline { hits } => {
                tracing::warn!(
                    server = %server,
                    session_id = %session_id,
                    task_id = %raw_task_id,
                    hits,
                    "monitor alert dropped: initiating session has no inbox"
                );
            }
            MonitorAlertAction::Suppressed => {
                tracing::debug!(
                    server = %server,
                    session_id = %session_id,
                    task_id = %raw_task_id,
                    hits = meta.alert_matched,
                    "monitor alert suppressed by the merge window"
                );
            }
        }
    }

    /// Build the alert summary merged into a terminal monitor reminder.
    ///
    /// The terminal reminder is the only wake for a finished task, so the
    /// unreported count is consumed here rather than delivered as a second
    /// alert. A missing or malformed projection only drops the counts (warned
    /// once per task); settlement never depends on the projection. Non-monitor
    /// servers return `None` without parsing.
    fn consume_monitor_terminal_alert(
        &self,
        session_id: &str,
        server: &str,
        raw_task_id: &str,
        meta: Option<&MetaObject>,
        state: &mut MonitorAlertState,
        warned_meta: &mut bool,
    ) -> Option<String> {
        if !self.is_monitor_server(server) {
            return None;
        }
        let meta = match parse_monitor_meta(meta) {
            Ok(meta) => meta,
            Err(reason) => {
                if !*warned_meta {
                    *warned_meta = true;
                    tracing::warn!(
                        server = %server,
                        session_id = %session_id,
                        task_id = %raw_task_id,
                        reason,
                        "monitor alert projection unavailable; terminal reminder omits alert counts"
                    );
                }
                return None;
            }
        };
        let unreported = state.take_unreported(meta.alert_matched);
        monitor_terminal_alert_note(
            raw_task_id,
            meta.alert_matched,
            unreported,
            meta.last_alert_seq,
            meta.label.as_deref(),
        )
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    async fn deliver_managed_task_status(
        &self,
        server: &str,
        ticket: &TaskRegistrationTicket,
        transition_id: &str,
        task: &DetailedTask,
        meta: Option<&MetaObject>,
        is_workspace_shell: bool,
        terminal_note: Option<&str>,
        manager: &dyn TaskManager,
    ) -> Result<(), String> {
        let task_id = ticket.task_id();
        let result = Self::map_task_result(server, task_id, task, meta, is_workspace_shell)?;
        self.settle_managed_task_result(ticket, transition_id, result, terminal_note, manager)
            .await
    }

    async fn settle_managed_task_result(
        &self,
        ticket: &TaskRegistrationTicket,
        transition_id: &str,
        mut result: BackgroundTaskResult,
        terminal_note: Option<&str>,
        manager: &dyn TaskManager,
    ) -> Result<(), String> {
        let task_id = ticket.task_id();
        if let Some(note) = terminal_note {
            // The note carries alert counts merged into this same terminal
            // wake; a blank line keeps the result statement distinct.
            result.output = format!("{}\n\n{note}", result.output);
        }
        if manager
            .settle_external(ticket, transition_id, result)
            .await?
            || ticket.is_settled()
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
        meta: Option<&MetaObject>,
        is_workspace_shell: bool,
    ) -> Result<BackgroundTaskResult, String> {
        let shell_result = if is_workspace_shell {
            match &task.payload {
                TaskPayload::Completed { result } => {
                    result.get("structuredContent").and_then(|value| {
                        serde_json::from_value::<BackgroundTaskResult>(value.clone()).ok()
                    })
                }
                TaskPayload::Cancelled => {
                    match meta.and_then(|meta| meta.get(SHELL_TASK_RESULT_META_KEY)) {
                        Some(value) => {
                            let evidence: ShellTaskResultMeta = serde_json::from_value(value.clone())
                                .map_err(|error| format!("invalid workspace shell result metadata: {error}"))?;
                            Some(evidence.into_cancelled_result(&task.task.task_id)
                                .map_err(|error| error.to_string())?)
                        }
                        // Older Workspace services speak standard CancelledTask
                        // but cannot prove executor cleanup or provide log files.
                        None => Some(BackgroundTaskResult {
                            task_id: task_id.to_owned(), agent_name: "mcp".into(), prompt_summary: server.to_owned(),
                            success: false,
                            output: "Remote task reported Cancelled without executor evidence. Its execution outcome is abandoned as unresolved: process cleanup, exit status, and output file references remain unconfirmed.".into(),
                            tool_calls_count: 0, duration_ms: 0, timed_out: false,
                            child_thread_id: None, subagent_failure: None, shell_output: None,
                        }),
                    }
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
        Ok(result)
    }
}

#[path = "subscription_task_binding.rs"]
mod binding;

#[cfg(test)]
#[path = "subscription_tasks_test.rs"]
mod tests;

#[cfg(test)]
#[path = "subscription_shell_result_test.rs"]
mod shell_result_tests;

// 该组用例用真实 POSIX shell 取证并精确断言换行；Windows 下由 PowerShell 执行不成立。
#[cfg(test)]
#[cfg(unix)]
#[path = "subscription_shell_validation_test.rs"]
mod shell_validation_tests;

#[cfg(test)]
#[path = "monitor_subscription_test.rs"]
mod monitor_subscription_tests;
