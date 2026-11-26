//! Append-only item log for one root session (`design §6.1`).
//!
//! The ACP reader task feeds every host notification into [`ItemLog::ingest`].
//! The log is the only place that turns a notification into a `swarm_items`
//! row, allocates `seq`, and then broadcasts the persisted item — persistence
//! always precedes broadcast, so a client can never observe an item that
//! `GET /items` cannot return.
//!
//! Text streams keep one stable `itemId` per `(agentId, messageId)`. Every
//! delta is persisted as its own row with the same id, and the turn terminal
//! appends a final row for the same id carrying the accumulated text
//! (`complete: true`). Clients fold by `itemId`, so the final row wins after a
//! reconnect (`design §6.6`); live clients can render deltas as they arrive.
//!
//! Turn lifecycle (`turn: started/completed/failed`) is owned here as well:
//! the gateway opens the turn before `session/prompt` and closes it from the
//! prompt response, or — after a steer handoff — from the queued run that
//! continues the turn (`user_input_run_started` … `peri/agent_event_done`).
//!
//! A steer does not open a second Swarm turn: the host cancels the in-flight
//! attempt and runs the handed-off input as a queued run. The prompt resolves
//! `cancelled`, the queued run resumes the same transcript, and the turn stays
//! open until that run terminates (`design §2.5` input/append path).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::{broadcast, watch, Mutex as AsyncMutex};
use uuid::Uuid;

use crate::domain::official::{unix_from_rfc3339, unix_now};
use crate::domain::{
    declared_approvals, requires_answer, turn_id_for_item, ApprovalDecision, ErrorCode, ErrorKind,
    ItemKind, ItemSource, ItemView, McpToolSpec, OfficialEvent, OfficialMessageItem,
    OfficialSession, OfficialTurn, RequiredAction, ServerApproval, SessionState, SessionStatus,
    SettlementReason, StreamEvent, SwarmError, TurnEvent, TurnStatus, APPROVAL_ID_PREFIX,
};
use crate::store::{store_failure, SwarmItemRow, SwarmSessionUpdate, SwarmStore, SwarmTurnRow};

use super::projection::{self, Projected, ProjectedItem};

/// Quiet period that ends the drain fence before a turn terminal is published.
const INGEST_SETTLE_WINDOW: Duration = Duration::from_millis(15);
/// Upper bound of the drain fence, so a noisy stream cannot delay the terminal.
const INGEST_SETTLE_LIMIT: Duration = Duration::from_millis(500);
/// Host approval wait, mirrored from the contract (single fact source).
const APPROVAL_VALIDITY: Duration = peri_acp_types::interaction::APPROVAL_WAIT;

/// How a `session/request_permission` was handled.
#[derive(Clone, Debug, PartialEq)]
pub enum PermissionStart {
    /// The declaration does not cover this tool: answer `allow_once` now.
    AutoApprove,
    /// A current pending approval exists; the caller answers it later through
    /// [`ItemLog::respond_approval`] and the original ACP JSON-RPC id.
    Pending,
    /// The request cannot belong to a gateway turn (no open `session/prompt`).
    /// There is no one to answer it, so the caller refuses it explicitly
    /// instead of leaving a dangling approval forever.
    Refuse,
}

/// One pending approval, kept only in this actor generation.
#[derive(Clone, Debug)]
struct PendingApproval {
    action: RequiredAction,
    /// Original `session/request_permission` JSON-RPC id; never public.
    acp_request_id: Value,
    /// ACP `session/prompt` request id that owned the tool call (execution
    /// generation). An answer from a later turn cannot match it.
    turn_request_id: String,
    /// `seq` of the persisted `approval` item that opened it.
    item_seq: u64,
    /// Unix seconds after which the host no longer accepts an answer
    /// (`peri_acp_types::interaction::APPROVAL_WAIT`).
    deadline_unix: i64,
}

/// Terminal fact of one approval, kept for idempotent retries and diagnostics.
#[derive(Clone, Debug)]
struct SettledApproval {
    decision: Option<ApprovalDecision>,
    reason: SettlementReason,
    /// `InputSubmission` idempotency key of the accepted answer, if any.
    input_id: Option<String>,
}

/// Projection of a settled approval returned to the HTTP layer.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalReceipt {
    pub request_id: String,
    pub decision: Option<String>,
    pub reason: String,
    pub input_id: Option<String>,
    /// `duplicate` only when the same idempotency key replayed the answer.
    pub disposition: String,
}

impl SettledApproval {
    fn receipt(&self, request_id: &str, disposition: &'static str) -> ApprovalReceipt {
        ApprovalReceipt {
            request_id: request_id.to_owned(),
            decision: self.decision.map(|decision| decision.as_str().to_owned()),
            reason: self.reason.as_str().to_owned(),
            input_id: self.input_id.clone(),
            disposition: disposition.to_owned(),
        }
    }
}

/// Outcome of one answer attempt.
#[derive(Clone, Debug)]
pub enum ApprovalReply {
    /// Accepted: deliver this body to the still-open ACP request.
    Applied {
        acp_request_id: Value,
        receipt: ApprovalReceipt,
    },
    /// Same idempotency key: same receipt, nothing delivered twice.
    Duplicate { receipt: ApprovalReceipt },
    /// Already settled (or expired): a 409 with the persisted reason.
    Settled { reason: SettlementReason },
    /// Another answer is currently being delivered. Nothing was settled; the
    /// action remains answerable after that attempt finishes.
    InFlight,
    /// The settlement could not be persisted. The action was not removed.
    SettlementFailed { error: SwarmError },
    /// Not issued by this actor generation or this session.
    Unknown,
}

/// Outcome of phase one of an approval answer.
#[derive(Clone, Debug)]
pub enum ApprovalClaim {
    /// The action is reserved for delivery; the pending fact is still present.
    Claimed { acp_request_id: Value },
    /// Same idempotency key: same receipt, nothing delivered twice.
    Duplicate { receipt: ApprovalReceipt },
    /// The action expired or its turn ended before delivery could start.
    Settled { reason: SettlementReason },
    /// A concurrent answer already reserved the action for delivery.
    InFlight,
    /// The settlement row could not be persisted. Nothing was removed from the
    /// registry; the caller must report a `5xx` instead of a settlement.
    Failed { error: SwarmError },
    /// Not issued by this actor generation or this session.
    Unknown,
}

/// Outcome of phase two after successful delivery.
#[derive(Clone, Debug)]
pub enum ApprovalCompletion {
    /// Durably settled; this receipt is the HTTP success body.
    Completed { receipt: ApprovalReceipt },
    /// The action was settled by another path (for example the turn ended)
    /// while the delivery was in flight.
    Settled { reason: SettlementReason },
    /// The settlement row could not be persisted. The delivery claim was
    /// released and the pending action remains answerable.
    Failed { error: SwarmError },
    /// The claim no longer exists.
    Unknown,
}

/// Terminal outcome of a gateway turn, as reported by the gateway.
#[derive(Clone, Debug, PartialEq)]
pub enum TurnOutcome {
    Completed(String),
    Failed { code: i64, message: String },
}

/// Key of one streamed text message: `(agentId, messageId, kind)`.
///
/// Peri gives reasoning and answer chunks of one model call the same
/// `messageId`; the kind is part of the identity so a `thought` chunk cannot
/// fix the stream kind and swallow the later answer text.
type StreamKey = (String, String, &'static str);

/// Payload bytes one append group may carry.
///
/// The limit is set by the remote store, not by this reader. One group becomes
/// one item transaction, and the payload JSON travels as a bound parameter whose
/// escaped text can be several times the payload size (SQL text, ids, source
/// JSON, JSON escaping). A small budget keeps each write short on a loaded
/// server and bounds how much one failed transaction can lose; an item whose
/// fitted payload alone exceeds it still gets its own group.
const APPEND_BATCH_BYTES: usize = 8 * 1024;

/// Non-payload bytes one row adds to the statement batch (SQL text, ids, source
/// JSON, timestamps). Estimated high on purpose.
const APPEND_ROW_OVERHEAD: usize = 256;

struct MessageStream {
    kind: ItemKind,
    item_id: String,
    text: String,
    /// `seq` of the message's first persisted delta chunk.
    ///
    /// Completion flushes happen in `finish_turn`, after all deltas; the first
    /// chunk's `seq` is the only persisted fact that orders two interleaved
    /// messages, so the `final_answer`/`commentary` decision uses it instead of
    /// a map or arrival order.
    first_seq: Option<u64>,
}

/// Whether an append also moves the session's durable cursor/state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProgressWrite {
    /// Mark the session `running` unconditionally: accepting input is progress
    /// even before the turn starts, and the deferred-turn test depends on it.
    Running,
    /// Mark `running` only while a turn is open. A straggler item after the
    /// terminal must not rewrite `idle` back to `running`, which would strand
    /// the session in `in_progress` with no turn to close.
    WhileTurnOpen,
}

/// One item prepared for a batched append: payload already fitted to the item
/// budget, identity already minted, so the batch loop only assigns seqs.
struct PreparedItem {
    kind: ItemKind,
    source: ItemSource,
    payload: Value,
    truncated: bool,
    item_id: String,
    /// Serialized payload size, used to keep one store transaction under the
    /// backend's request budget.
    payload_bytes: usize,
}

/// Items staged by [`ItemLog::ingest_batch`] between two non-item events.
#[derive(Default)]
struct StagedItems {
    items: Vec<(ProjectedItem, Option<String>)>,
    /// `(index into `items`, stream key)` of delta chunks whose `first_seq` the
    /// append must fill in.
    streams: Vec<(usize, StreamKey)>,
}

/// User item whose `turn.created`/`turn.item.added` frames are published when
/// the accepted turn actually starts.
#[derive(Clone, Debug)]
struct PendingUserItem {
    seq: u64,
    item_id: String,
    text: String,
    created_at: i64,
}

/// One queued host run that continues the open turn after a steer handoff.
#[derive(Clone, Debug, PartialEq)]
struct ContinuationRun {
    request_id: String,
    /// Steer generation this run claimed when it started: the oldest
    /// generation that no earlier run had claimed and that had not been rolled
    /// back. A run older than a steer that is still unresolved has been
    /// superseded by that steer, so its terminal is not the turn's.
    generation: u64,
}

/// A terminal deferred by a steer, kept so [`ItemLog::abandon_steer`] can close
/// the turn when the handoff fails.
#[derive(Debug)]
struct SuppressedTerminal {
    /// Request id of the turn the deferred terminal belongs to.
    request_id: String,
    outcome: TurnOutcome,
    /// Request id of the continuation run that produced the terminal, or
    /// `None` for the prompt attempt of the turn. A rollback may only replay a
    /// terminal whose source still owns the turn: a prompt `cancelled` must not
    /// close a turn a live queued run owns.
    source: Option<String>,
}

#[derive(Default)]
struct LogState {
    /// Text streams opened in the current turn, keyed for deterministic flush.
    streams: BTreeMap<StreamKey, MessageStream>,
    /// `(agentId, messageId, kind)` → stable `itemId`; kept across turns so a
    /// late chunk of an already-flushed message cannot mint a second identity.
    identities: HashMap<StreamKey, String>,
    /// Request id of the gateway turn currently running, if any.
    active: Option<String>,
    /// Internal turn identity derived from the persisted user item that opened
    /// the turn; stable in replay.
    turn_id: Option<String>,
    /// Unix seconds when this turn was opened, reused by `turn.created` and
    /// `turn.completed`/`turn.failed` so one turn reports one creation time.
    turn_created_at: Option<i64>,
    /// Unix seconds when execution started; persisted as `swarm_turns.started_at`.
    turn_started_at: Option<i64>,
    /// User item persisted at acceptance whose official frames wait for the
    /// turn to start (`begin_turn`). Emitting `turn.created` at acceptance
    /// would leave a created turn with no terminal if execution never starts.
    pending_user_item: Option<PendingUserItem>,
    /// Queue generation reported by the host for the active turn (diagnostics).
    generation: Option<String>,
    /// Last fatal agent message observed while the turn was active.
    failure: Option<String>,
    /// Monotonic count of ingested notifications; used as a drain fence.
    ingested: u64,
    /// Last seq this log appended, cached so a batched append does not pay a
    /// `MAX(seq)` read per batch. Cleared on a seq conflict, which is the only
    /// way another writer can advance it.
    next_seq: Option<u64>,
    /// Monotonic generation of the steers handed to the session. Each steer
    /// cancels the in-flight attempt, so the turn is not finished by that
    /// attempt's terminal. A failed handoff rolls back the outstanding entry but
    /// never this counter: a closer may already have observed the value, and
    /// reusing it would let a stale closer close a turn a later steer owns.
    ///
    /// The counter is global to the log, not per turn: a late `abandon_steer`
    /// from a closed turn must never match the first steer of the next turn.
    /// Ownership itself is [`Self::max_unresolved`], not this counter: the
    /// counter only never reuses a value, while the unresolved set says which
    /// steers still wait for their run.
    steer_generation: u64,
    /// Steers whose handoff has been dispatched but that no queued run claimed
    /// yet and that were not rolled back. A single flag would forget how many
    /// steers still own the turn, so rolling back the second one would close a
    /// turn the first one still owns.
    ///
    /// A queued run claims the smallest generation left here when its
    /// `user_input_run_started` is ingested. Run starts arrive in claim order:
    /// the service serializes each steer's reservation with its enqueue and
    /// dispatch on one per-session lock, and the host starts a queued run in
    /// enqueue order, so the oldest unclaimed steer is the one that run was
    /// dispatched for.
    outstanding_steers: BTreeSet<u64>,
    /// Latest queued run adopted by the open turn.
    continuation: Option<ContinuationRun>,
    /// Terminal deferred by a steer, kept so [`ItemLog::abandon_steer`] can
    /// close the turn if the handoff fails.
    suppressed_terminal: Option<SuppressedTerminal>,
}

/// Whether a steer's generation could be reserved for the turn the input was
/// admitted into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SteerReservation {
    /// The admitted turn is still open; this generation waits for its run.
    Reserved(u64),
    /// No turn is open: the input runs as a normal turn.
    NoTurn,
    /// Another turn owns the session now: the input must not be dispatched into
    /// it.
    OtherTurn,
}

impl LogState {
    /// Whether a steer still owns the open turn: an unrolled-back steer is
    /// outstanding, or a queued run was adopted as the turn's continuation.
    ///
    /// A rolled-back steer owns nothing. This decides whether a terminal is
    /// kept at all; whether a closer must yield is decided by
    /// [`Self::max_unresolved`] and the continuation identity, so a steer noted
    /// and rolled back while the closer settles does not defer the close to a
    /// safety net that only fires minutes later.
    fn steer_owns_turn(&self) -> bool {
        !self.outstanding_steers.is_empty() || self.continuation.is_some()
    }

    /// Reserve one steer generation for the open turn.
    ///
    /// The admission check and the insert share this lock on purpose: a caller
    /// that checked the turn elsewhere could find it closed and its replacement
    /// open, and its generation would then be claimed by that replacement's
    /// run. `admitted: None` accepts any open turn (the `note_steer` test
    /// helper); production callers name the turn the input was admitted into.
    fn reserve_steer(&mut self, admitted: Option<&str>) -> SteerReservation {
        let matches_admitted = match self.active.as_deref() {
            None => return SteerReservation::NoTurn,
            Some(active) => admitted.is_none_or(|turn| turn == active),
        };
        if !matches_admitted {
            return SteerReservation::OtherTurn;
        }
        let generation = self.steer_generation.saturating_add(1);
        self.steer_generation = generation;
        self.outstanding_steers.insert(generation);
        SteerReservation::Reserved(generation)
    }

    /// Generation of the newest steer that is still unresolved; `0` when none
    /// is.
    ///
    /// "Unresolved" means handed to the session but neither claimed by a queued
    /// run that started for it ([`ContinuationRun::generation`]) nor rolled
    /// back. A terminal belongs to the turn only while its run is at least this
    /// new: a run older than a steer that is still unresolved has been
    /// superseded by that steer.
    fn max_unresolved(&self) -> u64 {
        self.outstanding_steers.last().copied().unwrap_or(0)
    }

    /// Keep the terminal deferred by a steer.
    ///
    /// A continuation's terminal (`source: Some(_)`) is the turn's real
    /// terminal, the prompt attempt's terminal (`source: None`) only a fallback
    /// for a failed handoff, so a retained prompt terminal is replaced by a
    /// continuation's.
    ///
    /// A retained continuation terminal is replaced too when it comes from a
    /// run the turn no longer continues: the newer continuation's terminal is
    /// the only one [`ItemLog::abandon_steer`] may replay (a rollback only
    /// replays a terminal whose source still owns the turn). Keeping the first
    /// one would drop the newer run's terminal and leave a failed handoff to
    /// the safety net. Every other case keeps the first outcome, so a `Failed`
    /// timeout already queued as the turn's outcome is not overwritten by a
    /// later `Completed` from the same attempt.
    fn suppress_terminal(&mut self, request_id: &str, outcome: TurnOutcome, source: Option<&str>) {
        let replaces = match self.suppressed_terminal.as_ref() {
            None => true,
            Some(terminal) => match (terminal.source.as_deref(), source) {
                (None, Some(_)) => true,
                (Some(retained), Some(new)) => {
                    retained != new
                        && self
                            .continuation
                            .as_ref()
                            .is_some_and(|run| run.request_id == new)
                }
                _ => false,
            },
        };
        if replaces {
            self.suppressed_terminal = Some(SuppressedTerminal {
                request_id: request_id.to_owned(),
                outcome,
                source: source.map(str::to_owned),
            });
        }
    }

    /// Whether a terminal produced by `source` still belongs to the turn's
    /// owner.
    ///
    /// The prompt attempt (`source: None`) owns the turn only while no queued
    /// run continues it; a continuation's terminal (`source: Some(run)`) only
    /// while that same run is the current continuation.
    fn terminal_owner_matches(&self, source: Option<&str>) -> bool {
        match source {
            Some(run) => self
                .continuation
                .as_ref()
                .is_some_and(|continuation| continuation.request_id == run),
            None => self.continuation.is_none(),
        }
    }
}

/// Persisted per-session item log shared by the reader, the gateway and SSE.
pub struct ItemLog {
    logical_id: String,
    store: Arc<dyn SwarmStore>,
    channel: broadcast::Sender<StreamEvent>,
    seq_lock: AsyncMutex<()>,
    /// Shared `MAX(seq)` for SSE heartbeats; updated after each append.
    tail: Arc<AtomicU64>,
    state: Mutex<LogState>,
    /// Mirror of `state.active` for async waiters (turn handoff to a queued run).
    active_watch: watch::Sender<Option<String>>,
    /// Declared MCP approval bindings (empty = no approval scope).
    declarations: Vec<ServerApproval>,
    /// Current/tombstoned approvals. Loaded once per actor generation: a
    /// restarted actor mints a fresh registry, so old public ids die with it.
    approvals: Mutex<ApprovalState>,
}

/// Maximum settled receipts kept for duplicate-answer detection. Keys are
/// time-sortable UUIDv7 ids, so the cap drops the oldest fact first.
const SETTLED_APPROVAL_CAP: usize = 256;

#[derive(Default)]
struct ApprovalState {
    pending: BTreeMap<String, PendingApproval>,
    /// Pending actions reserved by an answer whose delivery has not completed.
    /// The pending row stays until [`ItemLog::complete_approval_claim`].
    delivering: std::collections::BTreeSet<String>,
    /// Pending actions reserved while their settlement row is being persisted.
    ///
    /// The in-memory transition happens only after the store accepted the
    /// settlement, so a storage failure leaves the action answerable instead of
    /// reporting a success that a restart would forget.
    settling: std::collections::BTreeSet<String>,
    settled: BTreeMap<String, SettledApproval>,
}

impl std::fmt::Debug for ItemLog {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ItemLog")
            .field("logical_id", &self.logical_id)
            .finish_non_exhaustive()
    }
}

impl ItemLog {
    #[must_use]
    pub fn new(
        logical_id: impl Into<String>,
        store: Arc<dyn SwarmStore>,
        channel: broadcast::Sender<StreamEvent>,
        tail: Arc<AtomicU64>,
    ) -> Self {
        let (active_watch, _receiver) = watch::channel(None);
        Self {
            logical_id: logical_id.into(),
            store,
            channel,
            seq_lock: AsyncMutex::new(()),
            tail,
            state: Mutex::new(LogState::default()),
            active_watch,
            declarations: Vec::new(),
            approvals: Mutex::new(ApprovalState::default()),
        }
    }

    /// Attach the session's declared approval bindings.
    ///
    /// Called once, before the log is shared with the reader.
    #[must_use]
    pub fn with_approval_declarations(mut self, tools: &[McpToolSpec]) -> Self {
        self.declarations = declared_approvals(tools);
        self
    }

    #[must_use]
    pub fn channel(&self) -> &broadcast::Sender<StreamEvent> {
        &self.channel
    }

    /// Shared session tail (`MAX(seq)`), also reported by SSE heartbeats.
    #[must_use]
    pub fn tail(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.tail)
    }

    /// Persist one accepted user input as a single-frame `message` item.
    ///
    /// The gateway records the input when the `swarm_inputs` receipt is
    /// inserted; the host's `user_message_chunk` echo is dropped by the
    /// projection, so this is the only user frame in the log. `inputId` keys
    /// the item identity (`itim_input_<inputId>`), while `seq` remains the
    /// ordering cursor. The frame deliberately has no `complete` flag: it is
    /// not part of an assistant delta stream and must not terminate a replay.
    pub async fn record_user_input(
        &self,
        input_id: Option<&str>,
        text: &str,
    ) -> Result<(u64, String), SwarmError> {
        let mut payload = json!({ "role": "user", "text": text });
        if let Some(input_id) = input_id {
            payload["inputId"] = json!(input_id);
        }
        let item_id = input_id
            .map(|input_id| format!("itim_input_{input_id}"))
            .unwrap_or_else(ItemView::new_item_id);
        let seq = self
            .append_starting_item(
                ProjectedItem {
                    kind: ItemKind::Message,
                    source: ItemSource::root(),
                    payload,
                },
                Some(item_id.clone()),
            )
            .await?;
        Ok((seq, item_id))
    }

    #[must_use]
    pub fn logical_id(&self) -> &str {
        &self.logical_id
    }

    /// Reconcile the lost host before ACP may launch any new work. A failed
    /// store write can be retried without interrupting a replacement runner.
    pub(super) async fn settle_interrupted_turn(&self) -> Result<(), SwarmError> {
        let error = json!({
            "code": "internal_error",
            "message": "the turn was interrupted before it reached a terminal",
        });
        self.store
            .swarm_close_open_turns(
                &self.logical_id,
                TurnStatus::Failed,
                unix_now(),
                Some(error),
            )
            .await
            .map_err(|error| store_failure("close_open_turns", error))?;
        self.set_session_state(SessionState::Idle, None).await
    }

    /// Recovered output has no new Swarm user turn. Its deltas may have moved
    /// the session to running; the actual ACP root terminal makes it available.
    pub(super) async fn finish_recovered_root(&self) {
        let failed = self.state.lock().failure.take().is_some();
        self.update_state_quiet(if failed {
            SessionState::Failed
        } else {
            SessionState::Idle
        })
        .await;
    }

    /// Consume one host notification. Projection errors are impossible by
    /// construction; persistence errors are logged and never abort the reader.
    pub async fn ingest(&self, method: &str, params: &Value) {
        self.ingest_batch(vec![(method.to_owned(), params.clone())])
            .await;
    }

    /// Consume a run of notifications in arrival order.
    ///
    /// Consecutive projected items are staged and appended as one store write,
    /// so a burst of stream deltas costs one transaction instead of one per
    /// chunk. A non-item projection flushes the staged run first: turn state and
    /// item order must stay exactly as the host emitted them.
    pub async fn ingest_batch(&self, messages: Vec<(String, Value)>) {
        let mut staged = StagedItems::default();
        for (method, params) in messages {
            let projected = projection::project(&method, &params);
            {
                let mut state = self.state.lock();
                state.ingested = state.ingested.wrapping_add(1);
            }
            for event in projected {
                match event {
                    Projected::Item(item) => self.stage_item(item, &mut staged),
                    event => {
                        self.flush_staged(&mut staged).await;
                        self.dispatch_event(event).await;
                    }
                }
            }
        }
        self.flush_staged(&mut staged).await;
    }

    /// Apply one non-item projection in host order.
    async fn dispatch_event(&self, event: Projected) {
        match event {
            // `ingest_batch` stages items; this arm keeps the match total for
            // callers that hand over a single projected event.
            Projected::Item(item) => self.push_item(item).await,
            // The gateway opens the turn before `session/prompt`, so the
            // host's own start frame only confirms the generation. A queued
            // (MQ) run has its own request id: it is adopted as the
            // continuation of the open turn, never a second Swarm turn.
            Projected::TurnStarted {
                request_id,
                generation,
                queued,
            } => {
                let mut state = self.state.lock();
                if state.active.as_deref() == Some(request_id.as_str())
                    && state.generation.is_none()
                {
                    state.generation = generation;
                } else if queued && state.active.is_some() {
                    // A queued run continues the turn only while a steer still
                    // waits to be adopted: an adopted run alone does not adopt a
                    // second one, so a run that starts after the last handoff
                    // was rolled back is late and stays ignored.
                    //
                    // It claims the OLDEST unresolved generation, not the
                    // current counter. The ordering that makes "oldest" mean
                    // "this run's steer" is established before the host sees
                    // the input: the service serializes reservation with
                    // enqueue and dispatch per session, and the host starts a
                    // queued run in enqueue order, so run starts reach the
                    // reader in claim order. A run_started can reach the reader
                    // after a later steer was noted (`runtime` coalesces frames
                    // for up to 60ms); inheriting the counter would hand that
                    // older run the newer steer's generation, and its
                    // `cancelled` terminal would then close a turn the newer
                    // steer owns.
                    if let Some(claimed) = state.outstanding_steers.first().copied() {
                        state.outstanding_steers.remove(&claimed);
                        state.continuation = Some(ContinuationRun {
                            request_id,
                            generation: claimed,
                        });
                    }
                }
            }
            Projected::QueueChanged { .. } => {}
            Projected::TurnCompleted {
                request_id,
                stop_reason,
            } => {
                let closes = {
                    let mut state = self.state.lock();
                    let Some(active) = state.active.clone() else {
                        // A terminal for a turn that is already closed: the
                        // turn was finished early and this one is stranded.
                        tracing::debug!(?request_id, "turn terminal ignored: no active turn");
                        return;
                    };
                    let max_unresolved = state.max_unresolved();
                    let steer_owns = state.steer_owns_turn();
                    match request_id.as_deref() {
                        // The prompt attempt of the open turn. A steer
                        // cancels it on purpose: the turn continues in the
                        // queued run, so this terminal is not the turn's.
                        Some(running) if running == active => {
                            if !steer_owns {
                                // No steer and no continuation own the turn,
                                // so the unresolved maximum is `0`.
                                Some((
                                    active,
                                    TurnOutcome::Completed(stop_reason),
                                    max_unresolved,
                                    None,
                                ))
                            } else {
                                // Keep the prompt outcome in case the handoff
                                // fails. It carries no source: only a replay
                                // with no queued run owning the turn may use it.
                                state.suppress_terminal(
                                    running,
                                    TurnOutcome::Completed(stop_reason),
                                    None,
                                );
                                None
                            }
                        }
                        // The queued run continuing the turn. Only the run the
                        // turn currently continues may terminate it; an older
                        // run's terminal is stranded.
                        //
                        // A continuation older than a steer that is still
                        // unresolved has been superseded by that steer: its
                        // terminal is kept (with its source) instead of
                        // dropped, because the newer steer's handoff may still
                        // fail and then this run's terminal is the turn's. A
                        // dropped terminal could never be replayed.
                        Some(running) => {
                            let continuation = state
                                .continuation
                                .as_ref()
                                .filter(|run| run.request_id == running);
                            match continuation.map(|run| run.generation) {
                                Some(generation) if generation < max_unresolved => {
                                    // `active` is the turn the terminal would
                                    // close; the run id is only its source.
                                    state.suppress_terminal(
                                        &active,
                                        TurnOutcome::Completed(stop_reason),
                                        Some(running),
                                    );
                                    None
                                }
                                Some(_) => Some((
                                    active,
                                    TurnOutcome::Completed(stop_reason),
                                    max_unresolved,
                                    Some(running.to_owned()),
                                )),
                                None => None,
                            }
                        }
                        // Legacy/terminal frames without a request id only
                        // close a turn that no steer has taken over.
                        None if !steer_owns => Some((
                            active,
                            TurnOutcome::Completed(stop_reason),
                            max_unresolved,
                            None,
                        )),
                        None => None,
                    }
                };
                if let Some((closed, outcome, expected, source)) = closes {
                    self.finish_turn_observed(&closed, outcome, Some(expected), source.as_deref())
                        .await;
                }
            }
            Projected::ExecutionFailed { message } => {
                let mut state = self.state.lock();
                if state.active.is_some() {
                    state.failure = Some(message);
                }
            }
        }
    }

    /// Open a gateway turn: persist `running`, then publish the official
    /// `agent.session.turn.created` (+ the accepted user item's
    /// `turn.item.added`) and the gateway's own `turn: started`.
    ///
    /// Acceptance appends the user item and marks the turn as pending, but
    /// defers both official frames until the turn actually starts: a turn that
    /// never executes then produces no `turn.created` that could never be
    /// closed. `turn.created` is always emitted before the item's frame, on the
    /// live stream and in backlog replay alike.
    pub async fn begin_turn(&self, request_id: &str, turn_id: &str) -> Result<(), SwarmError> {
        let started_at = unix_now();
        let (created, added) = {
            let mut state = self.state.lock();
            if state.active.is_some() {
                return Err(SwarmError::turn_in_progress());
            }
            state.active = Some(request_id.to_owned());
            let (created, added) = match state.turn_id.clone() {
                Some(open) if open == turn_id => {
                    // The accepted user item opened this turn; publish its
                    // frames now that execution starts.
                    match state.pending_user_item.clone() {
                        Some(pending) => {
                            let created_at = state.turn_created_at.unwrap_or(pending.created_at);
                            (
                                Some(OfficialEvent::turn_created(
                                    &self.logical_id,
                                    turn_id,
                                    OfficialTurn::new(
                                        &self.logical_id,
                                        turn_id,
                                        TurnStatus::InProgress,
                                        created_at,
                                        None,
                                    ),
                                )),
                                Some(OfficialEvent::turn_item_added(
                                    pending.seq,
                                    &self.logical_id,
                                    turn_id,
                                    OfficialMessageItem::user(
                                        &pending.item_id,
                                        turn_id,
                                        &pending.text,
                                    ),
                                )),
                            )
                        }
                        None => (None, None),
                    }
                }
                open => {
                    if let Some(stale) = open {
                        tracing::warn!(
                            logical_id = %self.logical_id,
                            stale_turn = stale,
                            turn_id,
                            "opening a turn while a previously announced turn was never closed"
                        );
                    }
                    let created_at = unix_now();
                    state.turn_id = Some(turn_id.to_owned());
                    state.turn_created_at = Some(created_at);
                    state.pending_user_item = None;
                    (
                        Some(OfficialEvent::turn_created(
                            &self.logical_id,
                            turn_id,
                            OfficialTurn::new(
                                &self.logical_id,
                                turn_id,
                                TurnStatus::InProgress,
                                created_at,
                                None,
                            ),
                        )),
                        None,
                    )
                }
            };
            state.generation = None;
            state.failure = None;
            state.streams.clear();
            // `steer_generation` keeps counting across turns: a late rollback
            // from the previous turn must not match this turn's first steer.
            state.outstanding_steers.clear();
            state.continuation = None;
            state.suppressed_terminal = None;
            state.turn_started_at = Some(started_at);
            (created, added)
        };
        let created_at = self.state.lock().turn_created_at.unwrap_or(started_at);
        if let Err(error) = self.set_session_state(SessionState::Running, None).await {
            let mut state = self.state.lock();
            state.active = None;
            return Err(error);
        }
        // The turn is durable before its first frame: a client can never see
        // `turn.created` for a turn that `turns.retrieve` does not know.
        if let Err(error) = self
            .store
            .swarm_upsert_turn(SwarmTurnRow {
                logical_id: self.logical_id.clone(),
                turn_id: turn_id.to_owned(),
                status: TurnStatus::InProgress,
                created_at,
                started_at: Some(started_at),
                completed_at: None,
                error: None,
                usage: None,
                subagent_id: None,
            })
            .await
        {
            // No frame was published yet. Return the session to `idle` so the
            // persisted `running` row does not outlive a turn nobody can close;
            // the caller abandons the pending turn.
            {
                let mut state = self.state.lock();
                state.active = None;
            }
            self.update_state_quiet(SessionState::Idle).await;
            return Err(store_failure("upsert_turn", error));
        }
        if let Some(event) = created {
            // The session enters `in_progress` before its first turn is
            // announced; the SDK observes the same order on replay.
            self.broadcast(StreamEvent::Official(OfficialEvent::session_in_progress(
                turn_id,
                self.official_session(SessionStatus::InProgress, None).await,
            )));
            self.broadcast(StreamEvent::Official(event));
            self.broadcast(StreamEvent::Official(OfficialEvent::turn_in_progress(
                &self.logical_id,
                turn_id,
                OfficialTurn::new(
                    &self.logical_id,
                    turn_id,
                    TurnStatus::InProgress,
                    created_at,
                    None,
                ),
            )));
        }
        if let Some(event) = added {
            self.broadcast(StreamEvent::Official(event));
        }
        {
            let mut state = self.state.lock();
            state.pending_user_item = None;
        }
        self.broadcast(StreamEvent::Turn(TurnEvent::started(request_id, turn_id)));
        // Publish after the terminal-safe state is visible: `wait_turn_closed`
        // subscribes to this watch, and a stale `None` would let a caller treat
        // the still-open turn as already closed (a steer's `cancelled` attempt
        // terminal must not close it before the queued run takes over).
        self.active_watch.send_replace(Some(request_id.to_owned()));
        Ok(())
    }

    /// Whether a turn is open or accepted-but-not-yet-started.
    #[must_use]
    pub fn has_open_turn(&self) -> bool {
        self.state.lock().turn_id.is_some()
    }

    /// Drop a pending turn whose execution never started.
    ///
    /// No turn row and no official frame exist for it (both wait for
    /// [`Self::begin_turn`]), so the stream simply never learns about that turn;
    /// the persisted user item stays readable from `/items`.
    pub fn abandon_pending_turn(&self, turn_id: &str) {
        let mut state = self.state.lock();
        if state.active.is_none() && state.turn_id.as_deref() == Some(turn_id) {
            state.turn_id = None;
            state.turn_created_at = None;
            state.turn_started_at = None;
            state.pending_user_item = None;
        }
    }

    /// Close a gateway turn: flush streamed text, then publish the terminal.
    ///
    /// Idempotent: a terminal for a turn that is no longer active (late
    /// `peri/agent_event_done`, or the prompt response after an early cancel)
    /// is a no-op, so a cancel bound to turn N can never terminate turn N+1.
    ///
    /// If a steer takes over during `settle`, keep the full `outcome` for a
    /// failed handoff instead of closing the turn immediately.
    pub async fn finish_turn(&self, request_id: &str, outcome: TurnOutcome) {
        self.finish_turn_observed(request_id, outcome, None, None)
            .await;
    }

    /// Close the turn whose terminal was observed under `request_id`, unless a
    /// steer that is still unresolved owns it.
    ///
    /// `expected_max_unresolved` is the [`LogState::max_unresolved`] the caller
    /// based its decision on; `None` means the caller decided unconditionally (a
    /// timeout or a lost terminal). `source` names the continuation run that
    /// produced `outcome` (`None` for the prompt attempt) and is only stored
    /// when the terminal is deferred.
    ///
    /// When the caller decided from an observation (`Some`), `source` must
    /// still own the turn when the close actually happens: the caller's decision
    /// and this method are separate critical sections, and a run adopted in
    /// between claims the generation the caller's expected value counted. The
    /// unresolved maximum then falls back to what the caller saw while a new
    /// continuation owns the turn, so closing would end that run's turn with a
    /// superseded terminal.
    ///
    /// That owner check runs once, immediately before the turn is cleared: the
    /// decision value and the snapshot come from separate earlier critical
    /// sections, so the close is the only place where ownership is still
    /// current. The entry block does not repeat it — one rule, one maintenance
    /// location.
    async fn finish_turn_observed(
        &self,
        request_id: &str,
        outcome: TurnOutcome,
        expected_max_unresolved: Option<u64>,
        source: Option<&str>,
    ) {
        let snapshot = {
            let mut state = self.state.lock();
            if state.active.as_deref() != Some(request_id) {
                return;
            }
            // A steer that is still unresolved owns the turn; keep the terminal
            // for a failed handoff instead of closing it. The token is the
            // maximum unresolved generation, not the counter: a steer noted and
            // rolled back before this closer wakes up leaves nothing unresolved,
            // and deferring on it would leave the turn to a safety net that only
            // fires minutes later.
            let snapshot = (
                state.max_unresolved(),
                state
                    .continuation
                    .as_ref()
                    .map(|run| run.request_id.clone()),
            );
            if expected_max_unresolved.is_some_and(|expected| snapshot.0 > expected) {
                state.suppress_terminal(request_id, outcome, source);
                return;
            }
            snapshot
        };
        // Notifications and JSON-RPC responses leave the host on the same
        // transport but are dispatched to the reader and the request router
        // separately; wait for the reader to go quiet so the completion item
        // carries the whole message rather than the text seen so far.
        self.settle().await;
        let (streams, failure) = {
            let mut state = self.state.lock();
            if state.active.as_deref() != Some(request_id) {
                return;
            }
            // Same rule after settle: only a steer that still owns the turn when
            // the closer wakes up keeps its duty. A steer noted and rolled back
            // during `settle` leaves the maximum at its snapshot, so this closer
            // closes the turn now.
            //
            // A continuation adopted during `settle` claims its generation out
            // of the unresolved set, so the maximum can fall back to the
            // snapshot even though a queued run now owns the turn: the
            // continuation identity is part of the snapshot for that reason.
            //
            // The owner check lives only here, immediately before the close: the
            // decision value and the snapshot come from earlier critical
            // sections, so this is the last point where the terminal's source can
            // still be compared with a live owner. A run adopted between them
            // claims the generation the caller's `expected` counted, so the
            // maximum falls back to the caller's value and the entry snapshot
            // already carries the new continuation — the two snapshot comparisons
            // then see no change, and only the owner check still catches the
            // superseded terminal. An unconditional closer (`None`) skips the
            // check: a timeout or lost terminal must still close the turn.
            let unresolved = state.max_unresolved();
            let continuation = state
                .continuation
                .as_ref()
                .map(|run| run.request_id.as_str());
            let superseded = (expected_max_unresolved.is_some()
                && !state.terminal_owner_matches(source))
                || unresolved > snapshot.0
                || continuation != snapshot.1.as_deref();
            if superseded {
                state.suppress_terminal(request_id, outcome, source);
                return;
            }
            state.active = None;
            state.generation = None;
            state.outstanding_steers.clear();
            state.continuation = None;
            state.suppressed_terminal = None;
            (std::mem::take(&mut state.streams), state.failure.take())
        };
        // Approvals owned by this turn can no longer be answered: the prompt
        // attempt (and its ACP waiter) is gone. Persist the settlement and
        // publish the empty snapshot before the turn terminal, so a reconnecting
        // client never believes a dead approval is still pending. A store
        // failure is logged: the turn terminal still has to be published, and
        // the durable pending row is settled again by actor recovery.
        if let Err(error) = self.settle_turn_approvals(request_id).await {
            tracing::warn!(
                logical_id = %self.logical_id,
                request_id,
                error = %error,
                "swarm turn-ended approval settlement was not persisted"
            );
        }
        // The turn identity stays set while completion items are flushed: their
        // `turn.item.added` frames belong to this turn.
        //
        // Messages are ordered by their first persisted chunk, so a turn that
        // interleaved two messages still reports them in output order. Only root
        // `Message` streams compete for `outputIndex` and the final-answer
        // phase: thought streams (same `messageId` as the answer) and child
        // agents are persisted as custom items but never become official
        // outputs.
        let mut streams: Vec<_> = streams
            .into_iter()
            .filter(|(_, stream)| !stream.text.is_empty())
            .map(|(key, stream)| (stream.first_seq.unwrap_or(u64::MAX), key, stream))
            .collect();
        streams.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
        let root_messages = streams
            .iter()
            .filter(|(_, (agent_id, _, _), stream)| {
                agent_id.as_str() == "root" && stream.kind == ItemKind::Message
            })
            .count();
        let mut root_message_index = 0usize;
        for (_, (agent_id, message_id, _), stream) in streams {
            let root_message = agent_id == "root" && stream.kind == ItemKind::Message;
            let mut payload = json!({
                "role": "assistant",
                "messageId": message_id,
                "text": stream.text,
                "complete": true,
            });
            if root_message {
                payload["phase"] = json!(if root_message_index + 1 == root_messages {
                    "final_answer"
                } else {
                    "commentary"
                });
                payload["outputIndex"] = json!(root_message_index);
                root_message_index += 1;
            }
            let item = ProjectedItem {
                kind: stream.kind,
                source: ItemSource {
                    agent_id,
                    task_id: None,
                },
                payload,
            };
            if let Err(error) = self.append_item(item, Some(stream.item_id)).await {
                tracing::warn!(
                    logical_id = %self.logical_id,
                    error = %error,
                    "swarm completion item was not persisted"
                );
            }
        }
        let (turn_id, created_at, started_at) = {
            let mut state = self.state.lock();
            (
                state.turn_id.take(),
                state.turn_created_at.take(),
                state.turn_started_at.take(),
            )
        };
        self.state.lock().pending_user_item = None;
        let Some(turn_id) = turn_id else {
            // Unreachable by construction: `begin_turn` always records the
            // turn id it was given. Neither custom nor official frames can name
            // a turn, so do not fabricate one.
            tracing::error!(
                logical_id = %self.logical_id,
                request_id,
                "swarm turn closed without a recorded turn id"
            );
            self.active_watch.send_replace(None);
            return;
        };
        match outcome {
            TurnOutcome::Completed(stop_reason) => {
                let completed_at = unix_now();
                // A host-side cancel is still a `turn.completed` frame, but the
                // official `Turn.status` carries `cancelled`; persisting
                // `completed` would make `turns.retrieve` disagree with the
                // official status vocabulary (`turns.d.ts`).
                let status = if stop_reason == "cancelled" {
                    TurnStatus::Cancelled
                } else {
                    TurnStatus::Completed
                };
                if let Err(error) = self
                    .persist_terminal_turn(
                        &turn_id,
                        status,
                        created_at.unwrap_or(completed_at),
                        started_at,
                        completed_at,
                        None,
                    )
                    .await
                {
                    tracing::warn!(
                        logical_id = %self.logical_id,
                        turn_id,
                        error = %error,
                        "swarm completed turn row was not persisted"
                    );
                }
                self.update_state_quiet(SessionState::Idle).await;
                self.broadcast(StreamEvent::Turn(TurnEvent::completed(
                    request_id,
                    &turn_id,
                    stop_reason,
                )));
                self.broadcast(StreamEvent::Official(OfficialEvent::turn_completed(
                    &self.logical_id,
                    &turn_id,
                    OfficialTurn::new(
                        &self.logical_id,
                        &turn_id,
                        status,
                        created_at.unwrap_or(completed_at),
                        Some(completed_at),
                    ),
                )));
                // Immediately after the terminal: the SDK's `TurnState` only
                // treats `agent.session.idle` as terminal after it observed the
                // matching `turn.completed`.
                self.broadcast(StreamEvent::Official(OfficialEvent::session_idle(
                    &turn_id,
                    self.official_session(SessionStatus::Idle, None).await,
                )));
            }
            TurnOutcome::Failed { code, message } => {
                let message = failure.unwrap_or(message);
                let completed_at = unix_now();
                if let Err(error) = self
                    .persist_terminal_turn(
                        &turn_id,
                        TurnStatus::Failed,
                        created_at.unwrap_or(completed_at),
                        started_at,
                        completed_at,
                        Some(json!({
                            "code": "internal_error",
                            "message": message,
                        })),
                    )
                    .await
                {
                    tracing::warn!(
                        logical_id = %self.logical_id,
                        turn_id,
                        error = %error,
                        "swarm failed turn row was not persisted"
                    );
                }
                self.update_state_quiet(SessionState::Failed).await;
                self.broadcast(StreamEvent::Turn(TurnEvent::failed(
                    request_id,
                    &turn_id,
                    code,
                    message.clone(),
                )));
                self.broadcast(StreamEvent::Official(OfficialEvent::turn_failed(
                    &self.logical_id,
                    &turn_id,
                    OfficialTurn::new(
                        &self.logical_id,
                        &turn_id,
                        TurnStatus::Failed,
                        created_at.unwrap_or(completed_at),
                        Some(completed_at),
                    )
                    // The failure text is now persisted on the turn row; the
                    // live frame and a later `turns.retrieve` agree.
                    .with_error(Some(message.clone())),
                )));
                self.broadcast(StreamEvent::Official(OfficialEvent::error(
                    &turn_id,
                    &self.logical_id,
                    &message,
                )));
                self.broadcast(StreamEvent::Official(OfficialEvent::session_failed(
                    &turn_id,
                    self.official_session(SessionStatus::Failed, Some(message))
                        .await,
                )));
            }
        }
        // Wake `wait_turn_closed` only after the terminal is out, so a caller
        // that returns from the prompt observes a fully closed turn.
        self.active_watch.send_replace(None);
    }

    /// Persist one terminal turn row, keeping the opening `created_at`.
    async fn persist_terminal_turn(
        &self,
        turn_id: &str,
        status: TurnStatus,
        created_at: i64,
        started_at: Option<i64>,
        completed_at: i64,
        error: Option<Value>,
    ) -> Result<(), crate::store::StoreError> {
        self.store
            .swarm_upsert_turn(SwarmTurnRow {
                logical_id: self.logical_id.clone(),
                turn_id: turn_id.to_owned(),
                status,
                created_at,
                started_at,
                completed_at: Some(completed_at),
                error,
                usage: None,
                subagent_id: None,
            })
            .await
    }

    /// Official session snapshot for `agent.session.idle`/`agent.session.failed`.
    ///
    /// A missing/failed row read still yields a snapshot: the terminal frame is
    /// the SDK's only way out of `in_progress`, so suppressing it would hang a
    /// stream. Unread timestamps are `0`, which is visibly not an observation.
    async fn official_session(
        &self,
        status: SessionStatus,
        error: Option<String>,
    ) -> OfficialSession {
        let required_actions = self.required_actions();
        let row = self.store.swarm_get_session(&self.logical_id).await;
        match row {
            Ok(Some(row)) => OfficialSession::new(
                &self.logical_id,
                status,
                unix_from_rfc3339(&row.created_at),
                unix_from_rfc3339(&row.last_activity_at),
                error,
                row.model,
                row.instructions,
            )
            .with_environment(row.environment.normalized())
            .with_required_actions(required_actions)
            .with_metadata(row.metadata)
            .with_prompt_cache_options(row.prompt_cache_options),
            Ok(None) => OfficialSession::new(&self.logical_id, status, 0, 0, error, None, None)
                .with_required_actions(required_actions),
            Err(read_error) => {
                tracing::warn!(
                    logical_id = %self.logical_id,
                    error = %read_error,
                    "swarm session row was not readable for the official terminal frame"
                );
                OfficialSession::new(&self.logical_id, status, 0, 0, error, None, None)
                    .with_required_actions(required_actions)
            }
        }
    }

    /// Key/value projection of the last item appended (test and gateway aid).
    #[must_use]
    pub fn active_turn(&self) -> Option<String> {
        self.state.lock().active.clone()
    }

    /// Record a steer handed to the running turn and return its generation.
    ///
    /// Test helper: it accepts any open turn, while the service names the turn
    /// it admitted the input into ([`Self::note_steer_for`]). Returns `0` when
    /// no turn is open, so the caller runs the input as a normal turn.
    #[cfg(test)]
    #[must_use]
    pub fn note_steer(&self) -> u64 {
        match self.state.lock().reserve_steer(None) {
            SteerReservation::Reserved(generation) => generation,
            SteerReservation::NoTurn => 0,
            // `None` accepts any open turn, so there is nothing to disagree
            // with.
            SteerReservation::OtherTurn => unreachable!("no admitted turn to reject"),
        }
    }

    /// Record a steer handed to `admitted_turn` and reserve its generation.
    ///
    /// The host answers a dispatched steer by cancelling the in-flight attempt,
    /// so the turn must survive that attempt's `cancelled` terminal. The
    /// generation stays in [`LogState::outstanding_steers`] until a queued run
    /// claims it or the caller passes it to [`Self::abandon_steer`].
    ///
    /// The previous `note_steer` only asked whether *a* turn was active. A caller
    /// waiting for the service's steer lock could find that turn closed and its
    /// replacement open, and the dispatch then landed in the replacement. The
    /// admission check and the insert happen in one `state.lock()`, so a turn
    /// that is no longer `admitted_turn` yields [`SteerReservation::OtherTurn`]
    /// and no generation at all.
    #[must_use]
    pub(crate) fn note_steer_for(&self, admitted_turn: &str) -> SteerReservation {
        self.state.lock().reserve_steer(Some(admitted_turn))
    }

    /// Undo [`Self::note_steer_for`] after a failed handoff.
    ///
    /// Only the rolled-back generation leaves the unresolved set, so an
    /// out-of-order rollback (a later steer first) never releases an earlier
    /// one. A generation already claimed by a queued run is no longer in the
    /// set, so rolling back a superseded steer cannot hand that run's ownership
    /// to the next one: the claim, not the rollback, decides which generation a
    /// run owns.
    ///
    /// A retained terminal is replayed only while it still belongs to the
    /// turn's owner: the prompt attempt's terminal (`source: None`) when no
    /// queued run took over and no earlier steer is still unresolved, or a
    /// continuation's own terminal while that same run still owns the turn and
    /// no newer steer supersedes it. A prompt `cancelled` must never close a
    /// turn a live run owns, and a superseded run's terminal must not close a
    /// turn a newer steer owns.
    pub async fn abandon_steer(&self, generation: u64) {
        let terminal = {
            let mut state = self.state.lock();
            if !state.outstanding_steers.remove(&generation) {
                // Already rolled back, already claimed by a queued run, or never
                // noted here (a late rollback from a closed turn): the current
                // steer set stays untouched.
                return;
            }
            let replayable = match state.suppressed_terminal.as_ref() {
                // No queued run took the turn over, so the prompt attempt's own
                // terminal is still the turn's. An earlier steer that is still
                // unresolved may yet hand the turn to its run, so the prompt
                // terminal has to wait for that steer to resolve too.
                Some(SuppressedTerminal { source: None, .. }) => {
                    state.continuation.is_none() && state.outstanding_steers.is_empty()
                }
                // A continuation's terminal closes the turn only while that
                // same run still owns it and no newer unresolved steer has
                // superseded it.
                Some(SuppressedTerminal {
                    source: Some(source),
                    ..
                }) => state.continuation.as_ref().is_some_and(|run| {
                    run.request_id == *source && run.generation >= state.max_unresolved()
                }),
                None => false,
            };
            if !replayable {
                return;
            }
            state.suppressed_terminal.take()
        };
        if let Some(suppressed) = terminal {
            // Replay with no steer unresolved: the rollback removed the last
            // one, and a retained continuation terminal implies its own
            // generation was claimed out of the set when its run started. A
            // steer admitted in the meantime makes the guard yield and retain
            // the terminal again, with the same source.
            self.finish_turn_observed(
                &suppressed.request_id,
                suppressed.outcome,
                Some(0),
                suppressed.source.as_deref(),
            )
            .await;
        }
    }

    /// Monotonic generation of the session's steers (`0` before the first).
    #[must_use]
    pub fn steer_generation(&self) -> u64 {
        self.state.lock().steer_generation
    }

    /// Whether a steer still owns the open turn: one is outstanding, or a
    /// queued run was adopted as its continuation.
    #[must_use]
    pub fn steer_pending(&self) -> bool {
        self.state.lock().steer_owns_turn()
    }

    /// Wait until `request_id` is no longer the open turn.
    ///
    /// `true` means the terminal was observed; `false` means the timeout
    /// expired first (the caller decides how to close the turn).
    pub async fn wait_turn_closed(&self, request_id: &str, within: Duration) -> bool {
        if self.active_turn().as_deref() != Some(request_id) {
            return true;
        }
        let mut receiver = self.active_watch.subscribe();
        let waited = tokio::time::timeout(
            within,
            receiver.wait_for(|active| active.as_deref() != Some(request_id)),
        )
        .await;
        waited.is_ok()
    }

    /// Close a turn whose run no longer reports a terminal.
    ///
    /// [`Self::finish_turn`] defers the close while a steer admitted during
    /// `settle` owns the turn. Wait up to `within` between retries so a host
    /// that accepts the steer and then goes silent cannot leave the turn open
    /// forever.
    ///
    /// The caller should run this on its own task: the retries must outlive the
    /// future that triggered them.
    pub async fn close_lost_turn(&self, request_id: &str, outcome: TurnOutcome, within: Duration) {
        self.finish_turn(request_id, outcome.clone()).await;
        while self.active_turn().as_deref() == Some(request_id) {
            if self.wait_turn_closed(request_id, within).await {
                return;
            }
            self.finish_turn(request_id, outcome.clone()).await;
        }
    }

    /// Close a turn whose run stopped reporting, but only after a grace period.
    ///
    /// A steer hands the turn to a queued run that may still be executing; an
    /// immediate [`Self::finish_turn`] would pre-empt it with one settle window
    /// and drop its terminal. Wait up to `within` for the continuation to close
    /// the turn itself; only a turn still open after that grace is force-closed
    /// (then retried like [`Self::close_lost_turn`]).
    ///
    /// The caller should run this on its own task: the grace and retries must
    /// outlive the future that triggered them.
    pub async fn close_lost_turn_after_grace(
        &self,
        request_id: &str,
        outcome: TurnOutcome,
        within: Duration,
    ) {
        if self.wait_turn_closed(request_id, within).await {
            return;
        }
        self.close_lost_turn(request_id, outcome, within).await;
    }

    // ── MCP approvals (`requires_action`) ──────────────────────────────────

    /// Handle one `session/request_permission` from the host.
    ///
    /// The decision is scoped by **binding identity**, not by tool name:
    ///
    /// - a declared `never` tool is answered `allow_once` immediately (the
    ///   caller sends it) and leaves no trace;
    /// - a declared `always`/scoped/unknown tool becomes a pending approval;
    /// - a request whose MCP origin is missing cannot be attributed to any
    ///   declaration and is therefore pending as well (fail-closed).
    pub async fn begin_permission(
        &self,
        acp_request_id: &Value,
        params: &Value,
    ) -> Result<PermissionStart, SwarmError> {
        let origin = params.get("_meta").and_then(|meta| meta.get("peri.mcp"));
        // An empty label/tool is not a fact: treat it as "unknown" so the
        // projection uses `server_label: ""` (official type has no null) and
        // `name: null` (official nullable field) rather than inventing empty
        // names.
        let server = origin
            .and_then(|origin| origin.get("server"))
            .and_then(Value::as_str)
            .filter(|server| !server.is_empty())
            .map(str::to_owned);
        let tool = origin
            .and_then(|origin| origin.get("tool"))
            .and_then(Value::as_str)
            .filter(|tool| !tool.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                params
                    .get("toolCall")
                    .and_then(|call| call.get("title"))
                    .and_then(Value::as_str)
                    .filter(|title| !title.is_empty())
                    .map(str::to_owned)
            });
        let arguments = params
            .get("toolCall")
            .and_then(|call| call.get("rawInput"))
            .cloned()
            .unwrap_or(Value::Null);
        let tool_call_id = params
            .get("toolCall")
            .and_then(|call| call.get("toolCallId"))
            .and_then(Value::as_str)
            .map(str::to_owned);

        let needs_answer = match (server.as_deref(), tool.as_deref()) {
            (Some(server), Some(tool)) => requires_answer(&self.declarations, server, tool),
            // Unattributable: fail-closed.
            _ => true,
        };
        if !needs_answer {
            return Ok(PermissionStart::AutoApprove);
        }

        let (turn_id, turn_request_id) = {
            let state = self.state.lock();
            (state.turn_id.clone(), state.active.clone())
        };
        let Some(turn_request_id) = turn_request_id else {
            return Ok(PermissionStart::Refuse);
        };
        // The official required action carries a `turn_id`; an approval that
        // cannot name its turn is not answerable and must not be projected.
        let Some(turn_id) = turn_id else {
            return Ok(PermissionStart::Refuse);
        };
        let request_id = format!("{APPROVAL_ID_PREFIX}{}", Uuid::now_v7());
        let action = RequiredAction {
            request_id: request_id.clone(),
            turn_id: turn_id.clone(),
            server_label: server.clone().unwrap_or_default(),
            tool: tool.clone(),
            arguments: arguments.clone(),
        };
        // The pending fact is persisted before the official frame is published:
        // a reconnect can always read the row that the snapshot names.
        let payload = json!({
            "type": crate::domain::MCP_APPROVAL_REQUEST,
            "state": "pending",
            "requestId": request_id,
            "turnId": turn_id,
            "turnRequestId": turn_request_id,
            "serverLabel": server,
            "name": tool,
            "toolCallId": tool_call_id,
            "arguments": arguments,
        });
        let seq = self
            .append_item(
                ProjectedItem {
                    kind: ItemKind::Approval,
                    source: ItemSource::root(),
                    payload,
                },
                None,
            )
            .await?;
        {
            let mut approvals = self.approvals.lock();
            approvals.pending.insert(
                request_id,
                PendingApproval {
                    action,
                    acp_request_id: acp_request_id.clone(),
                    turn_request_id,
                    item_seq: seq,
                    deadline_unix: unix_now().saturating_add(APPROVAL_VALIDITY.as_secs() as i64),
                },
            );
        }
        self.broadcast_required_actions(seq).await;
        Ok(PermissionStart::Pending)
    }

    /// Answer a pending approval exactly once.
    ///
    /// This is the non-delivering convenience used by tests and by callers
    /// that have no ACP response to send. The actor service uses the
    /// two-phase [`Self::claim_approval`] / [`Self::complete_approval_claim`]
    /// pair so a failed delivery leaves the action answerable.
    ///
    /// `input_id` is the caller's idempotency key: replaying the same key
    /// returns the same receipt instead of a conflict.
    pub async fn respond_approval(
        &self,
        request_id: &str,
        decision: ApprovalDecision,
        input_id: Option<String>,
    ) -> ApprovalReply {
        match self.claim_approval(request_id, input_id.clone()).await {
            ApprovalClaim::Claimed { acp_request_id } => {
                match self
                    .complete_approval_claim(request_id, decision, input_id)
                    .await
                {
                    ApprovalCompletion::Completed { receipt } => ApprovalReply::Applied {
                        acp_request_id,
                        receipt,
                    },
                    ApprovalCompletion::Settled { reason } => ApprovalReply::Settled { reason },
                    ApprovalCompletion::Failed { error } => {
                        ApprovalReply::SettlementFailed { error }
                    }
                    ApprovalCompletion::Unknown => ApprovalReply::Unknown,
                }
            }
            ApprovalClaim::Duplicate { receipt } => ApprovalReply::Duplicate { receipt },
            ApprovalClaim::Settled { reason } => ApprovalReply::Settled { reason },
            ApprovalClaim::InFlight => ApprovalReply::InFlight,
            ApprovalClaim::Failed { error } => ApprovalReply::SettlementFailed { error },
            ApprovalClaim::Unknown => ApprovalReply::Unknown,
        }
    }

    /// Phase one of an answer: reserve the pending action for delivery.
    ///
    /// The pending fact is **not** removed. The caller sends the decision to
    /// the suspended ACP request, then either
    /// [`Self::complete_approval_claim`] (durable settlement) or
    /// [`Self::release_approval_claim`] (delivery failed; still answerable).
    /// A concurrent answer observes [`ApprovalClaim::InFlight`] instead of
    /// deleting the action before delivery is known to have succeeded.
    pub async fn claim_approval(
        &self,
        request_id: &str,
        input_id: Option<String>,
    ) -> ApprovalClaim {
        let now = unix_now();
        // Lock order is `state` then `approvals` everywhere (see
        // `required_actions`); reading the generation first keeps that rule.
        let active = self.state.lock().active.clone();
        let invalid = {
            let mut approvals = self.approvals.lock();
            if approvals.delivering.contains(request_id) || approvals.settling.contains(request_id)
            {
                return ApprovalClaim::InFlight;
            }
            match approvals.pending.get(request_id).cloned() {
                None => {
                    return match approvals.settled.get(request_id) {
                        Some(settled)
                            if matches!(
                                (&settled.input_id, &input_id),
                                (Some(stored), Some(replayed)) if stored == replayed
                            ) =>
                        {
                            ApprovalClaim::Duplicate {
                                receipt: settled.receipt(request_id, "duplicate"),
                            }
                        }
                        Some(settled) => ApprovalClaim::Settled {
                            reason: settled.reason,
                        },
                        None => ApprovalClaim::Unknown,
                    };
                }
                Some(pending) => {
                    let invalid = if pending.deadline_unix < now {
                        Some(SettlementReason::Expired)
                    } else if active.as_deref() != Some(pending.turn_request_id.as_str()) {
                        // The turn that owned this call already ended (or a
                        // newer generation owns the session): the ACP waiter is
                        // gone.
                        Some(SettlementReason::TurnEnded)
                    } else {
                        None
                    };
                    match invalid {
                        None => {
                            approvals.delivering.insert(request_id.to_owned());
                            return ApprovalClaim::Claimed {
                                acp_request_id: pending.acp_request_id,
                            };
                        }
                        Some(reason) => {
                            // Reserve the transition before awaiting the store,
                            // so a concurrent answer cannot persist a second
                            // settlement for the same id.
                            approvals.settling.insert(request_id.to_owned());
                            reason
                        }
                    }
                }
            }
        };
        match self
            .persist_approval_settlement(request_id, None, invalid, None)
            .await
        {
            Ok(seq) => {
                {
                    let mut approvals = self.approvals.lock();
                    approvals.settling.remove(request_id);
                    approvals.pending.remove(request_id);
                    approvals.delivering.remove(request_id);
                    approvals.settled.insert(
                        request_id.to_owned(),
                        SettledApproval {
                            decision: None,
                            reason: invalid,
                            input_id: None,
                        },
                    );
                }
                self.broadcast_settlement(seq).await;
                ApprovalClaim::Settled { reason: invalid }
            }
            Err(error) => {
                // The store did not accept the settlement: keep the pending
                // fact and report the failure instead of a false 409.
                self.approvals.lock().settling.remove(request_id);
                ApprovalClaim::Failed { error }
            }
        }
    }

    /// Phase two (success): durably settle a claimed approval and publish the
    /// receipt. The action is removed only here, after delivery succeeded.
    ///
    /// The settlement row is persisted **before** the in-memory registry
    /// changes. If the store fails, the claim is released and the action stays
    /// answerable; a success receipt never describes an unpersisted settlement.
    pub async fn complete_approval_claim(
        &self,
        request_id: &str,
        decision: ApprovalDecision,
        input_id: Option<String>,
    ) -> ApprovalCompletion {
        // A settlement that began in parallel (turn end / expiry) may have
        // removed the pending fact; in that case report its durable outcome
        // instead of persisting an answer nobody can observe.
        {
            let approvals = self.approvals.lock();
            if !approvals.pending.contains_key(request_id) {
                return match approvals.settled.get(request_id) {
                    Some(settled) => ApprovalCompletion::Settled {
                        reason: settled.reason,
                    },
                    None => ApprovalCompletion::Unknown,
                };
            }
        }
        let seq = match self
            .persist_approval_settlement(
                request_id,
                Some(decision),
                SettlementReason::Answered,
                input_id.as_deref(),
            )
            .await
        {
            Ok(seq) => seq,
            Err(error) => {
                // Delivery already reached the host, but the durable fact does
                // not exist: release the reservation so the next answer is not
                // permanently reported as `delivering`.
                self.release_approval_claim(request_id);
                tracing::warn!(
                    logical_id = %self.logical_id,
                    request_id,
                    error = %error,
                    "swarm approval delivery succeeded but its settlement was not persisted"
                );
                return ApprovalCompletion::Failed { error };
            }
        };
        let completed = {
            let mut approvals = self.approvals.lock();
            approvals.delivering.remove(request_id);
            approvals.pending.remove(request_id);
            approvals.settled.insert(
                request_id.to_owned(),
                SettledApproval {
                    decision: Some(decision),
                    reason: SettlementReason::Answered,
                    input_id: input_id.clone(),
                },
            );
            if approvals.settled.len() > SETTLED_APPROVAL_CAP {
                let oldest = approvals.settled.keys().next().cloned();
                if let Some(oldest) = oldest {
                    approvals.settled.remove(&oldest);
                }
            }
            approvals
                .settled
                .get(request_id)
                .cloned()
                .expect("just inserted")
                .receipt(request_id, "inserted")
        };
        self.broadcast_settlement(seq).await;
        ApprovalCompletion::Completed { receipt: completed }
    }

    /// Phase two (failure): release the reservation without settling. The
    /// pending action stays answerable, so a 502 never deletes the action.
    pub fn release_approval_claim(&self, request_id: &str) {
        self.approvals.lock().delivering.remove(request_id);
    }

    /// Test-only: move one pending approval's deadline into the past so the
    /// expiry path can be exercised without waiting the host's 300 s window.
    #[cfg(test)]
    pub(crate) fn expire_approval_now(&self, request_id: &str) {
        if let Some(pending) = self.approvals.lock().pending.get_mut(request_id) {
            pending.deadline_unix = unix_now().saturating_sub(1);
        }
    }

    /// Lazily settle approvals whose host wait already expired.
    ///
    /// There is no second timer: the deadline is the host's own
    /// `APPROVAL_WAIT`, and this method just makes the persisted fact catch up
    /// when a reader needs the current state.
    ///
    /// A settlement is persisted before the in-memory pending fact is removed.
    /// A store failure leaves the action pending (filtered out by its deadline)
    /// and returns the explicit `5xx`; it is never silently reported as settled.
    pub async fn settle_expired_approvals(&self) -> Result<usize, SwarmError> {
        let now = unix_now();
        let expired: Vec<String> = {
            let approvals = self.approvals.lock();
            approvals
                .pending
                .iter()
                .filter(|(id, pending)| {
                    pending.deadline_unix < now
                        && !approvals.delivering.contains(*id)
                        && !approvals.settling.contains(*id)
                })
                .map(|(id, _)| id.clone())
                .collect()
        };
        let mut settled = 0;
        let mut first_error: Option<SwarmError> = None;
        for request_id in expired {
            {
                let mut approvals = self.approvals.lock();
                // The expiry decision was taken under the lock above; re-check
                // so an answer that started meanwhile keeps ownership.
                if approvals.delivering.contains(&request_id)
                    || approvals.settling.contains(&request_id)
                    || !approvals.pending.contains_key(&request_id)
                {
                    continue;
                }
                approvals.settling.insert(request_id.clone());
            }
            match self
                .persist_approval_settlement(&request_id, None, SettlementReason::Expired, None)
                .await
            {
                Ok(seq) => {
                    {
                        let mut approvals = self.approvals.lock();
                        approvals.settling.remove(&request_id);
                        approvals.pending.remove(&request_id);
                        approvals.delivering.remove(&request_id);
                        approvals.settled.insert(
                            request_id.clone(),
                            SettledApproval {
                                decision: None,
                                reason: SettlementReason::Expired,
                                input_id: None,
                            },
                        );
                    }
                    self.broadcast_settlement(seq).await;
                    settled += 1;
                }
                Err(error) => {
                    self.approvals.lock().settling.remove(&request_id);
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(settled),
        }
    }

    /// Drop pending approvals whose owning turn just ended.
    pub async fn settle_turn_approvals(&self, turn_request_id: &str) -> Result<usize, SwarmError> {
        let drained: Vec<String> = {
            let approvals = self.approvals.lock();
            approvals
                .pending
                .iter()
                .filter(|(id, pending)| {
                    pending.turn_request_id == turn_request_id
                        && !approvals.delivering.contains(*id)
                        && !approvals.settling.contains(*id)
                })
                .map(|(id, _)| id.clone())
                .collect()
        };
        let mut settled = 0;
        let mut first_error: Option<SwarmError> = None;
        for request_id in drained {
            {
                let mut approvals = self.approvals.lock();
                if approvals.delivering.contains(&request_id)
                    || approvals.settling.contains(&request_id)
                    || !approvals.pending.contains_key(&request_id)
                {
                    continue;
                }
                approvals.settling.insert(request_id.clone());
            }
            match self
                .persist_approval_settlement(&request_id, None, SettlementReason::TurnEnded, None)
                .await
            {
                Ok(seq) => {
                    {
                        let mut approvals = self.approvals.lock();
                        approvals.settling.remove(&request_id);
                        approvals.pending.remove(&request_id);
                        approvals.delivering.remove(&request_id);
                        approvals.settled.insert(
                            request_id.clone(),
                            SettledApproval {
                                decision: None,
                                reason: SettlementReason::TurnEnded,
                                input_id: None,
                            },
                        );
                    }
                    self.broadcast_settlement(seq).await;
                    settled += 1;
                }
                Err(error) => {
                    self.approvals.lock().settling.remove(&request_id);
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(settled),
        }
    }

    /// Drop every pending approval because the actor/transport is gone.
    pub async fn invalidate_approvals(
        &self,
        reason: SettlementReason,
    ) -> Result<usize, SwarmError> {
        let drained: Vec<String> = {
            let approvals = self.approvals.lock();
            approvals.pending.keys().cloned().collect()
        };
        let mut settled = 0;
        let mut first_error: Option<SwarmError> = None;
        for request_id in drained {
            match self
                .persist_approval_settlement(&request_id, None, reason, None)
                .await
            {
                Ok(seq) => {
                    {
                        let mut approvals = self.approvals.lock();
                        approvals.settling.remove(&request_id);
                        approvals.pending.remove(&request_id);
                        approvals.delivering.remove(&request_id);
                        approvals.settled.insert(
                            request_id.clone(),
                            SettledApproval {
                                decision: None,
                                reason,
                                input_id: None,
                            },
                        );
                    }
                    self.broadcast_settlement(seq).await;
                    settled += 1;
                }
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(settled),
        }
    }

    /// Current `required_actions`: unsettled, unexpired and owned by the open
    /// turn. Retrieval, list and SSE all read this one judgement.
    #[must_use]
    pub fn required_actions(&self) -> Vec<Value> {
        let now = unix_now();
        let active = self.state.lock().active.clone();
        let approvals = self.approvals.lock();
        approvals
            .pending
            .values()
            .filter(|pending| {
                pending.deadline_unix >= now
                    && active.as_deref() == Some(pending.turn_request_id.as_str())
            })
            .map(|pending| pending.action.to_json())
            .collect()
    }

    /// Official `agent.session.requires_action` snapshot for the current set.
    ///
    /// The `event_id` is derived from the oldest pending approval's persisted
    /// item `seq`; the same id is replayed for the same row, and the settlement
    /// frame uses the settlement row's `seq`, so the SDK's dedupe never drops a
    /// state change.
    pub async fn approval_snapshot_event(&self) -> Option<OfficialEvent> {
        let (seq, actions) = {
            let now = unix_now();
            let active = self.state.lock().active.clone();
            let approvals = self.approvals.lock();
            let seq = approvals
                .pending
                .values()
                .filter(|pending| {
                    pending.deadline_unix >= now
                        && active.as_deref() == Some(pending.turn_request_id.as_str())
                })
                .map(|pending| pending.item_seq)
                .min()?;
            let actions: Vec<Value> = approvals
                .pending
                .values()
                .filter(|pending| {
                    pending.deadline_unix >= now
                        && active.as_deref() == Some(pending.turn_request_id.as_str())
                })
                .map(|pending| pending.action.to_json())
                .collect();
            (seq, actions)
        };
        Some(OfficialEvent::requires_action(
            seq,
            self.official_session(SessionStatus::RequiresAction, None)
                .await
                .with_required_actions(actions),
        ))
    }

    async fn broadcast_required_actions(&self, seq: u64) {
        let session = self
            .official_session(SessionStatus::RequiresAction, None)
            .await;
        self.broadcast(StreamEvent::Official(OfficialEvent::requires_action(
            seq, session,
        )));
    }

    /// Persist one settlement row. The in-memory registry must not change until
    /// this succeeds: the durable log is the replay authority.
    ///
    /// The returned `seq` is the settlement row's persisted sequence, used as
    /// the snapshot `event_id` so live and replay frames dedupe.
    async fn persist_approval_settlement(
        &self,
        request_id: &str,
        decision: Option<ApprovalDecision>,
        reason: SettlementReason,
        input_id: Option<&str>,
    ) -> Result<u64, SwarmError> {
        let payload = json!({
            "type": crate::domain::MCP_APPROVAL_REQUEST,
            "state": "settled",
            "requestId": request_id,
            "decision": decision.map(ApprovalDecision::as_str),
            "reason": reason.as_str(),
            "inputId": input_id,
        });
        self.append_item(
            ProjectedItem {
                kind: ItemKind::Approval,
                source: ItemSource::root(),
                payload,
            },
            None,
        )
        .await
        .map_err(|error| {
            tracing::warn!(
                logical_id = %self.logical_id,
                request_id,
                error = %error,
                "swarm approval settlement was not persisted; the pending fact is kept"
            );
            SwarmError::required_action_settlement_failed(request_id, "append_settlement")
        })
    }

    /// Publish the post-settlement `requires_action` snapshot with the
    /// settlement row's `seq`.
    ///
    /// A settlement only clears the answered action: if another parallel
    /// approval is still open, the snapshot must keep reporting
    /// `requires_action` instead of pretending the session is progressing.
    async fn broadcast_settlement(&self, seq: u64) {
        let status = if self.required_actions().is_empty() {
            SessionStatus::InProgress
        } else {
            SessionStatus::RequiresAction
        };
        let session = self.official_session(status, None).await;
        self.broadcast(StreamEvent::Official(OfficialEvent::requires_action(
            seq, session,
        )));
    }

    /// Stage one projected item for the next batched append.
    ///
    /// Stream deltas first update the in-memory text (the turn flush uses it)
    /// and keep one identity per `(agent, messageId, kind)`; the delta itself is
    /// staged with that identity. Non-stream items are staged as they are.
    fn stage_item(&self, item: ProjectedItem, staged: &mut StagedItems) {
        if let Some((key, message_id, text)) = stream_parts(&item) {
            let item_id = {
                let mut state = self.state.lock();
                let item_id = state
                    .identities
                    .get(&key)
                    .cloned()
                    .unwrap_or_else(ItemView::new_item_id);
                state.identities.insert(key.clone(), item_id.clone());
                let stream = state
                    .streams
                    .entry(key.clone())
                    .or_insert_with(|| MessageStream {
                        kind: item.kind,
                        item_id: item_id.clone(),
                        text: String::new(),
                        first_seq: None,
                    });
                stream.text.push_str(&text);
                item_id
            };
            // Deltas keep the `messageId` so clients can associate a delta with
            // the final item even when they join mid-stream. `role` lets a
            // client filter assistant frames without assuming an item kind.
            let payload = json!({
                "role": "assistant",
                "messageId": message_id,
                "text": text,
                "delta": true,
            });
            let projected = ProjectedItem {
                kind: item.kind,
                source: item.source,
                payload,
            };
            staged.streams.push((staged.items.len(), key));
            staged.items.push((projected, Some(item_id)));
            return;
        }
        staged.items.push((item, None));
    }

    /// Persist and publish the staged run in one write.
    async fn flush_staged(&self, staged: &mut StagedItems) {
        if staged.items.is_empty() {
            return;
        }
        let streams = std::mem::take(&mut staged.streams);
        let mut committed = BTreeMap::new();
        let result = self
            .append_items_tracked(
                std::mem::take(&mut staged.items),
                ProgressWrite::WhileTurnOpen,
                &mut committed,
            )
            .await;
        // A run may need several durable groups; a failed group must not
        // discard the seqs the others already own, or `finish_turn` would order
        // those messages by key instead of by their persisted position.
        {
            // Persisted: the first chunk's seq orders this message among the
            // turn's outputs. A later chunk never rewrites it.
            let mut state = self.state.lock();
            for (index, key) in streams {
                if let (Some(seq), Some(stream)) =
                    (committed.get(&index), state.streams.get_mut(&key))
                {
                    stream.first_seq.get_or_insert(*seq);
                }
            }
        }
        if let Err(error) = result {
            self.report_append_failure("item", error).await;
        }
    }

    /// Append and publish one item (completion flushes; single-frame paths).
    async fn push_item(&self, item: ProjectedItem) {
        let mut staged = StagedItems::default();
        self.stage_item(item, &mut staged);
        self.flush_staged(&mut staged).await;
    }

    async fn append_item(
        &self,
        item: ProjectedItem,
        stable_item_id: Option<String>,
    ) -> Result<u64, SwarmError> {
        let mut seqs = self
            .append_items(vec![(item, stable_item_id)], ProgressWrite::WhileTurnOpen)
            .await?;
        Ok(seqs.remove(0))
    }

    /// Append a projected item and mark the session running (acceptance path).
    async fn append_starting_item(
        &self,
        item: ProjectedItem,
        stable_item_id: Option<String>,
    ) -> Result<u64, SwarmError> {
        let mut seqs = self
            .append_items(vec![(item, stable_item_id)], ProgressWrite::Running)
            .await?;
        Ok(seqs.remove(0))
    }

    /// Persist a run of items with consecutive seqs in one store write.
    ///
    /// The reader owns the sequence: `next_seq` is cached after the first
    /// append and only re-read when a conflict proves another writer advanced
    /// it. One transaction means a viewer never observes the second item of a
    /// batch before the first is durable, and the session cursor moves with the
    /// items instead of one round trip behind them.
    async fn append_items(
        &self,
        items: Vec<(ProjectedItem, Option<String>)>,
        progress: ProgressWrite,
    ) -> Result<Vec<u64>, SwarmError> {
        let mut committed = BTreeMap::new();
        self.append_items_tracked(items, progress, &mut committed)
            .await?;
        Ok(committed.into_values().collect())
    }

    /// [`Self::append_items`], recording each durable item's seq in
    /// `committed` under its index in `items`.
    ///
    /// Every group is attempted even when an earlier one fails. An item whose
    /// payload exceeds the store budget sits alone in its group (see
    /// [`Self::batch_end`]), so one rejected row must not discard the rest of
    /// the run; the first failure is reported once the remaining groups have
    /// been tried.
    async fn append_items_tracked(
        &self,
        items: Vec<(ProjectedItem, Option<String>)>,
        progress: ProgressWrite,
        committed: &mut BTreeMap<usize, u64>,
    ) -> Result<(), SwarmError> {
        if items.is_empty() {
            return Ok(());
        }
        let created_at = peri_time::now_utc_rfc3339();
        let prepared: Vec<PreparedItem> = items
            .into_iter()
            .map(|(item, stable_item_id)| {
                let (payload, truncated) = ItemView::fit_payload(item.payload);
                let payload_bytes = serde_json::to_vec(&payload).map_or(0, |bytes| bytes.len());
                PreparedItem {
                    kind: item.kind,
                    source: item.source,
                    payload,
                    truncated,
                    item_id: stable_item_id.unwrap_or_else(ItemView::new_item_id),
                    payload_bytes,
                }
            })
            .collect();
        let mut first_error = None;
        let mut start = 0;
        while start < prepared.len() {
            let end = Self::batch_end(&prepared, start);
            let group = &prepared[start..end];
            let payload_bytes: usize = group.iter().map(|item| item.payload_bytes).sum();
            let largest = group
                .iter()
                .map(|item| item.payload_bytes)
                .max()
                .unwrap_or(0);
            tracing::debug!(
                logical_id = %self.logical_id,
                rows = group.len(),
                payload_bytes,
                largest_row_bytes = largest,
                "append_items group"
            );
            match self.append_group(group, &created_at, progress).await {
                Ok(seqs) => committed.extend(
                    seqs.into_iter()
                        .enumerate()
                        .map(|(offset, seq)| (start + offset, seq)),
                ),
                Err(error) => first_error = first_error.or(Some(error)),
            }
            start = end;
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// End index of the group that starts at `start`.
    ///
    /// One durable write must stay under the store's per-request budget: an
    /// item's fitted payload alone can reach 1 MiB, so the batch stays below
    /// [`APPEND_BATCH_BYTES`]. A group always takes at least one item so an
    /// oversized item still fails on its own rather than stalling.
    fn batch_end(prepared: &[PreparedItem], start: usize) -> usize {
        let mut end = start + 1;
        let mut bytes = prepared[start].payload_bytes + APPEND_ROW_OVERHEAD;
        while end < prepared.len()
            && bytes + prepared[end].payload_bytes + APPEND_ROW_OVERHEAD <= APPEND_BATCH_BYTES
        {
            bytes += prepared[end].payload_bytes + APPEND_ROW_OVERHEAD;
            end += 1;
        }
        end
    }

    /// Persist and broadcast one group of items with consecutive seqs.
    async fn append_group(
        &self,
        prepared: &[PreparedItem],
        created_at: &str,
        progress: ProgressWrite,
    ) -> Result<Vec<u64>, SwarmError> {
        let mut appended: Option<Vec<u64>> = None;

        for attempt in 0..2 {
            // Held through the commit **and** the cache update below: an
            // append that reloaded `MAX(seq)` in the gap between the commit and
            // the cache write could otherwise be overwritten by a stale value,
            // costing the next writer a second conflict and its items.
            let _guard = self.seq_lock.lock().await;
            let last = self.next_seq().await?;
            let rows: Vec<SwarmItemRow> = prepared
                .iter()
                .enumerate()
                .map(|(index, item)| SwarmItemRow {
                    logical_id: self.logical_id.clone(),
                    seq: last + 1 + index as u64,
                    item_id: item.item_id.clone(),
                    kind: item.kind,
                    source: item.source.clone(),
                    payload: item.payload.clone(),
                    created_at: created_at.to_owned(),
                    truncated: item.truncated,
                })
                .collect();
            let write_progress = match progress {
                ProgressWrite::Running => true,
                ProgressWrite::WhileTurnOpen => self.state.lock().active.is_some(),
            };
            let update = write_progress.then(|| SwarmSessionUpdate {
                logical_id: self.logical_id.clone(),
                state: SessionState::Running,
                last_item_seq: last + rows.len() as u64,
                last_activity_at: created_at.to_owned(),
            });
            let result = self.store.swarm_append_items(&rows, update).await;
            match result {
                Ok(()) => {
                    let end = last + rows.len() as u64;
                    self.state.lock().next_seq = Some(end);
                    appended = Some(((last + 1)..=end).collect());
                    break;
                }
                Err(crate::store::StoreError::Conflict(reason)) if attempt == 0 => {
                    tracing::warn!(
                        logical_id = %self.logical_id,
                        reason = %reason,
                        "swarm seq conflict; reloading MAX(seq) and retrying once"
                    );
                    self.state.lock().next_seq = None;
                }
                Err(error) => return Err(store_failure("append_items", error)),
            }
        }

        let Some(seqs) = appended else {
            self.update_state_quiet(SessionState::Failed).await;
            return Err(SwarmError::new(
                ErrorKind::Unavailable,
                ErrorCode::SeqConflict,
                "swarm item sequence conflicted twice; session marked failed",
            ));
        };

        // Persisted before the broadcast: a heartbeat may report this seq
        // before a viewer has rendered the item, which is the intended
        // "session progressed" meaning.
        if let Some(last_seq) = seqs.last() {
            self.tail.fetch_max(*last_seq, Ordering::Relaxed);
        }
        for (seq, item) in seqs.iter().zip(prepared) {
            let view = ItemView {
                id: item.item_id.clone(),
                seq: *seq,
                item_id: item.item_id.clone(),
                kind: item.kind,
                source: item.source.clone(),
                payload: item.payload.clone(),
                created_at: created_at.to_owned(),
                truncated: item.truncated,
            };
            self.broadcast(StreamEvent::Item(view.clone()));
            for event in self.official_frames(&view) {
                self.broadcast(StreamEvent::Official(event));
            }
        }
        Ok(seqs)
    }

    /// Last persisted seq, read from the store only when the cache is cold.
    async fn next_seq(&self) -> Result<u64, SwarmError> {
        if let Some(seq) = self.state.lock().next_seq {
            return Ok(seq);
        }
        let last = self
            .store
            .swarm_last_seq(&self.logical_id)
            .await
            .map_err(|error| store_failure("last_seq", error))?;
        let mut state = self.state.lock();
        // A concurrent appender under the same `seq_lock` cannot interleave, so
        // the store value is still the cache's missing entry rather than a
        // value to overwrite.
        state.next_seq = Some(last);
        Ok(last)
    }

    /// Official frames one persisted message frame implies.
    ///
    /// Only the root agent's assistant `Message` items become official items.
    /// Thought chunks share the answer's `messageId`, and child agents have
    /// their own `source.agent_id`; both stay on the gateway's custom `item`
    /// surface and never enter `turn.item.added`/`item.done`.
    ///
    /// A user item opens a turn when none is open, but the opening turn's
    /// `turn.created`/`turn.item.added` frames are deferred to
    /// [`Self::begin_turn`]: acceptance must not publish a turn that may never
    /// run. A user item that arrives while a turn is open is a steer and is
    /// published against that turn immediately. Assistant deltas are skipped;
    /// only the final `complete:true` frame is the item the SDK consumes.
    fn official_frames(&self, view: &ItemView) -> Vec<OfficialEvent> {
        if !view.source.is_root() || view.kind != ItemKind::Message {
            return Vec::new();
        }
        let text = view
            .payload
            .get("text")
            .and_then(|text| text.as_str())
            .unwrap_or_default();
        match view.payload.get("role").and_then(|role| role.as_str()) {
            Some("user") => {
                let mut state = self.state.lock();
                match state.turn_id.clone() {
                    Some(turn_id) => vec![OfficialEvent::turn_item_added(
                        view.seq,
                        &self.logical_id,
                        &turn_id,
                        OfficialMessageItem::user(&view.item_id, &turn_id, text),
                    )],
                    None => {
                        let turn_id = turn_id_for_item(&view.item_id);
                        let created_at = unix_from_rfc3339(&view.created_at);
                        state.turn_id = Some(turn_id);
                        state.turn_created_at = Some(created_at);
                        state.pending_user_item = Some(PendingUserItem {
                            seq: view.seq,
                            item_id: view.item_id.clone(),
                            text: text.to_owned(),
                            created_at,
                        });
                        Vec::new()
                    }
                }
            }
            Some("assistant") if view.payload.get("complete") == Some(&json!(true)) => {
                let facts = {
                    let state = self.state.lock();
                    let Some(turn_id) = state.turn_id.clone() else {
                        return Vec::new();
                    };
                    // Written by `finish_turn`; a row persisted before these
                    // keys existed still replays as the final answer at index 0.
                    let phase = view
                        .payload
                        .get("phase")
                        .and_then(Value::as_str)
                        .unwrap_or("final_answer")
                        .to_owned();
                    let output_index = u32::try_from(
                        view.payload
                            .get("outputIndex")
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                    )
                    .unwrap_or(u32::MAX);
                    (turn_id, phase, output_index)
                };
                let (turn_id, phase, output_index) = facts;
                let item = OfficialMessageItem::assistant(&view.item_id, &turn_id, text)
                    .with_phase(Some(&phase));
                vec![
                    OfficialEvent::turn_item_added(
                        view.seq,
                        &self.logical_id,
                        &turn_id,
                        item.clone(),
                    ),
                    OfficialEvent::turn_content_part_added(
                        view.seq,
                        &self.logical_id,
                        &turn_id,
                        &view.item_id,
                        output_index,
                        text,
                    ),
                    OfficialEvent::turn_output_text_done(
                        view.seq,
                        &self.logical_id,
                        &turn_id,
                        &view.item_id,
                        output_index,
                        text,
                    ),
                    OfficialEvent::turn_content_part_done(
                        view.seq,
                        &self.logical_id,
                        &turn_id,
                        &view.item_id,
                        output_index,
                        text,
                    ),
                    OfficialEvent::turn_item_done(
                        view.seq,
                        &self.logical_id,
                        &turn_id,
                        output_index,
                        item,
                    ),
                ]
            }
            _ => Vec::new(),
        }
    }

    async fn report_append_failure(&self, operation: &'static str, error: SwarmError) {
        tracing::warn!(
            logical_id = %self.logical_id,
            operation,
            error = %error,
            "swarm item was not persisted; it is not broadcast either"
        );
        self.update_state_quiet(SessionState::Failed).await;
    }

    async fn set_session_state(
        &self,
        state: SessionState,
        last_item_seq: Option<u64>,
    ) -> Result<(), SwarmError> {
        // Serialized with item appends: an append decides whether to write
        // `running` and commits that decision under `seq_lock`, so the closing
        // `idle`/`failed` write takes the same lock. Otherwise a progress write
        // decided while the turn was open could land after the terminal and
        // leave the session `running` with no active turn.
        let _guard = self.seq_lock.lock().await;
        let last = match last_item_seq {
            Some(seq) => seq,
            None => self
                .store
                .swarm_last_seq(&self.logical_id)
                .await
                .map_err(|error| store_failure("last_seq", error))?,
        };
        self.store
            .swarm_update_session(SwarmSessionUpdate {
                logical_id: self.logical_id.clone(),
                state,
                last_item_seq: last,
                last_activity_at: peri_time::now_utc_rfc3339(),
            })
            .await
            .map_err(|error| store_failure("update_session", error))
    }

    async fn update_state_quiet(&self, state: SessionState) {
        if let Err(error) = self.set_session_state(state, None).await {
            tracing::warn!(
                logical_id = %self.logical_id,
                error = %error,
                "swarm session state update failed"
            );
        }
    }

    fn broadcast(&self, event: StreamEvent) {
        // No receivers is not an error: clients reload from `/items`.
        let _ = self.channel.send(event);
    }

    async fn settle(&self) {
        let deadline = Instant::now() + INGEST_SETTLE_LIMIT;
        loop {
            let before = self.state.lock().ingested;
            tokio::time::sleep(INGEST_SETTLE_WINDOW).await;
            let after = self.state.lock().ingested;
            if after == before || Instant::now() >= deadline {
                return;
            }
        }
    }
}

/// Stream identity and delta of a chunk item, if it is one.
fn stream_parts(item: &ProjectedItem) -> Option<(StreamKey, String, String)> {
    if !matches!(item.kind, ItemKind::Message | ItemKind::Thought) {
        return None;
    }
    let message_id = item.payload.get("messageId")?.as_str()?.to_owned();
    let text = item.payload.get("text")?.as_str()?.to_owned();
    // The kind separates reasoning from the answer even when Peri assigns both
    // the same `messageId`.
    let key = (
        item.source.agent_id.clone(),
        message_id.clone(),
        item.kind.as_str(),
    );
    Some((key, message_id, text))
}

#[cfg(test)]
#[path = "reader_test.rs"]
mod tests;

#[cfg(test)]
#[path = "approval_test.rs"]
mod approval_tests;
