//! Workspace-owned Bash task state exposed through the MCP Tasks extension.
//!
//! The legacy shell executor still implements process execution. This owner
//! deliberately has no session callback: completion stays with the Workspace
//! service and can be queried after a client disconnects.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
    time::Duration,
};

use parking_lot::Mutex;
use peri_acp_types::{
    event::BackgroundTaskResult,
    tasks::{BgShellHandle, BgTaskKind, OnBgCompleteFn, TaskManager, TaskShutdownReport},
};
use rmcp::{
    model::{
        CallToolResult, ContentBlock, DetailedTask, GetTaskResult, Task, TaskPayload, TaskStatus,
    },
    ErrorData as McpError,
};
use serde::Serialize;

use crate::task_scope::ExecutionGeneration;

const CHANGE_LIMIT: usize = 512;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ScopedTask {
    pub task: DetailedTask,
    pub summary: String,
    pub revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_transition_id: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ScopeSnapshot {
    pub cursor: u64,
    pub epoch: u64,
    pub closing: bool,
    pub tasks: Vec<ScopedTask>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ScopeChanges {
    pub cursor: u64,
    pub changes: Vec<ScopedTask>,
}

#[derive(Serialize)]
pub(crate) struct ScopeOpened {
    pub cursor: u64,
    pub epoch: u64,
}

#[derive(Default)]
struct ShellState {
    records: HashMap<String, DetailedTask>,
    scopes: HashMap<String, String>,
    summaries: HashMap<String, String>,
    revisions: HashMap<String, u64>,
    terminal_ids: HashMap<String, String>,
    changes: VecDeque<(u64, String, ScopedTask)>,
    cursor: u64,
    closing: HashSet<String>,
    epochs: HashMap<String, u64>,
    execution_floor: HashMap<String, ExecutionGeneration>,
    fencing: HashSet<String>,
    inflight: HashMap<String, usize>,
    cancel_pending: HashSet<String>,
    cancel_accepted: HashSet<String>,
    pending_results: HashMap<String, BackgroundTaskResult>,
}

impl ShellState {
    fn check_execution(
        &self,
        scope: &str,
        generation: Option<&ExecutionGeneration>,
    ) -> Result<(), McpError> {
        match self.execution_floor.get(scope) {
            Some(floor) if generation == Some(floor) => Ok(()),
            None if generation.is_none() => Ok(()),
            None => Err(McpError::invalid_params(
                "task execution owner is not fenced",
                None,
            )),
            Some(_) => Err(McpError::invalid_params("stale task execution owner", None)),
        }
    }

    fn publish(&mut self, task_id: &str) -> Option<DetailedTask> {
        let task = self.records.get(task_id)?.clone();
        let scope = self.scopes.get(task_id)?.clone();
        let revision = self.revisions.entry(task_id.into()).or_default();
        *revision += 1;
        if task.task.status.is_terminal() {
            self.terminal_ids
                .entry(task_id.into())
                .or_insert_with(|| format!("{task_id}:{revision}"));
        }
        self.cursor += 1;
        self.changes.push_back((
            self.cursor,
            scope,
            ScopedTask {
                task: task.clone(),
                summary: self.summaries.get(task_id).cloned().unwrap_or_default(),
                revision: *revision,
                terminal_transition_id: self.terminal_ids.get(task_id).cloned(),
            },
        ));
        while self.changes.len() > CHANGE_LIMIT {
            self.changes.pop_front();
        }
        Some(task)
    }

    fn scoped(&self, id: &str, scope: &str) -> bool {
        self.scopes
            .get(id)
            .is_some_and(|existing| existing == scope)
    }
}

#[derive(Clone)]
pub(crate) struct ShellTasks {
    manager: Arc<dyn TaskManager>,
    state: Arc<Mutex<ShellState>>,
    changed: Arc<tokio::sync::Notify>,
    updates: tokio::sync::broadcast::Sender<DetailedTask>,
    owner_references: Arc<()>,
    #[cfg(test)]
    spawn_gate: Option<Arc<tokio::sync::Notify>>,
}

impl ShellTasks {
    pub(crate) fn new() -> Self {
        let (updates, _) = tokio::sync::broadcast::channel(64);
        Self {
            manager: Arc::new(peri_mcp_common::create_local_task_manager()),
            state: Arc::new(Mutex::new(ShellState::default())),
            changed: Arc::new(tokio::sync::Notify::new()),
            updates,
            owner_references: Arc::new(()),
            #[cfg(test)]
            spawn_gate: None,
        }
    }

    pub(crate) fn subscribe(&self) -> tokio::sync::broadcast::Receiver<DetailedTask> {
        self.updates.subscribe()
    }

    pub(crate) fn manager(&self) -> Arc<dyn TaskManager> {
        Arc::clone(&self.manager)
    }

    pub(crate) fn check_execution(
        &self,
        scope: &str,
        generation: Option<&ExecutionGeneration>,
    ) -> Result<(), McpError> {
        self.state.lock().check_execution(scope, generation)
    }

    /// Advance the Store execution owner floor before admitting its tool work.
    /// Earlier admitted creations remain counted until their records exist.
    pub(crate) async fn fence_execution(
        &self,
        scope: &str,
        generation: &ExecutionGeneration,
    ) -> Result<u64, McpError> {
        // The caller may disconnect while earlier launches are still being
        // registered. Keep the floor transition and barrier with the owner.
        let owner = self.clone();
        let scope = scope.to_owned();
        let generation = generation.clone();
        let (reply, receive) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let result = owner.fence_execution_owned(&scope, &generation).await;
            let _ = reply.send(result);
        });
        receive
            .await
            .map_err(|_| McpError::internal_error("task fence owner stopped", None))?
    }

    async fn fence_execution_owned(
        &self,
        scope: &str,
        generation: &ExecutionGeneration,
    ) -> Result<u64, McpError> {
        {
            let mut state = self.state.lock();
            if let Some(current) = state.execution_floor.get(scope) {
                if generation.epoch < current.epoch
                    || (generation.epoch == current.epoch && generation.nonce != current.nonce)
                {
                    return Err(McpError::invalid_params("stale task execution owner", None));
                }
            }
            if state.execution_floor.get(scope) != Some(generation) {
                state
                    .execution_floor
                    .insert(scope.into(), generation.clone());
                state.fencing.insert(scope.into());
            }
        }
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut state = self.state.lock();
                state.check_execution(scope, Some(generation))?;
                if !state.fencing.contains(scope) {
                    return Ok(state.cursor);
                }
                if state.inflight.get(scope).copied().unwrap_or(0) == 0 {
                    state.fencing.remove(scope);
                    self.changed.notify_waiters();
                    return Ok(state.cursor);
                }
            }
            notified.await;
        }
    }

    pub(crate) fn completion_callback(&self) -> OnBgCompleteFn {
        self.completion_callback_for(None, None)
    }

    pub(crate) fn completion_callback_for(
        &self,
        scope: Option<String>,
        summary: Option<String>,
    ) -> OnBgCompleteFn {
        let state = Arc::clone(&self.state);
        let updates = self.updates.clone();
        let changed = Arc::clone(&self.changed);
        Arc::new(move |result: &BackgroundTaskResult, kind: BgTaskKind| {
            if kind != BgTaskKind::Shell {
                return;
            }
            let mut state = state.lock();
            if let Some(scope) = &scope {
                state
                    .scopes
                    .entry(result.task_id.clone())
                    .or_insert_with(|| scope.clone());
            }
            if let Some(summary) = &summary {
                state
                    .summaries
                    .entry(result.task_id.clone())
                    .or_insert_with(|| summary.clone());
            }
            if state.cancel_pending.contains(&result.task_id) {
                state
                    .pending_results
                    .insert(result.task_id.clone(), result.clone());
                return;
            }
            let cancelled = state.cancel_accepted.remove(&result.task_id);
            let record = state
                .records
                .entry(result.task_id.clone())
                .or_insert_with(|| {
                    let now = chrono::Utc::now().to_rfc3339();
                    DetailedTask::new(
                        Task::new(&result.task_id, TaskStatus::Working, &now, &now),
                        TaskPayload::Working,
                    )
                });
            if record.task.status.is_terminal() {
                return;
            }
            record.task.last_updated_at = chrono::Utc::now().to_rfc3339();
            let text = if cancelled {
                "Background shell cancellation completed."
            } else if result.success {
                "Background shell command completed."
            } else {
                "Background shell command failed."
            };
            record.task.status_message = Some(text.into());
            let mut output = CallToolResult::success(vec![ContentBlock::text(format!(
                "{text}\n{}",
                result.output
            ))]);
            output.is_error = Some(!result.success);
            output.structured_content = serde_json::to_value(result).ok();
            let result_value = serde_json::to_value(output)
                .ok()
                .and_then(|value| value.as_object().cloned())
                .unwrap_or_default();
            if cancelled {
                record.payload = TaskPayload::Cancelled;
                record.task.status = TaskStatus::Cancelled;
            } else {
                record.payload = TaskPayload::Completed {
                    result: result_value,
                };
                record.task.status = TaskStatus::Completed;
            }
            let task = state.publish(&result.task_id).unwrap_or_else(|| {
                state
                    .records
                    .get(&result.task_id)
                    .expect("completed shell record")
                    .clone()
            });
            let _ = updates.send(task);
            changed.notify_waiters();
        })
    }

    #[cfg(test)]
    pub(crate) fn admit(&self, scope: &str) -> Result<ScopeAdmission, McpError> {
        self.admit_fenced(scope, None)
    }

    pub(crate) fn admit_fenced(
        &self,
        scope: &str,
        generation: Option<&ExecutionGeneration>,
    ) -> Result<ScopeAdmission, McpError> {
        let mut state = self.state.lock();
        state.check_execution(scope, generation)?;
        if state.closing.contains(scope) || state.fencing.contains(scope) {
            return Err(McpError::invalid_params("task scope is closing", None));
        }
        *state.inflight.entry(scope.into()).or_default() += 1;
        Ok(ScopeAdmission {
            owner: self.clone(),
            scope: scope.into(),
        })
    }

    #[cfg(test)]
    pub(crate) async fn spawn(
        &self,
        command: String,
        cwd: String,
        timeout_ms: Option<u64>,
    ) -> Result<Task, McpError> {
        self.spawn_scoped(command, cwd, timeout_ms, None, None)
            .await
    }

    pub(crate) async fn spawn_scoped(
        &self,
        command: String,
        cwd: String,
        timeout_ms: Option<u64>,
        scope: Option<&str>,
        generation: Option<&ExecutionGeneration>,
    ) -> Result<Task, McpError> {
        // The MCP request may be cancelled while spawn_blocking is running.
        // Admission and registration must outlive that request future so a
        // closing barrier cannot miss a process already accepted by the owner.
        let admission = scope
            .map(|scope| self.admit_fenced(scope, generation))
            .transpose()?;
        let owner = self.clone();
        let scope = scope.map(str::to_owned);
        let (reply, receive) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let _admission = admission;
            let result = owner
                .spawn_scoped_owned(command, cwd, timeout_ms, scope.as_deref())
                .await;
            let _ = reply.send(result);
        });
        receive.await.map_err(|_| {
            McpError::internal_error("shell task owner stopped before registration", None)
        })?
    }

    async fn spawn_scoped_owned(
        &self,
        command: String,
        cwd: String,
        timeout_ms: Option<u64>,
        scope: Option<&str>,
    ) -> Result<Task, McpError> {
        #[cfg(test)]
        if let Some(gate) = &self.spawn_gate {
            gate.notified().await;
        }
        let manager = self.manager();
        let summary: String = command.chars().take(160).collect();
        let callback =
            self.completion_callback_for(scope.map(str::to_string), Some(summary.clone()));
        let handle: BgShellHandle = tokio::task::spawn_blocking(move || {
            manager.spawn_shell(command, cwd, timeout_ms, Some(callback))
        })
        .await
        .map_err(|_| McpError::internal_error("shell task launch failed", None))?
        .map_err(|_| McpError::internal_error("shell task launch failed", None))?;
        let now = chrono::Utc::now().to_rfc3339();
        let mut record = DetailedTask::new(
            Task::new(&handle.task_id, TaskStatus::Working, &now, &now)
                .with_status_message("Background shell command is running.")
                .with_poll_interval_ms(1000),
            TaskPayload::Working,
        );
        // A fast command can complete before spawn_shell returns. Keep its
        // terminal result rather than overwriting it with a working seed.
        let mut state = self.state.lock();
        if let Some(scope) = scope {
            state.scopes.insert(handle.task_id.clone(), scope.into());
        }
        state
            .summaries
            .entry(handle.task_id.clone())
            .or_insert(summary);
        if let Some(existing) = state.records.get(&handle.task_id) {
            record = existing.clone();
        } else {
            state.records.insert(handle.task_id.clone(), record.clone());
            if let Some(task) = state.publish(&handle.task_id) {
                let _ = self.updates.send(task);
                self.changed.notify_waiters();
            }
        }
        Ok(record.task)
    }

    /// Register a foreground call that the Bash executor already promoted to
    /// its owned background manager. The callback may have won the race.
    pub(crate) fn track_promoted_scoped(
        &self,
        task_id: &str,
        scope: Option<&str>,
        command: Option<&str>,
    ) -> Task {
        let now = chrono::Utc::now().to_rfc3339();
        let record = DetailedTask::new(
            Task::new(task_id, TaskStatus::Working, &now, &now)
                .with_status_message("Foreground timeout promoted to background execution.")
                .with_poll_interval_ms(1000),
            TaskPayload::Working,
        );
        let mut state = self.state.lock();
        if let Some(scope) = scope {
            state.scopes.insert(task_id.into(), scope.into());
        }
        if let Some(command) = command {
            state
                .summaries
                .insert(task_id.into(), command.chars().take(160).collect());
        }
        let inserted = !state.records.contains_key(task_id);
        let task = state
            .records
            .entry(task_id.into())
            .or_insert(record)
            .task
            .clone();
        // The BashTool callback can finish before timeout promotion returns.
        // It has no per-request scope, so publish its retained terminal record
        // as soon as the promoted task is bound to this trusted scope.
        if inserted || (scope.is_some() && !state.revisions.contains_key(task_id)) {
            if let Some(detail) = state.publish(task_id) {
                let _ = self.updates.send(detail);
                self.changed.notify_waiters();
            }
        }
        task
    }

    pub(crate) fn get(&self, task_id: &str) -> Result<GetTaskResult, McpError> {
        self.state
            .lock()
            .records
            .get(task_id)
            .cloned()
            .map(GetTaskResult::new)
            .ok_or_else(|| McpError::invalid_params("unknown task", None))
    }

    pub(crate) fn cancel(&self, task_id: &str) -> Result<(), McpError> {
        self.cancel_inner(task_id, None)
    }

    fn cancel_inner(
        &self,
        task_id: &str,
        scoped: Option<(&str, Option<&ExecutionGeneration>)>,
    ) -> Result<(), McpError> {
        {
            let mut state = self.state.lock();
            if let Some((scope, generation)) = scoped {
                state.check_execution(scope, generation)?;
                if !state.scoped(task_id, scope) {
                    return Err(McpError::invalid_params("unknown task", None));
                }
            }
            let record = state
                .records
                .get(task_id)
                .ok_or_else(|| McpError::invalid_params("unknown task", None))?;
            if record.task.status.is_terminal() {
                return Ok(());
            }
            if state.cancel_accepted.contains(task_id) {
                return Ok(());
            }
            if !state.cancel_pending.insert(task_id.into()) {
                return Err(McpError::internal_error(
                    "shell cancellation already in progress",
                    None,
                ));
            }
        }
        // The manager removes the Agent registry entry and requests process
        // termination. Its return value does not prove the process has exited.
        // A completion callback racing this call is parked until the request
        // outcome is known, then delivered after the executor's cleanup.
        let cancelled = self.manager.cancel(task_id);
        let mut state = self.state.lock();
        state.cancel_pending.remove(task_id);
        let pending = state.pending_results.remove(task_id);
        if cancelled.is_ok() {
            state.cancel_accepted.insert(task_id.into());
            if let Some(record) = state.records.get_mut(task_id) {
                record.task.status_message = Some(
                    "Background shell cancellation requested; waiting for process cleanup.".into(),
                );
                record.task.last_updated_at = chrono::Utc::now().to_rfc3339();
            }
            if let Some(task) = state.publish(task_id) {
                let _ = self.updates.send(task);
                self.changed.notify_waiters();
            }
        }
        drop(state);
        if let Some(result) = pending {
            self.completion_callback()(&result, BgTaskKind::Shell);
        }
        cancelled.map_err(|_| McpError::internal_error("shell task cancellation failed", None))
    }

    pub(crate) fn update(&self, task_id: &str) -> Result<(), McpError> {
        self.state
            .lock()
            .records
            .contains_key(task_id)
            .then_some(())
            .ok_or_else(|| McpError::invalid_params("unknown task", None))
    }

    pub(crate) fn get_scoped(&self, id: &str, scope: &str) -> Result<GetTaskResult, McpError> {
        if !self.state.lock().scoped(id, scope) {
            return Err(McpError::invalid_params("unknown task", None));
        }
        self.get(id)
    }

    pub(crate) fn belongs_to(&self, id: &str, scope: &str) -> bool {
        self.state.lock().scoped(id, scope)
    }

    pub(crate) fn cancel_scoped(
        &self,
        id: &str,
        scope: &str,
        generation: Option<&ExecutionGeneration>,
    ) -> Result<(), McpError> {
        self.cancel_inner(id, Some((scope, generation)))
    }

    pub(crate) fn update_scoped(&self, id: &str, scope: &str) -> Result<(), McpError> {
        if !self.state.lock().scoped(id, scope) {
            return Err(McpError::invalid_params("unknown task", None));
        }
        self.update(id)
    }

    pub(crate) fn snapshot(&self, scope: &str) -> ScopeSnapshot {
        let state = self.state.lock();
        let mut tasks: Vec<ScopedTask> = state
            .scopes
            .iter()
            .filter(|(_, s)| s.as_str() == scope)
            .filter_map(|(id, _)| {
                Some(ScopedTask {
                    task: state.records.get(id)?.clone(),
                    summary: state.summaries.get(id).cloned().unwrap_or_default(),
                    revision: *state.revisions.get(id).unwrap_or(&0),
                    terminal_transition_id: state.terminal_ids.get(id).cloned(),
                })
            })
            .collect();
        tasks.sort_by(|left, right| left.task.task.task_id.cmp(&right.task.task.task_id));
        ScopeSnapshot {
            cursor: state.cursor,
            epoch: *state.epochs.get(scope).unwrap_or(&0),
            closing: state.closing.contains(scope),
            tasks,
        }
    }

    pub(crate) async fn changes(
        &self,
        scope: &str,
        cursor: u64,
        wait_ms: u64,
    ) -> Result<ScopeChanges, McpError> {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(wait_ms.min(30_000));
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let state = self.state.lock();
                if cursor > state.cursor
                    || state
                        .changes
                        .front()
                        .is_some_and(|(first, _, _)| cursor < first.saturating_sub(1))
                {
                    return Err(McpError::invalid_params("task cursor expired", None));
                }
                let changes: Vec<_> = state
                    .changes
                    .iter()
                    .filter(|(seq, s, _)| *seq > cursor && s == scope)
                    .map(|(_, _, change)| change.clone())
                    .collect();
                if !changes.is_empty() || wait_ms == 0 || tokio::time::Instant::now() >= deadline {
                    return Ok(ScopeChanges {
                        cursor: state.cursor,
                        changes,
                    });
                }
            }
            let _ = tokio::time::timeout_at(deadline, notified).await;
        }
    }

    pub(crate) async fn close_scope(
        &self,
        scope: &str,
        epoch: u64,
        generation: Option<&ExecutionGeneration>,
    ) -> Result<u64, McpError> {
        {
            let mut state = self.state.lock();
            state.check_execution(scope, generation)?;
            if *state.epochs.get(scope).unwrap_or(&0) != epoch {
                return Err(McpError::invalid_params("stale task scope epoch", None));
            }
            state.closing.insert(scope.into());
        }
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let state = self.state.lock();
                state.check_execution(scope, generation)?;
                if *state.epochs.get(scope).unwrap_or(&0) != epoch || !state.closing.contains(scope)
                {
                    return Err(McpError::invalid_params("stale task scope epoch", None));
                }
                if state.inflight.get(scope).copied().unwrap_or(0) == 0 {
                    return Ok(state.cursor);
                }
            }
            notified.await;
        }
    }

    pub(crate) fn open_scope(
        &self,
        scope: &str,
        epoch: u64,
        generation: Option<&ExecutionGeneration>,
    ) -> Result<ScopeOpened, McpError> {
        let mut state = self.state.lock();
        state.check_execution(scope, generation)?;
        if *state.epochs.get(scope).unwrap_or(&0) != epoch {
            return Err(McpError::invalid_params("stale task scope epoch", None));
        }
        if !state.closing.contains(scope) {
            return Err(McpError::invalid_params("task scope is not closing", None));
        }
        if state.inflight.get(scope).copied().unwrap_or(0) != 0
            || state.scopes.iter().any(|(id, task_scope)| {
                task_scope == scope
                    && state
                        .records
                        .get(id)
                        .is_some_and(|task| !task.task.status.is_terminal())
            })
        {
            return Err(McpError::invalid_params(
                "task scope has unsettled work",
                None,
            ));
        }
        let next = epoch
            .checked_add(1)
            .ok_or_else(|| McpError::internal_error("task scope epoch exhausted", None))?;
        state.closing.remove(scope);
        state.epochs.insert(scope.into(), next);
        Ok(ScopeOpened {
            cursor: state.cursor,
            epoch: next,
        })
    }

    pub(crate) async fn shutdown(&self) -> TaskShutdownReport {
        self.manager.shutdown().await
    }
}

pub(crate) struct ScopeAdmission {
    owner: ShellTasks,
    scope: String,
}

impl Drop for ScopeAdmission {
    fn drop(&mut self) {
        let mut state = self.owner.state.lock();
        if let Some(count) = state.inflight.get_mut(&self.scope) {
            *count -= 1;
            if *count == 0 {
                state.inflight.remove(&self.scope);
            }
        }
        self.owner.changed.notify_waiters();
    }
}

impl Drop for ShellTasks {
    fn drop(&mut self) {
        // Emergency best effort only. Hosts must call shutdown() and await
        // process cleanup before their runtime stops.
        if Arc::strong_count(&self.owner_references) == 1 {
            self.manager.cancel_all();
        }
    }
}

#[cfg(all(test, unix))]
#[path = "shell_tasks_test.rs"]
mod tests;
