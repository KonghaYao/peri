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

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::{broadcast, watch, Mutex as AsyncMutex};

use crate::domain::official::{unix_from_rfc3339, unix_now};
use crate::domain::{
    turn_id_for_item, ErrorCode, ErrorKind, ItemKind, ItemSource, ItemView, OfficialEvent,
    OfficialMessageItem, OfficialSession, OfficialTurn, SessionState, SessionStatus, StreamEvent,
    SwarmError, TurnEvent, TurnStatus,
};
use crate::store::{store_failure, SwarmItemRow, SwarmSessionUpdate, SwarmStore, SwarmTurnRow};

use super::projection::{self, Projected, ProjectedItem};

/// Quiet period that ends the drain fence before a turn terminal is published.
const INGEST_SETTLE_WINDOW: Duration = Duration::from_millis(15);
/// Upper bound of the drain fence, so a noisy stream cannot delay the terminal.
const INGEST_SETTLE_LIMIT: Duration = Duration::from_millis(500);

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
    /// [`LogState::steer_epoch`] observed when this run started. A run whose
    /// epoch is older has already been superseded by a newer steer.
    epoch: u64,
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
    /// Steers handed to the running turn. Each one cancels the in-flight
    /// attempt, so the turn is not finished by that attempt's terminal.
    steer_epoch: u64,
    /// Latest queued run adopted by the open turn.
    continuation: Option<ContinuationRun>,
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
        }
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
            .append_item(
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

    /// Consume one host notification. Projection errors are impossible by
    /// construction; persistence errors are logged and never abort the reader.
    pub async fn ingest(&self, method: &str, params: &Value) {
        let projected = projection::project(method, params);
        {
            let mut state = self.state.lock();
            state.ingested = state.ingested.wrapping_add(1);
        }
        for event in projected {
            match event {
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
                    } else if queued && state.active.is_some() && state.steer_epoch > 0 {
                        state.continuation = Some(ContinuationRun {
                            request_id,
                            epoch: state.steer_epoch,
                        });
                    }
                }
                Projected::QueueChanged { .. } => {}
                Projected::TurnCompleted {
                    request_id,
                    stop_reason,
                } => {
                    let closes = {
                        let state = self.state.lock();
                        let Some(active) = state.active.clone() else {
                            continue;
                        };
                        match request_id.as_deref() {
                            // The prompt attempt of the open turn. A steer
                            // cancels it on purpose: the turn continues in the
                            // queued run, so this terminal is not the turn's.
                            Some(running) if running == active => {
                                if state.steer_epoch == 0 {
                                    Some((active, stop_reason))
                                } else {
                                    None
                                }
                            }
                            // The queued run continuing the turn. A newer steer
                            // supersedes it, so only the latest run closes it.
                            Some(running) => state
                                .continuation
                                .as_ref()
                                .filter(|run| {
                                    run.request_id == running && run.epoch == state.steer_epoch
                                })
                                .map(|_| (active, stop_reason)),
                            // Legacy/terminal frames without a request id only
                            // close a turn that no steer has taken over.
                            None if state.steer_epoch == 0 => Some((active, stop_reason)),
                            None => None,
                        }
                    };
                    if let Some((active, stop_reason)) = closes {
                        self.finish_turn(&active, TurnOutcome::Completed(stop_reason))
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
            state.steer_epoch = 0;
            state.continuation = None;
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
    pub async fn finish_turn(&self, request_id: &str, outcome: TurnOutcome) {
        {
            let state = self.state.lock();
            if state.active.as_deref() != Some(request_id) {
                return;
            }
        }
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
            state.active = None;
            state.generation = None;
            state.steer_epoch = 0;
            state.continuation = None;
            (std::mem::take(&mut state.streams), state.failure.take())
        };
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
            .with_metadata(row.metadata)
            .with_prompt_cache_options(row.prompt_cache_options),
            Ok(None) => OfficialSession::new(&self.logical_id, status, 0, 0, error, None, None),
            Err(read_error) => {
                tracing::warn!(
                    logical_id = %self.logical_id,
                    error = %read_error,
                    "swarm session row was not readable for the official terminal frame"
                );
                OfficialSession::new(&self.logical_id, status, 0, 0, error, None, None)
            }
        }
    }

    /// Key/value projection of the last item appended (test and gateway aid).
    #[must_use]
    pub fn active_turn(&self) -> Option<String> {
        self.state.lock().active.clone()
    }

    /// Record a steer handed to the running turn and return its epoch.
    ///
    /// The host answers a dispatched steer by cancelling the in-flight attempt,
    /// so the turn must survive that attempt's `cancelled` terminal. The epoch
    /// is passed to [`Self::abandon_steer`] when the handoff fails.
    pub fn note_steer(&self) -> u64 {
        let mut state = self.state.lock();
        if state.active.is_some() {
            state.steer_epoch = state.steer_epoch.saturating_add(1);
        }
        state.steer_epoch
    }

    /// Undo [`Self::note_steer`] after a failed handoff. Skipped when a queued
    /// run was already adopted for that epoch: that run owns the turn now.
    pub fn abandon_steer(&self, epoch: u64) {
        let mut state = self.state.lock();
        let adopted = state
            .continuation
            .as_ref()
            .is_some_and(|run| run.epoch >= epoch);
        if state.steer_epoch == epoch && !adopted {
            state.steer_epoch = epoch.saturating_sub(1);
        }
    }

    /// Number of steers handed to the open turn.
    #[must_use]
    pub fn steer_epoch(&self) -> u64 {
        self.state.lock().steer_epoch
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

    /// Persist one projected item, then broadcast it.
    async fn push_item(&self, item: ProjectedItem) {
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
            match self.append_item(projected, Some(item_id)).await {
                Ok(seq) => {
                    // Persisted: the first chunk's seq orders this message among
                    // the turn's outputs. A later chunk never rewrites it.
                    let mut state = self.state.lock();
                    if let Some(stream) = state.streams.get_mut(&key) {
                        stream.first_seq.get_or_insert(seq);
                    }
                }
                Err(error) => self.report_append_failure("item", error).await,
            }
            return;
        }
        let kind = item.kind;
        if let Err(error) = self.append_item(item, None).await {
            self.report_append_failure(kind.as_str(), error).await;
        }
    }

    async fn append_item(
        &self,
        item: ProjectedItem,
        stable_item_id: Option<String>,
    ) -> Result<u64, SwarmError> {
        let (payload, truncated) = ItemView::fit_payload(item.payload);
        let item_id = stable_item_id.unwrap_or_else(ItemView::new_item_id);
        let created_at = peri_time::now_utc_rfc3339();
        let mut appended = None;

        for attempt in 0..2 {
            let guard = self.seq_lock.lock().await;
            let last = self
                .store
                .swarm_last_seq(&self.logical_id)
                .await
                .map_err(|error| store_failure("last_seq", error))?;
            let seq = last + 1;
            let result = self
                .store
                .swarm_append_item(SwarmItemRow {
                    logical_id: self.logical_id.clone(),
                    seq,
                    item_id: item_id.clone(),
                    kind: item.kind,
                    source: item.source.clone(),
                    payload: payload.clone(),
                    created_at: created_at.clone(),
                    truncated,
                })
                .await;
            drop(guard);
            match result {
                Ok(()) => {
                    appended = Some(seq);
                    break;
                }
                Err(crate::store::StoreError::Conflict(reason)) if attempt == 0 => {
                    tracing::warn!(
                        logical_id = %self.logical_id,
                        reason = %reason,
                        "swarm seq conflict; reloading MAX(seq) and retrying once"
                    );
                }
                Err(error) => return Err(store_failure("append_item", error)),
            }
        }

        let Some(seq) = appended else {
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
        self.tail.fetch_max(seq, Ordering::Relaxed);
        self.set_session_state(SessionState::Running, Some(seq))
            .await?;
        let view = ItemView {
            id: item_id.clone(),
            seq,
            item_id,
            kind: item.kind,
            source: item.source,
            payload,
            created_at,
            truncated,
        };
        self.broadcast(StreamEvent::Item(view.clone()));
        for event in self.official_frames(&view) {
            self.broadcast(StreamEvent::Official(event));
        }
        Ok(seq)
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
