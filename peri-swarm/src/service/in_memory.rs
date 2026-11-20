use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::{Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::{broadcast, Mutex as AsyncMutex};
use uuid::Uuid;

use super::{CreateSessionCommand, CreateSessionResult, EventSubscription, SwarmService};
use crate::config::SwarmConfig;
use crate::domain::{
    new_root_session_id, ActorKey, Cursor, Disposition, EnvironmentKind, EnvironmentRef, ErrorCode,
    ErrorKind, InputKind, InputMode, InputOutcome, InputState, InputSubmission, ItemKind,
    ItemOrder, ItemPage, ItemSource, ItemView, ItemsWindow, LogicalId, SessionState, SessionView,
    StreamEvent, SwarmError, TurnEvent, TurnView,
};
use crate::store::{
    store_failure, InputPut, StoreError, SwarmInputRow, SwarmItemRow, SwarmSessionRow,
    SwarmSessionUpdate, SwarmStore,
};

/// Deterministic fake turn policy. `Manual` leaves the turn `running` so tests
/// can exercise `turn_in_progress` and steer paths without a real model.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TurnPolicy {
    Immediate,
    Manual,
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

#[derive(Debug)]
struct ActiveTurn {
    request_id: String,
    started_at: String,
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
        let turn = self
            .with_inner(|inner| {
                inner.turns.get(&row.logical_id).map(|active| TurnView {
                    request_id: Some(active.request_id.clone()),
                    started_at: Some(active.started_at.clone()),
                })
            })
            .unwrap_or_default();

        let sandbox = match row.environment.kind {
            EnvironmentKind::None => None,
            EnvironmentKind::OpenAiHosted => None,
        };

        Ok(SessionView {
            id: row.logical_id.clone(),
            object: "agent.session".to_owned(),
            logical_id: row.logical_id.clone(),
            root_session_id: row.root_session_id.clone(),
            cwd: row.cwd.clone(),
            state: row.state,
            turn,
            last_item_seq,
            created_at: row.created_at.clone(),
            last_activity_at: row.last_activity_at.clone(),
            agent: crate::domain::AgentView {
                model: row.model.clone(),
                instructions: row.instructions.clone(),
            },
            environment: row.environment.clone(),
            sandbox,
        })
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
        self.broadcast(
            logical_id.as_str(),
            StreamEvent::Item(ItemView {
                id: item_id.clone(),
                seq,
                item_id,
                kind,
                source,
                payload,
                created_at,
                truncated,
            }),
        );
        Ok(seq)
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
    ) -> Result<u64, SwarmError> {
        let mut payload = json!({ "role": "user", "text": text });
        if let Some(input_id) = input_id {
            payload["inputId"] = json!(input_id);
        }
        self.append_item_with_id(
            logical_id,
            ItemKind::Message,
            ItemSource::root(),
            payload,
            input_id.map(|input_id| format!("itim_input_{input_id}")),
        )
        .await
    }

    async fn fail_turn(&self, logical_id: &LogicalId, request_id: &str, error: &SwarmError) {
        self.with_inner(|inner| {
            inner.turns.remove(logical_id.as_str());
        });
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
            StreamEvent::Turn(TurnEvent::failed(request_id, -32603, error.message())),
        );
    }

    async fn run_turn(&self, logical_id: &LogicalId, text: &str) -> Result<u64, SwarmError> {
        let request_id = Uuid::now_v7().to_string();
        let started_at = peri_time::now_utc_rfc3339();
        self.with_inner(|inner| {
            inner.turns.insert(
                logical_id.as_str().to_owned(),
                ActiveTurn {
                    request_id: request_id.clone(),
                    started_at: started_at.clone(),
                },
            );
        });
        self.set_session_state(logical_id, SessionState::Running, None)
            .await?;
        self.broadcast(
            logical_id.as_str(),
            StreamEvent::Turn(TurnEvent::started(request_id.clone())),
        );

        let payload = json!({
            "messageId": format!("msg_{}", Uuid::now_v7()),
            "text": format!("fake-echo: {text}"),
        });
        let seq = match self
            .append_item(logical_id, ItemKind::Message, ItemSource::root(), payload)
            .await
        {
            Ok(seq) => seq,
            Err(error) => {
                self.fail_turn(logical_id, &request_id, &error).await;
                return Err(error);
            }
        };

        if self.config.turn_policy == TurnPolicy::Immediate {
            self.with_inner(|inner| {
                inner.turns.remove(logical_id.as_str());
            });
            self.set_session_state(logical_id, SessionState::Idle, Some(seq))
                .await?;
            self.broadcast(
                logical_id.as_str(),
                StreamEvent::Turn(TurnEvent::completed(request_id, "end_turn")),
            );
        }
        Ok(seq)
    }

    async fn replay_after(
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
            if let Some(text) = command.initial_input.as_deref() {
                if row.state == SessionState::Running {
                    return Err(SwarmError::turn_in_progress());
                }
                self.record_user_input(&command.logical_id, None, text)
                    .await?;
                self.run_turn(&command.logical_id, text).await?;
            }
            let session = self.view_by_id(&command.logical_id).await?;
            return Ok(CreateSessionResult {
                session,
                created: false,
            });
        }

        let now = peri_time::now_utc_rfc3339();
        // The fake records the requested environment but never contacts Bun;
        // hosted allocation and workspace injection are actor behavior.
        let environment = match command.environment {
            EnvironmentKind::None => EnvironmentRef::none(&command.logical_id),
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

        if let Some(text) = command.initial_input.as_deref() {
            self.record_user_input(&command.logical_id, None, text)
                .await?;
            self.run_turn(&command.logical_id, text).await?;
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
                return Ok(InputOutcome::new(
                    input_id,
                    Disposition::Existing,
                    submission.mode,
                    last_item_seq,
                ));
            }
            return Err(SwarmError::input_conflict(&input_id));
        }

        let row = self.require_session(logical_id).await?;
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

        if submission.kind == InputKind::User {
            // Recorded at acceptance, once per inputId (a replayed id takes the
            // `Existing` branch above). The fake never projects host history, so
            // this is the only user frame in the log.
            self.record_user_input(logical_id, Some(&input_id), &submission.effective_text())
                .await?;
        }

        if submission.kind == InputKind::Control || row.state == SessionState::Running {
            let last_item_seq = self.last_seq(logical_id).await?;
            return Ok(InputOutcome::new(
                input_id,
                Disposition::Inserted,
                submission.mode,
                last_item_seq,
            ));
        }

        let seq = self
            .run_turn(logical_id, &submission.effective_text())
            .await?;
        Ok(InputOutcome::new(
            input_id,
            Disposition::Inserted,
            submission.mode,
            seq,
        ))
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
        self.record(ServiceCall::Subscribe(logical_id.as_str().to_owned()));
        self.require_session(logical_id).await?;
        let hub = self.hub_for(logical_id.as_str());
        let live = hub.sender.subscribe();
        let backlog = self.replay_after(logical_id, cursor).await?;
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
}

#[cfg(test)]
#[path = "in_memory_test.rs"]
mod tests;
