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
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::{broadcast, watch, Mutex as AsyncMutex};

use crate::domain::{
    ErrorCode, ErrorKind, ItemKind, ItemSource, ItemView, SessionState, StreamEvent, SwarmError,
    TurnEvent,
};
use crate::store::{store_failure, SwarmItemRow, SwarmSessionUpdate, SwarmStore};

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

/// Key of one streamed text message: `(agentId, messageId)`.
type StreamKey = (String, String);

struct MessageStream {
    kind: ItemKind,
    item_id: String,
    text: String,
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
    /// `(agentId, messageId)` → stable `itemId`; kept across turns so a late
    /// chunk of an already-flushed message cannot mint a second identity.
    identities: HashMap<StreamKey, String>,
    /// Request id of the gateway turn currently running, if any.
    active: Option<String>,
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
    ) -> Self {
        let (active_watch, _receiver) = watch::channel(None);
        Self {
            logical_id: logical_id.into(),
            store,
            channel,
            seq_lock: AsyncMutex::new(()),
            state: Mutex::new(LogState::default()),
            active_watch,
        }
    }

    #[must_use]
    pub fn channel(&self) -> &broadcast::Sender<StreamEvent> {
        &self.channel
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

    /// Open a gateway turn: persist `running`, then announce `turn: started`.
    pub async fn begin_turn(&self, request_id: &str) -> Result<(), SwarmError> {
        {
            let mut state = self.state.lock();
            if state.active.is_some() {
                return Err(SwarmError::turn_in_progress());
            }
            state.active = Some(request_id.to_owned());
            state.generation = None;
            state.failure = None;
            state.streams.clear();
            state.steer_epoch = 0;
            state.continuation = None;
        }
        if let Err(error) = self.set_session_state(SessionState::Running, None).await {
            self.state.lock().active = None;
            return Err(error);
        }
        self.broadcast(StreamEvent::Turn(TurnEvent::started(request_id)));
        // Publish after the terminal-safe state is visible: `wait_turn_closed`
        // subscribes to this watch, and a stale `None` would let a caller treat
        // the still-open turn as already closed (a steer's `cancelled` attempt
        // terminal must not close it before the queued run takes over).
        self.active_watch.send_replace(Some(request_id.to_owned()));
        Ok(())
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
        for ((agent_id, message_id), stream) in streams {
            if stream.text.is_empty() {
                continue;
            }
            let item = ProjectedItem {
                kind: stream.kind,
                source: ItemSource {
                    agent_id,
                    task_id: None,
                },
                payload: json!({
                    "messageId": message_id,
                    "text": stream.text,
                    "complete": true,
                }),
            };
            if let Err(error) = self.append_item(item, Some(stream.item_id)).await {
                tracing::warn!(
                    logical_id = %self.logical_id,
                    error = %error,
                    "swarm completion item was not persisted"
                );
            }
        }
        match outcome {
            TurnOutcome::Completed(stop_reason) => {
                self.update_state_quiet(SessionState::Idle).await;
                self.broadcast(StreamEvent::Turn(TurnEvent::completed(
                    request_id,
                    stop_reason,
                )));
            }
            TurnOutcome::Failed { code, message } => {
                let message = failure.unwrap_or(message);
                self.update_state_quiet(SessionState::Failed).await;
                self.broadcast(StreamEvent::Turn(TurnEvent::failed(
                    request_id, code, message,
                )));
            }
        }
        // Wake `wait_turn_closed` only after the terminal is out, so a caller
        // that returns from the prompt observes a fully closed turn.
        self.active_watch.send_replace(None);
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
                let stream = state.streams.entry(key).or_insert_with(|| MessageStream {
                    kind: item.kind,
                    item_id: item_id.clone(),
                    text: String::new(),
                });
                stream.text.push_str(&text);
                item_id
            };
            // Deltas keep the `messageId` so clients can associate a delta with
            // the final item even when they join mid-stream.
            let payload = json!({ "messageId": message_id, "text": text, "delta": true });
            let projected = ProjectedItem {
                kind: item.kind,
                source: item.source,
                payload,
            };
            if let Err(error) = self.append_item(projected, Some(item_id)).await {
                self.report_append_failure("item", error).await;
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

        self.set_session_state(SessionState::Running, Some(seq))
            .await?;
        self.broadcast(StreamEvent::Item(ItemView {
            seq,
            item_id,
            kind: item.kind,
            source: item.source,
            payload,
            created_at,
            truncated,
        }));
        Ok(seq)
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
    let key = (item.source.agent_id.clone(), message_id.clone());
    Some((key, message_id, text))
}

#[cfg(test)]
#[path = "reader_test.rs"]
mod tests;
