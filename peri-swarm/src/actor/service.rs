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

use super::reader::{
    ApprovalClaim, ApprovalCompletion, ApprovalReceipt, ItemLog, SteerReservation, TurnOutcome,
};
use super::runtime::{
    local_session_store, AcpChild, ChildConfig, ChildError, ChildShutdown, SessionStoreDeployment,
    WORKSPACE_MCP_SERVER_NAME,
};
use crate::domain::{
    new_root_session_id, turn_id_for_item, ActorKey, ApprovalDecision, Disposition,
    EnvironmentKind, EnvironmentRef, ErrorCode, ErrorKind, EventCursor, InputKind, InputMode,
    InputOutcome, InputState, InputSubmission, ItemPage, ItemsWindow, LogicalId, McpToolSpec,
    OfficialTurn, SandboxView, SessionPage, SessionState, SessionView, SessionsWindow, StreamEvent,
    SwarmError, TurnPage, TurnView, TurnsWindow,
};
use crate::sandbox::{BunSandboxClient, SandboxClientError, SandboxMcpBinding};
use crate::service::official_items::item_page;
use crate::service::{
    AcceptedInput, CreateSessionCommand, CreateSessionResult, EventSubscription, PendingTurn,
    SessionUpdateCommand, SwarmService,
};
use crate::store::{
    store_failure, InputPut, SwarmBindingRow, SwarmInputRow, SwarmSessionPatch, SwarmSessionRow,
    SwarmSessionUpdate, SwarmStore,
};

/// Upper bound for a steered turn to be continued by the host's queued run.
///
/// The continuation is a full agent run (tools, child agents), so the bound is
/// only a safety net against a lost terminal, not a turn deadline.
const STEER_CONTINUATION_TIMEOUT: Duration = Duration::from_secs(300);

/// Upper bound while an attempt-level prompt reply (no `stopReason`) waits for
/// the turn's real terminal. The agent may legitimately run for minutes, so
/// this is only a safety net against a lost `peri/agent_event_done`.
const ATTEMPT_REPLY_TERMINAL_TIMEOUT: Duration = Duration::from_secs(1800);

/// How long `cancel_turn` waits for the host to leave the slot after
/// `session/cancel`.
///
/// A cancel that was delivered is delivered even if the host does not confirm
/// in time: the 202 still means "delivered", and a same-key replay re-sends the
/// notification while the target turn still owns the slot.
const CANCEL_SETTLE_WAIT: Duration = Duration::from_secs(30);

/// Test-only override for [`CANCEL_SETTLE_WAIT`] (milliseconds, 0 = default).
#[cfg(test)]
static CANCEL_SETTLE_WAIT_MS: AtomicU64 = AtomicU64::new(0);

fn cancel_settle_wait() -> Duration {
    #[cfg(test)]
    match CANCEL_SETTLE_WAIT_MS.load(Ordering::Relaxed) {
        0 => CANCEL_SETTLE_WAIT,
        ms => Duration::from_millis(ms),
    }
    #[cfg(not(test))]
    CANCEL_SETTLE_WAIT
}

/// Shorten the settle wait for one test (unit tests only).
#[cfg(test)]
pub(crate) fn set_cancel_settle_wait(wait: Duration) {
    CANCEL_SETTLE_WAIT_MS.store(
        u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
        Ordering::Relaxed,
    );
}

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
    /// Serializes one steer's reservation with its handoff: `note_steer_for` →
    /// `session/input/enqueue` → `session/input/dispatch`, and the rollback
    /// when the handoff fails. The reader claims the oldest unclaimed
    /// generation for the next run that starts, so the reservation order must
    /// be the enqueue order; without this lock two concurrent steers can
    /// enqueue in the opposite order and the earlier dispatch's rollback can
    /// no longer release the generation the later run claimed, leaving that
    /// generation unresolved forever.
    ///
    /// The reservation also names the turn the input was admitted into, and
    /// the check runs under this lock, because the turn can close and a
    /// replacement start while a steer waits here; a steer admitted for the
    /// closed turn must not be dispatched into its replacement.
    ///
    /// 调用方在释放 `admission` 后排队取锁，再把 guard 交给独立任务。
    /// 此锁不覆盖模型 turn；`NoTurn` 也要先释放它才能重新进入 `admission`。
    steer: Arc<AsyncMutex<()>>,
    /// 仅供测试观察准入快照已读取；生产路径没有此通知，不改变生产时序。
    #[cfg(test)]
    steer_waiting: tokio::sync::Notify,
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
            steer: Arc::new(AsyncMutex::new(())),
            #[cfg(test)]
            steer_waiting: tokio::sync::Notify::new(),
            turn,
        }
    }

    async fn ready_for_input(&self) -> Result<(), SwarmError> {
        if self.child.wait_for_recovery().await.map_err(child_error)? {
            return Ok(());
        }
        Err(SwarmError::new(
            ErrorKind::Conflict,
            ErrorCode::TurnInProgress,
            "the recovered session is processing durable work; retry this input after the turn",
        ))
    }

    fn is_running(&self) -> bool {
        self.turn.borrow().is_some()
    }
}

/// `409` for `sessions.update`/`sessions.delete`, or `None` when the mutation is
/// admitted.
///
/// `suggest=cancel` is only a real entry point while an admitted slot exists:
/// `accept_input` sends `session/cancel` only for that slot. The recovery window
/// (`child.recovering()`) and an open turn whose slot is already gone point at
/// `retry_later`: the former lets the next input wait for the recovery terminal,
/// while the latter only rejects new input until the host writes the terminal.
fn mutation_blocked(runtime: &SessionRuntime, logical_id: &LogicalId) -> Option<SwarmError> {
    if !(runtime.is_running() || runtime.log.has_open_turn() || runtime.child.recovering()) {
        return None;
    }
    Some(if runtime.is_running() {
        SwarmError::turn_in_progress_for_mutation(logical_id.as_str())
    } else {
        SwarmError::turn_in_progress_awaiting_recovery(logical_id.as_str())
    })
}

/// Keep the safety net alive when `finish_turn` deferred the close to a steer.
/// The retry runs detached, so cancelling the prompt future cannot drop the
/// only closer of the turn. It first waits one grace period for a continuation
/// terminal, so a steer that already took over is not pre-empted.
fn arm_deferred_close(runtime: &Arc<SessionRuntime>, request_id: &str, outcome: TurnOutcome) {
    if runtime.log.active_turn().as_deref() != Some(request_id) {
        return;
    }
    let runtime = Arc::clone(runtime);
    let request_id = request_id.to_owned();
    tokio::spawn(async move {
        runtime
            .log
            .close_lost_turn_after_grace(&request_id, outcome, STEER_CONTINUATION_TIMEOUT)
            .await;
    });
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
    /// Accepted cancels whose target turn still holds the slot, keyed by
    /// `(logicalId, inputId)`.
    ///
    /// A cancel receipt is written only after `session/cancel` was delivered.
    /// If the host does not leave the slot within the wait bound, a same-key
    /// replay must deliver the notification again — but only to that same turn.
    /// The map is live-only: an actor restart drops it, and a new actor never
    /// re-delivers a cancel addressed to a host that is gone.
    accepted_cancels: Mutex<HashMap<(String, String), String>>,
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
            accepted_cancels: Mutex::new(HashMap::new()),
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

    fn require_current_runtime(
        &self,
        logical_id: &LogicalId,
        admitted: &Arc<SessionRuntime>,
    ) -> Result<(), SwarmError> {
        if self
            .sessions
            .lock()
            .get(logical_id.as_str())
            .is_some_and(|current| Arc::ptr_eq(current, admitted))
        {
            return Ok(());
        }
        Err(SwarmError::upstream_unavailable("session_not_live"))
    }

    /// Attach the persisted identity on an explicit interaction. A lost
    /// synchronous turn fails and only ACP-owned durable work can continue.
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
        let interrupted =
            crate::service::persisted_turn_resume_required(self.store.as_ref(), &row).await?;
        let model = self.resolve_model(row.model.as_deref())?;
        let workspace = self.reattach_workspace(&row).await?;
        let hub = self.hub_for(logical_id.as_str());
        let log = Arc::new(
            ItemLog::new(
                logical_id.as_str(),
                Arc::clone(&self.store),
                hub.sender.clone(),
                hub.tail,
            )
            .with_approval_declarations(&row.mcp_tools),
        );
        let session_store = self
            .config
            .session_store
            .clone()
            .unwrap_or_else(|| local_session_store(self.config.session_store_path.clone()));
        let config = ChildConfig {
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
        };
        let child = if interrupted {
            AcpChild::attach_interrupted(config, log.clone()).await
        } else {
            AcpChild::attach(config, log.clone()).await
        }
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

    async fn existing_input(
        store: &dyn SwarmStore,
        logical_id: &LogicalId,
        submission: &InputSubmission,
        fingerprint: &str,
    ) -> Result<Option<AcceptedInput>, SwarmError> {
        let input_id = &submission.input_id;
        let Some(existing) = store
            .swarm_get_input(logical_id.as_str(), input_id)
            .await
            .map_err(|error| store_failure("get_input", error))?
        else {
            return Ok(None);
        };
        if existing.fingerprint != fingerprint {
            return Err(SwarmError::input_conflict(input_id));
        }
        let outcome =
            Self::input_outcome(store, logical_id, submission, Disposition::Existing).await?;
        Ok(Some(AcceptedInput::settled(outcome)))
    }

    async fn put_input(
        store: &dyn SwarmStore,
        logical_id: &LogicalId,
        submission: &InputSubmission,
        fingerprint: &str,
    ) -> Result<Disposition, SwarmError> {
        let receipt = SwarmInputRow {
            logical_id: logical_id.as_str().to_owned(),
            input_id: submission.input_id.clone(),
            fingerprint: fingerprint.to_owned(),
            state: InputState::Accepted,
            created_at: peri_time::now_utc_rfc3339(),
        };
        match store.swarm_put_input(receipt).await {
            Ok(InputPut::Inserted) => Ok(Disposition::Inserted),
            Ok(InputPut::Existing(existing)) if existing.fingerprint == fingerprint => {
                Ok(Disposition::Existing)
            }
            Ok(InputPut::Existing(_)) => Err(SwarmError::input_conflict(&submission.input_id)),
            Err(error) => {
                tracing::warn!(error = %error, "swarm input receipt write failed");
                Err(SwarmError::queue_unavailable("put_input"))
            }
        }
    }

    async fn input_outcome(
        store: &dyn SwarmStore,
        logical_id: &LogicalId,
        submission: &InputSubmission,
        disposition: Disposition,
    ) -> Result<InputOutcome, SwarmError> {
        Ok(InputOutcome::new(
            submission.input_id.clone(),
            disposition,
            submission.mode,
            Self::last_seq_in(store, logical_id).await?,
        ))
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
        Self::last_seq_in(self.store.as_ref(), logical_id).await
    }

    async fn last_seq_in(
        store: &dyn SwarmStore,
        logical_id: &LogicalId,
    ) -> Result<u64, SwarmError> {
        store
            .swarm_last_seq(logical_id.as_str())
            .await
            .map_err(|error| store_failure("last_seq", error))
    }

    fn session_view(
        &self,
        row: &SwarmSessionRow,
        turn: Option<TurnSlot>,
        sandbox: Option<SandboxView>,
        required_actions: Vec<Value>,
    ) -> SessionView {
        crate::service::project_session(
            row,
            turn.map(|slot| TurnView {
                request_id: Some(slot.request_id),
                started_at: Some(slot.started_at),
            }),
            sandbox,
            required_actions,
        )
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
        crate::service::sandbox_view(self.store.as_ref(), row).await
    }

    async fn view_by_id(&self, logical_id: &LogicalId) -> Result<SessionView, SwarmError> {
        let mut row = self.require_row(logical_id).await?;
        // Make the persisted approval facts catch up with the host deadline
        // before the row is projected.
        let runtime = self.sessions.lock().get(logical_id.as_str()).cloned();
        if let Some(runtime) = &runtime {
            if runtime.child.recovering() {
                row.state = SessionState::Running;
            }
            runtime.log.settle_expired_approvals().await?;
        }
        // Re-read the sequence so a concurrently appended item is reflected.
        let last_item_seq = self.last_seq(logical_id).await?;
        row.last_item_seq = last_item_seq;
        let turn = runtime.and_then(|runtime| runtime.turn.borrow().clone());
        let sandbox = self.sandbox_view(&row).await?;
        let required_actions = self.live_required_actions(logical_id);
        Ok(self.session_view(&row, turn, sandbox, required_actions))
    }

    /// Current pending approvals of the live runtime (empty when not live).
    fn live_required_actions(&self, logical_id: &LogicalId) -> Vec<Value> {
        self.sessions
            .lock()
            .get(logical_id.as_str())
            .map_or_else(Vec::new, |runtime| runtime.log.required_actions())
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
                let steered = runtime.log.steer_pending();
                // A steer cancels this attempt on purpose and the host continues
                // the turn as a queued run; that run owns the terminal now, so
                // the turn slot stays occupied until it reports one.
                if steered
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
                    // A steered attempt that was cancelled reports no stop
                    // reason; leaving the turn open would reject every later
                    // input with `turn_in_progress`, so close it the same way
                    // the unsteered timeout does.
                    let outcome = match reply.stop_reason.clone() {
                        Some(stop_reason) => TurnOutcome::Completed(stop_reason),
                        None => TurnOutcome::Failed {
                            code: -32603,
                            message: "steer continuation reported no terminal".to_owned(),
                        },
                    };
                    // Arm the detached closer before the first
                    // `finish_turn().await`: if `settle` is dropped with this
                    // future, the turn still has a closer. The task waits for a
                    // continuation terminal first and only force-closes after
                    // its grace.
                    //
                    // This does not cover the `wait_turn_closed` above: a
                    // cancel during that (300s/1800s) wait leaves the turn with
                    // no closer, an older window this change does not widen.
                    arm_deferred_close(runtime, &request_id, outcome.clone());
                    runtime.log.finish_turn(&request_id, outcome).await;
                } else if !steered {
                    // The prompt reply is **not** the turn terminal:
                    // `peri/agent_event_done`, ingested in item order, is.
                    // Closing on the reply flushed live streams as
                    // `complete:true` while the session's items were still
                    // queued, then discarded the real terminal as late and left
                    // the session `in_progress` forever. Wait for the reader's
                    // terminal; only a lost one trips the safety net.
                    tracing::debug!(
                        logical_id = %runtime.log.logical_id(),
                        request_id = %request_id,
                        stop_reason = ?reply.stop_reason,
                        pending_tasks = reply.pending_tasks,
                        "session/prompt replied; waiting for the turn terminal"
                    );
                    if !runtime
                        .log
                        .wait_turn_closed(&request_id, ATTEMPT_REPLY_TERMINAL_TIMEOUT)
                        .await
                    {
                        tracing::warn!(
                            logical_id = %runtime.log.logical_id(),
                            request_id = %request_id,
                            "agent turn reported no terminal"
                        );
                        let outcome = TurnOutcome::Failed {
                            code: -32603,
                            message: "agent turn reported no terminal".to_owned(),
                        };
                        // Arm before the cancellable close, same as the steer
                        // path above.
                        arm_deferred_close(runtime, &request_id, outcome.clone());
                        runtime.log.finish_turn(&request_id, outcome).await;
                    }
                }
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
                let outcome = TurnOutcome::Failed { code, message };
                // Arm before the cancellable close: a steer may defer this
                // terminal, and dropping the prompt future must not drop the
                // only closer of the turn.
                arm_deferred_close(runtime, &request_id, outcome.clone());
                runtime.log.finish_turn(&request_id, outcome).await;
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
        runtime.ready_for_input().await?;
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

    /// 在调用方交出的 guard 内检查幂等、预留 generation 并派发。
    /// 返回 `None` 表示 turn 已关闭且没有替换者；由调用方重新准入。
    async fn handoff_steer(
        store: &dyn SwarmStore,
        runtime: &SessionRuntime,
        submission: &InputSubmission,
        logical_id: &LogicalId,
        fingerprint: &str,
        admitted_turn: &str,
    ) -> Result<Option<AcceptedInput>, SwarmError> {
        if let Some(accepted) =
            Self::existing_input(store, logical_id, submission, fingerprint).await?
        {
            return Ok(Some(accepted));
        }

        // 先预留，才能阻止被 dispatch 取消的原 attempt 提前关闭 turn。
        let epoch = match runtime.log.note_steer_for(admitted_turn) {
            SteerReservation::Reserved(epoch) => epoch,
            SteerReservation::NoTurn => return Ok(None),
            SteerReservation::OtherTurn => return Err(SwarmError::turn_superseded()),
        };
        let result =
            Self::persist_and_dispatch_steer(store, runtime, logical_id, submission, fingerprint)
                .await;
        if !matches!(&result, Ok(outcome) if outcome.disposition == Disposition::Inserted) {
            // 必须在释放 guard 前回滚，避免下一次派发的 run 认领错误的 generation。
            runtime.log.abandon_steer(epoch).await;
        }
        result.map(|outcome| Some(AcceptedInput::settled(outcome)))
    }

    async fn persist_and_dispatch_steer(
        store: &dyn SwarmStore,
        runtime: &SessionRuntime,
        logical_id: &LogicalId,
        submission: &InputSubmission,
        fingerprint: &str,
    ) -> Result<InputOutcome, SwarmError> {
        let disposition = Self::put_input(store, logical_id, submission, fingerprint).await?;
        if disposition != Disposition::Inserted {
            return Self::input_outcome(store, logical_id, submission, disposition).await;
        }
        let text = submission.effective_text();
        runtime
            .log
            .record_user_input(Some(&submission.input_id), &text)
            .await?;
        // 在最后一个可能失败的本地步骤完成后才派发，避免在已派发后回滚。
        let outcome = Self::input_outcome(store, logical_id, submission, disposition).await?;
        Self::dispatch_steer(runtime, submission, &text).await?;
        Ok(outcome)
    }

    /// Enqueue and dispatch one input into the host's user-input queue.
    async fn dispatch_steer(
        runtime: &SessionRuntime,
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

    /// Answer one pending approval (`agent.session.input.mcp_approval_request_result`).
    ///
    /// Settlement is single-shot: the approval registry removes the pending fact
    /// before this returns, so a duplicate, a concurrent loser and an answer from
    /// an older actor generation observe `Settled`/`Unknown` and get a 4xx.
    /// The answer is delivered to the **original** ACP JSON-RPC request; it never
    /// creates or queues a turn.
    pub async fn respond_required_action(
        &self,
        logical_id: &LogicalId,
        request_id: &str,
        decision: ApprovalDecision,
        input_id: Option<String>,
    ) -> Result<ApprovalReceipt, SwarmError> {
        self.require_row(logical_id).await?;
        let runtime = self
            .sessions
            .lock()
            .get(logical_id.as_str())
            .cloned()
            .ok_or_else(|| SwarmError::required_action_settled(request_id, "actor_lost"))?;
        match runtime
            .log
            .claim_approval(request_id, input_id.clone())
            .await
        {
            ApprovalClaim::Claimed { acp_request_id } => {
                if let Err(error) = runtime
                    .child
                    .respond_permission(&acp_request_id, decision)
                    .await
                {
                    // The reservation is released and the pending action stays
                    // answerable: a 502 must never describe a settled action.
                    runtime.log.release_approval_claim(request_id);
                    tracing::warn!(
                        logical_id = logical_id.as_str(),
                        request_id,
                        error = %error,
                        "swarm approval decision could not reach the host; action remains pending"
                    );
                    return Err(SwarmError::upstream_unavailable("approval_delivery")
                        .with_details(json!({ "requestId": request_id })));
                }
                match runtime
                    .log
                    .complete_approval_claim(request_id, decision, input_id)
                    .await
                {
                    ApprovalCompletion::Completed { receipt } => Ok(receipt),
                    // The turn ended while delivery was in flight: the durable
                    // fact wins and the caller is told it is no longer
                    // answerable instead of receiving a false success.
                    ApprovalCompletion::Settled { reason } => Err(
                        SwarmError::required_action_settled(request_id, reason.as_str()),
                    ),
                    // Delivery reached the host but the durable settlement did
                    // not. Report the storage failure; the claim was released
                    // and the pending fact remains answerable.
                    ApprovalCompletion::Failed { error } => Err(error),
                    ApprovalCompletion::Unknown => {
                        Err(SwarmError::unknown_required_action(request_id))
                    }
                }
            }
            ApprovalClaim::Duplicate { receipt } => Ok(receipt),
            ApprovalClaim::Settled { reason } => Err(SwarmError::required_action_settled(
                request_id,
                reason.as_str(),
            )),
            // The settlement row could not be persisted; the pending fact was
            // not removed and no success receipt may be returned.
            ApprovalClaim::Failed { error } => Err(error),
            // A concurrent answer owns the delivery. Nothing was settled; the
            // next retry observes the real outcome.
            ApprovalClaim::InFlight => Err(SwarmError::required_action_settled(
                request_id,
                "delivering",
            )),
            ApprovalClaim::Unknown => Err(SwarmError::unknown_required_action(request_id)),
        }
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
            cancel_settle_wait(),
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
                "swarm cancel did not observe the turn terminal within the wait bound"
            );
        }
        Ok(())
    }

    /// Remember (or forget) the live turn an accepted cancel may still need to
    /// address on a same-key replay.
    ///
    /// The entry survives only while the target turn holds the slot: once the
    /// host left it, a retry is an `existing` no-op and cannot reach a later
    /// turn. The map is in-memory by design — a new actor has no host to
    /// re-notify.
    fn note_cancel_target(&self, logical_id: &LogicalId, input_id: &str, target: &str) {
        let key = (logical_id.as_str().to_owned(), input_id.to_owned());
        let live = self.live_slot_request(logical_id).as_deref() == Some(target);
        let mut cancels = self.accepted_cancels.lock();
        if live {
            cancels.insert(key, target.to_owned());
        } else {
            cancels.remove(&key);
        }
    }

    /// Re-deliver a cancel that was accepted but did not stop its turn.
    ///
    /// Only the turn bound at acceptance is targeted, so a retry cannot cancel
    /// a turn that started afterwards.
    async fn redeliver_cancel(
        &self,
        logical_id: &LogicalId,
        input_id: &str,
    ) -> Result<(), SwarmError> {
        let key = (logical_id.as_str().to_owned(), input_id.to_owned());
        let Some(target) = self.accepted_cancels.lock().get(&key).cloned() else {
            return Ok(());
        };
        if self.live_slot_request(logical_id).as_deref() != Some(target.as_str()) {
            self.accepted_cancels.lock().remove(&key);
            return Ok(());
        }
        self.cancel_turn(logical_id, Some(&target))
            .await
            .map_err(cancel_redelivery_failure)?;
        if self.live_slot_request(logical_id).as_deref() != Some(target.as_str()) {
            self.accepted_cancels.lock().remove(&key);
        }
        Ok(())
    }

    /// The request id currently holding the turn slot, if a live runtime has one.
    fn live_slot_request(&self, logical_id: &LogicalId) -> Option<String> {
        let runtime = self.sessions.lock().get(logical_id.as_str()).cloned()?;
        let slot = runtime.turn.borrow().clone();
        slot.map(|slot| slot.request_id)
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
        // No answer can reach a host that is going away: settle first so the
        // durable log and `sessions.retrieve` agree that the id is dead. A
        // store failure is logged: the runtime is already gone, and the
        // durable pending row is settled by actor recovery.
        if let Err(error) = runtime
            .log
            .invalidate_approvals(crate::domain::SettlementReason::ActorLost)
            .await
        {
            tracing::warn!(
                logical_id = logical_id.as_str(),
                error = %error,
                "swarm approval invalidation on shutdown was not persisted"
            );
        }
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
            if let Err(error) = runtime
                .log
                .invalidate_approvals(crate::domain::SettlementReason::ActorLost)
                .await
            {
                tracing::warn!(
                    error = %error,
                    "swarm approval invalidation on shutdown was not persisted"
                );
            }
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

    /// Backlog shared by every SSE subscriber (ACP `subscribe`, the actor's
    /// production `GET /events`, and the gateway's reconnect path).
    ///
    /// Order is part of the contract:
    ///
    /// 1. settle approvals whose host wait already expired **before** the
    ///    replay window is read, so a dead `pending` row cannot be projected as
    ///    a current action;
    /// 2. replay the persisted rows after the cursor;
    /// 3. append the current `required_actions` snapshot. A cursor that points
    ///    after the opening approval row would otherwise never re-learn a
    ///    still-answerable action. The snapshot reuses the live `event_id`
    ///    (the oldest pending row's `seq`), so replay and this frame dedupe.
    pub(crate) async fn subscribe_backlog(
        &self,
        logical_id: &LogicalId,
        start: EventCursor,
    ) -> Result<Vec<StreamEvent>, SwarmError> {
        let runtime = self.sessions.lock().get(logical_id.as_str()).cloned();
        if let Some(runtime) = &runtime {
            runtime.log.settle_expired_approvals().await?;
        }
        let mut backlog = self.replay_after(logical_id, start).await?;
        if let Some(runtime) = &runtime {
            if let Some(snapshot) = runtime.log.approval_snapshot_event().await {
                backlog.push(StreamEvent::Official(snapshot));
            }
        }
        Ok(backlog)
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
    ) -> Result<(EnvironmentRef, Option<(SandboxMcpBinding, SwarmBindingRow)>), SwarmError> {
        if command.environment == EnvironmentKind::None {
            return Ok((EnvironmentRef::none(), None));
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
        let binding = SwarmBindingRow {
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
        };

        Ok((
            EnvironmentRef {
                kind: EnvironmentKind::OpenAiHosted,
                id: Some(logical_env_id),
            },
            Some((reservation.binding, binding)),
        ))
    }

    async fn spawn_runtime(
        &self,
        request: SpawnRequest<'_>,
    ) -> Result<Arc<SessionRuntime>, SwarmError> {
        let hub = self.hub_for(request.logical_id.as_str());
        let log = Arc::new(
            ItemLog::new(
                request.logical_id.as_str(),
                Arc::clone(&self.store),
                hub.sender.clone(),
                hub.tail,
            )
            .with_approval_declarations(request.mcp_tools),
        );
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
            if command.metadata_conflicts_with(&row.metadata) {
                return Err(SwarmError::new(
                    ErrorKind::Conflict,
                    ErrorCode::IdentityConflict,
                    "logicalId already exists with different metadata",
                )
                .with_details(json!({ "field": "metadata" })));
            }
            if command.prompt_cache_options_conflict_with(&row.prompt_cache_options) {
                return Err(SwarmError::new(
                    ErrorKind::Conflict,
                    ErrorCode::IdentityConflict,
                    "logicalId already exists with different prompt cache options",
                )
                .with_details(json!({ "field": "prompt_cache_options" })));
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
            // A cold retry with an unkeyed initial input cannot distinguish the
            // interrupted command from new intent. Return the stored session;
            // a keyed user submission is the unambiguous re-entry operation.
            if !self.is_live(&command.logical_id)
                && (command.initial_input.is_none()
                    || self.last_seq(&command.logical_id).await? > 0
                    || crate::service::persisted_turn_resume_required(self.store.as_ref(), &row)
                        .await?)
            {
                return Ok(CreateSessionResult {
                    session: self.view_by_id(&command.logical_id).await?,
                    created: false,
                    pending_initial: None,
                });
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
        let (environment, reservation) = self.bind_environment(&command).await?;
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
            metadata: command.metadata_for_new_row(),
            prompt_cache_options: command.prompt_cache_options_for_new_row(),
            created_at: now.clone(),
            last_activity_at: now,
            last_item_seq: 0,
        };
        // Persist the reserved identity **before** `session/new` (`design §3`).
        // A crash between the two writes then leaves a row that names the root
        // id, and the retry reconciles against it instead of minting a second
        // one. The row is also the precondition for starting a host: without an
        // addressable mapping there is no session to create.
        let (workspace, persisted) = match reservation {
            Some((workspace, binding)) => (
                Some(workspace),
                self.store
                    .swarm_create_hosted_session(
                        row.clone(),
                        binding,
                        command.environment_id.is_some(),
                    )
                    .await,
            ),
            None => (None, self.store.swarm_upsert_session(row.clone()).await),
        };
        if let Err(error) = persisted {
            return Err(match error {
                crate::store::StoreError::Conflict(_) => SwarmError::new(
                    ErrorKind::Conflict,
                    ErrorCode::IdentityConflict,
                    "logicalId was created concurrently with a different identity",
                )
                .with_details(json!({ "field": "logicalId" })),
                crate::store::StoreError::NotFound(_) if command.environment_id.is_some() => {
                    SwarmError::upstream_unavailable("sandbox_binding_changed")
                }
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

    async fn list_sessions(&self, window: SessionsWindow) -> Result<SessionPage, SwarmError> {
        let mut page =
            crate::service::list_sessions_from_store(self.store.as_ref(), &window).await?;
        // The list reads rows, but a pending approval lives in the live actor.
        // Overlay it so the list is not stale for the one state the caller must
        // act on.
        for view in &mut page.data {
            let logical_id = LogicalId::parse(&view.logical_id)?;
            let runtime = self.sessions.lock().get(logical_id.as_str()).cloned();
            if let Some(runtime) = runtime {
                if runtime.child.recovering() {
                    view.state = SessionState::Running;
                    view.status = crate::domain::SessionStatus::InProgress;
                }
                runtime.log.settle_expired_approvals().await?;
            }
            let required_actions = self.live_required_actions(&logical_id);
            if required_actions.is_empty() {
                continue;
            }
            view.status = crate::domain::SessionStatus::RequiresAction;
            view.required_actions = required_actions;
        }
        Ok(page)
    }

    async fn update_session(
        &self,
        logical_id: &LogicalId,
        command: SessionUpdateCommand,
    ) -> Result<SessionView, SwarmError> {
        let _attach = self.attach_lock.lock().await;
        let row = self.require_row(logical_id).await?;
        // Same admission rule as `accept_input`: a running or accepted turn
        // owns the session. Holding the per-session admission lock across the
        // store write keeps a concurrent accept from starting a turn between
        // the check and the update.
        let runtime = self.sessions.lock().get(logical_id.as_str()).cloned();
        let _admission = match runtime.as_ref() {
            Some(runtime) => Some(runtime.admission.lock().await),
            None => None,
        };
        if let Some(error) = runtime
            .as_ref()
            .and_then(|runtime| mutation_blocked(runtime, logical_id))
        {
            return Err(error);
        }
        if crate::service::persisted_turn_resume_required(self.store.as_ref(), &row).await? {
            // The live host is gone but the durable row still owns an open
            // turn: mutating it would silently discard that turn, and the
            // existing recovery path answers `session_resume_required`.
            return Err(SwarmError::upstream_unavailable("session_resume_required"));
        }
        let model = match command.model.as_deref() {
            Some(requested) => {
                if requested.trim().is_empty() {
                    return Err(SwarmError::invalid_input("model must not be empty")
                        .with_details(json!({ "field": "agent.model" })));
                }
                self.resolve_model(Some(requested))?
            }
            None => None,
        };
        let host_changes = command.changes_host();
        let metadata = command.metadata_patch();
        let prompt_cache_options = command.prompt_cache_patch();
        self.store
            .swarm_patch_session(SwarmSessionPatch {
                logical_id: logical_id.as_str().to_owned(),
                model,
                mcp_tools: command.mcp_tools,
                metadata,
                prompt_cache_options,
                last_activity_at: Some(peri_time::now_utc_rfc3339()),
            })
            .await
            .map_err(|error| store_failure("patch_session", error))?;
        if host_changes {
            // `model` and `mcpServers` are fixed at `session/new`; keep the
            // stored row as the authority and let the next interaction
            // reattach with the new values. Keeping the old host would report a
            // configuration the next turn does not use.
            if let Some(shutdown) = self.shutdown(logical_id).await {
                tracing::info!(
                    logical_id = logical_id.as_str(),
                    report = shutdown.report,
                    "swarm host restarted after a session update"
                );
            }
            self.channels.lock().remove(logical_id.as_str());
        }
        self.view_by_id(logical_id).await
    }

    async fn delete_session(&self, logical_id: &LogicalId) -> Result<(), SwarmError> {
        let _attach = self.attach_lock.lock().await;
        let row = self.require_row(logical_id).await?;
        let runtime = self.sessions.lock().get(logical_id.as_str()).cloned();
        let _admission = match runtime.as_ref() {
            Some(runtime) => Some(runtime.admission.lock().await),
            None => None,
        };
        if let Some(error) = runtime
            .as_ref()
            .and_then(|runtime| mutation_blocked(runtime, logical_id))
        {
            return Err(error);
        }
        if crate::service::persisted_turn_resume_required(self.store.as_ref(), &row).await? {
            // No live host owns this row, but its durable turn is still open;
            // deleting it would erase an interrupted turn the resume path is
            // meant to surface.
            return Err(SwarmError::upstream_unavailable("session_resume_required"));
        }
        self.store
            .swarm_delete_session(logical_id.as_str())
            .await
            .map_err(|error| store_failure("delete_session", error))?;
        if let Some(shutdown) = self.shutdown(logical_id).await {
            tracing::info!(
                logical_id = logical_id.as_str(),
                report = shutdown.report,
                "swarm host stopped by session delete"
            );
        }
        self.channels.lock().remove(logical_id.as_str());
        Ok(())
    }

    async fn list_turns(
        &self,
        logical_id: &LogicalId,
        window: TurnsWindow,
    ) -> Result<TurnPage, SwarmError> {
        self.require_row(logical_id).await?;
        crate::service::list_turns_from_store(self.store.as_ref(), logical_id.as_str(), &window)
            .await
    }

    async fn get_turn(
        &self,
        logical_id: &LogicalId,
        turn_id: &str,
    ) -> Result<OfficialTurn, SwarmError> {
        self.require_row(logical_id).await?;
        crate::service::get_turn_from_store(self.store.as_ref(), logical_id.as_str(), turn_id).await
    }

    async fn accept_input(
        &self,
        logical_id: &LogicalId,
        submission: InputSubmission,
    ) -> Result<AcceptedInput, SwarmError> {
        let row = self.require_row(logical_id).await?;
        if submission.is_empty() {
            return Err(SwarmError::empty_input());
        }
        let fingerprint = submission.fingerprint();
        let store = self.store.as_ref();

        // 幂等重放必须早于 `ensure_live`：接管中断会话会失败丢失的 turn，
        // 让一次重复受理产生持久化副作用（`actor_runtime_test` 的
        // `interrupted::*` 三条断言正是这个契约）。
        if let Some(outcome) =
            Self::existing_input(store, logical_id, &submission, &fingerprint).await?
        {
            // 已送达但宿主没在等待窗口内退出 slot 的取消：重放要重新下发，
            // 目标仍是受理时绑定的 turn（见 `accepted_cancels`）。
            if submission.is_cancel() {
                self.redeliver_cancel(logical_id, &submission.input_id)
                    .await?;
            }
            return Ok(outcome);
        }

        // A lost approval or cancellation belongs to the dead host. Neither
        // is authority to start a replacement or schedule its durable queue.
        let runtime = if submission.kind == InputKind::User {
            self.ensure_live(logical_id).await?;
            let runtime = self.runtime(logical_id)?;
            runtime.ready_for_input().await?;
            Some(runtime)
        } else {
            self.sessions.lock().get(logical_id.as_str()).cloned()
        };
        let cancel = submission.is_cancel();
        if cancel
            && runtime.is_none()
            && crate::service::persisted_turn_resume_required(store, &row).await?
        {
            return Err(SwarmError::upstream_unavailable("session_resume_required"));
        }
        let mut admission = match &runtime {
            Some(runtime) => Some(runtime.admission.lock().await),
            None => None,
        };
        if let Some(admitted) = &runtime {
            self.require_current_runtime(logical_id, admitted)?;
        }
        // 在 admission 内只读一次 slot；旧 turn 可以不取 admission 就清理 slot。
        let admitted_slot = runtime
            .as_ref()
            .and_then(|runtime| runtime.turn.borrow().clone());
        if submission.kind == InputKind::User {
            let runtime = runtime.as_ref().expect("user input attached a host");
            if let Some(slot) = admitted_slot
                .as_ref()
                .filter(|_| submission.mode == InputMode::Steer)
            {
                drop(admission);
                #[cfg(test)]
                runtime.steer_waiting.notify_one();
                // 在调用方排队，不能让 spawn 调度重排取锁顺序。
                let guard = Arc::clone(&runtime.steer).lock_owned().await;
                let handle = {
                    let store = Arc::clone(&self.store);
                    let runtime = Arc::clone(runtime);
                    let submission = submission.clone();
                    let logical_id = logical_id.clone();
                    let fingerprint = fingerprint.clone();
                    let admitted_turn = slot.request_id.clone();
                    tokio::spawn(async move {
                        // 丢弃 JoinHandle 不会取消任务；guard 随任务保留至派发或回滚结束。
                        let _steer = guard;
                        Self::handoff_steer(
                            store.as_ref(),
                            &runtime,
                            &submission,
                            &logical_id,
                            &fingerprint,
                            &admitted_turn,
                        )
                        .await
                    })
                };
                if let Some(accepted) = handle.await.map_err(|_| {
                    SwarmError::new(
                        ErrorKind::Unavailable,
                        ErrorCode::AgentError,
                        "steer handoff task terminated",
                    )
                    .with_details(json!({ "operation": "steer_handoff" }))
                })?? {
                    return Ok(accepted);
                }
                // NoTurn：任务已经释放 steer，回退必须重新获得普通 turn 的准入权。
                admission = Some(runtime.admission.lock().await);
                self.require_current_runtime(logical_id, runtime)?;
                if runtime.is_running() || runtime.log.has_open_turn() {
                    return Self::existing_input(store, logical_id, &submission, &fingerprint)
                        .await?
                        .ok_or_else(SwarmError::turn_in_progress);
                }
            } else if admitted_slot.is_some() || runtime.log.has_open_turn() {
                return Self::existing_input(store, logical_id, &submission, &fingerprint)
                    .await?
                    .ok_or_else(SwarmError::turn_in_progress);
            }
        }

        let cancel_target = admitted_slot
            .as_ref()
            .filter(|_| cancel)
            .map(|slot| slot.request_id.clone());
        drop(admission);

        // 取消先下发、后写收据：未送达的取消不能留下“已受理”的持久事实，
        // 否则同键重试会命中收据，再也不会发第二次 `session/cancel`。
        if let Some(target) = cancel_target.as_deref() {
            self.cancel_turn(logical_id, Some(target))
                .await
                .map_err(cancel_delivery_failure)?;
        }

        let disposition = Self::put_input(store, logical_id, &submission, &fingerprint).await?;
        if let Some(target) = cancel_target.as_deref() {
            self.note_cancel_target(logical_id, &submission.input_id, target);
        }
        if disposition == Disposition::Existing {
            let outcome = Self::input_outcome(store, logical_id, &submission, disposition).await?;
            return Ok(AcceptedInput::settled(outcome));
        }
        let pending_turn = if let Some(runtime) = runtime
            .as_ref()
            .filter(|_| submission.kind == InputKind::User)
        {
            let text = submission.effective_text();
            let (_, item_id) = runtime
                .log
                .record_user_input(Some(&submission.input_id), &text)
                .await?;
            Some(PendingTurn {
                logical_id: logical_id.clone(),
                text,
                turn_id: turn_id_for_item(&item_id),
            })
        } else {
            None
        };
        let outcome = Self::input_outcome(store, logical_id, &submission, disposition).await?;
        // 普通 turn 仍由调用方在 202 后运行，不能在受理时等待模型。
        Ok(AcceptedInput {
            outcome,
            pending_turn,
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
        item_page(self.store.as_ref(), logical_id, window, last_item_seq).await
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
        let backlog = self.subscribe_backlog(logical_id, start).await?;
        let tail = runtime.log.tail();
        // A host restart rebuilds the log with an empty tail; the store is the
        // authority for items persisted before this process opened a stream.
        tail.fetch_max(self.last_seq(logical_id).await?, Ordering::Relaxed);
        Ok(EventSubscription::new(backlog, live, tail))
    }

    async fn respond_required_action(
        &self,
        logical_id: &LogicalId,
        request_id: &str,
        decision: ApprovalDecision,
        input_id: Option<String>,
    ) -> Result<ApprovalReceipt, SwarmError> {
        AcpSwarmService::respond_required_action(self, logical_id, request_id, decision, input_id)
            .await
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
/// capability keeps its own diagnostic in `details.operation`.
///
/// The host's reply is **not** echoed: an ACP message may embed provider
/// payloads or credentials (the same policy as
/// [`SwarmError::from_acp_error`]). Only the JSON-RPC code and a classified
/// reason stay inspectable, so a client can still tell a stale generation from a
/// closed transport without reading the host log.
fn queue_unavailable(operation: &'static str, error: ChildError) -> SwarmError {
    tracing::warn!(operation, error = %error, "swarm input handoff failed");
    match error {
        ChildError::Rpc { code: -32601, .. } => {
            // `operation` stays the method that failed; the diagnostic names the
            // missing capability so a client can tell it apart from a closed
            // transport.
            SwarmError::queue_unavailable(operation).with_details(serde_json::json!({
                "operation": operation,
                "reason": "capability_not_negotiated",
            }))
        }
        ChildError::Rpc { code, .. } => {
            SwarmError::queue_unavailable(operation).with_details(serde_json::json!({
                "operation": operation,
                "rpcCode": code,
                // The message is dropped; the code classifies the failure.
                "reason": "acp_error",
            }))
        }
        other => SwarmError::queue_unavailable(operation).with_details(
            serde_json::json!({ "operation": operation, "reason": child_reason(&other) }),
        ),
    }
}

/// Classified reason for a child failure; never the upstream text.
fn child_reason(error: &ChildError) -> &'static str {
    match error {
        ChildError::ProviderUnavailable => "provider_unavailable",
        ChildError::Local(_) => "local_error",
        ChildError::Rpc { .. } => "acp_error",
        ChildError::Closed => "closed",
    }
}

/// 503 for a cancel whose `session/cancel` never reached the host.
///
/// `cancel_turn` builds its error with the generic queue wording ("the
/// submission was not accepted"); a cancel is not a submission, and no receipt
/// was written, so the message must name the undelivered cancellation and say
/// that the same key may be retried. Details (operation/RPC code/reason) are
/// already redacted by [`queue_unavailable`].
fn cancel_delivery_failure(error: SwarmError) -> SwarmError {
    tracing::warn!(error = %error, "swarm cancel was not delivered");
    SwarmError::cancel_not_delivered("session/cancel").with_details(error.details().clone())
}

/// 503 for a cancel replay whose `session/cancel` re-delivery failed.
///
/// The receipt already exists, so the first-delivery wording (“no receipt was
/// written”) would be false; the retry still uses the same key. Delivery
/// details (operation/RPC code/reason) stay as redacted by
/// [`queue_unavailable`].
fn cancel_redelivery_failure(error: SwarmError) -> SwarmError {
    tracing::warn!(error = %error, "swarm cancel redelivery failed");
    let details = match error.details() {
        Value::Object(details) => {
            let mut details = details.clone();
            details.insert("receipt".to_owned(), json!("accepted"));
            Value::Object(details)
        }
        _ => json!({ "operation": "session/cancel", "receipt": "accepted" }),
    };
    SwarmError::cancel_redelivery_failed("session/cancel").with_details(details)
}

#[cfg(test)]
#[path = "service_test.rs"]
mod service_test;
