//! In-memory monitor task registry and command runner.
//!
//! v1 only owns command sources: one shell command per task, line-oriented
//! stdout/stderr capture, and a bounded in-memory event ring buffer. Task state
//! is never persisted. After a process restart or instance replacement every old
//! task id is unknown.
//!
//! Task ownership is the initiating session identity carried by
//! `_meta["peri/taskOwner"]`. A caller with an owner sees only that owner's
//! tasks. A caller without an owner (local standalone use) is unscoped,
//! matching the Workspace task-owner contract: it sees every task in the
//! instance, including tasks owned by another session.

use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use peri_acp_types::monitor::{MonitorAckOutcome, MonitorWake};
use peri_process::ProcessTree;
use regex::Regex;
use serde::Serialize;
use thiserror::Error;
use tokio::io::BufReader;
use tokio::process::{Child, ChildStderr, ChildStdout};
use tokio_util::sync::CancellationToken;

use crate::buffer::{
    capture_lines, EventAdmission, EventBuffer, EventPage, MonitorEvent, MAX_READ_BYTES,
    MAX_READ_EVENTS, MAX_TASK_BYTES, MAX_TASK_EVENTS, MIN_TASK_BYTES,
};

/// Default task lifetime: one hour.
pub const DEFAULT_DEADLINE_MS: u64 = 60 * 60 * 1000;
/// Maximum task lifetime: 24 hours. Requests above this are rejected.
pub const MAX_DEADLINE_MS: u64 = 24 * 60 * 60 * 1000;
/// Default ring buffer capacity.
pub const DEFAULT_MAX_EVENTS: usize = 2_000;
/// Default ring buffer byte budget.
pub const DEFAULT_MAX_BYTES: usize = 1024 * 1024;
/// Default number of events returned by one read.
pub const DEFAULT_READ_LIMIT: usize = 200;
/// Default byte budget for one read.
pub const DEFAULT_READ_BYTES: usize = 64 * 1024;
/// Retained task records per instance. Terminal tasks are pruned first.
pub const MAX_RETAINED_TASKS: usize = 128;
/// Pending alert wakes retained per task. The oldest wake is evicted first.
pub const MAX_PENDING_WAKES: usize = 256;
/// Per-task wake merge window. Matching events inside the window merge into the
/// newest pending wake instead of creating another one.
pub const WAKE_MERGE_WINDOW: Duration = Duration::from_secs(30);
/// Maximum characters accepted for the `alert` regex.
pub const MAX_ALERT_PATTERN_CHARS: usize = 512;
/// Maximum bytes accepted for the optional `label`.
pub const MAX_LABEL_BYTES: usize = 256;
/// Maximum bytes accepted for a stop `reason`.
pub const MAX_REASON_BYTES: usize = 256;
/// Maximum wakes returned by one `monitor_wakes` call.
pub const MAX_WAKES_PER_READ: usize = 200;
/// Default wakes returned by one `monitor_wakes` call.
pub const DEFAULT_WAKES_LIMIT: usize = 50;

/// Public task status. Internal cancellation cleanup projects as `working`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MonitorStatus {
    Working,
    Completed,
    Failed,
    Cancelled,
}

impl MonitorStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskState {
    Working,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
}

impl TaskState {
    fn public(self) -> MonitorStatus {
        match self {
            Self::Working | Self::Cancelling => MonitorStatus::Working,
            Self::Completed => MonitorStatus::Completed,
            Self::Failed => MonitorStatus::Failed,
            Self::Cancelled => MonitorStatus::Cancelled,
        }
    }
}

/// Cleanup facts reported by `monitor_stop`. Cancellation acknowledgement and
/// process cleanup are separate signals. Wire keys are camelCase
/// (`requested`, `processTerminated`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupReport {
    /// A stop or MCP cancellation was requested for this task.
    pub requested: bool,
    /// The owned process tree is confirmed gone.
    pub process_terminated: bool,
}

/// Read-only task projection used by `monitor_read`, `monitor_list`,
/// `monitor_stop`, and `tasks/get`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskSnapshot {
    pub task_id: String,
    pub label: Option<String>,
    pub status: MonitorStatus,
    pub status_message: Option<String>,
    pub started_at: String,
    pub last_updated_at: String,
    pub deadline_at: String,
    pub finished_at: Option<String>,
    pub event_count: usize,
    pub dropped: u64,
    pub pending_wakes: usize,
    pub wake_dropped: u64,
    pub alert_matched: u64,
    pub cleanup: CleanupReport,
}

/// Start receipt returned before the first `monitor_read`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartedMonitor {
    pub task_id: String,
    pub status: &'static str,
    pub started_at: String,
    pub deadline_at: String,
    pub label: Option<String>,
}

/// `monitor_read` response. Wire keys are camelCase (`nextSeq`,
/// `truncated`, `dropped`).
///
/// A task retains its events after the process reaches a terminal state; only
/// record pruning or instance replacement removes them. Callers therefore keep
/// reading while `truncated` is true even if `status` is already terminal.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadResult {
    pub events: Vec<MonitorEvent>,
    pub next_seq: u64,
    pub status: MonitorStatus,
    /// The page stopped at a read bound (event count or byte budget); more
    /// events may remain. This is not the per-event "line cut at 16 KiB" flag.
    pub truncated: bool,
    pub dropped: u64,
}

/// `monitor_start` request after JSON parsing.
#[derive(Debug, Clone, Default)]
pub struct StartRequest {
    pub command: String,
    pub cwd: Option<String>,
    pub label: Option<String>,
    pub alert: Option<String>,
    pub deadline_ms: Option<u64>,
    pub max_events: Option<usize>,
    pub max_bytes: Option<usize>,
}

/// Validated start input.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedStart {
    pub command: String,
    pub cwd: String,
    pub label: Option<String>,
    pub alert: Option<Regex>,
    pub deadline: Duration,
    pub max_events: usize,
    pub max_bytes: usize,
}

/// Monitor domain error. Text appears in the model-facing failure projection,
/// so it never includes captured output or credentials.
#[derive(Debug, Error)]
pub enum MonitorError {
    #[error("command must not be empty")]
    EmptyCommand,
    #[error("deadline_ms must be between 1 and {MAX_DEADLINE_MS}")]
    InvalidDeadline,
    #[error("max_events must be between 1 and {MAX_TASK_EVENTS}")]
    InvalidMaxEvents,
    #[error("max_bytes must be between {MIN_TASK_BYTES} and {MAX_TASK_BYTES}")]
    InvalidMaxBytes,
    #[error("limit must be between 1 and {max}")]
    InvalidLimit { max: usize },
    #[error("invalid alert pattern: {0}")]
    InvalidAlert(String),
    #[error("{field} exceeds {max} bytes")]
    TooLong { field: &'static str, max: usize },
    #[error("max_bytes must be between 1 and {max}")]
    InvalidReadBytes { max: usize },
    #[error("unknown task")]
    UnknownTask,
    #[error("{0}")]
    InvalidParameter(String),
    #[error("monitor task capacity reached; stop an existing task before starting another")]
    CapacityReached,
    #[error("failed to prepare monitor process tree: {0}")]
    ProcessTree(#[source] std::io::Error),
    #[error("failed to start monitor command: {0}")]
    Spawn(#[source] std::io::Error),
}

struct TaskInner {
    state: TaskState,
    status_message: Option<String>,
    last_updated_at: String,
    finished_at: Option<String>,
    terminal: bool,
    cleanup: CleanupReport,
}

/// Per-task alert wake outbox.
///
/// Matches merge into the newest pending wake while that wake is younger than
/// the merge window. Older unacknowledged wakes stay queued but receive no new
/// matches; after the window a fresh wake is opened, so a slow consumer cannot
/// hold one wake open forever. Once the host acknowledges the newest wake,
/// further matches are held in `draft` until the window elapses, so an eager
/// acknowledger still cannot cause a wake storm. `draft` is flushed by the next
/// read after the window or when the task reaches a terminal state.
struct WakeOutbox {
    ready: VecDeque<PendingWake>,
    draft: Option<PendingWake>,
    last_emitted: Option<Instant>,
    dropped: u64,
}

struct PendingWake {
    wake: MonitorWake,
    created: Instant,
}

impl WakeOutbox {
    fn new() -> Self {
        Self {
            ready: VecDeque::new(),
            draft: None,
            last_emitted: None,
            dropped: 0,
        }
    }

    fn admit_match(&mut self, task_id: &str, label: Option<&str>, seq: u64, now: Instant) {
        if let Some(latest) = self.ready.back_mut() {
            if now.saturating_duration_since(latest.created) < WAKE_MERGE_WINDOW {
                latest.wake.seq = seq;
                latest.wake.matched = latest.wake.matched.saturating_add(1);
                return;
            }
        }
        match self.draft.as_mut() {
            Some(draft) => {
                draft.wake.seq = seq;
                draft.wake.matched = draft.wake.matched.saturating_add(1);
            }
            None => {
                self.draft = Some(PendingWake {
                    wake: MonitorWake {
                        wake_id: format!("wake-{}", uuid::Uuid::now_v7()),
                        task_id: task_id.to_string(),
                        label: label.map(str::to_string),
                        seq,
                        matched: 1,
                        created_at: peri_time::now_utc_rfc3339(),
                    },
                    created: now,
                });
            }
        }
        self.flush_if_due(now, false);
    }

    fn flush_if_due(&mut self, now: Instant, terminal: bool) {
        if self.draft.is_none() {
            return;
        }
        let due = terminal
            || self
                .last_emitted
                .is_none_or(|last| now.saturating_duration_since(last) >= WAKE_MERGE_WINDOW);
        if !due {
            return;
        }
        if let Some(pending) = self.draft.take() {
            if self.ready.len() >= MAX_PENDING_WAKES {
                self.ready.pop_front();
                self.dropped = self.dropped.saturating_add(1);
            }
            self.ready.push_back(pending);
            self.last_emitted = Some(now);
        }
    }

    fn pending_len(&self) -> usize {
        self.ready.len() + usize::from(self.draft.is_some())
    }

    fn visible(&mut self, limit: usize, now: Instant, terminal: bool) -> Vec<MonitorWake> {
        self.flush_if_due(now, terminal);
        self.ready
            .iter()
            .take(limit)
            .map(|pending| pending.wake.clone())
            .collect()
    }

    fn ack(&mut self, wake_ids: &[String]) -> usize {
        let before = self.ready.len();
        self.ready
            .retain(|pending| !wake_ids.iter().any(|id| id == &pending.wake.wake_id));
        before - self.ready.len()
    }
}

struct MonitorTask {
    id: String,
    owner: Option<String>,
    label: Option<String>,
    alert: Option<Regex>,
    started_at: String,
    deadline_at: String,
    deadline: Duration,
    inner: Mutex<TaskInner>,
    buffer: Arc<Mutex<EventBuffer>>,
    wakes: Mutex<WakeOutbox>,
    alert_matched: AtomicU64,
    cancel: CancellationToken,
}

impl EventAdmission for MonitorTask {
    fn admit(&self, event: crate::buffer::CapturedEvent) {
        self.admit_event(event);
    }
}

impl MonitorTask {
    /// Admit one captured line: the ring buffer stores every event, while only
    /// alert matches enter the wake outbox.
    pub(crate) fn admit_event(&self, event: crate::buffer::CapturedEvent) {
        let matched = self
            .alert
            .as_ref()
            .is_some_and(|alert| alert.is_match(&event.text));
        let seq = self.buffer.lock().push(event);
        if !matched {
            return;
        }
        self.alert_matched.fetch_add(1, Ordering::Relaxed);
        self.wakes
            .lock()
            .admit_match(&self.id, self.label.as_deref(), seq, Instant::now());
    }

    fn visible_to(&self, owner: Option<&str>) -> bool {
        match (self.owner.as_deref(), owner) {
            (Some(task_owner), Some(caller)) => task_owner == caller,
            _ => true,
        }
    }

    fn is_terminal(&self) -> bool {
        self.inner.lock().terminal
    }

    fn status(&self) -> MonitorStatus {
        self.inner.lock().state.public()
    }

    fn request_stop(&self, reason: &str) -> bool {
        let mut inner = self.inner.lock();
        if inner.terminal {
            return false;
        }
        inner.cleanup.requested = true;
        if inner.state == TaskState::Working {
            inner.state = TaskState::Cancelling;
        }
        inner.status_message = Some(reason.to_string());
        inner.last_updated_at = peri_time::now_utc_rfc3339();
        drop(inner);
        self.cancel.cancel();
        true
    }

    fn finalize(&self, status: MonitorStatus, message: String, process_terminated: bool) {
        let terminal_state = match status {
            MonitorStatus::Completed => TaskState::Completed,
            MonitorStatus::Failed => TaskState::Failed,
            // A non-terminal projection cannot commit a terminal record.
            MonitorStatus::Working | MonitorStatus::Cancelled => TaskState::Cancelled,
        };
        let mut inner = self.inner.lock();
        if inner.terminal {
            return;
        }
        // Preserve the stop reason recorded by `request_stop`; cancellation
        // acknowledgement must not erase why the task ended.
        let message = if terminal_state == TaskState::Cancelled {
            inner
                .status_message
                .clone()
                .filter(|message| !message.is_empty())
                .unwrap_or(message)
        } else {
            message
        };
        let now = peri_time::now_utc_rfc3339();
        inner.state = terminal_state;
        inner.status_message = Some(message);
        inner.last_updated_at = now.clone();
        inner.finished_at = Some(now);
        inner.cleanup.process_terminated = process_terminated;
        inner.terminal = true;
        drop(inner);
        // A merged draft must not be lost because the command ended inside the
        // merge window; terminal tasks are observed once more by `tasks/get`
        // and `monitor_wakes`.
        self.wakes.lock().flush_if_due(Instant::now(), true);
    }

    fn snapshot(&self) -> TaskSnapshot {
        let (
            status,
            status_message,
            started_at,
            last_updated_at,
            deadline_at,
            finished_at,
            cleanup,
        ) = {
            let inner = self.inner.lock();
            (
                inner.state.public(),
                inner.status_message.clone(),
                self.started_at.clone(),
                inner.last_updated_at.clone(),
                self.deadline_at.clone(),
                inner.finished_at.clone(),
                inner.cleanup,
            )
        };
        let (event_count, dropped) = {
            let buffer = self.buffer.lock();
            (buffer.len(), buffer.dropped())
        };
        let (pending_wakes, wake_dropped) = {
            let wakes = self.wakes.lock();
            (wakes.pending_len(), wakes.dropped)
        };
        TaskSnapshot {
            task_id: self.id.clone(),
            label: self.label.clone(),
            status,
            status_message,
            started_at,
            last_updated_at,
            deadline_at,
            finished_at,
            event_count,
            dropped,
            pending_wakes,
            wake_dropped,
            alert_matched: self.alert_matched.load(Ordering::Relaxed),
            cleanup,
        }
    }
}

/// Instance-scoped monitor state. One state is shared by all connections to the
/// same builtin instance; tasks are filtered by session owner on access.
pub struct MonitorState {
    cwd: String,
    tasks: Mutex<HashMap<String, Arc<MonitorTask>>>,
}

impl MonitorState {
    pub fn new(cwd: impl Into<String>) -> Self {
        Self {
            cwd: cwd.into(),
            tasks: Mutex::new(HashMap::new()),
        }
    }

    /// Start one command-backed monitor task and return its receipt.
    pub async fn start(
        &self,
        request: StartRequest,
        owner: Option<String>,
    ) -> Result<StartedMonitor, MonitorError> {
        let resolved = resolve_start(&request, &self.cwd)?;

        {
            let mut tasks = self.tasks.lock();
            if tasks.len() >= MAX_RETAINED_TASKS {
                prune_terminal(&mut tasks);
                if tasks.len() >= MAX_RETAINED_TASKS {
                    return Err(MonitorError::CapacityReached);
                }
            }
        }

        let task_id = format!("monitor-{}", uuid::Uuid::now_v7());
        let started_at = peri_time::now_utc_rfc3339();
        let deadline_at = peri_time::format_utc_rfc3339(peri_time::now_wall() + resolved.deadline);

        let mut command = peri_mcp_common::shell::shell_command(&resolved.command, &[]);
        command
            .current_dir(&resolved.cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut tree = ProcessTree::new().map_err(MonitorError::ProcessTree)?;
        tree.prepare(&mut command);
        let mut child = command.spawn().map_err(MonitorError::Spawn)?;
        if let Err(error) = tree.attach(&child) {
            tree.terminate();
            let _ = child.start_kill();
            return Err(MonitorError::ProcessTree(error));
        }
        let stdout = child
            .stdout
            .take()
            .expect("piped monitor stdout is present after spawn");
        let stderr = child
            .stderr
            .take()
            .expect("piped monitor stderr is present after spawn");

        let task = Arc::new(MonitorTask {
            id: task_id.clone(),
            owner,
            label: resolved.label.clone(),
            alert: resolved.alert.clone(),
            started_at: started_at.clone(),
            deadline_at: deadline_at.clone(),
            deadline: resolved.deadline,
            inner: Mutex::new(TaskInner {
                state: TaskState::Working,
                status_message: Some(
                    "Monitor command started; read incremental events with monitor_read.".into(),
                ),
                last_updated_at: started_at.clone(),
                finished_at: None,
                terminal: false,
                cleanup: CleanupReport {
                    requested: false,
                    process_terminated: false,
                },
            }),
            buffer: Arc::new(Mutex::new(EventBuffer::new(
                resolved.max_events,
                resolved.max_bytes,
            ))),
            wakes: Mutex::new(WakeOutbox::new()),
            alert_matched: AtomicU64::new(0),
            cancel: CancellationToken::new(),
        });

        self.tasks.lock().insert(task_id.clone(), Arc::clone(&task));
        tokio::spawn(run_task(task, child, tree, stdout, stderr));

        Ok(StartedMonitor {
            task_id,
            status: MonitorStatus::Working.as_str(),
            started_at,
            deadline_at,
            label: resolved.label,
        })
    }

    /// Fetch one page after an exclusive sequence cursor.
    pub fn read(
        &self,
        task_id: &str,
        owner: Option<&str>,
        after_seq: Option<u64>,
        limit: usize,
        max_bytes: usize,
    ) -> Result<ReadResult, MonitorError> {
        let task = self.task(task_id, owner)?;
        let (limit, max_bytes) = validate_read_limits(limit, max_bytes)?;
        let EventPage {
            events,
            next_seq,
            truncated,
            dropped,
        } = task.buffer.lock().read(after_seq, limit, max_bytes);
        Ok(ReadResult {
            events,
            next_seq,
            status: task.status(),
            truncated,
            dropped,
        })
    }

    /// Request stop and wait up to three seconds for process cleanup.
    pub async fn stop(
        &self,
        task_id: &str,
        owner: Option<&str>,
        reason: Option<String>,
    ) -> Result<TaskSnapshot, MonitorError> {
        let task = self.task(task_id, owner)?;
        let reason = reason
            .as_deref()
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
            .unwrap_or("Stop requested.")
            .to_string();
        if reason.len() > MAX_REASON_BYTES {
            return Err(MonitorError::TooLong {
                field: "reason",
                max: MAX_REASON_BYTES,
            });
        }
        task.request_stop(&reason);
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !task.is_terminal() && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Ok(task.snapshot())
    }

    /// MCP `tasks/cancel`: acknowledge immediately; cleanup is observed later
    /// through `tasks/get`.
    pub fn request_cancel(
        &self,
        task_id: &str,
        owner: Option<&str>,
        reason: &str,
    ) -> Result<(), MonitorError> {
        let task = self.task(task_id, owner)?;
        task.request_stop(reason);
        Ok(())
    }

    /// `tasks/get` projection.
    pub fn snapshot(
        &self,
        task_id: &str,
        owner: Option<&str>,
    ) -> Result<TaskSnapshot, MonitorError> {
        Ok(self.task(task_id, owner)?.snapshot())
    }

    /// Session-scoped summaries, newest first, without event bodies.
    pub fn list(
        &self,
        owner: Option<&str>,
        include_ended: bool,
        limit: usize,
    ) -> Vec<TaskSnapshot> {
        let tasks = self.tasks.lock();
        let mut snapshots: Vec<TaskSnapshot> = tasks
            .values()
            .filter(|task| task.visible_to(owner))
            .map(|task| task.snapshot())
            .filter(|snapshot| include_ended || !snapshot.status.is_terminal())
            .collect();
        snapshots.sort_by(|left, right| right.started_at.cmp(&left.started_at));
        snapshots.truncate(limit);
        snapshots
    }

    /// Pending alert wakes for one owner, oldest task first.
    ///
    /// The response never carries event text; callers read events with
    /// `monitor_read` and treat them as untrusted data.
    pub fn wakes(
        &self,
        owner: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MonitorWake>, MonitorError> {
        if limit == 0 || limit > MAX_WAKES_PER_READ {
            return Err(MonitorError::InvalidLimit {
                max: MAX_WAKES_PER_READ,
            });
        }
        let tasks = self.tasks.lock();
        let mut ordered: Vec<&Arc<MonitorTask>> = tasks
            .values()
            .filter(|task| task.visible_to(owner))
            .collect();
        ordered.sort_by(|left, right| left.started_at.cmp(&right.started_at));
        let now = Instant::now();
        let mut wakes = Vec::new();
        for task in ordered {
            let remaining = limit - wakes.len();
            if remaining == 0 {
                break;
            }
            let terminal = task.is_terminal();
            wakes.extend(task.wakes.lock().visible(remaining, now, terminal));
        }
        Ok(wakes)
    }

    /// Acknowledge delivered or offline wakes. Idempotent: unknown ids are
    /// ignored and repeat acknowledgements report zero.
    ///
    /// Ownership is enforced by visibility: a wake owned by another session is
    /// simply not found, matching the `unknown task` contract without revealing
    /// the other session's wake.
    pub fn ack(
        &self,
        owner: Option<&str>,
        wake_ids: &[String],
        outcome: MonitorAckOutcome,
    ) -> usize {
        let tasks = self.tasks.lock();
        let mut acked = 0;
        for task in tasks.values().filter(|task| task.visible_to(owner)) {
            let count = task.wakes.lock().ack(wake_ids);
            if count > 0 {
                tracing::debug!(
                    task_id = %task.id,
                    count,
                    outcome = outcome.as_str(),
                    "monitor wakes acknowledged"
                );
                acked += count;
            }
        }
        acked
    }

    fn task(&self, task_id: &str, owner: Option<&str>) -> Result<Arc<MonitorTask>, MonitorError> {
        let tasks = self.tasks.lock();
        let task = tasks.get(task_id).ok_or(MonitorError::UnknownTask)?;
        if !task.visible_to(owner) {
            return Err(MonitorError::UnknownTask);
        }
        Ok(Arc::clone(task))
    }
}

impl Drop for MonitorState {
    fn drop(&mut self) {
        // Instance replacement / process shutdown: request cancellation for
        // every live command. The runner owns process-tree cleanup; this drop
        // only stops new work from outliving the instance's registry.
        for task in self.tasks.get_mut().values() {
            task.cancel.cancel();
        }
    }
}

/// Validate start input and resolve the effective cwd and limits.
pub(crate) fn resolve_start(
    request: &StartRequest,
    default_cwd: &str,
) -> Result<ResolvedStart, MonitorError> {
    let command = request.command.trim();
    if command.is_empty() {
        return Err(MonitorError::EmptyCommand);
    }
    let deadline_ms = request.deadline_ms.unwrap_or(DEFAULT_DEADLINE_MS);
    if deadline_ms == 0 || deadline_ms > MAX_DEADLINE_MS {
        return Err(MonitorError::InvalidDeadline);
    }
    let max_events = request.max_events.unwrap_or(DEFAULT_MAX_EVENTS);
    if max_events == 0 || max_events > MAX_TASK_EVENTS {
        return Err(MonitorError::InvalidMaxEvents);
    }
    let max_bytes = request.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);
    if !(MIN_TASK_BYTES..=MAX_TASK_BYTES).contains(&max_bytes) {
        return Err(MonitorError::InvalidMaxBytes);
    }
    let cwd = resolve_cwd(request.cwd.as_deref(), default_cwd);
    let label = request
        .label
        .as_deref()
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(str::to_string);
    if label
        .as_ref()
        .is_some_and(|label| label.len() > MAX_LABEL_BYTES)
    {
        return Err(MonitorError::TooLong {
            field: "label",
            max: MAX_LABEL_BYTES,
        });
    }
    let alert = resolve_alert(request.alert.as_deref())?;
    Ok(ResolvedStart {
        command: command.to_string(),
        cwd,
        label,
        alert,
        deadline: Duration::from_millis(deadline_ms),
        max_events,
        max_bytes,
    })
}

/// Compile the alert pattern with the linear-time `regex` engine.
///
/// Invalid or oversized patterns are rejected before any process starts.
fn resolve_alert(alert: Option<&str>) -> Result<Option<Regex>, MonitorError> {
    let Some(pattern) = alert.map(str::trim).filter(|pattern| !pattern.is_empty()) else {
        return Ok(None);
    };
    if pattern.chars().count() > MAX_ALERT_PATTERN_CHARS {
        return Err(MonitorError::InvalidAlert(format!(
            "pattern exceeds {MAX_ALERT_PATTERN_CHARS} characters"
        )));
    }
    Regex::new(pattern)
        .map(Some)
        .map_err(|error| MonitorError::InvalidAlert(error.to_string()))
}

fn resolve_cwd(requested: Option<&str>, default_cwd: &str) -> String {
    let Some(requested) = requested.map(str::trim).filter(|cwd| !cwd.is_empty()) else {
        return default_cwd.to_string();
    };
    let path = Path::new(requested);
    if path.is_absolute() {
        requested.to_string()
    } else {
        PathBuf::from(default_cwd)
            .join(path)
            .to_string_lossy()
            .into_owned()
    }
}

fn prune_terminal(tasks: &mut HashMap<String, Arc<MonitorTask>>) {
    let terminal: Vec<String> = tasks
        .values()
        .filter(|task| task.is_terminal())
        .map(|task| task.id.clone())
        .collect();
    for task_id in terminal {
        tasks.remove(&task_id);
    }
}

async fn run_task(
    task: Arc<MonitorTask>,
    mut child: Child,
    tree: ProcessTree,
    stdout: ChildStdout,
    stderr: ChildStderr,
) {
    let mut out = tokio::spawn(capture_lines(
        BufReader::new(stdout),
        "stdout",
        Arc::clone(&task),
    ));
    let mut err = tokio::spawn(capture_lines(
        BufReader::new(stderr),
        "stderr",
        Arc::clone(&task),
    ));

    let deadline = tokio::time::sleep(task.deadline);
    tokio::pin!(deadline);
    let mut cancelled = false;
    let mut timed_out = false;

    let exit = tokio::select! {
        biased;
        _ = task.cancel.cancelled() => {
            cancelled = true;
            tree.terminate();
            let _ = child.start_kill();
            child.wait().await
        }
        _ = &mut deadline => {
            timed_out = true;
            tree.terminate();
            let _ = child.start_kill();
            child.wait().await
        }
        result = child.wait() => result,
    };

    if task.cancel.is_cancelled() {
        cancelled = true;
    }

    // Descendants may outlive the direct shell. Bound the wait, then stop the
    // capture workers so they cannot append events after terminal commit.
    let _ = tokio::time::timeout(Duration::from_secs(2), tree.wait_for_exit()).await;
    let process_terminated = tree.is_stopped();
    let drain = async {
        let _ = (&mut out).await;
        let _ = (&mut err).await;
    };
    if tokio::time::timeout(Duration::from_secs(1), drain)
        .await
        .is_err()
    {
        out.abort();
        err.abort();
    }

    let (status, message) = if cancelled {
        (
            MonitorStatus::Cancelled,
            "Monitor stopped before the command exited.".to_string(),
        )
    } else if timed_out {
        (
            MonitorStatus::Failed,
            "Monitor deadline exceeded; command terminated.".to_string(),
        )
    } else {
        match exit {
            Ok(exit) if exit.success() => (
                MonitorStatus::Completed,
                "Command exited successfully.".to_string(),
            ),
            Ok(exit) => (
                MonitorStatus::Failed,
                match exit.code() {
                    Some(code) => format!("Command exited with status {code}."),
                    None => "Command terminated by a signal.".to_string(),
                },
            ),
            Err(error) => (
                MonitorStatus::Failed,
                format!("Failed to wait for command exit: {error}"),
            ),
        }
    };
    task.finalize(status, message, process_terminated);
}

/// Parse-level constants exposed for request validation tests.
pub(crate) fn validate_read_limits(
    limit: usize,
    max_bytes: usize,
) -> Result<(usize, usize), MonitorError> {
    if limit == 0 || limit > MAX_READ_EVENTS {
        return Err(MonitorError::InvalidLimit {
            max: MAX_READ_EVENTS,
        });
    }
    if max_bytes == 0 || max_bytes > MAX_READ_BYTES {
        return Err(MonitorError::InvalidReadBytes {
            max: MAX_READ_BYTES,
        });
    }
    Ok((limit, max_bytes))
}

#[cfg(test)]
#[path = "state_test.rs"]
mod tests;
