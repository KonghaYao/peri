//! Workspace-owned Bash task state exposed through the MCP Tasks extension.
//!
//! The legacy shell executor still implements process execution. This owner
//! deliberately has no session callback: completion stays with the Workspace
//! service and can be queried after a client disconnects.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use parking_lot::Mutex;
use peri_acp_types::{
    event::BackgroundTaskResult,
    tasks::{BgShellHandle, BgTaskKind, OnBgCompleteFn, TaskManager, TaskShutdownReport},
};
use peri_agent::agent::async_tasks::BackgroundRegistryError;
use rmcp::{
    model::{
        CallToolResult, ContentBlock, DetailedTask, GetTaskResult, Task, TaskPayload, TaskStatus,
    },
    ErrorData as McpError,
};

#[derive(Default)]
struct ShellState {
    records: HashMap<String, DetailedTask>,
    owners: HashMap<String, String>,
    /// Agent thread that issued the task (design §6: recovery queries by it).
    thread_owners: HashMap<String, String>,
    cancel_pending: HashSet<String>,
    cancel_accepted: HashSet<String>,
    pending_results: HashMap<String, BackgroundTaskResult>,
    /// Tasks whose process has started but whose request future may not have
    /// registered a visible record yet (cancellation during spawn).
    tracked: HashSet<String>,
}

impl ShellState {
    fn owned_by(&self, id: &str, owner: &str) -> bool {
        self.owners
            .get(id)
            .is_some_and(|existing| existing == owner)
    }

    /// Live (non-terminal) tasks, optionally narrowed to one direct owner.
    ///
    /// Terminal records stay queryable through `tasks/get` but are not live
    /// tasks a recovery pass can still observe.
    fn live_tasks(&self, owner: Option<&str>) -> Vec<ShellTaskEntry> {
        let mut tasks: Vec<_> = self
            .records
            .iter()
            .filter(|(id, record)| {
                !record.task.status.is_terminal()
                    && owner.is_none_or(|owner| self.owned_by(id, owner))
            })
            .map(|(id, record)| ShellTaskEntry {
                task_id: id.clone(),
                owner: self.owners.get(id).cloned(),
                thread_owner: self.thread_owners.get(id).cloned(),
                state: shell_task_state(record.task.status).to_owned(),
                started_at: record.task.created_at.clone(),
            })
            .collect();
        tasks.sort_by(|left, right| {
            left.started_at
                .cmp(&right.started_at)
                .then_with(|| left.task_id.cmp(&right.task_id))
        });
        tasks
    }
}

/// Host-only `tools/call` name for querying live shell tasks by direct owner
/// (design §6). It is answered directly by the handler, not advertised in
/// `tools/list`: the model-facing surface stays the seven workspace tools.
pub(crate) const SHELL_TASK_LIST_TOOL: &str = "shell_task_list";

/// One live shell task in the query result.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct ShellTaskEntry {
    task_id: String,
    owner: Option<String>,
    thread_owner: Option<String>,
    state: String,
    started_at: String,
}

/// Query result: `unknown` is a first-class outcome, never an empty list.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ShellTaskList {
    unknown: bool,
    detail: Option<String>,
    tasks: Vec<ShellTaskEntry>,
}

impl ShellTaskList {
    fn known(tasks: Vec<ShellTaskEntry>) -> Self {
        Self {
            unknown: false,
            detail: None,
            tasks,
        }
    }

    fn unknown(detail: impl Into<String>) -> Self {
        Self {
            unknown: true,
            detail: Some(detail.into()),
            tasks: Vec::new(),
        }
    }
}

fn shell_task_state(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Working => "working",
        TaskStatus::InputRequired => "input_required",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
        // `TaskStatus` is non-exhaustive; a future status is reported as unknown
        // rather than being mistaken for a live or terminal task.
        _ => "unknown",
    }
}

/// `shell_task_list` query face; `None` state reports `unknown` instead of an
/// MCP error so callers can tell "state unavailable" from "no live tasks".
pub(crate) struct ShellTaskListTool {
    tasks: Option<ShellTasks>,
}

impl ShellTaskListTool {
    pub(crate) fn new(tasks: Option<ShellTasks>) -> Self {
        Self { tasks }
    }

    /// Serialize the query result; a serialization failure is still `unknown`,
    /// never an empty task list.
    pub(crate) fn list_text(&self, owner: Option<&str>) -> String {
        let list = match &self.tasks {
            Some(tasks) => tasks.live_tasks(owner),
            // Not every Workspace instance owns shell task state (legacy `new`).
            None => {
                ShellTaskList::unknown("workspace shell task state is not enabled on this instance")
            }
        };
        serde_json::to_string(&list).unwrap_or_else(|_| {
            r#"{"unknown":true,"detail":"shell task list encoding failed","tasks":[]}"#.to_owned()
        })
    }
}

#[cfg(test)]
pub(crate) struct SpawnAfterGate {
    pub(crate) entered: tokio::sync::Notify,
    pub(crate) release: std::sync::Barrier,
}

#[cfg(test)]
impl SpawnAfterGate {
    pub(crate) fn new() -> Self {
        Self {
            entered: tokio::sync::Notify::new(),
            release: std::sync::Barrier::new(2),
        }
    }
}

#[derive(Clone)]
pub(crate) struct ShellTasks {
    manager: Arc<dyn TaskManager>,
    state: Arc<Mutex<ShellState>>,
    updates: tokio::sync::broadcast::Sender<DetailedTask>,
    owner_references: Arc<()>,
    #[cfg(test)]
    spawn_after_gate: Option<Arc<SpawnAfterGate>>,
}

struct SpawnCancelGuard {
    tasks: ShellTasks,
    started: Option<tokio::sync::oneshot::Receiver<String>>,
    armed: bool,
}

impl SpawnCancelGuard {
    fn new(tasks: ShellTasks, started: tokio::sync::oneshot::Receiver<String>) -> Self {
        Self {
            tasks,
            started: Some(started),
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for SpawnCancelGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Some(started) = self.started.take() else {
            return;
        };
        let tasks = self.tasks.clone();
        tokio::spawn(async move {
            if let Ok(task_id) = started.await {
                let _ = tasks.cancel_and_wait(&task_id).await;
            }
        });
    }
}

impl ShellTasks {
    pub(crate) fn new() -> Self {
        let (updates, _) = tokio::sync::broadcast::channel(64);
        Self {
            manager: Arc::new(peri_mcp_common::create_local_task_manager()),
            state: Arc::new(Mutex::new(ShellState::default())),
            updates,
            owner_references: Arc::new(()),
            #[cfg(test)]
            spawn_after_gate: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn hold_spawn_after_gate(&mut self, gate: Arc<SpawnAfterGate>) {
        self.spawn_after_gate = Some(gate);
    }

    #[cfg(test)]
    pub(crate) fn clear_spawn_after_gate(&mut self) {
        self.spawn_after_gate = None;
    }

    #[cfg(test)]
    pub(crate) fn tracked_count(&self) -> usize {
        self.state.lock().tracked.len()
    }

    pub(crate) fn subscribe(&self) -> tokio::sync::broadcast::Receiver<DetailedTask> {
        self.updates.subscribe()
    }

    pub(crate) fn manager(&self) -> Arc<dyn TaskManager> {
        Arc::clone(&self.manager)
    }

    pub(crate) fn completion_callback(&self) -> OnBgCompleteFn {
        self.completion_callback_for_scope(None, None)
    }

    /// Completion callback with both owner identities recorded on the task.
    ///
    /// `thread_owner` is the agent thread that issued the work; recovery queries
    /// live tasks by it (design §6).
    pub(crate) fn completion_callback_for_scope(
        &self,
        owner: Option<String>,
        thread_owner: Option<String>,
    ) -> OnBgCompleteFn {
        let state = Arc::clone(&self.state);
        let updates = self.updates.clone();
        Arc::new(move |result: &BackgroundTaskResult, kind: BgTaskKind| {
            if kind != BgTaskKind::Shell {
                return Err("shell task owner received a non-shell result".into());
            }
            let mut state = state.lock();
            if let Some(owner) = &owner {
                state
                    .owners
                    .entry(result.task_id.clone())
                    .or_insert_with(|| owner.clone());
            }
            if let Some(thread_owner) = &thread_owner {
                state
                    .thread_owners
                    .entry(result.task_id.clone())
                    .or_insert_with(|| thread_owner.clone());
            }
            if state.cancel_pending.contains(&result.task_id) {
                state
                    .pending_results
                    .insert(result.task_id.clone(), result.clone());
                return Ok(());
            }
            let cancelled = state.cancel_accepted.remove(&result.task_id);
            let record = state
                .records
                .entry(result.task_id.clone())
                .or_insert_with(|| {
                    let now = peri_time::now_utc_rfc3339();
                    DetailedTask::new(
                        Task::new(&result.task_id, TaskStatus::Working, &now, &now),
                        TaskPayload::Working,
                    )
                });
            if record.task.status.is_terminal() {
                state.tracked.remove(&result.task_id);
                return Ok(());
            }
            record.task.last_updated_at = peri_time::now_utc_rfc3339();
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
            let detail = record.clone();
            state.tracked.remove(&result.task_id);
            drop(state);
            let _ = updates.send(detail);
            Ok(())
        })
    }

    #[cfg(test)]
    pub(crate) async fn spawn_owned(
        &self,
        command: String,
        cwd: String,
        timeout_ms: Option<u64>,
        owner: Option<&str>,
    ) -> Result<Task, McpError> {
        self.spawn_owned_scoped(command, cwd, timeout_ms, owner, None)
            .await
    }

    /// Spawn with both owner identities: the direct session and the agent
    /// thread that issued the work.
    pub(crate) async fn spawn_owned_scoped(
        &self,
        command: String,
        cwd: String,
        timeout_ms: Option<u64>,
        owner: Option<&str>,
        thread_owner: Option<&str>,
    ) -> Result<Task, McpError> {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let mut guard = SpawnCancelGuard::new(self.clone(), started_rx);
        let result = self
            .spawn_owned_inner(
                command,
                cwd,
                timeout_ms,
                owner,
                thread_owner,
                Some(started_tx),
            )
            .await;
        if result.is_ok() {
            guard.disarm();
        }
        result
    }

    pub(crate) async fn cancel_and_wait(&self, task_id: &str) -> Result<(), McpError> {
        let result = self.cancel(task_id);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let terminal = self
                .state
                .lock()
                .records
                .get(task_id)
                .is_none_or(|record| record.task.status.is_terminal());
            if terminal || tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        result
    }

    #[cfg(test)]
    pub(crate) async fn spawn(
        &self,
        command: String,
        cwd: String,
        timeout_ms: Option<u64>,
    ) -> Result<Task, McpError> {
        self.spawn_owned(command, cwd, timeout_ms, None).await
    }

    async fn spawn_owned_inner(
        &self,
        command: String,
        cwd: String,
        timeout_ms: Option<u64>,
        owner: Option<&str>,
        thread_owner: Option<&str>,
        started: Option<tokio::sync::oneshot::Sender<String>>,
    ) -> Result<Task, McpError> {
        let manager = self.manager();
        let callback = self.completion_callback_for_scope(
            owner.map(str::to_owned),
            thread_owner.map(str::to_owned),
        );
        let state = Arc::clone(&self.state);
        let updates = self.updates.clone();
        let owner = owner.map(str::to_owned);
        let thread_owner = thread_owner.map(str::to_owned);
        #[cfg(test)]
        let spawn_after_gate = self.spawn_after_gate.clone();
        let handle: BgShellHandle = tokio::task::spawn_blocking(move || {
            let handle = manager.spawn_shell(command, cwd, timeout_ms, Some(callback))?;
            #[cfg(test)]
            if let Some(gate) = &spawn_after_gate {
                gate.entered.notify_one();
                gate.release.wait();
            }
            // Register before returning from the blocking worker. The request
            // future may be cancelled while this worker is running; session
            // cleanup must still be able to find the process.
            let now = peri_time::now_utc_rfc3339();
            let record = DetailedTask::new(
                Task::new(&handle.task_id, TaskStatus::Working, &now, &now)
                    .with_status_message("Background shell command is running.")
                    .with_poll_interval_ms(1000),
                TaskPayload::Working,
            );
            let mut state = state.lock();
            let already_terminal = state
                .records
                .get(&handle.task_id)
                .is_some_and(|detail| detail.task.status.is_terminal());
            if !already_terminal {
                state.tracked.insert(handle.task_id.clone());
            }
            if let Some(owner) = &owner {
                state.owners.insert(handle.task_id.clone(), owner.clone());
            }
            if let Some(thread_owner) = &thread_owner {
                state
                    .thread_owners
                    .insert(handle.task_id.clone(), thread_owner.clone());
            }
            let inserted = !state.records.contains_key(&handle.task_id);
            let detail = state
                .records
                .entry(handle.task_id.clone())
                .or_insert(record)
                .clone();
            drop(state);
            if let Some(started) = started {
                let _ = started.send(handle.task_id.clone());
            }
            if inserted {
                let _ = updates.send(detail);
            }
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(handle)
        })
        .await
        .map_err(shell_launch_worker_error)?
        .map_err(shell_launch_registration_error)?;
        // A fast command can complete before the blocking worker registers it;
        // keep the terminal result instead of seeding a working record.
        let task = self
            .state
            .lock()
            .records
            .get(&handle.task_id)
            .map(|detail| detail.task.clone())
            .ok_or_else(|| {
                McpError::internal_error(
                    "background shell task registration missing after launch",
                    None,
                )
            })?;
        Ok(task)
    }

    /// Register a foreground call that the Bash executor already promoted to
    /// its owned background manager. The callback may have won the race.
    ///
    /// `thread_owner` is the issuing agent thread recorded for recovery queries.
    pub(crate) fn track_promoted_scoped(
        &self,
        task_id: &str,
        owner: Option<&str>,
        thread_owner: Option<&str>,
    ) -> Task {
        let now = peri_time::now_utc_rfc3339();
        let record = DetailedTask::new(
            Task::new(task_id, TaskStatus::Working, &now, &now)
                .with_status_message("Foreground timeout promoted to background execution.")
                .with_poll_interval_ms(1000),
            TaskPayload::Working,
        );
        let mut state = self.state.lock();
        let already_terminal = state
            .records
            .get(task_id)
            .is_some_and(|detail| detail.task.status.is_terminal());
        if !already_terminal {
            state.tracked.insert(task_id.into());
        }
        if let Some(owner) = owner {
            state.owners.insert(task_id.into(), owner.into());
        }
        if let Some(thread_owner) = thread_owner {
            state
                .thread_owners
                .insert(task_id.into(), thread_owner.into());
        }
        let inserted = !state.records.contains_key(task_id);
        let detail = state
            .records
            .entry(task_id.into())
            .or_insert(record)
            .clone();
        let task = detail.task.clone();
        drop(state);
        if inserted {
            let _ = self.updates.send(detail);
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

    fn cancel_inner(&self, task_id: &str, owner: Option<&str>) -> Result<(), McpError> {
        {
            let mut state = self.state.lock();
            if let Some(owner) = owner {
                if !state.owned_by(task_id, owner) {
                    return Err(McpError::invalid_params("unknown task", None));
                }
            }
            if state
                .records
                .get(task_id)
                .ok_or_else(|| McpError::invalid_params("unknown task", None))?
                .task
                .status
                .is_terminal()
            {
                state.tracked.remove(task_id);
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
        let mut updated = None;
        if cancelled.is_ok() {
            state.tracked.remove(task_id);
            state.cancel_accepted.insert(task_id.into());
            if let Some(record) = state.records.get_mut(task_id) {
                record.task.status_message = Some(
                    "Background shell cancellation requested; waiting for process cleanup.".into(),
                );
                record.task.last_updated_at = peri_time::now_utc_rfc3339();
                updated = Some(record.clone());
            }
        }
        drop(state);
        if let Some(task) = updated {
            let _ = self.updates.send(task);
        }
        if let Some(result) = pending {
            self.completion_callback()(&result, BgTaskKind::Shell)
                .map_err(|reason| McpError::internal_error(reason, None))?;
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

    pub(crate) fn get_owned(&self, id: &str, owner: &str) -> Result<GetTaskResult, McpError> {
        if !self.state.lock().owned_by(id, owner) {
            return Err(McpError::invalid_params("unknown task", None));
        }
        self.get(id)
    }

    pub(crate) fn belongs_to(&self, id: &str, owner: &str) -> bool {
        self.state.lock().owned_by(id, owner)
    }

    /// Read live tasks for one direct owner (design §6 recovery query).
    pub(crate) fn live_tasks(&self, owner: Option<&str>) -> ShellTaskList {
        ShellTaskList::known(self.state.lock().live_tasks(owner))
    }

    pub(crate) fn cancel_owned(&self, id: &str, owner: &str) -> Result<(), McpError> {
        self.cancel_inner(id, Some(owner))
    }

    pub(crate) fn update_owned(&self, id: &str, owner: &str) -> Result<(), McpError> {
        if !self.state.lock().owned_by(id, owner) {
            return Err(McpError::invalid_params("unknown task", None));
        }
        self.update(id)
    }

    pub(crate) async fn shutdown(&self) -> TaskShutdownReport {
        self.manager.shutdown().await
    }
}

/// 后台 shell 启动失败分类。
///
/// 执行环境的自由文本错误可能包含命令、环境变量或 panic payload。
/// 模型响应与此处 tracing 均只使用结构化分类，不重复输出这些原始内容。
enum ShellLaunchFailure {
    WorkerPanicked,
    WorkerCancelled,
    WorkerStopped,
    ConcurrentLimit {
        kind: String,
        current: usize,
        limit: usize,
    },
    Unclassified,
}

impl ShellLaunchFailure {
    fn of_join_error(error: &tokio::task::JoinError) -> Self {
        if error.is_panic() {
            Self::WorkerPanicked
        } else if error.is_cancelled() {
            Self::WorkerCancelled
        } else {
            Self::WorkerStopped
        }
    }

    /// `KindConcurrentLimit` is the only launch error with structural
    /// backpressure facts today; other variants stay opaque rather than having
    /// their free-form fields echoed to the model.
    fn of_registration_error(error: &(dyn std::error::Error + Send + Sync + 'static)) -> Self {
        match error.downcast_ref::<BackgroundRegistryError>() {
            Some(BackgroundRegistryError::KindConcurrentLimit {
                kind,
                current,
                limit,
            }) => Self::ConcurrentLimit {
                kind: kind.clone(),
                current: *current,
                limit: *limit,
            },
            _ => Self::Unclassified,
        }
    }

    fn class(&self) -> &'static str {
        match self {
            Self::WorkerPanicked => "worker_panicked",
            Self::WorkerCancelled => "worker_cancelled",
            Self::WorkerStopped => "worker_stopped",
            Self::ConcurrentLimit { .. } => "concurrent_limit",
            Self::Unclassified => "unclassified",
        }
    }

    fn message(&self) -> String {
        match self {
            Self::WorkerPanicked => {
                "background shell task launch worker panicked before registration".to_owned()
            }
            Self::WorkerCancelled => {
                "background shell task launch worker was cancelled before registration".to_owned()
            }
            Self::WorkerStopped => {
                "background shell task launch worker stopped before registration".to_owned()
            }
            Self::ConcurrentLimit {
                kind,
                current,
                limit,
            } => format!(
                "background shell concurrency limit reached ({kind} {current}/{limit}); cancel a running background shell task or wait for one to finish before retrying"
            ),
            Self::Unclassified => {
                "background shell task could not be started; failure class: unclassified".to_owned()
            }
        }
    }
}

fn shell_launch_worker_error(error: tokio::task::JoinError) -> McpError {
    let failure = ShellLaunchFailure::of_join_error(&error);
    tracing::error!(
        failure_class = failure.class(),
        "background shell launch worker did not return"
    );
    McpError::internal_error(failure.message(), None)
}

fn shell_launch_registration_error(error: Box<dyn std::error::Error + Send + Sync>) -> McpError {
    let failure = ShellLaunchFailure::of_registration_error(error.as_ref());
    tracing::error!(
        failure_class = failure.class(),
        "background shell task launch failed before registration"
    );
    McpError::internal_error(failure.message(), None)
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
