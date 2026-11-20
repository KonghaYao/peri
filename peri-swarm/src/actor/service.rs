//! In-process ACP-backed [`SwarmService`] (S4).
//!
//! One [`SessionRuntime`] owns one [`AcpChild`] and its [`ItemLog`]. Turn
//! admission is serialized per session so that:
//!
//! - an idle `mode=turn` input runs `session/prompt` and awaits its reply;
//! - an input submitted while a turn is running goes through the host's
//!   user-input queue (`session/input/enqueue` → `session/input/dispatch`) and
//!   is appended to the running turn;
//! - `kind=control` `cancel` is bound to the turn observed at admission and
//!   waits for that turn to end before returning, so a late cancel can never
//!   stop the next turn.
//!
//! The last point is why `session/cancel` is sent **untargeted**: the host only
//! honors a targeted cancel for queue-managed attempts
//! (`UserInputMailbox::stop_matching` requires `active.managed`), while a root
//! `session/prompt` registers an external, unmanaged attempt. Binding is
//! therefore enforced here — the running turn holds the slot, a new turn needs
//! the slot to be empty, and cancel waits for the slot to leave its turn.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::{broadcast, watch, Mutex as AsyncMutex};
use uuid::Uuid;

use super::reader::{ItemLog, TurnOutcome};
use super::runtime::{
    local_session_store, AcpChild, ChildConfig, ChildError, ChildShutdown, SessionStoreDeployment,
};
use crate::domain::{
    new_root_session_id, ActorKey, Cursor, Disposition, EnvironmentKind, EnvironmentRef, ErrorCode,
    ErrorKind, InputKind, InputMode, InputOutcome, InputState, InputSubmission, ItemOrder,
    ItemPage, ItemsWindow, LogicalId, SessionState, SessionView, StreamEvent, SwarmError, TurnView,
};
use crate::service::{CreateSessionCommand, CreateSessionResult, EventSubscription, SwarmService};
use crate::store::{store_failure, InputPut, SwarmInputRow, SwarmSessionRow, SwarmStore};

/// Upper bound for a steered turn to be continued by the host's queued run.
///
/// The continuation is a full agent run (tools, child agents), so the bound is
/// only a safety net against a lost terminal, not a turn deadline.
const STEER_CONTINUATION_TIMEOUT: Duration = Duration::from_secs(300);

/// Turn identity visible to `GET /sessions` while the turn runs.
#[derive(Clone, Debug)]
struct TurnSlot {
    request_id: String,
    started_at: String,
}

/// Configuration of the in-process runtime.
#[derive(Clone, Debug)]
pub struct ActorServiceConfig {
    /// Trusted settings document (tests / SDK). `None` loads the layered
    /// configuration from `cwd`; either way credentials come from the
    /// deployment configuration, never from a request.
    pub settings_json: Option<String>,
    /// Local Peri session store used by the host. `None` uses the deployment
    /// default (`~/.peri/threads/threads.db`). Ignored when
    /// [`Self::session_store`] is set.
    pub session_store_path: Option<PathBuf>,
    /// Remote Peri session store for the child hosts (S6b). When set, a child
    /// opens this deployment remotely (`design §4.1`); it never falls back to
    /// the local SQLite file.
    pub session_store: Option<SessionStoreDeployment>,
    /// Stable machine UUID required by the remote/virtual-workspace open.
    pub machine_id: Option<Uuid>,
    /// Absolute, normalized virtual workspace root for the remote open.
    pub workspace_root: Option<PathBuf>,
    /// Deployment default model. A request that names a different model is
    /// rejected: one model per actor (`design §0.4-3`).
    pub model: Option<String>,
    pub channel_capacity: usize,
}

impl Default for ActorServiceConfig {
    fn default() -> Self {
        Self {
            settings_json: None,
            session_store_path: None,
            session_store: None,
            machine_id: None,
            workspace_root: None,
            model: None,
            channel_capacity: 256,
        }
    }
}

/// One live root session: child host, item log, turn slot and SSE channel.
struct SessionRuntime {
    child: Arc<AcpChild>,
    log: Arc<ItemLog>,
    channel: broadcast::Sender<StreamEvent>,
    /// Serializes turn admission checks; never held across a model turn.
    admission: AsyncMutex<()>,
    /// Active turn identity. `Some` means "a turn is running"; the value is
    /// cleared by the turn owner only.
    turn: watch::Sender<Option<TurnSlot>>,
}

impl SessionRuntime {
    fn new(
        child: Arc<AcpChild>,
        log: Arc<ItemLog>,
        channel: broadcast::Sender<StreamEvent>,
    ) -> Self {
        let (turn, _receiver) = watch::channel(None);
        Self {
            child,
            log,
            channel,
            admission: AsyncMutex::new(()),
            turn,
        }
    }

    fn is_running(&self) -> bool {
        self.turn.borrow().is_some()
    }
}

/// Clears the turn slot on drop so a failed/panicked turn cannot wedge the
/// session, and so a cancel waiting on this turn is always released.
struct TurnLease {
    turn: watch::Sender<Option<TurnSlot>>,
}

impl Drop for TurnLease {
    fn drop(&mut self) {
        self.turn.send_replace(None);
    }
}

/// A history page read back from the ACP host (`session/load`).
///
/// Paging is read-only and never becomes `swarm_items`; `swarm_items` stays the
/// single replayable record (`design §6.1`, §6.5).
#[derive(Clone, Debug, PartialEq)]
pub struct HostHistoryPage {
    pub items: usize,
    pub next_cursor: Option<String>,
    pub snapshot_token: Option<String>,
}

pub struct AcpSwarmService {
    store: Arc<dyn SwarmStore>,
    config: ActorServiceConfig,
    sessions: Mutex<HashMap<String, Arc<SessionRuntime>>>,
    channels: Mutex<HashMap<String, broadcast::Sender<StreamEvent>>>,
    /// Serializes `create_session` so two concurrent creates cannot reserve two
    /// hosts for the same logical id.
    create_lock: AsyncMutex<()>,
}

impl std::fmt::Debug for AcpSwarmService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AcpSwarmService")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl AcpSwarmService {
    #[must_use]
    pub fn new(store: Arc<dyn SwarmStore>, config: ActorServiceConfig) -> Self {
        Self {
            store,
            config,
            sessions: Mutex::new(HashMap::new()),
            channels: Mutex::new(HashMap::new()),
            create_lock: AsyncMutex::new(()),
        }
    }

    #[must_use]
    pub fn in_memory(config: ActorServiceConfig) -> Self {
        Self::new(Arc::new(crate::store::MemorySwarmStore::new()), config)
    }

    /// SSE channel of a session. Callers register the stream before
    /// `create_session` runs the first turn (`SwarmService::open_stream`).
    fn channel_for(&self, logical_id: &str) -> broadcast::Sender<StreamEvent> {
        let mut channels = self.channels.lock();
        channels
            .entry(logical_id.to_owned())
            .or_insert_with(|| {
                let (sender, _receiver) = broadcast::channel(self.config.channel_capacity.max(1));
                sender
            })
            .clone()
    }

    /// Whether this instance owns a live ACP host for `logical_id`.
    #[must_use]
    pub fn is_live(&self, logical_id: &LogicalId) -> bool {
        self.sessions.lock().contains_key(logical_id.as_str())
    }

    fn runtime(&self, logical_id: &LogicalId) -> Result<Arc<SessionRuntime>, SwarmError> {
        self.sessions
            .lock()
            .get(logical_id.as_str())
            .cloned()
            .ok_or_else(|| SwarmError::unknown_agent(logical_id.as_str()))
    }

    async fn require_row(&self, logical_id: &LogicalId) -> Result<SwarmSessionRow, SwarmError> {
        self.store
            .swarm_get_session(logical_id.as_str())
            .await
            .map_err(|error| store_failure("get_session", error))?
            .ok_or_else(|| SwarmError::unknown_agent(logical_id.as_str()))
    }

    async fn last_seq(&self, logical_id: &LogicalId) -> Result<u64, SwarmError> {
        self.store
            .swarm_last_seq(logical_id.as_str())
            .await
            .map_err(|error| store_failure("last_seq", error))
    }

    fn session_view(&self, row: &SwarmSessionRow, turn: Option<TurnSlot>) -> SessionView {
        SessionView {
            id: row.logical_id.clone(),
            object: "agent.session".to_owned(),
            logical_id: row.logical_id.clone(),
            root_session_id: row.root_session_id.clone(),
            cwd: row.cwd.clone(),
            state: row.state,
            turn: turn.map_or_else(TurnView::default, |slot| TurnView {
                request_id: Some(slot.request_id),
                started_at: Some(slot.started_at),
            }),
            last_item_seq: row.last_item_seq,
            created_at: row.created_at.clone(),
            last_activity_at: row.last_activity_at.clone(),
            agent: crate::domain::AgentView {
                model: row.model.clone(),
                instructions: row.instructions.clone(),
            },
            environment: row.environment.clone(),
            sandbox: None,
        }
    }

    async fn view_by_id(&self, logical_id: &LogicalId) -> Result<SessionView, SwarmError> {
        let row = self.require_row(logical_id).await?;
        // Re-read the sequence so a concurrently appended item is reflected.
        let last_item_seq = self.last_seq(logical_id).await?;
        let mut row = row;
        row.last_item_seq = last_item_seq;
        let turn = self
            .sessions
            .lock()
            .get(logical_id.as_str())
            .and_then(|runtime| runtime.turn.borrow().clone());
        Ok(self.session_view(&row, turn))
    }

    fn resolve_model(&self, requested: Option<&str>) -> Result<Option<String>, SwarmError> {
        match (self.config.model.as_deref(), requested) {
            (Some(configured), Some(requested)) if configured != requested => Err(SwarmError::new(
                ErrorKind::BadRequest,
                ErrorCode::ModelNotSupported,
                "requested model differs from the deployment model for this actor",
            )
            .with_details(json!({ "configured": configured, "requested": requested }))),
            (Some(configured), _) => Ok(Some(configured.to_owned())),
            (None, Some(requested)) => Ok(Some(requested.to_owned())),
            (None, None) => Ok(None),
        }
    }

    /// Run one `session/prompt` turn and await its reply.
    async fn run_turn(&self, runtime: &Arc<SessionRuntime>, text: &str) -> Result<(), SwarmError> {
        let request_id = Uuid::now_v7().to_string();
        let started_at = peri_time::now_utc_rfc3339();
        {
            let _admission = runtime.admission.lock().await;
            if runtime.is_running() {
                return Err(SwarmError::turn_in_progress());
            }
            runtime.turn.send_replace(Some(TurnSlot {
                request_id: request_id.clone(),
                started_at,
            }));
        }
        // From here on the lease is the only writer of the slot terminal.
        let _lease = TurnLease {
            turn: runtime.turn.clone(),
        };
        runtime.log.begin_turn(&request_id).await?;
        match runtime.child.prompt(text, &request_id).await {
            Ok(reply) => {
                // A steer cancels this attempt on purpose and the host continues
                // the turn as a queued run; that run owns the terminal now, so
                // the turn slot stays occupied until it reports one.
                if runtime.log.steer_epoch() > 0
                    && !runtime
                        .log
                        .wait_turn_closed(&request_id, STEER_CONTINUATION_TIMEOUT)
                        .await
                {
                    tracing::warn!(
                        logical_id = %runtime.log.logical_id(),
                        request_id = %request_id,
                        "steer continuation reported no terminal; closing the turn"
                    );
                }
                runtime
                    .log
                    .finish_turn(&request_id, TurnOutcome::Completed(reply.stop_reason))
                    .await;
                if reply.pending_tasks > 0 {
                    tracing::debug!(
                        logical_id = %runtime.log.logical_id(),
                        pending_tasks = reply.pending_tasks,
                        "swarm turn finished with pending background tasks"
                    );
                }
                Ok(())
            }
            Err(error) => {
                let (code, message) = match &error {
                    ChildError::Rpc { code, message } => (*code, message.clone()),
                    ChildError::ProviderUnavailable => (
                        -32603,
                        "no LLM provider is configured for this deployment".to_owned(),
                    ),
                    ChildError::Local(_) => (-32603, "agent runtime failed locally".to_owned()),
                    ChildError::Closed => (-32603, "agent transport is closed".to_owned()),
                };
                runtime
                    .log
                    .finish_turn(&request_id, TurnOutcome::Failed { code, message })
                    .await;
                Err(child_error(error))
            }
        }
    }

    /// Append an input to the running turn through the host user-input queue.
    async fn steer(
        &self,
        runtime: &Arc<SessionRuntime>,
        submission: &InputSubmission,
        logical_id: &LogicalId,
    ) -> Result<InputOutcome, SwarmError> {
        let text = submission.effective_text();
        // Record the handoff before dispatch: the dispatched input cancels the
        // in-flight attempt, and that attempt's `cancelled` terminal must not
        // close the turn. `0` means the turn ended before the handoff — the
        // input then runs as a normal turn instead of vanishing into the queue.
        let epoch = runtime.log.note_steer();
        if epoch == 0 {
            self.run_turn(runtime, &text).await?;
            let last_item_seq = self.last_seq(logical_id).await?;
            return Ok(InputOutcome::new(
                submission.input_id.clone(),
                Disposition::Inserted,
                submission.mode,
                last_item_seq,
            ));
        }
        if let Err(error) = self.dispatch_steer(runtime, submission, &text).await {
            runtime.log.abandon_steer(epoch);
            return Err(error);
        }
        let last_item_seq = self.last_seq(logical_id).await?;
        Ok(InputOutcome::new(
            submission.input_id.clone(),
            Disposition::Inserted,
            submission.mode,
            last_item_seq,
        ))
    }

    /// Enqueue and dispatch one input into the host's user-input queue.
    async fn dispatch_steer(
        &self,
        runtime: &Arc<SessionRuntime>,
        submission: &InputSubmission,
        text: &str,
    ) -> Result<(), SwarmError> {
        let snapshot = runtime
            .child
            .snapshot()
            .await
            .map_err(|error| queue_unavailable("session/input/snapshot", error))?;
        let generation = snapshot
            .get("generation")
            .and_then(Value::as_str)
            .filter(|generation| !generation.is_empty())
            .ok_or_else(|| SwarmError::queue_unavailable("session/input/snapshot"))?
            .to_owned();
        // `commandId` is the host's idempotency key *per RPC*: enqueue and
        // dispatch must not share it, or dispatch is rejected as IdentityConflict.
        let enqueue_command = Uuid::now_v7().to_string();
        runtime
            .child
            .enqueue_input(&submission.input_id, &generation, &enqueue_command, text)
            .await
            .map_err(|error| queue_unavailable("session/input/enqueue", error))?;
        let dispatch_command = Uuid::now_v7().to_string();
        runtime
            .child
            .dispatch_inputs(
                &generation,
                &dispatch_command,
                std::slice::from_ref(&submission.input_id),
            )
            .await
            .map_err(|error| queue_unavailable("session/input/dispatch", error))?;
        Ok(())
    }

    /// Cancel the turn that is running **now** (or the one named by
    /// `expected`), and wait for it to leave the slot.
    ///
    /// `expected = Some(request_id)` makes a stale cancel a no-op: it does not
    /// reach the host and therefore cannot stop a later turn.
    pub async fn cancel_turn(
        &self,
        logical_id: &LogicalId,
        expected: Option<&str>,
    ) -> Result<(), SwarmError> {
        let runtime = self.runtime(logical_id)?;
        let active = {
            let _admission = runtime.admission.lock().await;
            let receiver = runtime.turn.subscribe();
            let Some(slot) = receiver.borrow().clone() else {
                // Nothing is running: a late cancel is accepted as a no-op.
                return Ok(());
            };
            if expected.is_some_and(|expected| expected != slot.request_id) {
                return Ok(());
            }
            runtime
                .child
                .cancel(None)
                .await
                .map_err(|error| queue_unavailable("session/cancel", error))?;
            slot.request_id
        };
        // Wait for *this* turn to leave the slot. A new turn cannot occupy it
        // before that happens, and the predicate also releases us if the slot
        // was already taken by a newer turn.
        let mut receiver = runtime.turn.subscribe();
        let waited = tokio::time::timeout(
            Duration::from_secs(30),
            receiver.wait_for(|slot| {
                slot.as_ref()
                    .map(|slot| slot.request_id.as_str())
                    .is_none_or(|running| running != active.as_str())
            }),
        )
        .await;
        if waited.is_err() {
            tracing::warn!(
                logical_id = logical_id.as_str(),
                request_id = %active,
                "swarm cancel did not observe the turn terminal within 30s"
            );
        }
        Ok(())
    }

    /// Read one `session/load` page from the host. Paging never mutates
    /// `swarm_items` and never starts a turn.
    pub async fn load_history(
        &self,
        logical_id: &LogicalId,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<HostHistoryPage, SwarmError> {
        let runtime = self.runtime(logical_id)?;
        let response = match cursor {
            Some(cursor) => runtime.child.load_page(cursor, limit).await,
            None => runtime.child.load_initial(limit).await,
        }
        .map_err(child_error)?;
        Ok(HostHistoryPage {
            items: response
                .get("items")
                .and_then(Value::as_array)
                .map_or(0, Vec::len),
            next_cursor: response
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_owned),
            snapshot_token: response
                .get("snapshotToken")
                .and_then(Value::as_str)
                .map(str::to_owned),
        })
    }

    /// Stop one live host and forget its runtime. Idempotent.
    pub async fn shutdown(&self, logical_id: &LogicalId) -> Option<ChildShutdown> {
        let runtime = self.sessions.lock().remove(logical_id.as_str())?;
        Some(runtime.child.shutdown().await)
    }

    /// Stop every live host. Tests call this before dropping the service so no
    /// host task outlives its session store.
    pub async fn shutdown_all(&self) -> Vec<ChildShutdown> {
        let runtimes: Vec<_> = self
            .sessions
            .lock()
            .drain()
            .map(|(_, runtime)| runtime)
            .collect();
        let mut reports = Vec::with_capacity(runtimes.len());
        for runtime in runtimes {
            reports.push(runtime.child.shutdown().await);
        }
        reports
    }

    /// Persisted backlog after `cursor`, used by the actor SSE endpoint and
    /// the gateway's reconnect path.
    pub async fn replay_after(
        &self,
        logical_id: &LogicalId,
        cursor: Cursor,
    ) -> Result<Vec<StreamEvent>, SwarmError> {
        let mut backlog = Vec::new();
        let mut after = cursor.value();
        loop {
            let page = self
                .store
                .swarm_items_after(logical_id.as_str(), after, ItemsWindow::MAX_LIMIT)
                .await
                .map_err(|error| store_failure("items_after", error))?;
            if page.is_empty() {
                break;
            }
            let page_len = page.len();
            if let Some(last) = page.last() {
                after = last.seq;
            }
            backlog.extend(page.into_iter().map(|row| StreamEvent::Item(row.to_view())));
            if page_len < ItemsWindow::MAX_LIMIT as usize {
                break;
            }
        }
        Ok(backlog)
    }

    async fn spawn_runtime(
        &self,
        command: &CreateSessionCommand,
        model: Option<String>,
        root_session_id: String,
    ) -> Result<Arc<SessionRuntime>, SwarmError> {
        let channel = self.channel_for(command.logical_id.as_str());
        let log = Arc::new(ItemLog::new(
            command.logical_id.as_str(),
            Arc::clone(&self.store),
            channel.clone(),
        ));
        let session_store = self
            .config
            .session_store
            .clone()
            .unwrap_or_else(|| local_session_store(self.config.session_store_path.clone()));
        let child = AcpChild::spawn(
            ChildConfig {
                cwd: command.cwd.clone(),
                session_store,
                machine_id: self.config.machine_id,
                workspace_root: self.config.workspace_root.clone(),
                settings_json: self.config.settings_json.clone(),
                model,
                instructions: command.instructions.clone(),
                root_session_id: root_session_id.clone(),
            },
            Arc::clone(&log),
        )
        .await
        .map_err(|error| {
            tracing::warn!(
                logical_id = command.logical_id.as_str(),
                error = %error,
                "swarm ACP host assembly failed"
            );
            child_error(error)
        })?;
        if child.session_id() != root_session_id {
            let shutdown = child.shutdown().await;
            tracing::warn!(
                logical_id = command.logical_id.as_str(),
                expected = %root_session_id,
                report = shutdown.report,
                "reserved root session id was not honored"
            );
            return Err(SwarmError::new(
                ErrorKind::BadGateway,
                ErrorCode::AgentError,
                "agent session identity does not match the reserved id",
            ));
        }
        Ok(Arc::new(SessionRuntime::new(Arc::new(child), log, channel)))
    }
}

#[async_trait]
impl SwarmService for AcpSwarmService {
    async fn create_session(
        &self,
        command: CreateSessionCommand,
    ) -> Result<CreateSessionResult, SwarmError> {
        if command.environment != EnvironmentKind::None {
            return Err(SwarmError::mcp_mode_unsupported());
        }
        let model = self.resolve_model(command.model.as_deref())?;
        let cwd = command.cwd.to_string_lossy().into_owned();
        let _create = self.create_lock.lock().await;

        if let Some(row) = self
            .store
            .swarm_get_session(command.logical_id.as_str())
            .await
            .map_err(|error| store_failure("get_session", error))?
        {
            if row.cwd != cwd {
                return Err(SwarmError::new(
                    ErrorKind::Conflict,
                    ErrorCode::IdentityConflict,
                    "logicalId already exists with a different cwd",
                )
                .with_details(json!({ "field": "cwd" })));
            }
            if let Some(requested) = command.instructions.as_deref() {
                if row.instructions.as_deref() != Some(requested) {
                    return Err(SwarmError::new(
                        ErrorKind::Conflict,
                        ErrorCode::IdentityConflict,
                        "logicalId already exists with different instructions",
                    )
                    .with_details(json!({ "field": "instructions" })));
                }
            }
            if command.mcp_tools != row.mcp_tools {
                return Err(SwarmError::new(
                    ErrorKind::Conflict,
                    ErrorCode::IdentityConflict,
                    "logicalId already exists with a different MCP tool declaration",
                )
                .with_details(json!({ "field": "tools" })));
            }
            if model != row.model {
                return Err(SwarmError::new(
                    ErrorKind::BadRequest,
                    ErrorCode::ModelNotSupported,
                    "requested model differs from the session model",
                )
                .with_details(json!({ "configured": row.model, "requested": model })));
            }
            let runtime = self.runtime(&command.logical_id).map_err(|_| {
                // Cross-process/agent recovery (`session/load` into a fresh
                // child) belongs to S5/S6; S4 must not pretend the session is
                // live when no host owns it.
                tracing::warn!(
                    logical_id = command.logical_id.as_str(),
                    "session row exists without a live in-process host"
                );
                SwarmError::upstream_unavailable("session_not_live")
            })?;
            if let Some(text) = command.initial_input.as_deref() {
                self.run_turn(&runtime, text).await?;
            }
            let session = self.view_by_id(&command.logical_id).await?;
            return Ok(CreateSessionResult {
                session,
                created: false,
            });
        }

        let root_session_id = command
            .root_session_id
            .clone()
            .unwrap_or_else(new_root_session_id);
        let runtime = self
            .spawn_runtime(&command, model.clone(), root_session_id.clone())
            .await?;

        let now = peri_time::now_utc_rfc3339();
        let row = SwarmSessionRow {
            logical_id: command.logical_id.as_str().to_owned(),
            root_session_id,
            actor_key: ActorKey::new(command.logical_id.clone()).storage_key(),
            cwd,
            state: SessionState::Idle,
            model,
            instructions: command.instructions.clone(),
            mcp_tools: command.mcp_tools.clone(),
            environment: EnvironmentRef::none(&command.logical_id),
            created_at: now.clone(),
            last_activity_at: now,
            last_item_seq: 0,
        };
        match self.store.swarm_upsert_session(row).await {
            Ok(()) => {}
            Err(error) => {
                // A session without its row is not addressable; do not leak a
                // live host for it (`design §7.4-1`).
                let shutdown = runtime.child.shutdown().await;
                tracing::warn!(
                    logical_id = command.logical_id.as_str(),
                    report = shutdown.report,
                    "swarm session row write failed; host shut down"
                );
                return Err(match error {
                    crate::store::StoreError::Conflict(_) => SwarmError::new(
                        ErrorKind::Conflict,
                        ErrorCode::IdentityConflict,
                        "logicalId was created concurrently with a different identity",
                    )
                    .with_details(json!({ "field": "logicalId" })),
                    error => store_failure("upsert_session", error),
                });
            }
        }
        self.sessions
            .lock()
            .insert(command.logical_id.as_str().to_owned(), Arc::clone(&runtime));

        if let Some(text) = command.initial_input.as_deref() {
            self.run_turn(&runtime, text).await?;
        }
        let session = self.view_by_id(&command.logical_id).await?;
        Ok(CreateSessionResult {
            session,
            created: true,
        })
    }

    async fn get_session(&self, logical_id: &LogicalId) -> Result<SessionView, SwarmError> {
        self.view_by_id(logical_id).await
    }

    async fn submit_input(
        &self,
        logical_id: &LogicalId,
        submission: InputSubmission,
    ) -> Result<InputOutcome, SwarmError> {
        let runtime = self.runtime(logical_id)?;
        self.require_row(logical_id).await?;
        if submission.is_empty() {
            return Err(SwarmError::empty_input());
        }
        let fingerprint = submission.fingerprint();
        let input_id = submission.input_id.clone();

        if let Some(existing) = self
            .store
            .swarm_get_input(logical_id.as_str(), &input_id)
            .await
            .map_err(|error| store_failure("get_input", error))?
        {
            if existing.fingerprint == fingerprint {
                let last_item_seq = self.last_seq(logical_id).await?;
                return Ok(InputOutcome::new(
                    input_id,
                    Disposition::Existing,
                    submission.mode,
                    last_item_seq,
                ));
            }
            return Err(SwarmError::input_conflict(&input_id));
        }

        if submission.kind == InputKind::User
            && submission.mode == InputMode::Turn
            && runtime.is_running()
        {
            return Err(SwarmError::turn_in_progress());
        }

        let receipt = SwarmInputRow {
            logical_id: logical_id.as_str().to_owned(),
            input_id: input_id.clone(),
            fingerprint: fingerprint.clone(),
            state: InputState::Accepted,
            created_at: peri_time::now_utc_rfc3339(),
        };
        match self.store.swarm_put_input(receipt).await {
            Ok(InputPut::Inserted) => {}
            Ok(InputPut::Existing(existing)) => {
                if existing.fingerprint != fingerprint {
                    return Err(SwarmError::input_conflict(&input_id));
                }
                let last_item_seq = self.last_seq(logical_id).await?;
                return Ok(InputOutcome::new(
                    input_id,
                    Disposition::Existing,
                    submission.mode,
                    last_item_seq,
                ));
            }
            Err(error) => {
                tracing::warn!(error = %error, "swarm input receipt write failed");
                return Err(SwarmError::queue_unavailable("put_input"));
            }
        }

        if submission.kind == InputKind::Control {
            // `kind=control` is recorded only; `cancel` is the one control
            // command S4 defines (`design §2.4`).
            if submission
                .effective_text()
                .trim()
                .eq_ignore_ascii_case("cancel")
            {
                self.cancel_turn(logical_id, None).await?;
            }
            let last_item_seq = self.last_seq(logical_id).await?;
            return Ok(InputOutcome::new(
                input_id,
                Disposition::Inserted,
                submission.mode,
                last_item_seq,
            ));
        }

        let outcome = if runtime.is_running() {
            self.steer(&runtime, &submission, logical_id).await?
        } else {
            self.run_turn(&runtime, &submission.effective_text())
                .await?;
            let last_item_seq = self.last_seq(logical_id).await?;
            InputOutcome::new(
                input_id,
                Disposition::Inserted,
                submission.mode,
                last_item_seq,
            )
        };
        Ok(outcome)
    }

    async fn list_items(
        &self,
        logical_id: &LogicalId,
        window: ItemsWindow,
    ) -> Result<ItemPage, SwarmError> {
        self.require_row(logical_id).await?;
        if window.limit == 0 || window.limit > ItemsWindow::MAX_LIMIT {
            return Err(SwarmError::invalid_query("limit must be within 1..=500"));
        }
        let last_item_seq = self.last_seq(logical_id).await?;
        let probe = window.limit.saturating_add(1);

        let mut rows = match window.order {
            ItemOrder::Asc => self
                .store
                .swarm_items_after(logical_id.as_str(), window.after.unwrap_or(0), probe)
                .await
                .map_err(|error| store_failure("items_after", error))?,
            ItemOrder::Desc => self
                .store
                .swarm_items_before(logical_id.as_str(), window.after, probe)
                .await
                .map_err(|error| store_failure("items_before", error))?,
        };

        let has_more = rows.len() > window.limit as usize;
        if has_more {
            rows.truncate(window.limit as usize);
        }
        let next_cursor = if has_more {
            rows.last().map(|row| row.seq)
        } else {
            None
        };
        let items = rows.into_iter().map(|row| row.to_view()).collect();
        Ok(ItemPage::new(
            logical_id.as_str(),
            items,
            next_cursor,
            last_item_seq,
            has_more,
            window.order,
        ))
    }

    async fn subscribe(
        &self,
        logical_id: &LogicalId,
        cursor: Cursor,
    ) -> Result<EventSubscription, SwarmError> {
        self.require_row(logical_id).await?;
        let runtime = self.runtime(logical_id)?;
        // Receiver first, then backlog: no event can slip between replay and
        // live delivery.
        let live = runtime.channel.subscribe();
        let backlog = self.replay_after(logical_id, cursor).await?;
        Ok(EventSubscription::new(backlog, live))
    }

    async fn open_stream(&self, logical_id: &LogicalId) -> EventSubscription {
        let live = self.channel_for(logical_id.as_str()).subscribe();
        EventSubscription::new(Vec::new(), live)
    }
}

/// Map a child failure to the HTTP contract. Upstream messages are dropped
/// from the response body (they may embed provider payloads); the JSON-RPC code
/// is preserved.
fn child_error(error: ChildError) -> SwarmError {
    match error {
        ChildError::Rpc { code, .. } => SwarmError::from_acp_error(code),
        ChildError::ProviderUnavailable => SwarmError::new(
            ErrorKind::BadGateway,
            ErrorCode::AgentError,
            "no LLM provider is configured for this deployment",
        ),
        ChildError::Local(_) => {
            tracing::warn!("swarm ACP child failed locally");
            SwarmError::upstream_unavailable("acp_child")
        }
        ChildError::Closed => SwarmError::upstream_unavailable("acp_closed"),
    }
}

/// Queue handoff failures are always `503 queue_unavailable`; a missing
/// capability keeps its own diagnostic in `details.operation`. The host's
/// rejection reason is carried in `details.reason` so the client can diagnose
/// a stale generation without reading the host log.
fn queue_unavailable(operation: &'static str, error: ChildError) -> SwarmError {
    tracing::warn!(operation, error = %error, "swarm input handoff failed");
    match error {
        ChildError::Rpc { code: -32601, .. } => {
            SwarmError::queue_unavailable("capability_not_negotiated")
        }
        ChildError::Rpc { code, message } => SwarmError::queue_unavailable(operation).with_details(
            serde_json::json!({ "operation": operation, "rpcCode": code, "reason": message }),
        ),
        other => SwarmError::queue_unavailable(operation).with_details(
            serde_json::json!({ "operation": operation, "reason": other.to_string() }),
        ),
    }
}
