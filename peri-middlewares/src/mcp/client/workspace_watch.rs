//! Recovery-side discovery of Workspace Shell tasks that outlived the client.
//!
//! Design `session-durability` §6: Peri does not restore MCP services or old
//! connections. It only adopts tasks the peripheral still reports as live and
//! then observes them through the existing `tasks/get` subscription, so a
//! terminal result settles through the normal `settle_external` → owner inbox
//! path. There is deliberately no protocol-level `subscriptions/listen` here.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use peri_acp_types::tasks::BgTaskKind;
use tokio_util::sync::CancellationToken;

use crate::mcp::task_scope::TaskAdmissionError;

use super::McpClientPool;

/// The configured instance name of the trusted Workspace owner.
const WORKSPACE_INSTANCE: &str = "workspace";

/// Discovery cadence. The first scan runs immediately; later scans only retry
/// while the workspace connection is unavailable. Normal runtime tasks are
/// registered by the tool bridge and are not adopted (see below).
fn watch_interval() -> Duration {
    #[cfg(test)]
    {
        // Keep the periodic-retry behavior testable without multi-second waits.
        Duration::from_millis(50)
    }
    #[cfg(not(test))]
    {
        Duration::from_secs(5)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdoptionOutcome {
    Adopted,
    AlreadyMonitored,
}

impl McpClientPool {
    /// Discover live Workspace Shell tasks for this session and subscribe them.
    ///
    /// Idempotent by remote task id: repeated scans and repeated watchers either
    /// see the id in `adopted` or get the registry's `DuplicateTask` fact, which
    /// means the current process already monitors it. The watcher never sends
    /// `tasks/cancel`; stopping it leaves peripheral work untouched.
    pub(super) async fn watch_workspace_tasks(
        self: Arc<Self>,
        session_id: &str,
        cancel: CancellationToken,
    ) {
        // Owner == session id. ACP currently treats the session and thread
        // identities as the same value; if they ever diverge, recovery must
        // pass the real thread id instead (see `task_owner_metadata`).
        let cutoff = peri_time::now_utc_rfc3339();
        let mut adopted = HashSet::new();
        let mut warned_unknown = false;
        loop {
            if cancel.is_cancelled() {
                break;
            }
            if self.clients.read().contains_key(WORKSPACE_INSTANCE) {
                match self
                    .adopt_workspace_tasks_before(session_id, &cutoff, &mut adopted, &cancel)
                    .await
                {
                    Some(detail) => {
                        if !warned_unknown {
                            warned_unknown = true;
                            tracing::warn!(
                                session_id,
                                %detail,
                                "workspace shell task discovery unavailable; nothing adopted this round"
                            );
                        }
                    }
                    None => warned_unknown = false,
                }
            } else {
                // Sessions without a Workspace instance simply have nothing to
                // adopt; a late-registered connection is retried next round.
                tracing::debug!(
                    session_id,
                    "workspace instance is not registered; shell task discovery idle"
                );
            }
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = peri_time::sleep(watch_interval()) => {}
            }
        }
    }

    /// Adopt tasks whose owner creation timestamp predates this watcher.
    ///
    /// The cutoff is the admission rule that keeps recovery and normal runtime
    /// apart: a task started after this process is being registered by the tool
    /// bridge that started it, and adopting it here would race that registration
    /// (the bridge would then lose to `DuplicateTask`).
    ///
    /// Returns `Some(detail)` when this round's query could not be answered
    /// (for warn-once), `None` otherwise.
    async fn adopt_workspace_tasks_before(
        self: &Arc<Self>,
        session_id: &str,
        cutoff: &str,
        adopted: &mut HashSet<String>,
        cancel: &CancellationToken,
    ) -> Option<String> {
        let tasks = match self.workspace_shell_tasks(session_id).await {
            Ok(tasks) => tasks,
            Err(detail) => {
                // Unknown is not an empty list: deliver nothing and retry.
                return Some(detail);
            }
        };
        for task in tasks {
            if cancel.is_cancelled() {
                return None;
            }
            if adopted.contains(&task.task_id) {
                continue;
            }
            if !started_before(task.started_at.as_deref(), cutoff) {
                tracing::debug!(
                    session_id,
                    task_id = %task.task_id,
                    started_at = ?task.started_at,
                    "workspace shell task started after this watcher; normal registration owns it"
                );
                continue;
            }
            match self
                .adopt_workspace_shell_task(session_id, &task.task_id, task.started_at.as_deref())
                .await
            {
                Ok(AdoptionOutcome::Adopted) => {
                    adopted.insert(task.task_id.clone());
                    tracing::info!(
                        session_id,
                        task_id = %task.task_id,
                        "adopted live workspace shell task after session recovery"
                    );
                }
                Ok(AdoptionOutcome::AlreadyMonitored) => {
                    adopted.insert(task.task_id.clone());
                    tracing::debug!(
                        session_id,
                        task_id = %task.task_id,
                        "workspace shell task already monitored; duplicate watch ignored"
                    );
                }
                Err(error) => {
                    // Leave the id unmarked so a later scan can retry the
                    // transient failure (manager not bound yet, admission closed).
                    tracing::warn!(
                        session_id,
                        task_id = %task.task_id,
                        %error,
                        "workspace shell task adoption failed; retrying next scan"
                    );
                }
            }
        }
        None
    }

    async fn adopt_workspace_shell_task(
        self: &Arc<Self>,
        session_id: &str,
        raw_task_id: &str,
        started_at: Option<&str>,
    ) -> Result<AdoptionOutcome, String> {
        let peer = self
            .clients
            .read()
            .get(WORKSPACE_INSTANCE)
            .and_then(|client| client.peer.clone())
            .ok_or_else(|| "workspace shell task owner is not connected".to_owned())?;
        let owner_scoped = self
            .task_owner_meta_for(WORKSPACE_INSTANCE, session_id)
            .is_some();
        let started_at = started_at
            .map(str::to_owned)
            .unwrap_or_else(peri_time::now_utc_rfc3339);
        let summary = format!("Workspace shell task {raw_task_id}");
        let (manager, registration) = self.external_task_registration(
            session_id,
            Some(session_id),
            None,
            WORKSPACE_INSTANCE,
            raw_task_id,
            BgTaskKind::Shell,
            &summary,
            owner_scoped,
            &started_at,
        )?;
        let ticket = match manager.register_external(registration) {
            Ok(ticket) => ticket,
            Err(error) if is_duplicate_task(&error) => {
                return Ok(AdoptionOutcome::AlreadyMonitored)
            }
            Err(error) => return Err(error),
        };
        match self.spawn_managed_task_subscription(
            WORKSPACE_INSTANCE.to_owned(),
            session_id.to_owned(),
            raw_task_id.to_owned(),
            ticket,
            owner_scoped,
            true,
            peer,
        ) {
            Ok(()) => Ok(AdoptionOutcome::Adopted),
            Err(TaskAdmissionError::DuplicateKey) => Ok(AdoptionOutcome::AlreadyMonitored),
            Err(TaskAdmissionError::OwnerClosed) => {
                Err("MCP task owner closed before subscription admission".to_owned())
            }
        }
    }
}

/// Recovery may only adopt tasks that existed before this watcher started.
///
/// A missing or unreadable timestamp is not enough evidence to adopt: the
/// normal tool-call registration remains authoritative for that task.
fn started_before(started_at: Option<&str>, cutoff: &str) -> bool {
    let (Some(started_at), Ok(cutoff)) = (started_at, chrono::DateTime::parse_from_rfc3339(cutoff))
    else {
        return false;
    };
    chrono::DateTime::parse_from_rfc3339(started_at).is_ok_and(|started| started < cutoff)
}

/// `TaskManager::register_external` flattens the registry's typed
/// `DuplicateTask` to its display text. That text is the only cross-layer fact
/// available here; it means "this source identity is already registered".
fn is_duplicate_task(error: &str) -> bool {
    error.starts_with("Task ") && error.ends_with(" already exists")
}

#[cfg(test)]
#[path = "workspace_watch_test.rs"]
mod tests;
