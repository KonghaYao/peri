use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::{Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::{broadcast, Mutex as AsyncMutex};
use uuid::Uuid;

use super::official_items::item_page;
use super::{
    AcceptedInput, CreateSessionCommand, CreateSessionResult, EventSubscription, PendingTurn,
    SessionUpdateCommand, SwarmService,
};
use crate::config::SwarmConfig;
use crate::domain::official::{unix_from_rfc3339, unix_now};
use crate::domain::{
    new_root_session_id, turn_id_for_item, ActorKey, Disposition, EnvironmentKind, EnvironmentRef,
    ErrorCode, ErrorKind, EventCursor, InputKind, InputMode, InputOutcome, InputState,
    InputSubmission, ItemKind, ItemPage, ItemSource, ItemView, ItemsWindow, LogicalId,
    OfficialEvent, OfficialMessageItem, OfficialSession, OfficialTurn, SessionPage, SessionState,
    SessionStatus, SessionView, SessionsWindow, StreamEvent, SwarmError, TurnEvent, TurnPage,
    TurnStatus, TurnView, TurnsWindow,
};
use crate::store::{
    store_failure, InputPut, StoreError, SwarmInputRow, SwarmItemRow, SwarmSessionPatch,
    SwarmSessionRow, SwarmSessionUpdate, SwarmStore, SwarmTurnRow,
};

/// Deterministic fake turn policy. `Manual` leaves the turn `running` so tests
/// can exercise `turn_in_progress` and steer paths without a real model.
/// `Deferred` returns the pending turn from acceptance without executing it, so
/// tests can observe the `202`-before-terminal boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TurnPolicy {
    Immediate,
    Manual,
    Deferred,
}

#[derive(Clone, Debug)]
pub struct ServiceConfig {
    /// Deployment default model; a request that names a different model is
    /// rejected (`design §0.4-3`: one model per actor).
    pub model: Option<String>,
    pub turn_policy: TurnPolicy,
    pub channel_capacity: usize,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            model: None,
            turn_policy: TurnPolicy::Immediate,
            channel_capacity: 256,
        }
    }
}

impl ServiceConfig {
    #[must_use]
    pub fn from_deployment(config: &SwarmConfig) -> Self {
        Self {
            model: config.model.default_model.clone(),
            ..Self::default()
        }
    }
}

/// Observable call order for the fake; used by the `stream:true` ordering test.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServiceCall {
    OpenStream(String),
    CreateSession(String),
    Subscribe(String),
    SubmitInput(String),
    ListItems(String),
}

#[derive(Clone, Debug)]
struct ActiveTurn {
    request_id: String,
    /// Internal turn identity derived from the user item that opened the turn.
    turn_id: String,
    started_at: String,
    /// Unix second when the turn opened; persisted and used for `turn.created`.
    created_at: i64,
}

#[derive(Clone, Debug)]
struct StreamHub {
    sender: broadcast::Sender<StreamEvent>,
    /// Session tail reported by SSE heartbeats; updated on every append.
    tail: Arc<AtomicU64>,
}

#[derive(Debug, Default)]
struct ServiceInner {
    channels: HashMap<String, StreamHub>,
    turns: HashMap<String, ActiveTurn>,
    calls: Vec<ServiceCall>,
}

/// S3 fake. Persists through any [`SwarmStore`], so the same test can swap in a
/// failing or remote implementation.
pub struct InMemorySwarmService {
    store: std::sync::Arc<dyn SwarmStore>,
    config: ServiceConfig,
    inner: Mutex<ServiceInner>,
    seq_lock: AsyncMutex<()>,
    // 防止 fake 的开轮写入与取消终态交错。
    turn_lock: AsyncMutex<()>,
}

impl std::fmt::Debug for InMemorySwarmService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InMemorySwarmService")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl InMemorySwarmService {
    #[must_use]
    pub fn new(store: std::sync::Arc<dyn SwarmStore>, config: ServiceConfig) -> Self {
        Self {
            store,
            config,
            inner: Mutex::new(ServiceInner::default()),
            seq_lock: AsyncMutex::new(()),
            turn_lock: AsyncMutex::new(()),
        }
    }

    #[must_use]
    pub fn in_memory(config: ServiceConfig) -> Self {
        Self::new(
            std::sync::Arc::new(crate::store::MemorySwarmStore::new()),
            config,
        )
    }

    /// Recorded calls of this fake, in invocation order.
    #[must_use]
    pub fn calls(&self) -> Vec<ServiceCall> {
        self.with_inner(|inner| inner.calls.clone())
    }

    fn with_inner<T>(&self, read: impl FnOnce(&mut ServiceInner) -> T) -> T {
        let mut guard: MutexGuard<'_, ServiceInner> =
            self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        read(&mut guard)
    }

    fn record(&self, call: ServiceCall) {
        self.with_inner(|inner| inner.calls.push(call));
    }

    fn hub_for(&self, logical_id: &str) -> StreamHub {
        self.with_inner(|inner| {
            inner
                .channels
                .entry(logical_id.to_owned())
                .or_insert_with(|| {
                    let (sender, _receiver) =
                        broadcast::channel(self.config.channel_capacity.max(1));
                    StreamHub {
                        sender,
                        tail: Arc::new(AtomicU64::new(0)),
                    }
                })
                .clone()
        })
    }

    fn note_tail(&self, logical_id: &str, seq: u64) {
        let tail = self.with_inner(|inner| {
            inner
                .channels
                .get(logical_id)
                .map(|hub| Arc::clone(&hub.tail))
        });
        if let Some(tail) = tail {
            tail.fetch_max(seq, Ordering::Relaxed);
        }
    }

    fn broadcast(&self, logical_id: &str, event: StreamEvent) {
        let sender =
            self.with_inner(|inner| inner.channels.get(logical_id).map(|hub| hub.sender.clone()));
        if let Some(sender) = sender {
            // No receivers is not an error: SSE clients reload from `/items`.
            let _ = sender.send(event);
        }
    }

    async fn require_session(&self, logical_id: &LogicalId) -> Result<SwarmSessionRow, SwarmError> {
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

    async fn session_view(&self, row: &SwarmSessionRow) -> Result<SessionView, SwarmError> {
        let last_item_seq = self
            .store
            .swarm_last_seq(&row.logical_id)
            .await
            .map_err(|error| store_failure("last_seq", error))?;
        let mut row = row.clone();
        row.last_item_seq = last_item_seq;
        let active = self.with_inner(|inner| inner.turns.get(&row.logical_id).cloned());
        let turn = active.as_ref().map(|active| TurnView {
            request_id: Some(active.request_id.clone()),
            started_at: Some(active.started_at.clone()),
        });

        let sandbox = match row.environment.kind {
            EnvironmentKind::None => None,
            EnvironmentKind::OpenAiHosted => None,
        };

        Ok(super::project_session(&row, turn, sandbox, Vec::new()))
    }

    /// Minimal official session snapshot for `agent.session.idle/failed`.
    async fn official_session(
        &self,
        logical_id: &LogicalId,
        status: SessionStatus,
        error: Option<String>,
    ) -> Result<OfficialSession, SwarmError> {
        let row = self.require_session(logical_id).await?;
        Ok(OfficialSession::new(
            logical_id.as_str(),
            status,
            unix_from_rfc3339(&row.created_at),
            unix_from_rfc3339(&row.last_activity_at),
            error,
            row.model,
            row.instructions,
        )
        .with_environment(row.environment.clone().normalized())
        .with_metadata(row.metadata)
        .with_prompt_cache_options(row.prompt_cache_options))
    }

    async fn view_by_id(&self, logical_id: &LogicalId) -> Result<SessionView, SwarmError> {
        let row = self.require_session(logical_id).await?;
        self.session_view(&row).await
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

    async fn set_session_state(
        &self,
        logical_id: &LogicalId,
        state: SessionState,
        last_item_seq: Option<u64>,
    ) -> Result<(), SwarmError> {
        let row = self.require_session(logical_id).await?;
        let update = SwarmSessionUpdate {
            logical_id: logical_id.as_str().to_owned(),
            state,
            last_item_seq: last_item_seq.unwrap_or(row.last_item_seq),
            last_activity_at: peri_time::now_utc_rfc3339(),
        };
        self.store
            .swarm_update_session(update)
            .await
            .map_err(|error| store_failure("update_session", error))
    }

    /// Append one item, allocating `seq` from the store's `MAX(seq)`. A conflict
    /// is retried exactly once; a second conflict is surfaced as `503` and the
    /// session is marked failed (never renumbered silently).
    async fn append_item(
        &self,
        logical_id: &LogicalId,
        kind: ItemKind,
        source: ItemSource,
        payload: serde_json::Value,
    ) -> Result<u64, SwarmError> {
        self.append_item_with_id(logical_id, kind, source, payload, None)
            .await
    }

    async fn append_item_with_id(
        &self,
        logical_id: &LogicalId,
        kind: ItemKind,
        source: ItemSource,
        payload: serde_json::Value,
        stable_item_id: Option<String>,
    ) -> Result<u64, SwarmError> {
        let (payload, truncated) = ItemView::fit_payload(payload);
        let item_id = stable_item_id.unwrap_or_else(ItemView::new_item_id);
        let created_at = peri_time::now_utc_rfc3339();
        let mut appended_seq = None;

        for attempt in 0..2 {
            let guard = self.seq_lock.lock().await;
            let last = self
                .store
                .swarm_last_seq(logical_id.as_str())
                .await
                .map_err(|error| store_failure("last_seq", error))?;
            let seq = last + 1;
            let row = SwarmItemRow {
                logical_id: logical_id.as_str().to_owned(),
                seq,
                item_id: item_id.clone(),
                kind,
                source: source.clone(),
                payload: payload.clone(),
                created_at: created_at.clone(),
                truncated,
            };
            let result = self.store.swarm_append_item(row).await;
            drop(guard);
            match result {
                Ok(()) => {
                    appended_seq = Some(seq);
                    break;
                }
                Err(StoreError::Conflict(reason)) if attempt == 0 => {
                    tracing::warn!(
                        logical_id = logical_id.as_str(),
                        reason = %reason,
                        "swarm seq conflict; reloading MAX(seq) and retrying once"
                    );
                }
                Err(error) => return Err(store_failure("append_item", error)),
            }
        }

        let Some(seq) = appended_seq else {
            self.set_session_state(logical_id, SessionState::Failed, None)
                .await?;
            return Err(SwarmError::new(
                ErrorKind::Unavailable,
                ErrorCode::SeqConflict,
                "swarm item sequence conflicted twice; session marked failed",
            ));
        };

        self.note_tail(logical_id.as_str(), seq);
        self.set_session_state(logical_id, SessionState::Running, Some(seq))
            .await?;
        let view = ItemView {
            id: item_id.clone(),
            seq,
            item_id: item_id.clone(),
            kind,
            source,
            payload,
            created_at,
            truncated,
        };
        self.broadcast(logical_id.as_str(), StreamEvent::Item(view.clone()));
        self.broadcast_official_item(logical_id, &view).await;
        Ok(seq)
    }

    /// Official frames for message items.
    ///
    /// A user item accepted while no turn is open opens a turn: the session
    /// enters `in_progress`, then `turn.created`, `turn.in_progress` and the
    /// item. Assistant items use the open turn and report completion through the
    /// item, content-part and output-text frames. This fake completes in one
    /// step and has no live chunks, so its live path sends no delta; replay
    /// synthesizes one full-text delta from the completion row.
    async fn broadcast_official_item(&self, logical_id: &LogicalId, view: &ItemView) {
        if !view.source.is_root() || view.kind != ItemKind::Message {
            // Thought chunks share the answer's `messageId`; child items have
            // their own source. Neither produces an official item frame.
            return;
        }
        let role = view.payload.get("role").and_then(|role| role.as_str());
        let open_turn = self.with_inner(|inner| {
            inner
                .turns
                .get(logical_id.as_str())
                .map(|turn| turn.turn_id.clone())
        });
        let (turn_id, item, opened) = match role {
            Some("user") => {
                let text = view
                    .payload
                    .get("text")
                    .and_then(|text| text.as_str())
                    .unwrap_or_default();
                let (turn_id, opened) = match open_turn {
                    Some(turn_id) => (turn_id, false),
                    None => (turn_id_for_item(&view.item_id), true),
                };
                (
                    turn_id.clone(),
                    OfficialMessageItem::user(&view.item_id, &turn_id, text),
                    opened,
                )
            }
            Some("assistant") if view.payload.get("complete") == Some(&json!(true)) => {
                let Some(turn_id) = open_turn else {
                    return;
                };
                let text = view
                    .payload
                    .get("text")
                    .and_then(|text| text.as_str())
                    .unwrap_or_default();
                let phase = view
                    .payload
                    .get("phase")
                    .and_then(|phase| phase.as_str())
                    .unwrap_or("final_answer");
                let output_index = u32::try_from(
                    view.payload
                        .get("outputIndex")
                        .and_then(|index| index.as_u64())
                        .unwrap_or(0),
                )
                .unwrap_or(u32::MAX);
                self.broadcast_assistant_item(
                    logical_id,
                    view,
                    &turn_id,
                    text,
                    phase,
                    output_index,
                );
                return;
            }
            _ => return,
        };
        if opened {
            let session = match self
                .official_session(logical_id, SessionStatus::InProgress, None)
                .await
            {
                Ok(session) => session,
                Err(error) => {
                    tracing::warn!(
                        logical_id = logical_id.as_str(),
                        error = %error,
                        "official session.in_progress snapshot was not readable"
                    );
                    OfficialSession::new(
                        logical_id.as_str(),
                        SessionStatus::InProgress,
                        0,
                        0,
                        None,
                        None,
                        None,
                    )
                }
            };
            self.broadcast(
                logical_id.as_str(),
                StreamEvent::Official(OfficialEvent::session_in_progress(&turn_id, session)),
            );
            self.broadcast(
                logical_id.as_str(),
                StreamEvent::Official(OfficialEvent::turn_created(
                    logical_id.as_str(),
                    &turn_id,
                    OfficialTurn::new(
                        logical_id.as_str(),
                        &turn_id,
                        TurnStatus::InProgress,
                        crate::domain::official::unix_from_rfc3339(&view.created_at),
                        None,
                    ),
                )),
            );
            self.broadcast(
                logical_id.as_str(),
                StreamEvent::Official(OfficialEvent::turn_in_progress(
                    logical_id.as_str(),
                    &turn_id,
                    OfficialTurn::new(
                        logical_id.as_str(),
                        &turn_id,
                        TurnStatus::InProgress,
                        crate::domain::official::unix_from_rfc3339(&view.created_at),
                        None,
                    ),
                )),
            );
        }
        self.broadcast(
            logical_id.as_str(),
            StreamEvent::Official(OfficialEvent::turn_item_added(
                view.seq,
                logical_id.as_str(),
                &turn_id,
                item,
            )),
        );
    }

    fn broadcast_assistant_item(
        &self,
        logical_id: &LogicalId,
        view: &ItemView,
        turn_id: &str,
        text: &str,
        phase: &str,
        output_index: u32,
    ) {
        let message =
            OfficialMessageItem::assistant(&view.item_id, turn_id, text).with_phase(Some(phase));
        for event in [
            OfficialEvent::turn_assistant_item_added(
                &view.item_id,
                logical_id.as_str(),
                turn_id,
                message.clone(),
            ),
            OfficialEvent::turn_content_part_added(
                &view.item_id,
                logical_id.as_str(),
                turn_id,
                output_index,
                text,
            ),
            OfficialEvent::turn_output_text_done(
                view.seq,
                logical_id.as_str(),
                turn_id,
                &view.item_id,
                output_index,
                text,
            ),
            OfficialEvent::turn_content_part_done(
                view.seq,
                logical_id.as_str(),
                turn_id,
                &view.item_id,
                output_index,
                text,
            ),
            OfficialEvent::turn_item_done(
                view.seq,
                logical_id.as_str(),
                turn_id,
                output_index,
                message,
            ),
        ] {
            self.broadcast(logical_id.as_str(), StreamEvent::Official(event));
        }
    }

    /// Persist one accepted user input as a single-frame `message` item.
    ///
    /// Mirrors the actor: `inputId` keys the item identity and the frame has no
    /// `complete` flag, so it never terminates an assistant replay.
    async fn record_user_input(
        &self,
        logical_id: &LogicalId,
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
            .append_item_with_id(
                logical_id,
                ItemKind::Message,
                ItemSource::root(),
                payload,
                Some(item_id.clone()),
            )
            .await?;
        Ok((seq, item_id))
    }

    /// Persist the turn row. `created_at` stays the opening value across the
    /// terminal write.
    async fn persist_turn(&self, row: SwarmTurnRow) -> Result<(), SwarmError> {
        self.store
            .swarm_upsert_turn(row)
            .await
            .map_err(|error| store_failure("upsert_turn", error))
    }

    async fn fail_turn(
        &self,
        logical_id: &LogicalId,
        request_id: &str,
        turn_id: &str,
        error: &SwarmError,
    ) {
        let active = self.with_inner(|inner| inner.turns.remove(logical_id.as_str()));
        let created_at = active.map_or_else(unix_now, |active| active.created_at);
        if let Err(update_error) = self
            .persist_turn(SwarmTurnRow {
                logical_id: logical_id.as_str().to_owned(),
                turn_id: turn_id.to_owned(),
                status: TurnStatus::Failed,
                created_at,
                started_at: Some(created_at),
                completed_at: Some(unix_now()),
                error: Some(json!({ "code": "internal_error", "message": error.message() })),
                usage: None,
                subagent_id: None,
            })
            .await
        {
            tracing::warn!(
                logical_id = logical_id.as_str(),
                error = %update_error,
                "failed to persist the failed turn row"
            );
        }
        if let Err(update_error) = self
            .set_session_state(logical_id, SessionState::Failed, None)
            .await
        {
            tracing::warn!(
                logical_id = logical_id.as_str(),
                error = %update_error,
                "failed to persist failed session state"
            );
        }
        self.broadcast(
            logical_id.as_str(),
            StreamEvent::Turn(TurnEvent::failed(
                request_id,
                turn_id,
                -32603,
                error.message(),
            )),
        );
        let failed_turn = OfficialTurn::new(
            logical_id.as_str(),
            turn_id,
            TurnStatus::Failed,
            created_at,
            Some(unix_now()),
        )
        .with_error(Some(error.message().to_owned()));
        self.broadcast(
            logical_id.as_str(),
            StreamEvent::Official(OfficialEvent::turn_failed(
                logical_id.as_str(),
                turn_id,
                failed_turn,
            )),
        );
        self.broadcast(
            logical_id.as_str(),
            StreamEvent::Official(OfficialEvent::error(
                turn_id,
                logical_id.as_str(),
                error.message(),
            )),
        );
        match self
            .official_session(
                logical_id,
                SessionStatus::Failed,
                Some(error.message().to_owned()),
            )
            .await
        {
            Ok(session) => self.broadcast(
                logical_id.as_str(),
                StreamEvent::Official(OfficialEvent::session_failed(turn_id, session)),
            ),
            Err(read_error) => tracing::warn!(
                logical_id = logical_id.as_str(),
                error = %read_error,
                "official session.failed snapshot was not readable"
            ),
        }
    }

    async fn run_turn(
        &self,
        logical_id: &LogicalId,
        text: &str,
        turn_id: &str,
    ) -> Result<u64, SwarmError> {
        let _turn = self.turn_lock.lock().await;
        let active = ActiveTurn {
            request_id: Uuid::now_v7().to_string(),
            turn_id: turn_id.to_owned(),
            started_at: peri_time::now_utc_rfc3339(),
            created_at: unix_now(),
        };
        let request_id = &active.request_id;
        let created_at = active.created_at;
        self.with_inner(|inner| {
            inner
                .turns
                .insert(logical_id.as_str().to_owned(), active.clone());
        });
        self.set_session_state(logical_id, SessionState::Running, None)
            .await?;
        if let Err(error) = self
            .persist_turn(SwarmTurnRow {
                logical_id: logical_id.as_str().to_owned(),
                turn_id: turn_id.to_owned(),
                status: TurnStatus::InProgress,
                created_at,
                started_at: Some(created_at),
                completed_at: None,
                error: None,
                usage: None,
                subagent_id: None,
            })
            .await
        {
            // No official frame was published for this attempt yet; clear the
            // slot so the session is not wedged on a turn nobody can observe.
            self.with_inner(|inner| {
                inner.turns.remove(logical_id.as_str());
            });
            let _ = self
                .set_session_state(logical_id, SessionState::Failed, None)
                .await;
            return Err(error);
        }
        self.broadcast(
            logical_id.as_str(),
            StreamEvent::Turn(TurnEvent::started(request_id.clone(), turn_id)),
        );

        let payload = json!({
            "role": "assistant",
            "messageId": format!("msg_{}", Uuid::now_v7()),
            "text": format!("fake-echo: {text}"),
            "complete": true,
            // The fake turn has exactly one assistant message: it is the final
            // answer and output 0. Persisting both keeps live and replay equal.
            "phase": "final_answer",
            "outputIndex": 0,
        });
        let seq = match self
            .append_item(logical_id, ItemKind::Message, ItemSource::root(), payload)
            .await
        {
            Ok(seq) => seq,
            Err(error) => {
                self.fail_turn(logical_id, request_id, turn_id, &error)
                    .await;
                return Err(error);
            }
        };

        if self.config.turn_policy == TurnPolicy::Immediate {
            self.with_inner(|inner| inner.turns.remove(logical_id.as_str()));
            self.finish_turn(logical_id, active, TurnStatus::Completed, "end_turn", seq)
                .await?;
        }
        Ok(seq)
    }

    async fn finish_turn(
        &self,
        logical_id: &LogicalId,
        active: ActiveTurn,
        status: TurnStatus,
        stop_reason: &str,
        last_seq: u64,
    ) -> Result<(), SwarmError> {
        let ActiveTurn {
            request_id,
            turn_id,
            created_at,
            ..
        } = active;
        let completed_at = Some(unix_now());
        if let Err(error) = self
            .persist_turn(SwarmTurnRow {
                logical_id: logical_id.as_str().to_owned(),
                turn_id: turn_id.clone(),
                status,
                created_at,
                started_at: Some(created_at),
                completed_at,
                error: None,
                usage: None,
                subagent_id: None,
            })
            .await
        {
            tracing::warn!(logical_id = logical_id.as_str(), error = %error, "failed to persist the terminal turn row");
        }
        self.set_session_state(logical_id, SessionState::Idle, Some(last_seq))
            .await?;
        self.broadcast(
            logical_id.as_str(),
            StreamEvent::Turn(TurnEvent::completed(request_id, &turn_id, stop_reason)),
        );
        let completed_turn = OfficialTurn::new(
            logical_id.as_str(),
            &turn_id,
            status,
            created_at,
            completed_at,
        );
        self.broadcast(
            logical_id.as_str(),
            StreamEvent::Official(OfficialEvent::turn_completed(
                logical_id.as_str(),
                &turn_id,
                completed_turn,
            )),
        );
        match self
            .official_session(logical_id, SessionStatus::Idle, None)
            .await
        {
            Ok(session) => self.broadcast(
                logical_id.as_str(),
                StreamEvent::Official(OfficialEvent::session_idle(&turn_id, session)),
            ),
            Err(read_error) => tracing::warn!(
                logical_id = logical_id.as_str(), error = %read_error, "official session.idle snapshot was not readable"
            ),
        }
        Ok(())
    }

    async fn cancel_turn(
        &self,
        logical_id: &LogicalId,
        request_id: &str,
    ) -> Result<(), SwarmError> {
        let _turn = self.turn_lock.lock().await;
        let last_seq = self.last_seq(logical_id).await?;
        let active = self.with_inner(|inner| {
            let id = logical_id.as_str();
            if inner
                .turns
                .get(id)
                .is_some_and(|active| active.request_id == request_id)
            {
                inner.turns.remove(id)
            } else {
                None
            }
        });
        if let Some(active) = active {
            self.finish_turn(
                logical_id,
                active,
                TurnStatus::Cancelled,
                "cancelled",
                last_seq,
            )
            .await?;
        }
        Ok(())
    }

    /// Persist the create's initial input and either run it or defer it.
    ///
    /// The user item is written exactly once in both modes. A deferred turn
    /// carries the `turn_id` derived from that item, so running it later opens
    /// the same turn the synchronous path would and cannot duplicate the input.
    async fn accept_initial_turn(
        &self,
        logical_id: &LogicalId,
        text: &str,
        defer: bool,
    ) -> Result<Option<PendingTurn>, SwarmError> {
        let (_, item_id) = self.record_user_input(logical_id, None, text).await?;
        let turn_id = turn_id_for_item(&item_id);
        if defer {
            return Ok(Some(PendingTurn {
                logical_id: logical_id.clone(),
                text: text.to_owned(),
                turn_id,
            }));
        }
        self.run_turn(logical_id, text, &turn_id).await?;
        Ok(None)
    }

    /// Backlog for a subscription. A tail subscription has no backlog and is
    /// answered without reading the session row (the real actor has the same
    /// rule, so `stream:true` create works before the row exists). An explicit
    /// cursor still requires a known session.
    async fn replay_after(
        &self,
        logical_id: &LogicalId,
        start: EventCursor,
    ) -> Result<Vec<StreamEvent>, SwarmError> {
        if start.cursor().is_none() {
            return Ok(Vec::new());
        }
        let session = self.require_session(logical_id).await?;
        let live_open_turn = self.with_inner(|inner| inner.turns.contains_key(logical_id.as_str()));
        super::replay_backlog(self.store.as_ref(), &session, start, live_open_turn).await
    }
}

#[async_trait]
impl SwarmService for InMemorySwarmService {
    async fn create_session(
        &self,
        command: CreateSessionCommand,
    ) -> Result<CreateSessionResult, SwarmError> {
        self.record(ServiceCall::CreateSession(
            command.logical_id.as_str().to_owned(),
        ));
        let model = self.resolve_model(command.model.as_deref())?;
        let cwd = command.cwd.to_string_lossy().into_owned();

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
                .with_details(json!({
                    "configured": row.model,
                    "requested": model,
                })));
            }
            let mut pending_initial = None;
            if let Some(text) = command.initial_input.as_deref() {
                if row.state == SessionState::Running {
                    return Err(SwarmError::turn_in_progress());
                }
                pending_initial = self
                    .accept_initial_turn(&command.logical_id, text, command.defer_initial_turn)
                    .await?;
            }
            let session = self.view_by_id(&command.logical_id).await?;
            return Ok(CreateSessionResult {
                session,
                created: false,
                pending_initial,
            });
        }

        let now = peri_time::now_utc_rfc3339();
        // The fake records the requested environment but never contacts Bun;
        // hosted allocation and workspace injection are actor behavior.
        let environment = match command.environment {
            EnvironmentKind::None => EnvironmentRef::none(),
            EnvironmentKind::OpenAiHosted => EnvironmentRef {
                kind: EnvironmentKind::OpenAiHosted,
                id: Some(
                    command
                        .environment_id
                        .clone()
                        .unwrap_or_else(|| format!("env_{}", Uuid::now_v7())),
                ),
            },
        };
        let row = SwarmSessionRow {
            logical_id: command.logical_id.as_str().to_owned(),
            root_session_id: command
                .root_session_id
                .clone()
                .unwrap_or_else(new_root_session_id),
            actor_key: ActorKey::new(command.logical_id.clone()).storage_key(),
            cwd,
            state: SessionState::Idle,
            model,
            instructions: command.instructions.clone(),
            mcp_tools: command.mcp_tools.clone(),
            environment,
            metadata: command.metadata_for_new_row(),
            prompt_cache_options: command.prompt_cache_options_for_new_row(),
            created_at: now.clone(),
            last_activity_at: now,
            last_item_seq: 0,
        };
        match self.store.swarm_upsert_session(row).await {
            Ok(()) => {}
            Err(StoreError::Conflict(_)) => {
                return Err(SwarmError::new(
                    ErrorKind::Conflict,
                    ErrorCode::IdentityConflict,
                    "logicalId was created concurrently with a different identity",
                )
                .with_details(json!({ "field": "logicalId" })));
            }
            Err(error) => return Err(store_failure("upsert_session", error)),
        }

        let mut pending_initial = None;
        if let Some(text) = command.initial_input.as_deref() {
            pending_initial = self
                .accept_initial_turn(&command.logical_id, text, command.defer_initial_turn)
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
        super::list_sessions_from_store(self.store.as_ref(), &window).await
    }

    async fn update_session(
        &self,
        logical_id: &LogicalId,
        command: SessionUpdateCommand,
    ) -> Result<SessionView, SwarmError> {
        let row = self.require_session(logical_id).await?;
        if self.with_inner(|inner| inner.turns.contains_key(logical_id.as_str())) {
            return Err(SwarmError::turn_in_progress_for_mutation(
                logical_id.as_str(),
            ));
        }
        if super::persisted_turn_resume_required(self.store.as_ref(), &row).await? {
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
        self.view_by_id(logical_id).await
    }

    async fn delete_session(&self, logical_id: &LogicalId) -> Result<(), SwarmError> {
        let row = self.require_session(logical_id).await?;
        if self.with_inner(|inner| inner.turns.contains_key(logical_id.as_str())) {
            return Err(SwarmError::turn_in_progress_for_mutation(
                logical_id.as_str(),
            ));
        }
        if super::persisted_turn_resume_required(self.store.as_ref(), &row).await? {
            return Err(SwarmError::upstream_unavailable("session_resume_required"));
        }
        self.store
            .swarm_delete_session(logical_id.as_str())
            .await
            .map_err(|error| store_failure("delete_session", error))?;
        self.with_inner(|inner| {
            inner.turns.remove(logical_id.as_str());
            inner.channels.remove(logical_id.as_str());
        });
        Ok(())
    }

    async fn list_turns(
        &self,
        logical_id: &LogicalId,
        window: TurnsWindow,
    ) -> Result<TurnPage, SwarmError> {
        self.require_session(logical_id).await?;
        super::list_turns_from_store(self.store.as_ref(), logical_id.as_str(), &window).await
    }

    async fn get_turn(
        &self,
        logical_id: &LogicalId,
        turn_id: &str,
    ) -> Result<OfficialTurn, SwarmError> {
        self.require_session(logical_id).await?;
        super::get_turn_from_store(self.store.as_ref(), logical_id.as_str(), turn_id).await
    }

    async fn accept_input(
        &self,
        logical_id: &LogicalId,
        submission: InputSubmission,
    ) -> Result<AcceptedInput, SwarmError> {
        self.record(ServiceCall::SubmitInput(logical_id.as_str().to_owned()));
        self.require_session(logical_id).await?;
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

        let row = self.require_session(logical_id).await?;
        let cancel_request = if submission.is_cancel() {
            self.with_inner(|inner| {
                inner
                    .turns
                    .get(logical_id.as_str())
                    .map(|active| active.request_id.clone())
            })
        } else {
            None
        };
        if submission.is_cancel() && cancel_request.is_none() {
            // 与 actor 的取消门禁一致：没有 live turn 但持久 turn 仍未完成时，
            // `session/cancel` 没有可达目标，不能只写一张“已受理”的收据。
            if super::persisted_turn_resume_required(self.store.as_ref(), &row).await? {
                return Err(SwarmError::upstream_unavailable("session_resume_required"));
            }
        }
        if submission.mode == InputMode::Turn
            && submission.kind == InputKind::User
            && row.state == SessionState::Running
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

        let mut turn_id = None;
        if submission.kind == InputKind::User {
            // Recorded at acceptance, once per inputId (a replayed id takes the
            // `Existing` branch above). The fake never projects host history, so
            // this is the only user frame in the log.
            let (_, item_id) = self
                .record_user_input(logical_id, Some(&input_id), &submission.effective_text())
                .await?;
            turn_id = Some(turn_id_for_item(&item_id));
        }

        if let Some(request_id) = cancel_request {
            self.cancel_turn(logical_id, &request_id).await?;
        }
        if submission.kind == InputKind::Control || row.state == SessionState::Running {
            let last_item_seq = self.last_seq(logical_id).await?;
            return Ok(AcceptedInput::settled(InputOutcome::new(
                input_id,
                Disposition::Inserted,
                submission.mode,
                last_item_seq,
            )));
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
        // The deterministic fake completes the turn inside acceptance; its
        // HTTP response still reports `202` (there is no external model wait).
        // `Deferred` returns the work instead, so a test can observe that the
        // response does not wait for the terminal.
        if self.config.turn_policy == TurnPolicy::Deferred {
            let last_item_seq = self.last_seq(logical_id).await?;
            return Ok(AcceptedInput {
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
            });
        }
        let seq = self
            .run_turn(logical_id, &submission.effective_text(), &turn_id)
            .await?;
        Ok(AcceptedInput::settled(InputOutcome::new(
            input_id,
            Disposition::Inserted,
            submission.mode,
            seq,
        )))
    }

    async fn run_pending_turn(&self, pending: PendingTurn) -> Result<(), SwarmError> {
        self.run_turn(&pending.logical_id, &pending.text, &pending.turn_id)
            .await
            .map(|_| ())
    }

    async fn list_items(
        &self,
        logical_id: &LogicalId,
        window: ItemsWindow,
    ) -> Result<ItemPage, SwarmError> {
        self.record(ServiceCall::ListItems(logical_id.as_str().to_owned()));
        self.require_session(logical_id).await?;
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
        self.record(ServiceCall::Subscribe(logical_id.as_str().to_owned()));
        self.require_session(logical_id).await?;
        let hub = self.hub_for(logical_id.as_str());
        let live = hub.sender.subscribe();
        let backlog = self.replay_after(logical_id, start).await?;
        hub.tail
            .fetch_max(self.last_seq(logical_id).await?, Ordering::Relaxed);
        Ok(EventSubscription::new(backlog, live, hub.tail))
    }

    async fn open_stream(&self, logical_id: &LogicalId) -> EventSubscription {
        self.record(ServiceCall::OpenStream(logical_id.as_str().to_owned()));
        let hub = self.hub_for(logical_id.as_str());
        // Best effort: a new session has no items; an existing one may have
        // history from before this stream was opened (`stream:true` create).
        if let Ok(last_seq) = self.store.swarm_last_seq(logical_id.as_str()).await {
            hub.tail.fetch_max(last_seq, Ordering::Relaxed);
        }
        let live = hub.sender.subscribe();
        EventSubscription::new(Vec::new(), live, hub.tail)
    }

    async fn respond_required_action(
        &self,
        _logical_id: &LogicalId,
        request_id: &str,
        _decision: crate::domain::ApprovalDecision,
        _input_id: Option<String>,
    ) -> Result<super::ApprovalReceipt, SwarmError> {
        // The fake never emits `requires_action`; an id can only be a stale or
        // fabricated one.
        Err(SwarmError::unknown_required_action(request_id))
    }
}

#[cfg(test)]
#[path = "in_memory_test.rs"]
mod tests;
