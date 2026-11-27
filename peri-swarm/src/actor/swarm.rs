//! Rivet actor hosting one Swarm session (S5).
//!
//! Actor identity (`design §3.3`): name [`ACTOR_NAME`], key `["agent", logicalId]`.
//! The key is the only identity this actor trusts. Actor input is a deployment
//! hint that may be absent when the engine restarts the actor, so every
//! decision falls back to the key plus `swarm_sessions`.
//!
//! The key stays anchored on `logicalId` rather than on the root session id:
//! `open_stream` runs before `create_session` (no root session id exists yet)
//! and must reach the same actor, `swarm_sessions.actor_key` already stores this
//! shape (S3), and a retried create cannot mint a second actor.
//!
//! Actions are the typed control plane used by
//! [`crate::gateway::RivetSwarmService`]; `GET /events` is the streaming plane:
//! it replays `swarm_items` after the cursor and then forwards the session's
//! broadcast channel as SSE frames ([`crate::event_stream`]).
//!
//! On restart the actor serves history from the session and item stores. A
//! fresh keyed user input attaches the existing root and reconciles any lost
//! turn before accepting the new one.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use rivetkit::prelude::{anyhow, Result};
use rivetkit::{
    Actor, ActorConfig, ActorHttpResponse, ActorKeySegment, Ctx, Handler, HttpCallbackClass,
    Registry, Request, Response, ResponseChunk, StreamingResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::deployment::current_deployment;
use super::service::AcpSwarmService;
use crate::domain::{
    ActorKey, ContentBlock, Cursor, EnvironmentKind, ErrorCode, ErrorKind, EventCursor, InputKind,
    InputMode, InputOutcome, InputSubmission, ItemOrder, ItemPage, ItemView, ItemsWindow,
    LogicalId, McpToolSpec, OfficialTurn, SessionDeleted, SessionState, SessionView, StreamEvent,
    SwarmError, TurnPage, TurnsWindow,
};
use crate::event_stream::FrameWriter;
use crate::service::{
    ApprovalReceipt, CreateSessionCommand, CreateSessionResult, EventSubscription,
    SessionUpdateCommand, SwarmService,
};
use crate::store::SwarmStore;

/// Registered actor name. It is part of the routing contract with the gateway.
pub const ACTOR_NAME: &str = ActorKey::ACTOR_NAME;

/// Explicit idle timeout. The engine default is not a contract (`design §6.3`).
pub const SLEEP_TIMEOUT: Duration = Duration::from_secs(300);

/// Maximum concurrent SSE viewers per actor. Each viewer is one HTTP callback.
const MAX_LIVE_STREAMS: usize = 32;
/// Bound of the actor→engine SSE chunk queue.
const STREAM_CHUNK_CAPACITY: usize = 64;
/// Upper bound of one `subscribe` page.
const SUBSCRIBE_PAGE_LIMIT: u32 = ItemsWindow::MAX_LIMIT;

/// Optional deployment hint carried by `get_or_create`.
///
/// Every field is optional: the engine may restart an actor without input, and
/// `create` must still work from the key plus the store (`design §3.3`).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SwarmActorInput {
    pub logical_id: Option<String>,
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub instructions: Option<String>,
}

/// Actor factory configuration: stateless actor (the runtime lives in memory),
/// explicit 300 s sleep timeout.
#[must_use]
pub fn actor_config() -> ActorConfig {
    ActorConfig {
        has_state: false,
        sleep_timeout: SLEEP_TIMEOUT,
        ..ActorConfig::default()
    }
}

/// Registry pre-populated with the Swarm actor.
#[must_use]
pub fn registry() -> Registry {
    let mut registry = Registry::new();
    registry.register_actor_with::<SwarmActor>(ACTOR_NAME, actor_config());
    registry
}

/// Read-only facts `on_start` recovered from the store.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoverySnapshot {
    pub root_session_id: String,
    pub state: SessionState,
    pub last_item_seq: u64,
    pub recovered_at: String,
}

/// One Swarm session hosted by Rivet.
pub struct SwarmActor {
    logical_id: LogicalId,
    service: Arc<AcpSwarmService>,
    store: Arc<dyn SwarmStore>,
    recovery: Mutex<Option<RecoverySnapshot>>,
}

impl SwarmActor {
    #[must_use]
    pub fn logical_id(&self) -> &LogicalId {
        &self.logical_id
    }

    /// Identity from the actor key. `ActorKeySegment::Number` cannot be a
    /// `logicalId`, so it is rejected instead of coerced to text.
    fn logical_id_from_key(ctx: &Ctx<Self>) -> Result<LogicalId> {
        let key = ctx.key();
        let [ActorKeySegment::String(namespace), ActorKeySegment::String(logical_id)] =
            key.as_slice()
        else {
            return Err(anyhow!(
                "swarm actor key must be [\"agent\", <logicalId>], got {key:?}"
            ));
        };
        if namespace != "agent" {
            return Err(anyhow!(
                "swarm actor namespace segment must be \"agent\", got {namespace:?}"
            ));
        }
        LogicalId::parse(logical_id).map_err(|error| anyhow!(error.to_string()))
    }

    /// Reject a call whose `logicalId` does not match the actor key. A routed
    /// mismatch must fail visibly instead of leaking another session's data.
    fn check_logical_id(&self, logical_id: &str) -> std::result::Result<LogicalId, SwarmError> {
        let parsed = LogicalId::parse(logical_id)?;
        if parsed != self.logical_id {
            return Err(SwarmError::new(
                ErrorKind::BadRequest,
                ErrorCode::InvalidRequest,
                "logicalId does not match the actor key",
            ));
        }
        Ok(parsed)
    }

    fn info(&self) -> InfoOutput {
        InfoOutput {
            logical_id: self.logical_id.as_str().to_owned(),
            actor_key: actor_key_segments(&self.logical_id),
            live: self.service.is_live(&self.logical_id),
            recovery: self.recovery.lock().clone(),
        }
    }

    async fn events_response(&self, request: &Request) -> Result<ActorHttpResponse> {
        let start = match query_cursor(request.uri().query(), "afterSeq") {
            Ok(Some(after)) => EventCursor::after(after),
            Ok(None) => EventCursor::Tail,
            Err(message) => {
                return buffered_json(
                    400,
                    &json!({ "error": { "code": "invalid_query", "message": message } }),
                );
            }
        };
        // Receiver first, then backlog: no item can slip between replay and
        // live delivery (`design §6.1`).
        let live = self.service.open_stream(&self.logical_id).await;
        // The production path must behave exactly like `AcpSwarmService::subscribe`:
        // expiry settlement precedes the replay window, and a reconnect cursor
        // that points after the opening approval row still receives the current
        // `required_actions` snapshot.
        let backlog = match self
            .service
            .subscribe_backlog(&self.logical_id, start)
            .await
        {
            Ok(backlog) => backlog,
            Err(error) => {
                return buffered_json(
                    503,
                    &json!({
                        "error": {
                            "code": error.code().as_str(),
                            "message": error.message(),
                        }
                    }),
                );
            }
        };
        let (sender, receiver) = mpsc::channel(STREAM_CHUNK_CAPACITY);
        tokio::spawn(forward_events(backlog, live, start, sender));
        let headers = HashMap::from([
            ("content-type".to_owned(), "text/event-stream".to_owned()),
            ("cache-control".to_owned(), "no-cache".to_owned()),
            ("x-accel-buffering".to_owned(), "no".to_owned()),
        ]);
        Ok(ActorHttpResponse::Stream(StreamingResponse::from_parts(
            200, headers, receiver,
        )?))
    }
}

#[async_trait]
impl Actor for SwarmActor {
    type Input = SwarmActorInput;
    type Actions = (
        Info,
        Create,
        Get,
        Submit,
        ListItems,
        Subscribe,
        Update,
        Delete,
        ListTurns,
        GetTurn,
    );
    type Events = ();
    type ConnParams = ();
    type Action = Value;

    /// The SSE endpoint must stay open while actions keep running, so HTTP
    /// callbacks are dispatched off the actor event loop (`rivetkit` default is
    /// serialized).
    const CONCURRENT_HTTP_CALLBACKS: bool = true;
    const MAX_CONCURRENT_HTTP_CALLBACKS: usize = 64;
    const MAX_CONCURRENT_LIVE_HTTP_CALLBACK_STARTS: usize = MAX_LIVE_STREAMS;

    fn classify_http_request(request: &Request) -> HttpCallbackClass {
        if request.uri().path() == "/events" {
            HttpCallbackClass::Live
        } else {
            HttpCallbackClass::Standard
        }
    }

    async fn create(ctx: &Ctx<Self>) -> Result<Self> {
        let logical_id = Self::logical_id_from_key(ctx)?;
        let deployment = current_deployment()
            .ok_or_else(|| anyhow!("swarm actor deployment is not installed in this process"))?;
        // Deployment hints are optional and never identity: when present they
        // must agree with the key, otherwise routing is wrong.
        let input: SwarmActorInput = ctx.input().unwrap_or_default();
        if let Some(input_logical_id) = input.logical_id.as_deref() {
            if input_logical_id != logical_id.as_str() {
                return Err(anyhow!(
                    "actor input logicalId {input_logical_id:?} does not match the actor key"
                ));
            }
        }
        let store = deployment.store();
        let service = Arc::new(AcpSwarmService::new_with_session_resources(
            Arc::clone(&store),
            deployment.service_config(),
            deployment.session_resources(),
        ));
        Ok(Self {
            logical_id,
            service,
            store,
            recovery: Mutex::new(None),
        })
    }

    /// Restore the read-only view from `swarm_sessions` and `MAX(seq)`.
    ///
    /// The first user interaction owns host attachment. A failed store read
    /// leaves the actor available for a later request to retry.
    async fn on_start(self: Arc<Self>, _ctx: Ctx<Self>) -> Result<()> {
        let logical_id = self.logical_id.as_str();
        match self.store.swarm_get_session(logical_id).await {
            Ok(Some(row)) => {
                // A hard crash leaves the opening approval rows `pending`, but
                // this generation owns no pending registry: settle them before
                // the recovery view is captured so replay/list/retrieve never
                // advertise an unanswerable id and the tail includes the new
                // settlement rows.
                match crate::service::settle_orphan_approvals(self.store.as_ref(), logical_id).await
                {
                    Ok(0) => {}
                    Ok(settled) => tracing::warn!(
                        logical_id,
                        settled,
                        "swarm settled approvals orphaned by the previous actor"
                    ),
                    Err(error) => tracing::warn!(
                        logical_id,
                        error = %error,
                        "swarm could not settle orphaned approvals"
                    ),
                }
                let last_item_seq = match self.store.swarm_last_seq(logical_id).await {
                    Ok(seq) => seq,
                    Err(error) => {
                        tracing::warn!(
                            logical_id,
                            error = %error,
                            "swarm recovery could not read MAX(seq); using the session row"
                        );
                        row.last_item_seq
                    }
                };
                tracing::info!(
                    logical_id,
                    root_session_id = %row.root_session_id,
                    last_item_seq,
                    state = ?row.state,
                    "swarm actor restored a read-only session view"
                );
                *self.recovery.lock() = Some(RecoverySnapshot {
                    root_session_id: row.root_session_id,
                    state: row.state,
                    last_item_seq,
                    recovered_at: peri_time::now_utc_rfc3339(),
                });
            }
            Ok(None) => {
                tracing::debug!(logical_id, "swarm actor started before its session row");
                *self.recovery.lock() = None;
            }
            Err(error) => {
                tracing::warn!(
                    logical_id,
                    error = %error,
                    "swarm recovery could not read the session row"
                );
                *self.recovery.lock() = None;
            }
        }
        Ok(())
    }

    async fn on_fetch_response(
        self: Arc<Self>,
        _ctx: Ctx<Self>,
        request: Request,
    ) -> Result<ActorHttpResponse> {
        let path = request.uri().path().to_owned();
        match (request.method().as_str(), path.as_str()) {
            ("GET", "/health") => Ok(buffered_json(200, &serde_json::to_value(self.info())?)?),
            ("GET", "/events") => self.events_response(&request).await,
            _ => Ok(ActorHttpResponse::Buffered(Response::from_parts(
                404,
                HashMap::new(),
                Vec::new(),
            )?)),
        }
    }

    /// Release live ACP hosts when the engine sleeps the actor. Subscriptions
    /// die with the actor; turns are not cancelled mid-flight because sleep only
    /// happens when nothing is keeping the actor awake.
    async fn on_sleep(self: Arc<Self>, _ctx: Ctx<Self>) -> Result<()> {
        let reports = self.service.shutdown_all().await;
        tracing::info!(
            logical_id = %self.logical_id,
            hosts = reports.len(),
            "swarm actor sleeping; live hosts shut down"
        );
        Ok(())
    }

    async fn on_destroy(self: Arc<Self>, _ctx: Ctx<Self>) -> Result<()> {
        self.service.shutdown_all().await;
        Ok(())
    }
}

/// Append SSE frames from the replay backlog and then the live channel.
///
/// The writer dedupes items against the replay cursor; a tail subscription has
/// no cursor, so every live item passes.
async fn forward_events(
    backlog: Vec<StreamEvent>,
    live: EventSubscription,
    start: EventCursor,
    sender: mpsc::Sender<ResponseChunk>,
) {
    let mut writer = FrameWriter::new(start.cursor().map_or(0, Cursor::value));
    let (_, mut receiver) = live.into_parts();
    for event in backlog {
        if let Some(frame) = writer.render(&event) {
            if !send_frame(&sender, frame).await {
                return;
            }
        }
    }
    loop {
        let event = tokio::select! {
            biased;
            () = sender.closed() => return,
            event = receiver.recv() => event,
        };
        match event {
            Ok(event) => {
                if let Some(frame) = writer.render(&event) {
                    if !send_frame(&sender, frame).await {
                        return;
                    }
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                // Events were dropped, possibly including a turn terminal. Report
                // the loss and end the subscription instead of leaving the client
                // waiting forever: an official SDK sees EOF (and throws), a native
                // client reconnects from the last item `seq` it rendered.
                let _ = send_frame(&sender, writer.gap(skipped)).await;
                break;
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
    let _ = sender
        .send(ResponseChunk::Data {
            data: Vec::new(),
            finish: true,
        })
        .await;
}

/// `false` once the engine dropped the response (gateway or client disconnect).
async fn send_frame(sender: &mpsc::Sender<ResponseChunk>, frame: String) -> bool {
    sender
        .send(ResponseChunk::Data {
            data: frame.into_bytes(),
            finish: false,
        })
        .await
        .is_ok()
}

fn buffered_json(status: u16, body: &Value) -> Result<ActorHttpResponse> {
    let response = Response::from_parts(
        status,
        HashMap::from([("content-type".to_owned(), "application/json".to_owned())]),
        serde_json::to_vec(body)?,
    )?;
    Ok(ActorHttpResponse::Buffered(response))
}

/// Read an optional numeric query parameter.
///
/// `Ok(None)` means the parameter is absent; the caller decides what absence
/// means (an SSE subscription without a cursor starts at the tail).
fn query_cursor(query: Option<&str>, name: &str) -> std::result::Result<Option<u64>, String> {
    let Some(query) = query else {
        return Ok(None);
    };
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key == name {
            return value
                .parse::<u64>()
                .map(Some)
                .map_err(|_| format!("{name} must be a non-negative integer"));
        }
    }
    Ok(None)
}

/// `["agent", logicalId]` — the routing key rendered for diagnostics.
fn actor_key_segments(logical_id: &LogicalId) -> Vec<String> {
    ActorKey::new(logical_id.clone())
        .segments()
        .iter()
        .map(|segment| (*segment).to_owned())
        .collect()
}

// ── action wire format ──────────────────────────────────────────────────────

/// Domain failure carried inside a successful action envelope.
///
/// Rivet action errors are opaque strings, so the HTTP error contract travels
/// as data: the gateway re-creates the same [`SwarmError`] with its status class
/// and machine-readable code intact.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionFailure {
    pub kind: ErrorKind,
    pub code: ErrorCode,
    pub message: String,
    pub details: Value,
}

impl ActionFailure {
    #[must_use]
    pub fn from_swarm(error: &SwarmError) -> Self {
        Self {
            kind: error.kind(),
            code: error.code(),
            message: error.message().to_owned(),
            details: error.details().clone(),
        }
    }

    #[must_use]
    pub fn into_swarm(self) -> SwarmError {
        SwarmError::new(self.kind, self.code, self.message).with_details(self.details)
    }
}

/// `{"ok": ...}` or `{"error": ...}`; exactly one side is set.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionEnvelope<T> {
    pub ok: Option<T>,
    pub error: Option<ActionFailure>,
}

impl<T> ActionEnvelope<T> {
    #[must_use]
    pub fn success(value: T) -> Self {
        Self {
            ok: Some(value),
            error: None,
        }
    }

    #[must_use]
    pub fn failure(error: &SwarmError) -> Self {
        Self {
            ok: None,
            error: Some(ActionFailure::from_swarm(error)),
        }
    }

    pub fn into_result(self) -> std::result::Result<T, SwarmError> {
        match (self.ok, self.error) {
            (Some(value), None) => Ok(value),
            (None, Some(error)) => Err(error.into_swarm()),
            _ => Err(SwarmError::upstream_unavailable("swarm_actor_envelope")),
        }
    }
}

fn envelope<T>(result: std::result::Result<T, SwarmError>) -> ActionEnvelope<T> {
    match result {
        Ok(value) => ActionEnvelope::success(value),
        Err(error) => ActionEnvelope::failure(&error),
    }
}

/// Diagnostic action output; also the actor's minimal proof of life.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InfoOutput {
    pub logical_id: String,
    pub actor_key: Vec<String>,
    /// `true` when this actor instance owns a live ACP host.
    pub live: bool,
    /// Read-only view restored by `on_start`; `null` before the first row write.
    pub recovery: Option<RecoverySnapshot>,
}

/// Reports the actor identity derived from the key.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Info {}

impl rivetkit::Action for Info {
    type Output = InfoOutput;

    const NAME: &'static str = "info";
}

impl Handler<Info> for SwarmActor {
    async fn handle(self: Arc<Self>, _ctx: Ctx<Self>, _action: Info) -> Result<InfoOutput> {
        Ok(self.info())
    }
}

/// Create (or reopen) the session this actor hosts.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Create {
    pub logical_id: String,
    pub cwd: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub instructions: Option<String>,
    /// Caller-declared MCP tools, already normalized by the HTTP layer.
    #[serde(default)]
    pub mcp_tools: Vec<McpToolSpec>,
    /// Tri-state so an attach can distinguish omission from an explicit empty
    /// value (see [`CreateSessionCommand::metadata_conflicts_with`]). Omitted
    /// keeps the stored value across the action hop, so `Keep` must not be
    /// serialized as `null`.
    #[serde(default, skip_serializing_if = "crate::service::UpdateField::is_keep")]
    pub metadata: crate::service::UpdateField<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "crate::service::UpdateField::is_keep")]
    pub prompt_cache_options: crate::service::UpdateField<crate::domain::PromptCacheOptions>,
    #[serde(default)]
    pub initial_input: Option<String>,
    pub environment: EnvironmentKind,
    /// Caller-supplied hosted environment id (S8a3a); `None` mints one.
    #[serde(default)]
    pub environment_id: Option<String>,
    /// Reserved by the gateway before routing; `None` keeps S4 behavior.
    #[serde(default)]
    pub root_session_id: Option<String>,
    /// Internal (`stream:true`): record the initial input but answer the action
    /// before the first turn runs. The actor then schedules the turn with
    /// `ctx.wait_until`, so the gateway can return the SSE response first.
    #[serde(default)]
    pub defer_initial_turn: bool,
}

impl rivetkit::Action for Create {
    type Output = ActionEnvelope<CreateSessionResult>;

    const NAME: &'static str = "create";
}

impl Handler<Create> for SwarmActor {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        action: Create,
    ) -> Result<ActionEnvelope<CreateSessionResult>> {
        let logical_id = match self.check_logical_id(&action.logical_id) {
            Ok(logical_id) => logical_id,
            Err(error) => return Ok(envelope(Err(error))),
        };
        let command = CreateSessionCommand {
            logical_id,
            cwd: PathBuf::from(action.cwd),
            model: action.model,
            instructions: action.instructions,
            mcp_tools: action.mcp_tools,
            metadata: action.metadata,
            prompt_cache_options: action.prompt_cache_options,
            initial_input: action.initial_input,
            environment: action.environment,
            environment_id: action.environment_id,
            root_session_id: action.root_session_id,
            defer_initial_turn: action.defer_initial_turn,
        };
        // A create may run an initial turn: hold the actor awake for the
        // durable half (`design §6.3`). A deferred turn is registered below.
        let result = ctx.keep_awake(self.service.create_session(command)).await;
        match result {
            Ok(mut result) => {
                // A streamed create answered the action before its first turn;
                // run that turn out-of-band, like `Submit`, so the gateway can
                // construct the SSE response without waiting for it. The reply
                // may still be followed immediately by the turn: `wait_until`
                // starts it as soon as the action reply is sent (`design §4.3`).
                if let Some(pending) = result.pending_initial.take() {
                    let service = Arc::clone(&self.service);
                    ctx.wait_until(async move {
                        if let Err(error) = service.run_pending_turn(pending).await {
                            tracing::warn!(
                                error = %error,
                                "deferred swarm first turn failed to run"
                            );
                        }
                    });
                }
                Ok(envelope(Ok(result)))
            }
            Err(error) => Ok(envelope(Err(error))),
        }
    }
}

/// Current session view.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Get {
    pub logical_id: String,
}

impl rivetkit::Action for Get {
    type Output = ActionEnvelope<SessionView>;

    const NAME: &'static str = "get";
}

impl Handler<Get> for SwarmActor {
    async fn handle(
        self: Arc<Self>,
        _ctx: Ctx<Self>,
        action: Get,
    ) -> Result<ActionEnvelope<SessionView>> {
        let logical_id = match self.check_logical_id(&action.logical_id) {
            Ok(logical_id) => logical_id,
            Err(error) => return Ok(envelope(Err(error))),
        };
        Ok(envelope(self.service.get_session(&logical_id).await))
    }
}

/// Submit one input (`POST /events`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Submit {
    pub logical_id: String,
    pub input_id: String,
    pub kind: InputKind,
    pub mode: InputMode,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub attachments: Vec<ContentBlock>,
    #[serde(default)]
    pub plugin_selections: Vec<String>,
    #[serde(default)]
    pub original_draft: Option<String>,
}

impl rivetkit::Action for Submit {
    type Output = ActionEnvelope<InputOutcome>;

    const NAME: &'static str = "submit";
}

impl Handler<Submit> for SwarmActor {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        action: Submit,
    ) -> Result<ActionEnvelope<InputOutcome>> {
        let logical_id = match self.check_logical_id(&action.logical_id) {
            Ok(logical_id) => logical_id,
            Err(error) => return Ok(envelope(Err(error))),
        };
        let submission = InputSubmission {
            input_id: action.input_id,
            kind: action.kind,
            mode: action.mode,
            text: action.text,
            attachments: action.attachments,
            plugin_selections: action.plugin_selections,
            original_draft: action.original_draft,
        };
        // Acceptance is the durable half: receipt, user item, and turn
        // reservation. The turn itself runs out-of-band so the action can
        // answer `202` without waiting for the terminal.
        let accepted = ctx
            .keep_awake(self.service.accept_input(&logical_id, submission))
            .await;
        match accepted {
            Ok(accepted) => {
                if let Some(pending) = accepted.pending_turn {
                    let service = Arc::clone(&self.service);
                    ctx.wait_until(async move {
                        if let Err(error) = service.run_pending_turn(pending).await {
                            tracing::warn!(
                                error = %error,
                                "accepted swarm turn failed to run"
                            );
                        }
                    });
                }
                Ok(envelope(Ok(accepted.outcome)))
            }
            Err(error) => Ok(envelope(Err(error))),
        }
    }
}

/// Answer one pending MCP approval (`agent.session.input.mcp_approval_request_result`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RespondRequiredAction {
    pub logical_id: String,
    pub request_id: String,
    pub decision: crate::domain::ApprovalDecision,
    #[serde(default)]
    pub input_id: Option<String>,
}

impl rivetkit::Action for RespondRequiredAction {
    type Output = ActionEnvelope<ApprovalReceipt>;

    const NAME: &'static str = "respond_required_action";
}

impl Handler<RespondRequiredAction> for SwarmActor {
    async fn handle(
        self: Arc<Self>,
        _ctx: Ctx<Self>,
        action: RespondRequiredAction,
    ) -> Result<ActionEnvelope<ApprovalReceipt>> {
        let logical_id = match self.check_logical_id(&action.logical_id) {
            Ok(logical_id) => logical_id,
            Err(error) => return Ok(envelope(Err(error))),
        };
        match self
            .service
            .respond_required_action(
                &logical_id,
                &action.request_id,
                action.decision,
                action.input_id,
            )
            .await
        {
            Ok(receipt) => Ok(envelope(Ok(receipt))),
            Err(error) => Ok(envelope(Err(error))),
        }
    }
}

/// Persisted item page.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListItems {
    pub logical_id: String,
    pub order: ItemOrder,
    #[serde(default)]
    pub after: Option<u64>,
    /// Official `CursorPage` cursor (`after=<itemId>`); resolved to a seq by
    /// the service across the actor boundary.
    #[serde(default)]
    pub after_id: Option<String>,
    pub limit: u32,
}

impl rivetkit::Action for ListItems {
    type Output = ActionEnvelope<ItemPage>;

    const NAME: &'static str = "list_items";
}

impl Handler<ListItems> for SwarmActor {
    async fn handle(
        self: Arc<Self>,
        _ctx: Ctx<Self>,
        action: ListItems,
    ) -> Result<ActionEnvelope<ItemPage>> {
        let logical_id = match self.check_logical_id(&action.logical_id) {
            Ok(logical_id) => logical_id,
            Err(error) => return Ok(envelope(Err(error))),
        };
        let window = ItemsWindow {
            order: action.order,
            after: action.after,
            after_id: action.after_id,
            limit: action.limit,
        };
        Ok(envelope(self.service.list_items(&logical_id, window).await))
    }
}

/// Incremental item read used for gap recovery and the non-streaming fallback.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Subscribe {
    pub logical_id: String,
    #[serde(default)]
    pub after_seq: u64,
    #[serde(default = "default_subscribe_limit")]
    pub limit: u32,
}

fn default_subscribe_limit() -> u32 {
    SUBSCRIBE_PAGE_LIMIT
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeOutcome {
    pub items: Vec<ItemView>,
    pub last_seq: u64,
    pub has_more: bool,
}

impl rivetkit::Action for Subscribe {
    type Output = ActionEnvelope<SubscribeOutcome>;

    const NAME: &'static str = "subscribe";
}

impl Handler<Subscribe> for SwarmActor {
    async fn handle(
        self: Arc<Self>,
        _ctx: Ctx<Self>,
        action: Subscribe,
    ) -> Result<ActionEnvelope<SubscribeOutcome>> {
        let logical_id = match self.check_logical_id(&action.logical_id) {
            Ok(logical_id) => logical_id,
            Err(error) => return Ok(envelope(Err(error))),
        };
        let limit = action.limit.clamp(1, SUBSCRIBE_PAGE_LIMIT);
        let window = ItemsWindow {
            order: ItemOrder::Asc,
            after: Some(action.after_seq),
            after_id: None,
            limit,
        };
        let page = match self.service.list_items(&logical_id, window).await {
            Ok(page) => page,
            Err(error) => return Ok(envelope(Err(error))),
        };
        Ok(envelope(Ok(SubscribeOutcome {
            items: page.items,
            last_seq: page.last_item_seq,
            has_more: page.has_more,
        })))
    }
}

/// `POST /sessions/{id}` (`sessions.update`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Update {
    pub logical_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_tools: Option<Vec<McpToolSpec>>,
    /// Omitted keeps the stored map; `null` or `{}` clears it.
    #[serde(default, skip_serializing_if = "crate::service::UpdateField::is_keep")]
    pub metadata: crate::service::UpdateField<BTreeMap<String, String>>,
    /// Omitted keeps the stored value; `null` clears it. The field must be
    /// omitted, not sent as `null`, when the patch keeps the value.
    #[serde(default, skip_serializing_if = "crate::service::UpdateField::is_keep")]
    pub prompt_cache_options: crate::service::UpdateField<crate::domain::PromptCacheOptions>,
}

impl rivetkit::Action for Update {
    type Output = ActionEnvelope<SessionView>;

    const NAME: &'static str = "update";
}

impl Handler<Update> for SwarmActor {
    async fn handle(
        self: Arc<Self>,
        _ctx: Ctx<Self>,
        action: Update,
    ) -> Result<ActionEnvelope<SessionView>> {
        let logical_id = match self.check_logical_id(&action.logical_id) {
            Ok(logical_id) => logical_id,
            Err(error) => return Ok(envelope(Err(error))),
        };
        let command = SessionUpdateCommand {
            model: action.model,
            mcp_tools: action.mcp_tools,
            metadata: action.metadata,
            prompt_cache_options: action.prompt_cache_options,
        };
        Ok(envelope(
            self.service.update_session(&logical_id, command).await,
        ))
    }
}

/// `DELETE /sessions/{id}` (`sessions.delete`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Delete {
    pub logical_id: String,
}

impl rivetkit::Action for Delete {
    type Output = ActionEnvelope<SessionDeleted>;

    const NAME: &'static str = "delete";
}

impl Handler<Delete> for SwarmActor {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        action: Delete,
    ) -> Result<ActionEnvelope<SessionDeleted>> {
        let logical_id = match self.check_logical_id(&action.logical_id) {
            Ok(logical_id) => logical_id,
            Err(error) => return Ok(envelope(Err(error))),
        };
        let result = self.service.delete_session(&logical_id).await;
        match result {
            Ok(()) => {
                // The rows are gone and an in-process host may still hold an
                // idle ACP child or an old broadcast hub. Destroying the actor
                // makes the next create a fresh generation with no stale
                // memory; the action reply is sent before the stop is applied.
                let stop = ctx.clone();
                ctx.wait_until(async move {
                    if let Err(error) = stop.destroy() {
                        tracing::warn!(error = %error, "swarm actor could not be destroyed after delete");
                    }
                });
                Ok(envelope(Ok(SessionDeleted::new(logical_id.as_str()))))
            }
            Err(error) => Ok(envelope(Err(error))),
        }
    }
}

/// `GET /sessions/{id}/turns`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListTurns {
    pub logical_id: String,
    pub order: ItemOrder,
    #[serde(default)]
    pub after: Option<String>,
    pub limit: u32,
}

impl rivetkit::Action for ListTurns {
    type Output = ActionEnvelope<TurnPage>;

    const NAME: &'static str = "list_turns";
}

impl Handler<ListTurns> for SwarmActor {
    async fn handle(
        self: Arc<Self>,
        _ctx: Ctx<Self>,
        action: ListTurns,
    ) -> Result<ActionEnvelope<TurnPage>> {
        let logical_id = match self.check_logical_id(&action.logical_id) {
            Ok(logical_id) => logical_id,
            Err(error) => return Ok(envelope(Err(error))),
        };
        let window = TurnsWindow {
            order: action.order,
            after: action.after,
            limit: action.limit,
        };
        Ok(envelope(self.service.list_turns(&logical_id, window).await))
    }
}

/// `GET /sessions/{id}/turns/{turnId}`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetTurn {
    pub logical_id: String,
    pub turn_id: String,
}

impl rivetkit::Action for GetTurn {
    type Output = ActionEnvelope<OfficialTurn>;

    const NAME: &'static str = "get_turn";
}

impl Handler<GetTurn> for SwarmActor {
    async fn handle(
        self: Arc<Self>,
        _ctx: Ctx<Self>,
        action: GetTurn,
    ) -> Result<ActionEnvelope<OfficialTurn>> {
        let logical_id = match self.check_logical_id(&action.logical_id) {
            Ok(logical_id) => logical_id,
            Err(error) => return Ok(envelope(Err(error))),
        };
        Ok(envelope(
            self.service.get_turn(&logical_id, &action.turn_id).await,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    #[tokio::test]
    async fn idle_sse_drops_live_subscription_when_response_closes() {
        let (events, live) = tokio::sync::broadcast::channel(1);
        let live = EventSubscription::new(Vec::new(), live, Arc::new(AtomicU64::new(0)));
        let (output, viewer) = mpsc::channel(1);
        let task = forward_events(Vec::new(), live, EventCursor::Tail, output);
        tokio::pin!(task);
        assert!(futures::poll!(&mut task).is_pending());
        drop(viewer);
        assert!(futures::poll!(&mut task).is_ready());
        assert_eq!(events.receiver_count(), 0);
    }
}
