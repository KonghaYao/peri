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
//! Recovery boundary: `on_start` restores a **read-only** view (session row and
//! `MAX(seq)`). Re-attaching a live ACP host needs the remote store (S6) and the
//! full-chain recovery in S9; until then a rebuilt actor answers reads and
//! rejects turns instead of faking a live session.

use std::collections::HashMap;
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
    ActorKey, ContentBlock, Cursor, EnvironmentKind, ErrorCode, ErrorKind, InputKind, InputMode,
    InputOutcome, InputSubmission, ItemOrder, ItemPage, ItemView, ItemsWindow, LogicalId,
    McpToolSpec, SessionState, SessionView, StreamEvent, SwarmError,
};
use crate::event_stream::FrameWriter;
use crate::service::{CreateSessionCommand, CreateSessionResult, EventSubscription, SwarmService};
use crate::store::{store_failure, SwarmStore};

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

    /// S5 recovery boundary: a rebuilt actor restores a read-only view but owns
    /// no ACP host until the first interaction. `ensure_live` attaches the
    /// persisted session with `session/load` (S9a4b); this check then only
    /// covers the remaining host-less cases (no row yet, or an attach that
    /// deliberately did not run) and keeps the error a `502`
    /// `session_not_live` instead of the host-less service's `404
    /// unknown_agent`.
    async fn host_less_error(&self, logical_id: &LogicalId) -> Option<SwarmError> {
        if self.service.is_live(logical_id) {
            return None;
        }
        match self.store.swarm_get_session(logical_id.as_str()).await {
            Ok(Some(_)) => Some(SwarmError::upstream_unavailable("session_not_live")),
            Ok(None) => None,
            Err(error) => Some(store_failure("get_session", error)),
        }
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
        let after = match query_u64(request.uri().query(), "afterSeq") {
            Ok(after) => after,
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
        let backlog = match self
            .service
            .replay_after(&self.logical_id, Cursor::new(after))
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
        tokio::spawn(forward_events(backlog, live, after, sender));
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
    type Actions = (Info, Create, Get, Submit, ListItems, Subscribe);
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
        let service = Arc::new(AcpSwarmService::new(
            Arc::clone(&store),
            deployment.service_config(),
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
    /// No ACP host is spawned: `session/load` into a fresh child needs the
    /// remote store (S6) and is verified end to end in S9. Store failures are
    /// logged instead of failing the start so reads keep surfacing the actual
    /// error instead of a crash loop.
    async fn on_start(self: Arc<Self>, _ctx: Ctx<Self>) -> Result<()> {
        let logical_id = self.logical_id.as_str();
        match self.store.swarm_get_session(logical_id).await {
            Ok(Some(row)) => {
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
async fn forward_events(
    backlog: Vec<StreamEvent>,
    live: EventSubscription,
    after: u64,
    sender: mpsc::Sender<ResponseChunk>,
) {
    let mut writer = FrameWriter::new(after);
    let (_, mut receiver) = live.into_parts();
    for event in backlog {
        if let Some(frame) = writer.render(&event) {
            if !send_frame(&sender, frame).await {
                return;
            }
        }
    }
    loop {
        match receiver.recv().await {
            Ok(event) => {
                if let Some(frame) = writer.render(&event) {
                    if !send_frame(&sender, frame).await {
                        return;
                    }
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                if !send_frame(&sender, writer.gap(skipped)).await {
                    return;
                }
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

fn query_u64(query: Option<&str>, name: &str) -> std::result::Result<u64, String> {
    let Some(query) = query else {
        return Ok(0);
    };
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key == name {
            return value
                .parse::<u64>()
                .map_err(|_| format!("{name} must be a non-negative integer"));
        }
    }
    Ok(0)
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
    #[serde(default)]
    pub initial_input: Option<String>,
    pub environment: EnvironmentKind,
    /// Caller-supplied hosted environment id (S8a3a); `None` mints one.
    #[serde(default)]
    pub environment_id: Option<String>,
    /// Reserved by the gateway before routing; `None` keeps S4 behavior.
    #[serde(default)]
    pub root_session_id: Option<String>,
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
            initial_input: action.initial_input,
            environment: action.environment,
            environment_id: action.environment_id,
            root_session_id: action.root_session_id,
        };
        // A create with an initial input runs a turn: hold the actor awake for
        // its whole duration (`design §6.3`).
        let result = ctx.keep_awake(self.service.create_session(command)).await;
        Ok(envelope(result))
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
        // A rebuilt actor holds only the read-only view. The first turn is the
        // interaction that reattaches the persisted session (`session/load`,
        // no prompt and no queued-input replay).
        if let Err(error) = self.service.ensure_live(&logical_id).await {
            return Ok(envelope(Err(error)));
        }
        if let Some(error) = self.host_less_error(&logical_id).await {
            return Ok(envelope(Err(error)));
        }
        let submission = InputSubmission {
            input_id: action.input_id,
            kind: action.kind,
            mode: action.mode,
            text: action.text,
            attachments: action.attachments,
            plugin_selections: action.plugin_selections,
            original_draft: action.original_draft,
        };
        // An idle `mode=turn` input runs a full turn inside this call; the
        // actor must not sleep in the middle of it.
        let result = ctx
            .keep_awake(self.service.submit_input(&logical_id, submission))
            .await;
        Ok(envelope(result))
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
