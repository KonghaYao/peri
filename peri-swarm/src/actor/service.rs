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
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
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
    WORKSPACE_MCP_SERVER_NAME,
};
use crate::domain::{
    new_root_session_id, turn_id_for_item, ActorKey, Disposition, EnvironmentKind, EnvironmentRef,
    ErrorCode, ErrorKind, EventCursor, InputKind, InputMode, InputOutcome, InputState,
    InputSubmission, ItemPage, ItemsWindow, LogicalId, McpToolSpec, SandboxView, SessionState,
    SessionStatus, SessionView, StreamEvent, SwarmError, TurnView,
};
use crate::sandbox::{BunSandboxClient, SandboxClientError, SandboxMcpBinding};
use crate::service::{
    resolve_window_after, AcceptedInput, CreateSessionCommand, CreateSessionResult,
    EventSubscription, PendingTurn, SwarmService,
};
use crate::store::{
    store_failure, InputPut, SwarmInputRow, SwarmSessionRow, SwarmSessionUpdate, SwarmStore,
};

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
    /// Bun sandbox bridge (S8a3a). `None` makes `openai_hosted` fail with a
    /// diagnosable error; `none` sessions never touch it.
    pub sandbox: Option<BunSandboxClient>,
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
            sandbox: None,
        }
    }
}

/// Everything [`AcpSwarmService::spawn_runtime`] needs to assemble one host.
///
/// The create path fills it from the validated command; the interrupted-create
/// recovery fills it from the persisted row, so a retry cannot drift from the
/// identity and declaration that row already records.
struct SpawnRequest<'a> {
    logical_id: &'a LogicalId,
    cwd: &'a Path,
    model: Option<String>,
    instructions: Option<String>,
    mcp_tools: &'a [McpToolSpec],
    workspace: Option<&'a SandboxMcpBinding>,
    root_session_id: String,
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

/// Per-session SSE hub: the broadcast sender plus the shared item tail.
///
/// The tail is updated by the `ItemLog` after each persisted append and read by
/// SSE heartbeats, so `lastItemSeq` reflects the session, not one viewer.
#[derive(Clone, Debug)]
struct StreamHub {
    sender: broadcast::Sender<StreamEvent>,
    tail: Arc<AtomicU64>,
}

pub struct AcpSwarmService {
    store: Arc<dyn SwarmStore>,
    config: ActorServiceConfig,
    sessions: Mutex<HashMap<String, Arc<SessionRuntime>>>,
    channels: Mutex<HashMap<String, StreamHub>>,
    /// Serializes `create_session` so two concurrent creates cannot reserve two
    /// hosts for the same logical id.
    create_lock: AsyncMutex<()>,
    /// Serializes `ensure_live` so concurrent interactions after a host restart
    /// attach one host, not one per action.
    attach_lock: AsyncMutex<()>,
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
            attach_lock: AsyncMutex::new(()),
        }
    }

    #[must_use]
    pub fn in_memory(config: ActorServiceConfig) -> Self {
        Self::new(Arc::new(crate::store::MemorySwarmStore::new()), config)
    }

    /// SSE channel and tail of a session. Callers register the stream before
    /// `create_session` runs the first turn (`SwarmService::open_stream`).
    fn hub_for(&self, logical_id: &str) -> StreamHub {
        let mut channels = self.channels.lock();
        channels
            .entry(logical_id.to_owned())
            .or_insert_with(|| {
                let (sender, _receiver) = broadcast::channel(self.config.channel_capacity.max(1));
                StreamHub {
                    sender,
                    tail: Arc::new(AtomicU64::new(0)),
                }
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

    /// Restore a live ACP host for a persisted session after the Rust host was
    /// killed and restarted (S9a4b).
    ///
    /// `Ok(false)` means no session row exists: recovery must not create an
    /// identity for an unknown id. An idle row is attached with
    /// `session/load` (persisted root id + `cwd` + `mcpServers`); no turn is
    /// prompted, no queued input is replayed, and `session/resume` is never
    /// called. A row whose recorded turn was still running is refused as
    /// `session_resume_required`, because resuming an interrupted turn is a
    /// separate recovery path this stage does not implement.
    pub async fn ensure_live(&self, logical_id: &LogicalId) -> Result<bool, SwarmError> {
        if self.is_live(logical_id) {
            return Ok(true);
        }
        let _attach = self.attach_lock.lock().await;
        if self.is_live(logical_id) {
            return Ok(true);
        }
        let Some(row) = self
            .store
            .swarm_get_session(logical_id.as_str())
            .await
            .map_err(|error| store_failure("get_session", error))?
        else {
            return Ok(false);
        };
        if row.state == SessionState::Running {
            tracing::warn!(
                logical_id = logical_id.as_str(),
                "swarm session row still records an unfinished turn"
            );
            return Err(SwarmError::upstream_unavailable("session_resume_required"));
        }
        let model = self.resolve_model(row.model.as_deref())?;
        let workspace = self.reattach_workspace(&row).await?;
        let hub = self.hub_for(logical_id.as_str());
        let log = Arc::new(ItemLog::new(
            logical_id.as_str(),
            Arc::clone(&self.store),
            hub.sender.clone(),
            hub.tail,
        ));
        let session_store = self
            .config
            .session_store
            .clone()
            .unwrap_or_else(|| local_session_store(self.config.session_store_path.clone()));
        let child = AcpChild::attach(
            ChildConfig {
                cwd: PathBuf::from(&row.cwd),
                session_store,
                machine_id: self.config.machine_id,
                workspace_root: self.config.workspace_root.clone(),
                settings_json: self.config.settings_json.clone(),
                model,
                instructions: row.instructions.clone(),
                mcp_tools: row.mcp_tools.clone(),
                workspace,
                root_session_id: row.root_session_id.clone(),
            },
            log.clone(),
        )
        .await
        .map_err(|error| {
            tracing::warn!(
                logical_id = logical_id.as_str(),
                root_session_id = %row.root_session_id,
                error = %error,
                "swarm ACP reattach failed"
            );
            child_error(error)
        })?;
        if child.session_id() != row.root_session_id {
            let shutdown = child.shutdown().await;
            tracing::warn!(
                logical_id = logical_id.as_str(),
                expected = %row.root_session_id,
                report = shutdown.report,
                "session/load returned a different session identity"
            );
            return Err(SwarmError::upstream_unavailable("session_id_mismatch"));
        }
        let runtime = Arc::new(SessionRuntime::new(Arc::new(child), log, hub.sender));
        self.sessions
            .lock()
            .insert(row.logical_id, Arc::clone(&runtime));
        tracing::info!(
            logical_id = logical_id.as_str(),
            root_session_id = %row.root_session_id,
            "swarm reattached the persisted session"
        );
        Ok(true)
    }

    /// Rebuild the internal workspace binding from the stored row (S9a4b).
    ///
    /// The store binding is checked first: a missing binding must fail instead
    /// of reserving a fresh environment, which would silently move an old
    /// session onto a new workspace. `reserve` is the lazy Bun call; it reuses
    /// the running sandbox (same `runId`) and never starts SRT by itself.
    async fn reattach_workspace(
        &self,
        row: &SwarmSessionRow,
    ) -> Result<Option<SandboxMcpBinding>, SwarmError> {
        if row.environment.kind == EnvironmentKind::None {
            return Ok(None);
        }
        let Some(env_id) = row.environment.id.as_deref() else {
            return Err(SwarmError::upstream_unavailable("sandbox_binding_missing"));
        };
        let bound = self
            .store
            .swarm_get_binding(env_id)
            .await
            .map_err(|error| store_failure("get_binding", error))?
            .ok_or_else(|| SwarmError::upstream_unavailable("sandbox_binding_missing"))?;
        let client = self.config.sandbox.as_ref().ok_or_else(|| {
            SwarmError::new(
                ErrorKind::Unavailable,
                ErrorCode::UpstreamUnavailable,
                "openai_hosted requires the Bun sandbox bridge",
            )
            .with_details(json!({
                "field": "environment",
                "operation": "sandbox_bridge_unconfigured",
                "reason": "PERI_SWARM_BUN_BASE_URL is not set",
            }))
        })?;
        let reservation = client.reserve(env_id).await.map_err(sandbox_bridge_error)?;
        let same_sandbox = reservation.binding.sandbox_id == bound.sandbox_id;
        let same_workspace = reservation.binding.workspace_path.as_os_str()
            == std::ffi::OsStr::new(&bound.workspace_path);
        if !same_sandbox || !same_workspace {
            tracing::warn!(
                logical_env_id = env_id,
                stored_sandbox = %bound.sandbox_id,
                bridge_sandbox = %reservation.binding.sandbox_id,
                "swarm reattach refused a changed sandbox binding"
            );
            return Err(SwarmError::upstream_unavailable("sandbox_binding_changed"));
        }
        Ok(Some(reservation.binding))
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

    fn session_view(
        &self,
        row: &SwarmSessionRow,
        turn: Option<TurnSlot>,
        sandbox: Option<SandboxView>,
    ) -> SessionView {
        SessionView {
            id: row.logical_id.clone(),
            object: "agent.session".to_owned(),
            logical_id: row.logical_id.clone(),
            root_session_id: row.root_session_id.clone(),
            cwd: row.cwd.clone(),
            state: row.state,
            status: SessionStatus::project(row.state, turn.is_some()),
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
            sandbox,
        }
    }

    /// Project the booked Bun environment without contacting Bun.
    ///
    /// `swarm_bindings` is the authority for the sandbox identity and workspace
    /// path; a missing row keeps `sandbox: null` instead of failing the read.
    /// `runId` is the last run Bun reported at reservation time. `state` stays
    /// `null`: only the reservation response carries Bun's SRT lifecycle
    /// (`reserved`/`ready`) and the binding row does not persist it, so there is
    /// no durable fact to project (`design §4.1`).
    async fn sandbox_view(&self, row: &SwarmSessionRow) -> Result<Option<SandboxView>, SwarmError> {
        if row.environment.kind != EnvironmentKind::OpenAiHosted {
            return Ok(None);
        }
        let Some(env_id) = row.environment.id.as_deref() else {
            return Ok(None);
        };
        let binding = self
            .store
            .swarm_get_binding(env_id)
            .await
            .map_err(|error| store_failure("get_binding", error))?;
        Ok(binding.map(|binding| SandboxView {
            logical_env_id: binding.logical_env_id,
            sandbox_id: Some(binding.sandbox_id),
            run_id: binding.last_run_id,
            state: None,
            workspace_path: Some(binding.workspace_path),
        }))
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
        let sandbox = self.sandbox_view(&row).await?;
        Ok(self.session_view(&row, turn, sandbox))
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
    async fn run_turn(
        &self,
        runtime: &Arc<SessionRuntime>,
        text: &str,
        turn_id: &str,
    ) -> Result<(), SwarmError> {
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
        if let Err(error) = runtime.log.begin_turn(&request_id, turn_id).await {
            // The turn was opened at acceptance; execution cannot start, so do
            // not leave a turn that the stream can never terminate.
            runtime.log.abandon_pending_turn(turn_id);
            return Err(error);
        }
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

    /// Persist the create's initial input and either run it or defer it.
    ///
    /// The user item is written exactly once in both modes. A deferred turn
    /// carries the `turn_id` derived from that item, so running it later opens
    /// the same turn the synchronous path would and cannot duplicate the input.
    ///
    /// Admission follows [`Self::accept_input`] and happens **before** the user
    /// item is recorded: a running turn, or a turn accepted earlier that has not
    /// started yet, owns the session. Recording the initial input there would
    /// append an opener whose turn can never start (`run_turn` reports
    /// `turn_in_progress`), leaving a stray user item and no terminal on a
    /// streamed create. The in-memory fake answers `409` in the same situation
    /// (`design §4.3`).
    async fn accept_initial_input(
        &self,
        runtime: &Arc<SessionRuntime>,
        logical_id: &LogicalId,
        text: &str,
        defer: bool,
    ) -> Result<Option<PendingTurn>, SwarmError> {
        let item_id = {
            let _admission = runtime.admission.lock().await;
            // Same rule as `accept_input`: a running turn owns the session.
            if runtime.is_running() {
                return Err(SwarmError::turn_in_progress());
            }
            // A turn accepted earlier but not started yet owns it too; a second
            // opener would race its user item into that turn.
            if runtime.log.has_open_turn() {
                return Err(SwarmError::turn_in_progress());
            }
            let (_, item_id) = runtime.log.record_user_input(None, text).await?;
            item_id
        };
        let turn_id = turn_id_for_item(&item_id);
        if defer {
            return Ok(Some(PendingTurn {
                logical_id: logical_id.clone(),
                text: text.to_owned(),
                turn_id,
            }));
        }
        self.run_turn(runtime, text, &turn_id).await?;
        Ok(None)
    }

    /// Append an input to the running turn through the host user-input queue.
    ///
    /// `turn_id` belongs to the accepted user item; it is only used by the
    /// `epoch == 0` fallback, where the turn ended before the handoff and the
    /// input runs as a normal turn.
    async fn steer(
        &self,
        runtime: &Arc<SessionRuntime>,
        submission: &InputSubmission,
        logical_id: &LogicalId,
        turn_id: &str,
    ) -> Result<InputOutcome, SwarmError> {
        let text = submission.effective_text();
        // Record the handoff before dispatch: the dispatched input cancels the
        // in-flight attempt, and that attempt's `cancelled` terminal must not
        // close the turn. `0` means the turn ended before the handoff — the
        // input then runs as a normal turn instead of vanishing into the queue.
        let epoch = runtime.log.note_steer();
        if epoch == 0 {
            self.run_turn(runtime, &text, turn_id).await?;
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
    ///
    /// The window carries the custom `item` frames plus the official frames
    /// rebuilt from the same rows ([`crate::service::replay_backlog`]), so a
    /// reconnecting SDK observes the same `turn_id`/`event_id` sequence as a
    /// live stream.
    ///
    /// A tail subscription has no backlog **by definition**, so it is answered
    /// before the session row is read. `POST /sessions?stream=true` opens the
    /// stream before the row exists; requiring the row here made the actor
    /// answer `503`, and the create response then carried no first-turn events
    /// (pre-existing since `05d93fe1e`, fixed here). An explicit cursor still
    /// needs the row and reports `unknown_agent`.
    pub async fn replay_after(
        &self,
        logical_id: &LogicalId,
        start: EventCursor,
    ) -> Result<Vec<StreamEvent>, SwarmError> {
        if start.cursor().is_none() {
            return Ok(Vec::new());
        }
        let session = self.require_row(logical_id).await?;
        let live_open_turn = self
            .sessions
            .lock()
            .get(logical_id.as_str())
            .is_some_and(|runtime| runtime.log.has_open_turn());
        crate::service::replay_backlog(self.store.as_ref(), &session, start, live_open_turn).await
    }

    /// Resolve the session environment and reserve its Bun workspace (S8a3a).
    ///
    /// `none` never contacts Bun. `openai_hosted` requires a configured bridge;
    /// a caller-supplied `environment_id` must already exist in
    /// `swarm_bindings`, and an anonymous environment mints `env_<uuid>`. The
    /// reservation writes no credential: the outer Bun token stays in the
    /// deployment configuration and the inner token never leaves Bun.
    async fn bind_environment(
        &self,
        command: &CreateSessionCommand,
    ) -> Result<(EnvironmentRef, Option<SandboxMcpBinding>), SwarmError> {
        if command.environment == EnvironmentKind::None {
            return Ok((EnvironmentRef::none(&command.logical_id), None));
        }

        // The injected entry owns the ACP `workspace` name. A caller using the
        // same label is a real duplicate, not a policy refusal; fail before
        // touching the bridge so no reservation is leaked.
        if let Some(tool) = command
            .mcp_tools
            .iter()
            .find(|tool| tool.server_label == WORKSPACE_MCP_SERVER_NAME)
        {
            return Err(SwarmError::new(
                ErrorKind::Conflict,
                ErrorCode::IdentityConflict,
                "server_label \"workspace\" is already bound to the session workspace",
            )
            .with_details(json!({
                "field": "tools",
                "serverLabel": tool.server_label,
                "reason": "duplicate_server_label",
            })));
        }

        let client = self.config.sandbox.as_ref().ok_or_else(|| {
            SwarmError::new(
                ErrorKind::Unavailable,
                ErrorCode::UpstreamUnavailable,
                "openai_hosted requires the Bun sandbox bridge",
            )
            .with_details(json!({
                "field": "environment",
                "operation": "sandbox_bridge_unconfigured",
                "reason": "PERI_SWARM_BUN_BASE_URL is not set",
            }))
        })?;

        let logical_env_id = match command.environment_id.as_deref() {
            Some(requested) => {
                if !crate::sandbox::is_valid_logical_env_id(requested) {
                    return Err(SwarmError::invalid_input(
                        "environment.environment_id must match [A-Za-z0-9][A-Za-z0-9._-]{0,127}",
                    )
                    .with_details(json!({
                        "field": "environment.environment_id",
                        "received": requested,
                    })));
                }
                let bound = self
                    .store
                    .swarm_get_binding(requested)
                    .await
                    .map_err(|error| store_failure("get_binding", error))?;
                if bound.is_none() {
                    return Err(SwarmError::invalid_input(
                        "environment.environment_id is not bound in this deployment",
                    )
                    .with_details(json!({
                        "field": "environment.environment_id",
                        "received": requested,
                        "reason": "not_bound",
                    })));
                }
                requested.to_owned()
            }
            None => format!("env_{}", Uuid::now_v7()),
        };

        let reservation = client
            .reserve(&logical_env_id)
            .await
            .map_err(sandbox_bridge_error)?;
        let workspace_root = reservation
            .binding
            .workspace_path
            .parent()
            .map_or_else(String::new, |path| path.to_string_lossy().into_owned());
        self.store
            .swarm_put_binding(crate::store::SwarmBindingRow {
                logical_env_id: logical_env_id.clone(),
                logical_id: command.logical_id.as_str().to_owned(),
                sandbox_id: reservation.binding.sandbox_id.clone(),
                workspace_path: reservation
                    .binding
                    .workspace_path
                    .to_string_lossy()
                    .into_owned(),
                sandbox_root: workspace_root,
                settings_profile: None,
                token_file: None,
                last_run_id: reservation.run_id,
                last_used_at: reservation
                    .last_used_at
                    .unwrap_or_else(peri_time::now_utc_rfc3339),
            })
            .await
            .map_err(|error| store_failure("put_binding", error))?;

        Ok((
            EnvironmentRef {
                kind: EnvironmentKind::OpenAiHosted,
                id: Some(logical_env_id),
            },
            Some(reservation.binding),
        ))
    }

    async fn spawn_runtime(
        &self,
        request: SpawnRequest<'_>,
    ) -> Result<Arc<SessionRuntime>, SwarmError> {
        let hub = self.hub_for(request.logical_id.as_str());
        let log = Arc::new(ItemLog::new(
            request.logical_id.as_str(),
            Arc::clone(&self.store),
            hub.sender.clone(),
            hub.tail,
        ));
        let session_store = self
            .config
            .session_store
            .clone()
            .unwrap_or_else(|| local_session_store(self.config.session_store_path.clone()));
        let root_session_id = request.root_session_id;
        let child = AcpChild::spawn(
            ChildConfig {
                cwd: request.cwd.to_path_buf(),
                session_store,
                machine_id: self.config.machine_id,
                workspace_root: self.config.workspace_root.clone(),
                settings_json: self.config.settings_json.clone(),
                model: request.model,
                instructions: request.instructions,
                mcp_tools: request.mcp_tools.to_vec(),
                workspace: request.workspace.cloned(),
                root_session_id: root_session_id.clone(),
            },
            Arc::clone(&log),
        )
        .await
        .map_err(|error| {
            tracing::warn!(
                logical_id = request.logical_id.as_str(),
                error = %error,
                "swarm ACP host assembly failed"
            );
            child_error(error)
        })?;
        if child.session_id() != root_session_id {
            let shutdown = child.shutdown().await;
            tracing::warn!(
                logical_id = request.logical_id.as_str(),
                expected = %root_session_id,
                report = shutdown.report,
                "reserved root session id was not honored"
            );
            return Err(SwarmError::upstream_unavailable("session_id_mismatch"));
        }
        Ok(Arc::new(SessionRuntime::new(
            Arc::new(child),
            log,
            hub.sender,
        )))
    }

    /// Reconcile a create that was interrupted after its row was persisted.
    ///
    /// The row's `root_session_id` is the only identity this path may use. The
    /// row stays `Creating` until a host is live, so an interrupted attempt is
    /// visible instead of being mistaken for an idle session.
    async fn recover_creating_row(
        &self,
        command: &CreateSessionCommand,
        row: &SwarmSessionRow,
        model: Option<String>,
    ) -> Result<CreateSessionResult, SwarmError> {
        let runtime = self.live_for_creating_row(command, row, model).await?;
        // Durable progress, not the row copy: an attach can race an interaction
        // that is already appending items.
        let last_seq = self.last_seq(&command.logical_id).await?;
        // A concurrent interaction may already own a turn on the attached host;
        // promoting it to `idle` would hide that turn from `GET /sessions`.
        let live_state = if runtime.is_running() {
            SessionState::Running
        } else {
            SessionState::Idle
        };
        self.store
            .swarm_update_session(SwarmSessionUpdate {
                logical_id: row.logical_id.clone(),
                state: live_state,
                last_item_seq: last_seq,
                last_activity_at: peri_time::now_utc_rfc3339(),
            })
            .await
            .map_err(|error| store_failure("update_session", error))?;
        // The initial input runs at most once: a persisted item means the
        // interrupted attempt already recorded it, so replaying it here would
        // duplicate the user message and its turn.
        let mut pending_initial = None;
        if last_seq == 0 {
            if let Some(text) = command.initial_input.as_deref() {
                pending_initial = self
                    .accept_initial_input(
                        &runtime,
                        &command.logical_id,
                        text,
                        command.defer_initial_turn,
                    )
                    .await?;
            }
        }
        let session = self.view_by_id(&command.logical_id).await?;
        Ok(CreateSessionResult {
            session,
            created: false,
            pending_initial,
        })
    }

    /// Attach to the persisted root id, and only create the Peri session when
    /// the interrupted attempt provably never reached `session/new`.
    ///
    /// `session/new` reusing an existing id is refused by the host (`-32010`),
    /// so a failed create is followed by one more attach before the failure is
    /// reported. Every failure keeps the row `Creating`.
    async fn live_for_creating_row(
        &self,
        command: &CreateSessionCommand,
        row: &SwarmSessionRow,
        model: Option<String>,
    ) -> Result<Arc<SessionRuntime>, SwarmError> {
        let attach_error = match self.ensure_live(&command.logical_id).await {
            Ok(true) => {
                return self
                    .runtime(&command.logical_id)
                    .map_err(|_| SwarmError::upstream_unavailable("session_not_live"))
            }
            Ok(false) => return Err(SwarmError::unknown_agent(command.logical_id.as_str())),
            Err(error) => error,
        };
        tracing::warn!(
            logical_id = command.logical_id.as_str(),
            root_session_id = %row.root_session_id,
            error = %attach_error,
            "swarm interrupted create could not attach the persisted session"
        );
        let workspace = self.reattach_workspace(row).await?;
        let spawn_error = match self
            .spawn_runtime(SpawnRequest {
                logical_id: &command.logical_id,
                cwd: Path::new(&row.cwd),
                model,
                instructions: row.instructions.clone(),
                mcp_tools: &row.mcp_tools,
                workspace: workspace.as_ref(),
                root_session_id: row.root_session_id.clone(),
            })
            .await
        {
            Ok(runtime) => {
                self.sessions
                    .lock()
                    .insert(row.logical_id.clone(), Arc::clone(&runtime));
                return Ok(runtime);
            }
            Err(error) => error,
        };
        // The interrupted attempt may have created the session before dying;
        // the attach that just failed can succeed once the store settles.
        match self.ensure_live(&command.logical_id).await {
            Ok(true) => self
                .runtime(&command.logical_id)
                .map_err(|_| SwarmError::upstream_unavailable("session_not_live")),
            _ => Err(create_recovery_failure(row, &attach_error, &spawn_error)),
        }
    }
}

#[async_trait]
impl SwarmService for AcpSwarmService {
    async fn create_session(
        &self,
        command: CreateSessionCommand,
    ) -> Result<CreateSessionResult, SwarmError> {
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
            check_environment_identity(&row.environment, &command)?;
            if row.state == SessionState::Creating {
                // The previous attempt persisted the identity before starting
                // the Peri session and never reached `Idle`; reconcile against
                // that row instead of creating a second identity.
                return self.recover_creating_row(&command, &row, model).await;
            }
            // A host restart leaves the row but no in-process child; attach it
            // with the persisted identity before running an initial input
            // (`design §6.4-2`, S9a4b). `ensure_live` never prompts or replays.
            if !self.ensure_live(&command.logical_id).await? {
                tracing::warn!(
                    logical_id = command.logical_id.as_str(),
                    "session row disappeared during reattach"
                );
                return Err(SwarmError::upstream_unavailable("session_not_live"));
            }
            let runtime = self
                .runtime(&command.logical_id)
                .map_err(|_| SwarmError::upstream_unavailable("session_not_live"))?;
            let mut pending_initial = None;
            if let Some(text) = command.initial_input.as_deref() {
                pending_initial = self
                    .accept_initial_input(
                        &runtime,
                        &command.logical_id,
                        text,
                        command.defer_initial_turn,
                    )
                    .await?;
            }
            let session = self.view_by_id(&command.logical_id).await?;
            return Ok(CreateSessionResult {
                session,
                created: false,
                pending_initial,
            });
        }

        // Resolve the identity before any reservation: a caller-supplied id the
        // host can never honor must not leave a workspace binding behind.
        let root_session_id = match command.root_session_id.as_deref() {
            Some(reserved) => {
                validate_root_session_id(reserved)?;
                reserved.to_owned()
            }
            None => new_root_session_id(),
        };
        // S8a3a: reserve (never start) the Bun workspace before the host is
        // assembled, so `session/new` already carries the workspace entry.
        let (environment, workspace) = self.bind_environment(&command).await?;
        let now = peri_time::now_utc_rfc3339();
        let row = SwarmSessionRow {
            logical_id: command.logical_id.as_str().to_owned(),
            root_session_id: root_session_id.clone(),
            actor_key: ActorKey::new(command.logical_id.clone()).storage_key(),
            cwd: cwd.clone(),
            state: SessionState::Creating,
            model: model.clone(),
            instructions: command.instructions.clone(),
            mcp_tools: command.mcp_tools.clone(),
            environment,
            created_at: now.clone(),
            last_activity_at: now,
            last_item_seq: 0,
        };
        // Persist the reserved identity **before** `session/new` (`design §3`).
        // A crash between the two writes then leaves a row that names the root
        // id, and the retry reconciles against it instead of minting a second
        // one. The row is also the precondition for starting a host: without an
        // addressable mapping there is no session to create.
        if let Err(error) = self.store.swarm_upsert_session(row.clone()).await {
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
        let runtime = match self
            .spawn_runtime(SpawnRequest {
                logical_id: &command.logical_id,
                cwd: &command.cwd,
                model: model.clone(),
                instructions: command.instructions.clone(),
                mcp_tools: &command.mcp_tools,
                workspace: workspace.as_ref(),
                root_session_id: root_session_id.clone(),
            })
            .await
        {
            Ok(runtime) => runtime,
            Err(error) => {
                // Keep the `Creating` row: it is the durable record of the
                // reserved id, so the retry addresses the same session and
                // never re-derives an identity from a fresh UUID.
                tracing::warn!(
                    logical_id = command.logical_id.as_str(),
                    root_session_id = %root_session_id,
                    "swarm session create failed after the identity was persisted; the row stays creating"
                );
                return Err(error);
            }
        };
        if let Err(error) = self
            .store
            .swarm_update_session(SwarmSessionUpdate {
                logical_id: row.logical_id.clone(),
                state: SessionState::Idle,
                last_item_seq: row.last_item_seq,
                last_activity_at: peri_time::now_utc_rfc3339(),
            })
            .await
        {
            // The host is not registered in `sessions` yet; shutting it down
            // avoids leaking an untracked child. The row stays `Creating` and a
            // retry attaches to the now existing Peri session.
            let shutdown = runtime.child.shutdown().await;
            tracing::warn!(
                logical_id = command.logical_id.as_str(),
                report = shutdown.report,
                "swarm session row promotion failed; host shut down"
            );
            return Err(store_failure("update_session", error));
        }
        self.sessions
            .lock()
            .insert(command.logical_id.as_str().to_owned(), Arc::clone(&runtime));

        let mut pending_initial = None;
        if let Some(text) = command.initial_input.as_deref() {
            pending_initial = self
                .accept_initial_input(
                    &runtime,
                    &command.logical_id,
                    text,
                    command.defer_initial_turn,
                )
                .await?;
        }
        let session = self.view_by_id(&command.logical_id).await?;
        Ok(CreateSessionResult {
            session,
            created: true,
            pending_initial,
        })
    }

    async fn get_session(&self, logical_id: &LogicalId) -> Result<SessionView, SwarmError> {
        self.view_by_id(logical_id).await
    }

    async fn accept_input(
        &self,
        logical_id: &LogicalId,
        submission: InputSubmission,
    ) -> Result<AcceptedInput, SwarmError> {
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
                return Ok(AcceptedInput::settled(InputOutcome::new(
                    input_id,
                    Disposition::Existing,
                    submission.mode,
                    last_item_seq,
                )));
            }
            return Err(SwarmError::input_conflict(&input_id));
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
                return Ok(AcceptedInput::settled(InputOutcome::new(
                    input_id,
                    Disposition::Existing,
                    submission.mode,
                    last_item_seq,
                )));
            }
            Err(error) => {
                tracing::warn!(error = %error, "swarm input receipt write failed");
                return Err(SwarmError::queue_unavailable("put_input"));
            }
        }

        // Admission is serialized so two concurrent accepts cannot both see an
        // idle session and attribute their user items to different turns. The
        // lock is released before any prompt/queue work runs.
        let (running, turn_id) = {
            let _admission = runtime.admission.lock().await;
            let running = runtime.is_running();
            if submission.kind == InputKind::User && running && submission.mode == InputMode::Turn {
                return Err(SwarmError::turn_in_progress());
            }
            // A previously accepted turn that has not started yet owns the
            // session; accepting a second one would race its user item into the
            // first turn.
            if submission.kind == InputKind::User && !running && runtime.log.has_open_turn() {
                return Err(SwarmError::turn_in_progress());
            }
            let mut turn_id = None;
            if submission.kind == InputKind::User {
                // Recorded at acceptance. The host echoes `user_message_chunk`,
                // but the projection drops that echo, so this is the only user
                // frame in the log. A retried `inputId` returns `Existing` above
                // and never reaches this branch, so the item is appended once.
                let (_, item_id) = runtime
                    .log
                    .record_user_input(Some(&input_id), &submission.effective_text())
                    .await?;
                turn_id = Some(turn_id_for_item(&item_id));
            }
            (running, turn_id)
        };

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
            return Ok(AcceptedInput::settled(InputOutcome::new(
                input_id,
                Disposition::Inserted,
                submission.mode,
                last_item_seq,
            )));
        }

        if running {
            let turn_id = turn_id.as_deref().unwrap_or_default();
            let outcome = self
                .steer(&runtime, &submission, logical_id, turn_id)
                .await?;
            return Ok(AcceptedInput::settled(outcome));
        }

        let Some(turn_id) = turn_id else {
            // A non-user input never opens a turn; the branch above returned it.
            let last_item_seq = self.last_seq(logical_id).await?;
            return Ok(AcceptedInput::settled(InputOutcome::new(
                input_id,
                Disposition::Inserted,
                submission.mode,
                last_item_seq,
            )));
        };
        // `202` is sent now; the execution runs out-of-band.
        let last_item_seq = self.last_seq(logical_id).await?;
        Ok(AcceptedInput {
            outcome: InputOutcome::new(
                input_id,
                Disposition::Inserted,
                submission.mode,
                last_item_seq,
            ),
            pending_turn: Some(PendingTurn {
                logical_id: logical_id.clone(),
                text: submission.effective_text(),
                turn_id,
            }),
        })
    }

    async fn run_pending_turn(&self, pending: PendingTurn) -> Result<(), SwarmError> {
        let runtime = self.runtime(&pending.logical_id)?;
        self.run_turn(&runtime, &pending.text, &pending.turn_id)
            .await
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
        // The official `CursorPage` sends the last item's `id` as `after`; map
        // it to that item's greatest seq before querying. The store then pages
        // by item, so a long message cannot fill the row budget.
        let after = resolve_window_after(self.store.as_ref(), logical_id, &window).await?;
        let page = self
            .store
            .swarm_item_page(logical_id.as_str(), window.order, after, window.limit)
            .await
            .map_err(|error| store_failure("item_page", error))?;
        let items = page.rows.into_iter().map(|row| row.to_view()).collect();
        let data = page.items.into_iter().map(|row| row.to_view()).collect();
        Ok(ItemPage::new(
            logical_id.as_str(),
            items,
            data,
            page.next_cursor,
            last_item_seq,
            page.has_more,
            window.order,
        ))
    }

    async fn subscribe(
        &self,
        logical_id: &LogicalId,
        start: EventCursor,
    ) -> Result<EventSubscription, SwarmError> {
        self.require_row(logical_id).await?;
        let runtime = self.runtime(logical_id)?;
        // Receiver first, then backlog: no event can slip between replay and
        // live delivery.
        let live = runtime.channel.subscribe();
        let backlog = self.replay_after(logical_id, start).await?;
        let tail = runtime.log.tail();
        // A host restart rebuilds the log with an empty tail; the store is the
        // authority for items persisted before this process opened a stream.
        tail.fetch_max(self.last_seq(logical_id).await?, Ordering::Relaxed);
        Ok(EventSubscription::new(backlog, live, tail))
    }

    async fn open_stream(&self, logical_id: &LogicalId) -> EventSubscription {
        let hub = self.hub_for(logical_id.as_str());
        // Best effort: a new session has no items; an existing one may have
        // history from before this stream was opened (`stream:true` create).
        if let Ok(last_seq) = self.store.swarm_last_seq(logical_id.as_str()).await {
            hub.tail.fetch_max(last_seq, Ordering::Relaxed);
        }
        let live = hub.sender.subscribe();
        EventSubscription::new(Vec::new(), live, hub.tail)
    }
}

/// A caller-reserved root id must be the shape Peri accepts (UUIDv7).
///
/// Validating before the row is written keeps a refused identity out of the
/// store: a `creating` row the host can never honor would make every retry fail
/// the same way with no way forward.
fn validate_root_session_id(root_session_id: &str) -> Result<(), SwarmError> {
    let valid = Uuid::parse_str(root_session_id)
        .is_ok_and(|id| id.get_version() == Some(uuid::Version::SortRand));
    if valid {
        return Ok(());
    }
    Err(SwarmError::invalid_input("rootSessionId must be a UUIDv7")
        .with_details(json!({ "field": "rootSessionId" })))
}

/// One diagnosable failure for a create that could neither attach nor create
/// its persisted identity.
///
/// Upstream messages are not echoed (they may embed provider payloads); the
/// contract codes and details stay inspectable so the caller can tell an
/// unreachable host from a refused identity.
fn create_recovery_failure(
    row: &SwarmSessionRow,
    attach: &SwarmError,
    spawn: &SwarmError,
) -> SwarmError {
    tracing::warn!(
        logical_id = row.logical_id.as_str(),
        root_session_id = %row.root_session_id,
        attach = %attach,
        spawn = %spawn,
        "swarm interrupted create could neither attach nor create the peri session"
    );
    SwarmError::upstream_unavailable("create_recovery_failed").with_details(json!({
        "logicalId": row.logical_id,
        "rootSessionId": row.root_session_id,
        "attach": { "code": attach.code().as_str(), "details": attach.details() },
        "spawn": { "code": spawn.code().as_str(), "details": spawn.details() },
    }))
}

/// A retried create must address the same environment identity; changing the
/// kind or the explicit environment id is a real identity conflict.
fn check_environment_identity(
    row: &EnvironmentRef,
    command: &CreateSessionCommand,
) -> Result<(), SwarmError> {
    let conflict = |field: &str| {
        SwarmError::new(
            ErrorKind::Conflict,
            ErrorCode::IdentityConflict,
            "logicalId already exists with a different environment",
        )
        .with_details(json!({ "field": field }))
    };
    if row.kind != command.environment {
        return Err(conflict("environment"));
    }
    if let (Some(bound), Some(requested)) = (row.id.as_deref(), command.environment_id.as_deref()) {
        if bound != requested {
            return Err(conflict("environment.environment_id"));
        }
    }
    Ok(())
}

/// Map a Bun bridge failure to the HTTP contract without echoing credentials or
/// the bridge response body.
fn sandbox_bridge_error(error: SandboxClientError) -> SwarmError {
    tracing::warn!(error = %error, "bun sandbox bridge call failed");
    let (status, upstream_code, reason) = match error {
        SandboxClientError::Unavailable(_) => (None, None, "unreachable"),
        SandboxClientError::Rejected {
            status,
            code,
            message: _,
        } => (Some(status), Some(code), "rejected"),
        SandboxClientError::Invalid(_) => (None, None, "invalid_response"),
    };
    let mut details = json!({
        "operation": "sandbox_reserve",
        "reason": reason,
    });
    if let Some(status) = status {
        details["upstreamStatus"] = json!(status);
    }
    if let Some(code) = upstream_code {
        details["upstreamCode"] = json!(code);
    }
    SwarmError::upstream_unavailable("sandbox_reserve").with_details(details)
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
