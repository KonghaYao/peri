//! Service port consumed by the HTTP layer.
//!
//! S3 ships [`InMemorySwarmService`] as a deterministic fake. S4 replaces it
//! with an ACP-backed actor service that implements the same trait; the HTTP
//! layer never branches on which implementation is present.

mod in_memory;

pub use in_memory::{InMemorySwarmService, ServiceCall, ServiceConfig, TurnPolicy};

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::broadcast;

use crate::domain::{
    EnvironmentKind, EventCursor, InputOutcome, InputSubmission, ItemPage, ItemsWindow, LogicalId,
    McpToolSpec, OfficialEvent, OfficialMessageItem, OfficialSession, OfficialTurn,
    PromptCacheOptions, SessionPage, SessionState, SessionView, SessionsWindow, StreamEvent,
    SwarmError, TurnPage, TurnStatus, TurnsWindow,
};
use crate::store::{store_failure, SwarmSessionRow, SwarmStore};

/// Fully validated create command. The HTTP layer owns request parsing and cwd
/// normalization; the service owns identity and turn semantics.
#[derive(Clone, Debug)]
pub struct CreateSessionCommand {
    pub logical_id: LogicalId,
    pub cwd: PathBuf,
    pub model: Option<String>,
    pub instructions: Option<String>,
    /// Caller-declared MCP tools, already normalized by the HTTP layer. S8a
    /// persists and compares them; ACP injection is S8a2 (`design §5.1`).
    pub mcp_tools: Vec<McpToolSpec>,
    /// Caller metadata, validated by the HTTP layer.
    ///
    /// Tri-state so an attach can tell an omitted field (keep the stored
    /// value) from an explicit `{}`/`null` (assert "no metadata"). Only a
    /// provided value that differs from the stored one is an identity
    /// conflict.
    pub metadata: UpdateField<BTreeMap<String, String>>,
    /// Session-default prompt cache lifetime, validated by the HTTP layer.
    /// Same omission/clear/value distinction as [`Self::metadata`].
    pub prompt_cache_options: UpdateField<PromptCacheOptions>,
    pub initial_input: Option<String>,
    pub environment: EnvironmentKind,
    /// Caller-supplied hosted environment id (`environment.environment_id`).
    /// Only meaningful with [`EnvironmentKind::OpenAiHosted`]; an omitted id
    /// mints `env_<uuid>` at the actor.
    pub environment_id: Option<String>,
    /// Root ACP session id reserved by the caller (S5 gateway). A trusted host
    /// generates it before routing to the actor so a retried create addresses
    /// the same identity; `None` keeps the S4 behavior (service generates one).
    pub root_session_id: Option<String>,
    /// Internal `stream:true` switch: record the initial input and return its
    /// turn as [`CreateSessionResult::pending_initial`] instead of executing it
    /// before the response. Every other create runs the first turn inside
    /// `create_session`, so its errors surface as the HTTP status.
    ///
    /// The service still owns acceptance: the user item is persisted here, and
    /// the deferred [`PendingTurn`] reuses its `turn_id`, so running it later
    /// cannot duplicate the input.
    pub defer_initial_turn: bool,
}

impl CreateSessionCommand {
    /// Metadata to persist for a new row: omission, explicit `null` and `{}`
    /// all mean "no metadata" on create.
    #[must_use]
    pub fn metadata_for_new_row(&self) -> BTreeMap<String, String> {
        match &self.metadata {
            UpdateField::Set(metadata) => metadata.clone(),
            UpdateField::Keep | UpdateField::Clear => BTreeMap::new(),
        }
    }

    /// Prompt cache options to persist for a new row (same rule as
    /// [`Self::metadata_for_new_row`]).
    #[must_use]
    pub fn prompt_cache_options_for_new_row(&self) -> Option<PromptCacheOptions> {
        match &self.prompt_cache_options {
            UpdateField::Set(options) => Some(*options),
            UpdateField::Keep | UpdateField::Clear => None,
        }
    }

    /// Whether an attach request's metadata disagrees with the stored value.
    ///
    /// Omission keeps the stored value (`false`). An explicit value, including
    /// `null`/`{}`, must equal it; only a difference is an identity conflict.
    #[must_use]
    pub fn metadata_conflicts_with(&self, stored: &BTreeMap<String, String>) -> bool {
        match &self.metadata {
            UpdateField::Keep => false,
            UpdateField::Clear => !stored.is_empty(),
            UpdateField::Set(metadata) => metadata != stored,
        }
    }

    /// Whether an attach request's prompt cache options disagree with the
    /// stored value; `null` means "unset".
    #[must_use]
    pub fn prompt_cache_options_conflict_with(&self, stored: &Option<PromptCacheOptions>) -> bool {
        match &self.prompt_cache_options {
            UpdateField::Keep => false,
            UpdateField::Clear => stored.is_some(),
            UpdateField::Set(options) => stored.as_ref() != Some(options),
        }
    }
}

/// Tri-state write used by `sessions.update` across the process boundary.
///
/// Serde maps an omitted field (through `#[serde(default)]`) to [`Keep`] and an
/// explicit `null` to [`Clear`]. A plain `Option<Option<T>>` cannot express
/// this: `null` deserializes as the outer `None`, i.e. "keep".
///
/// [`Keep`]: UpdateField::Keep
/// [`Clear`]: UpdateField::Clear
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum UpdateField<T> {
    #[default]
    Keep,
    Clear,
    Set(T),
}

impl<T> UpdateField<T> {
    #[must_use]
    pub fn is_keep(&self) -> bool {
        matches!(self, Self::Keep)
    }
}

impl<T: Serialize> Serialize for UpdateField<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Keep | Self::Clear => serializer.serialize_none(),
            Self::Set(value) => value.serialize(serializer),
        }
    }
}

impl<'de, T: serde::de::DeserializeOwned> Deserialize<'de> for UpdateField<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match serde_json::Value::deserialize(deserializer)? {
            serde_json::Value::Null => Ok(Self::Clear),
            value => serde_json::from_value(value)
                .map(Self::Set)
                .map_err(serde::de::Error::custom),
        }
    }
}

/// Fully validated `sessions.update` command (official `SessionUpdateParams`).
///
/// `None` keeps the stored value for `model`/`mcp_tools`; the tri-state fields
/// distinguish omission, explicit `null` (clear) and a replacement.
#[derive(Clone, Debug, Default)]
pub struct SessionUpdateCommand {
    pub model: Option<String>,
    pub mcp_tools: Option<Vec<McpToolSpec>>,
    pub metadata: UpdateField<BTreeMap<String, String>>,
    pub prompt_cache_options: UpdateField<PromptCacheOptions>,
}

impl SessionUpdateCommand {
    /// Whether the update changes the ACP host configuration (`model` or
    /// `mcpServers`). Such a change requires a host restart to take effect.
    #[must_use]
    pub fn changes_host(&self) -> bool {
        self.model.is_some() || self.mcp_tools.is_some()
    }

    /// Store-level prompt cache write.
    #[must_use]
    pub fn prompt_cache_patch(&self) -> Option<Option<PromptCacheOptions>> {
        match self.prompt_cache_options {
            UpdateField::Keep => None,
            UpdateField::Clear => Some(None),
            UpdateField::Set(options) => Some(Some(options)),
        }
    }

    /// Store-level metadata write; `Clear` becomes an empty replacement.
    #[must_use]
    pub fn metadata_patch(&self) -> Option<BTreeMap<String, String>> {
        match &self.metadata {
            UpdateField::Keep => None,
            UpdateField::Clear => Some(BTreeMap::new()),
            UpdateField::Set(metadata) => Some(metadata.clone()),
        }
    }
}

/// Create response payload shared by the in-memory fake and the actor.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSessionResult {
    pub session: SessionView,
    pub created: bool,
    /// First turn deferred by [`CreateSessionCommand::defer_initial_turn`].
    ///
    /// Never serialized: the actor runs it locally after answering the action,
    /// and the gateway envelope must not carry execution state across the
    /// process boundary. An in-process caller (the HTTP handler on the fake)
    /// consumes it to run the turn after returning the SSE response.
    #[serde(skip)]
    pub pending_initial: Option<PendingTurn>,
}

/// A live SSE subscription: persisted backlog first, then broadcast events.
///
/// Implementations must create the broadcast receiver **before** reading the
/// backlog so no event can fall between replay and live delivery.
///
/// `tail` is the session's current `MAX(seq)`, shared with the writer that
/// appends items. The SSE heartbeat reports it, so a client can distinguish
/// "no new items" from "the session is still progressing". It is progress
/// information only: the resume cursor stays the `seq` of a delivered item.
#[derive(Debug)]
pub struct EventSubscription {
    backlog: Vec<StreamEvent>,
    live: broadcast::Receiver<StreamEvent>,
    tail: Arc<AtomicU64>,
}

impl EventSubscription {
    #[must_use]
    pub fn new(
        backlog: Vec<StreamEvent>,
        live: broadcast::Receiver<StreamEvent>,
        tail: Arc<AtomicU64>,
    ) -> Self {
        Self {
            backlog,
            live,
            tail,
        }
    }

    /// Shared handle to the session tail; readers observe appends live.
    #[must_use]
    pub fn tail(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.tail)
    }

    #[must_use]
    pub fn into_parts(self) -> (Vec<StreamEvent>, broadcast::Receiver<StreamEvent>) {
        (self.backlog, self.live)
    }

    #[must_use]
    pub fn backlog(&self) -> &[StreamEvent] {
        &self.backlog
    }
}

/// A turn that became due during acceptance.
///
/// `turn_id` is derived from the persisted user item (see
/// [`crate::domain::turn_id_for_item`]); it must be reused by replay, so it is
/// carried here instead of being minted when the execution starts.
#[derive(Clone, Debug)]
pub struct PendingTurn {
    pub logical_id: LogicalId,
    pub text: String,
    pub turn_id: String,
}

/// Result of the durable half of `POST /events`.
///
/// `outcome` is safe to return with `202`; `pending_turn` is the work the caller
/// may run after responding. The HTTP layer spawns it, so the response no longer
/// waits for the turn terminal.
#[derive(Clone, Debug)]
pub struct AcceptedInput {
    pub outcome: InputOutcome,
    pub pending_turn: Option<PendingTurn>,
}

impl AcceptedInput {
    #[must_use]
    pub fn settled(outcome: InputOutcome) -> Self {
        Self {
            outcome,
            pending_turn: None,
        }
    }
}

/// Resolve the official `after=<item id>` cursor to a seq.
///
/// A numeric cursor passes through. An id must exist in this session; when it
/// is combined with `afterSeq`, both must name the same position
/// (`400 conflicting_cursor`).
pub async fn resolve_window_after(
    store: &dyn crate::store::SwarmStore,
    logical_id: &LogicalId,
    window: &ItemsWindow,
) -> Result<Option<u64>, SwarmError> {
    let Some(item_id) = window.after_id.as_deref() else {
        return Ok(window.after);
    };
    let row = store
        .swarm_get_item(logical_id.as_str(), item_id)
        .await
        .map_err(|error| crate::store::store_failure("get_item", error))?
        .ok_or_else(|| {
            SwarmError::invalid_query("after must reference an existing item id")
                .with_details(json!({ "field": "after", "received": item_id }))
        })?;
    match window.after {
        Some(after) if after != row.seq => Err(SwarmError::conflicting_cursor()),
        _ => Ok(Some(row.seq)),
    }
}

/// Rebuild official frames from the persisted item log.
///
/// Live delivery and backlog replay share this mapping, so a reconnect sees the
/// same `turn_id`/`event_id` sequence. `swarm_items` has no turn-terminal row,
/// so a replay reconstructs the turn structure from the rows it can see:
///
/// - a user message opens a turn when no turn is open, or when the open turn
///   already produced its final assistant frame;
/// - a user message that arrives before the open turn produced output is a
///   steer and attaches to that turn;
/// - every `complete:true` assistant message attaches to the open turn (one
///   turn can persist several messages, e.g. across tool calls);
/// - a turn's terminal is emitted immediately before the next turn's
///   `turn.created`, or at the end of the window. The last turn is `failed` only
///   while the session row still records `failed`; every other closed turn is
///   `completed` followed immediately by `agent.session.idle`.
///
/// A turn that is still live is left open: its terminal arrives on the live
/// stream instead of being invented by the replay. `replay_backlog` seeds the
/// turn that was open at the cursor from history the client already consumed.
/// The remaining gaps are listed in
/// `docs/verification/peri-swarm-official-sdk-2026-11-21.md`.
struct OfficialReplay {
    logical_id: String,
    session_created_at: i64,
    session_last_active_at: i64,
    model: Option<String>,
    instructions: Option<String>,
    session_failed: bool,
    /// The session has a turn open in the live runtime right now.
    live_open_turn: bool,
}

struct ReplayTurn {
    turn_id: String,
    created_at: i64,
    /// Unix seconds of the last persisted final assistant frame.
    completed_at: Option<i64>,
}

/// Turn identity the replay window starts inside, recovered from history at or
/// before the reconnect cursor.
struct TurnSeed {
    turn_id: String,
    created_at: i64,
    /// Last assistant final frame of this turn before the cursor, if any. The
    /// window's own rows refine it.
    completed_at: Option<i64>,
}

/// A persisted turn whose opener is at or before the reconnect cursor.
struct PreviousTurn {
    turn_id: String,
    created_at: i64,
    /// Last assistant final frame of this turn at or before the cursor.
    completed_at: Option<i64>,
}

impl PreviousTurn {
    /// No assistant final frame after the opener: the turn had not produced
    /// output when the cursor was taken, so a user item right after it is a
    /// steer of this turn rather than a new turn.
    fn open(&self) -> bool {
        self.completed_at.is_none()
    }
}

/// Official frames one persisted row implies.
enum RowFrame {
    /// First row of a turn inside the replay window: `turn.created` + user item.
    OpensTurn { turn: usize, text: String },
    /// User message that joins an already-open turn (a steer).
    SteerUser { turn: usize, text: String },
    /// Final assistant message of the open turn.
    Assistant { turn: usize, text: String },
    /// Delta or non-message row: the custom `item` frame only.
    Nothing,
}

/// Classify one persisted row and advance the open-turn state.
fn classify_row(
    item: &crate::domain::ItemView,
    turns: &mut Vec<ReplayTurn>,
    current: &mut Option<usize>,
) -> RowFrame {
    if item.kind != crate::domain::ItemKind::Message {
        return RowFrame::Nothing;
    }
    let text = item
        .payload
        .get("text")
        .and_then(|text| text.as_str())
        .unwrap_or_default();
    match item.payload.get("role").and_then(|role| role.as_str()) {
        Some("user") => {
            // A user item after the open turn produced its final assistant
            // frame starts the next turn: live accepts a new turn only while
            // no turn is open. Before that point it is a steer of the open turn.
            let produces_output = current.is_some_and(|turn| turns[turn].completed_at.is_some());
            if current.is_none() || produces_output {
                turns.push(ReplayTurn {
                    turn_id: crate::domain::turn_id_for_item(&item.item_id),
                    created_at: crate::domain::official::unix_from_rfc3339(&item.created_at),
                    completed_at: None,
                });
                *current = Some(turns.len() - 1);
                RowFrame::OpensTurn {
                    turn: turns.len() - 1,
                    text: text.to_owned(),
                }
            } else {
                RowFrame::SteerUser {
                    turn: (*current).expect("open turn"),
                    text: text.to_owned(),
                }
            }
        }
        Some("assistant") if item.payload.get("complete") == Some(&json!(true)) => {
            let Some(turn) = *current else {
                // An assistant frame whose opener is not in this window and
                // could not be seeded: no turn identity to attach it to.
                return RowFrame::Nothing;
            };
            turns[turn].completed_at =
                Some(crate::domain::official::unix_from_rfc3339(&item.created_at));
            RowFrame::Assistant {
                turn,
                text: text.to_owned(),
            }
        }
        _ => RowFrame::Nothing,
    }
}

impl OfficialReplay {
    fn new(row: &crate::store::SwarmSessionRow, live_open_turn: bool) -> Self {
        Self {
            logical_id: row.logical_id.clone(),
            session_created_at: crate::domain::official::unix_from_rfc3339(&row.created_at),
            session_last_active_at: crate::domain::official::unix_from_rfc3339(
                &row.last_activity_at,
            ),
            model: row.model.clone(),
            instructions: row.instructions.clone(),
            session_failed: row.state == crate::domain::SessionState::Failed,
            live_open_turn,
        }
    }

    /// Interleave the custom `item` frames with the official frames they imply.
    ///
    /// `seed` names the turn that was already open when the window starts (the
    /// reconnect cursor points inside it, or the window is empty and the last
    /// turn's terminal was lost). Its `turn.created` is not repeated: the client
    /// received it before the cursor, and re-sending it would make a fresh
    /// `TurnState` follow the old turn and end on its terminal.
    fn render(
        &self,
        items: &[crate::domain::ItemView],
        seed: Option<&TurnSeed>,
    ) -> Vec<StreamEvent> {
        let mut turns: Vec<ReplayTurn> = Vec::new();
        let mut current: Option<usize> = None;
        if let Some(seed) = seed {
            turns.push(ReplayTurn {
                turn_id: seed.turn_id.clone(),
                created_at: seed.created_at,
                completed_at: seed.completed_at,
            });
            current = Some(0);
        }
        let mut frames = Vec::with_capacity(items.len());
        for item in items {
            frames.push(classify_row(item, &mut turns, &mut current));
        }

        let mut out = Vec::with_capacity(items.len().saturating_mul(2));
        let mut open: Option<usize> = if seed.is_some() { Some(0) } else { None };
        for (item, frame) in items.iter().zip(frames.iter()) {
            if let RowFrame::OpensTurn { .. } = frame {
                if let Some(previous) = open {
                    self.push_terminal(&turns[previous], previous + 1 == turns.len(), &mut out);
                }
            }
            out.push(StreamEvent::Item(item.clone()));
            if let RowFrame::OpensTurn { turn, text } = frame {
                open = Some(*turn);
                out.push(StreamEvent::Official(OfficialEvent::turn_created(
                    &self.logical_id,
                    &turns[*turn].turn_id,
                    OfficialTurn::new(
                        &self.logical_id,
                        &turns[*turn].turn_id,
                        TurnStatus::InProgress,
                        turns[*turn].created_at,
                        None,
                    ),
                )));
                self.push_item_added(item, &turns[*turn].turn_id, text, false, &mut out);
                continue;
            }
            match frame {
                RowFrame::SteerUser { turn, text } => {
                    self.push_item_added(item, &turns[*turn].turn_id, text, false, &mut out);
                }
                RowFrame::Assistant { turn, text } => {
                    self.push_item_added(item, &turns[*turn].turn_id, text, true, &mut out);
                }
                RowFrame::OpensTurn { .. } | RowFrame::Nothing => {}
            }
        }
        if let Some(last) = open {
            self.push_terminal(&turns[last], last + 1 == turns.len(), &mut out);
        }
        out
    }

    /// Emit one turn terminal plus the session frame the SDK reads after it.
    fn push_terminal(&self, turn: &ReplayTurn, is_last: bool, out: &mut Vec<StreamEvent>) {
        if is_last && self.live_open_turn {
            // The turn is still running; its terminal comes from the live
            // stream. Replaying one here would end the SDK stream early.
            return;
        }
        if is_last && self.session_failed {
            out.push(StreamEvent::Official(OfficialEvent::turn_failed(
                &self.logical_id,
                &turn.turn_id,
                OfficialTurn::new(
                    &self.logical_id,
                    &turn.turn_id,
                    TurnStatus::Failed,
                    turn.created_at,
                    // The failure time is not persisted; keep it null instead
                    // of pretending the last item's timestamp is one.
                    None,
                ),
            )));
            out.push(StreamEvent::Official(OfficialEvent::session_failed(
                &turn.turn_id,
                OfficialSession::new(
                    &self.logical_id,
                    crate::domain::SessionStatus::Failed,
                    self.session_created_at,
                    self.session_last_active_at,
                    // The persisted log carries no failure detail; keep it null
                    // instead of inventing a message.
                    None,
                    self.model.clone(),
                    self.instructions.clone(),
                ),
            )));
            return;
        }
        out.push(StreamEvent::Official(OfficialEvent::turn_completed(
            &self.logical_id,
            &turn.turn_id,
            OfficialTurn::new(
                &self.logical_id,
                &turn.turn_id,
                TurnStatus::Completed,
                turn.created_at,
                turn.completed_at,
            ),
        )));
        out.push(StreamEvent::Official(OfficialEvent::session_idle(
            &turn.turn_id,
            OfficialSession::new(
                &self.logical_id,
                crate::domain::SessionStatus::Idle,
                self.session_created_at,
                self.session_last_active_at,
                None,
                self.model.clone(),
                self.instructions.clone(),
            ),
        )));
    }

    fn push_item_added(
        &self,
        item: &crate::domain::ItemView,
        turn_id: &str,
        text: &str,
        assistant: bool,
        out: &mut Vec<StreamEvent>,
    ) {
        let message = if assistant {
            OfficialMessageItem::assistant(&item.item_id, turn_id, text)
        } else {
            OfficialMessageItem::user(&item.item_id, turn_id, text)
        };
        out.push(StreamEvent::Official(OfficialEvent::turn_item_added(
            item.seq,
            &self.logical_id,
            turn_id,
            message,
        )));
    }
}

/// Rebuild the persisted backlog: custom `item` frames plus the official frames
/// every row implies.
///
/// Both replay call sites (actor and in-memory fake) use this, so a reconnect
/// observes the same identities as the live stream regardless of which service
/// served it. `live_open_turn` suppresses a fabricated terminal for a turn that
/// is still running.
///
/// [`EventCursor::Tail`] (no cursor supplied) has **no** backlog: it is the
/// "subscribe before the input" path, and replaying an older turn's terminal
/// would end the official SDK's stream on that turn.
pub async fn replay_backlog(
    store: &dyn crate::store::SwarmStore,
    session: &crate::store::SwarmSessionRow,
    start: EventCursor,
    live_open_turn: bool,
) -> Result<Vec<StreamEvent>, SwarmError> {
    let Some(cursor) = start.cursor() else {
        return Ok(Vec::new());
    };
    let mut items = Vec::new();
    let mut after = cursor.value();
    loop {
        let page = store
            .swarm_items_after(&session.logical_id, after, ItemsWindow::MAX_LIMIT)
            .await
            .map_err(|error| crate::store::store_failure("items_after", error))?;
        let page_len = page.len();
        if let Some(last) = page.last() {
            after = last.seq;
        }
        items.extend(page.into_iter().map(|row| row.to_view()));
        if page_len < ItemsWindow::MAX_LIMIT as usize {
            break;
        }
    }
    // A window that starts inside a turn has no opener row, and a window whose
    // first row is a steer user item would otherwise mint a second turn id for
    // the same turn. Look back past the cursor for the opener of the turn that
    // was open there; an empty window still uses it to republish a terminal the
    // client may have missed.
    let previous = if cursor.value() == 0 {
        None
    } else {
        turn_at_cursor(store, &session.logical_id, cursor.value()).await?
    };
    let seed = seed_for(&items, previous.as_ref());
    let replay = OfficialReplay::new(session, live_open_turn);
    Ok(replay.render(&items, seed.as_ref()))
}

/// Decide whether the window continues a turn from before the cursor.
///
/// A window whose first turn-relevant row is a user item starts a new turn only
/// when the previous turn already produced output under the same boundary rule
/// [`classify_row`] applies (`!previous.open()`); otherwise that user item is a
/// steer and must keep the opener's `turn_id`. Any other first row (assistant
/// frame, tool frame) means the window starts inside the previous turn. An
/// empty window keeps the seed so the last turn's terminal can be republished.
fn seed_for(
    items: &[crate::domain::ItemView],
    previous: Option<&PreviousTurn>,
) -> Option<TurnSeed> {
    let previous = previous?;
    if first_turn_row_role(items) == Some("user") && !previous.open() {
        return None;
    }
    Some(TurnSeed {
        turn_id: previous.turn_id.clone(),
        created_at: previous.created_at,
        completed_at: previous.completed_at,
    })
}

/// Role of the first row that participates in turn classification.
fn first_turn_row_role(items: &[crate::domain::ItemView]) -> Option<&'static str> {
    for item in items {
        if item.kind != crate::domain::ItemKind::Message {
            continue;
        }
        match item.payload.get("role").and_then(|role| role.as_str()) {
            Some("user") => return Some("user"),
            Some("assistant") if item.payload.get("complete") == Some(&json!(true)) => {
                return Some("assistant");
            }
            _ => {}
        }
    }
    None
}

/// Walk persisted rows backwards from `up_to` to the opener of the turn that
/// was open there, together with its last assistant final frame.
///
/// The walk mirrors [`classify_row`]: a user item is a steer when no assistant
/// final frame separates it from the next older user item. Scanning stops as
/// soon as the opener is confirmed, so a normal session reads only the tail of
/// the previous turn; a session whose turns never produced output reads back to
/// its first user item.
async fn turn_at_cursor(
    store: &dyn crate::store::SwarmStore,
    logical_id: &str,
    up_to: u64,
) -> Result<Option<PreviousTurn>, SwarmError> {
    // The reconnect cursor is the last delivered seq, so its row is included.
    let mut before = Some(up_to.saturating_add(1));
    let mut opener: Option<crate::domain::ItemView> = None;
    let mut newest_complete: Option<(u64, i64)> = None;
    let mut complete_since_opener = false;
    while let Some(bound) = before {
        let page = store
            .swarm_items_before(logical_id, Some(bound), ItemsWindow::MAX_LIMIT)
            .await
            .map_err(|error| crate::store::store_failure("items_before", error))?;
        let Some(oldest_seq) = page.last().map(|row| row.seq) else {
            break;
        };
        for row in page {
            let view = row.to_view();
            if view.kind != crate::domain::ItemKind::Message {
                continue;
            }
            match view.payload.get("role").and_then(|role| role.as_str()) {
                Some("user") => {
                    if opener.is_some() && complete_since_opener {
                        return Ok(Some(previous_turn(opener, newest_complete)));
                    }
                    opener = Some(view);
                    complete_since_opener = false;
                }
                Some("assistant") if view.payload.get("complete") == Some(&json!(true)) => {
                    if newest_complete.is_none() {
                        newest_complete = Some((
                            view.seq,
                            crate::domain::official::unix_from_rfc3339(&view.created_at),
                        ));
                    }
                    if opener.is_some() {
                        complete_since_opener = true;
                    }
                }
                _ => {}
            }
        }
        // The page is descending, so the oldest row is the next exclusive bound.
        before = (oldest_seq < bound).then_some(oldest_seq);
    }
    Ok(opener.map(|item| previous_turn(Some(item), newest_complete)))
}

/// Build the cursor turn; a completion older than its opener belongs to an
/// earlier turn.
fn previous_turn(
    opener: Option<crate::domain::ItemView>,
    newest_complete: Option<(u64, i64)>,
) -> PreviousTurn {
    let opener = opener.expect("a turn only exists once a user item opened it");
    PreviousTurn {
        turn_id: crate::domain::turn_id_for_item(&opener.item_id),
        created_at: crate::domain::official::unix_from_rfc3339(&opener.created_at),
        completed_at: newest_complete
            .filter(|(seq, _)| *seq > opener.seq)
            .map(|(_, at)| at),
    }
}

/// Project one persisted row into the public session shape.
///
/// `turn` is the live in-process turn slot: its presence is what makes
/// `status=in_progress` even before the row state catches up.
#[must_use]
pub(crate) fn project_session(
    row: &SwarmSessionRow,
    turn: Option<crate::domain::TurnView>,
    sandbox: Option<crate::domain::SandboxView>,
) -> SessionView {
    let status = crate::domain::SessionStatus::project(row.state, turn.is_some());
    SessionView {
        id: row.logical_id.clone(),
        object: "agent.session".to_owned(),
        logical_id: row.logical_id.clone(),
        root_session_id: row.root_session_id.clone(),
        cwd: row.cwd.clone(),
        state: row.state,
        status,
        turn: turn.unwrap_or_default(),
        last_item_seq: row.last_item_seq,
        created_at: row.created_at.clone(),
        last_activity_at: row.last_activity_at.clone(),
        agent: crate::domain::AgentView {
            model: row.model.clone(),
            instructions: row.instructions.clone(),
        },
        environment: row.environment.clone(),
        sandbox,
        metadata: row.metadata.clone(),
        prompt_cache_options: row.prompt_cache_options,
    }
}

/// Project the booked Bun environment without contacting Bun (`design §4.5`).
pub(crate) async fn sandbox_view(
    store: &dyn SwarmStore,
    row: &SwarmSessionRow,
) -> Result<Option<crate::domain::SandboxView>, SwarmError> {
    if row.environment.kind != EnvironmentKind::OpenAiHosted {
        return Ok(None);
    }
    let Some(env_id) = row.environment.id.as_deref() else {
        return Ok(None);
    };
    let binding = store
        .swarm_get_binding(env_id)
        .await
        .map_err(|error| store_failure("get_binding", error))?;
    Ok(binding.map(|binding| crate::domain::SandboxView {
        logical_env_id: binding.logical_env_id,
        sandbox_id: Some(binding.sandbox_id),
        run_id: binding.last_run_id,
        state: None,
        workspace_path: Some(binding.workspace_path),
    }))
}

/// One `sessions.list` page from the store (shared by both services).
pub(crate) async fn list_sessions_from_store(
    store: &dyn SwarmStore,
    window: &SessionsWindow,
) -> Result<SessionPage, SwarmError> {
    if window.limit == 0 || window.limit > SessionsWindow::MAX_LIMIT {
        return Err(SwarmError::invalid_query(format!(
            "limit must be within 1..={}",
            SessionsWindow::MAX_LIMIT
        )));
    }
    let mut rows = store
        .swarm_list_sessions(window)
        .await
        .map_err(|error| store_failure("list_sessions", error))?;
    let probe = rows.len();
    rows.truncate(window.limit as usize);
    let has_more = probe > rows.len();
    let next_cursor = has_more
        .then(|| rows.last().map(|row| row.logical_id.clone()))
        .flatten();
    let mut data = Vec::with_capacity(rows.len());
    for row in &rows {
        let sandbox = sandbox_view(store, row).await?;
        data.push(project_session(row, None, sandbox));
    }
    Ok(SessionPage::new(data, has_more, next_cursor))
}

/// One `turns.list` page from the store (shared by both services).
pub(crate) async fn list_turns_from_store(
    store: &dyn SwarmStore,
    logical_id: &str,
    window: &TurnsWindow,
) -> Result<TurnPage, SwarmError> {
    if window.limit == 0 || window.limit > TurnsWindow::MAX_LIMIT {
        return Err(SwarmError::invalid_query(format!(
            "limit must be within 1..={}",
            TurnsWindow::MAX_LIMIT
        )));
    }
    if let Some(after) = window.after.as_deref() {
        let exists = store
            .swarm_get_turn(logical_id, after)
            .await
            .map_err(|error| store_failure("get_turn", error))?
            .is_some();
        if !exists {
            return Err(
                SwarmError::invalid_query("after must name a turn of this session")
                    .with_details(json!({ "field": "after", "received": after })),
            );
        }
    }
    let mut rows = store
        .swarm_turn_page(logical_id, window)
        .await
        .map_err(|error| store_failure("turn_page", error))?;
    let probe = rows.len();
    rows.truncate(window.limit as usize);
    let has_more = probe > rows.len();
    let next_cursor = has_more
        .then(|| rows.last().map(|row| row.turn_id.clone()))
        .flatten();
    let data = rows
        .iter()
        .map(crate::store::SwarmTurnRow::to_official)
        .collect();
    Ok(TurnPage::new(data, has_more, next_cursor))
}

/// One `turns.retrieve` read; a turn of another session is not found.
pub(crate) async fn get_turn_from_store(
    store: &dyn SwarmStore,
    logical_id: &str,
    turn_id: &str,
) -> Result<OfficialTurn, SwarmError> {
    store
        .swarm_get_turn(logical_id, turn_id)
        .await
        .map_err(|error| store_failure("get_turn", error))?
        .map(|row| row.to_official())
        .ok_or_else(|| {
            SwarmError::new(
                crate::domain::ErrorKind::NotFound,
                crate::domain::ErrorCode::UnknownAgent,
                "unknown turn for this session",
            )
            .with_details(json!({ "logicalId": logical_id, "turnId": turn_id }))
        })
}

/// Whether the persisted session still owns an unfinished turn.
///
/// A crash between the opening write and the terminal leaves `state=running`; a
/// crash that reset the session row first can leave an open turn row behind.
/// Either fact means the session must be resumed, not mutated or deleted.
///
/// This is the single admission rule shared by `ensure_live` and the
/// update/delete paths: callers that already hold a live turn answer
/// `409 turn_in_progress` themselves and only reach this probe when the runtime
/// has no turn.
pub(crate) async fn persisted_turn_resume_required(
    store: &dyn SwarmStore,
    row: &SwarmSessionRow,
) -> Result<bool, SwarmError> {
    if row.state == SessionState::Running {
        return Ok(true);
    }
    store
        .swarm_has_open_turn(&row.logical_id)
        .await
        .map_err(|error| store_failure("has_open_turn", error))
}

/// Typed port between HTTP and the session runtime (S4 actor / S3 fake).
#[async_trait]
pub trait SwarmService: Send + Sync {
    /// Create or reuse the session.
    ///
    /// With [`CreateSessionCommand::defer_initial_turn`] the initial user item
    /// is persisted but its turn is returned as
    /// [`CreateSessionResult::pending_initial`] for the caller to run through
    /// [`SwarmService::run_pending_turn`].
    async fn create_session(
        &self,
        command: CreateSessionCommand,
    ) -> Result<CreateSessionResult, SwarmError>;

    async fn get_session(&self, logical_id: &LogicalId) -> Result<SessionView, SwarmError>;

    /// `sessions.list`: one id-keyset page with optional metadata/agent filters.
    async fn list_sessions(&self, window: SessionsWindow) -> Result<SessionPage, SwarmError>;

    /// `sessions.update`: replace the mutable official fields.
    ///
    /// A live turn owns the session (`409 turn_in_progress`). A model or MCP
    /// tools change restarts the ACP host, because both are fixed at
    /// `session/new`; the next interaction reattaches with the new values.
    async fn update_session(
        &self,
        logical_id: &LogicalId,
        command: SessionUpdateCommand,
    ) -> Result<SessionView, SwarmError>;

    /// `sessions.delete`: remove every Swarm row and invalidate live runtime
    /// state so a later create with the same id starts clean.
    async fn delete_session(&self, logical_id: &LogicalId) -> Result<(), SwarmError>;

    /// `sessions.turns.list`: one `(created_at, turn_id)` keyset page.
    async fn list_turns(
        &self,
        logical_id: &LogicalId,
        window: TurnsWindow,
    ) -> Result<TurnPage, SwarmError>;

    /// `sessions.turns.retrieve`; a turn of another session is `404`.
    async fn get_turn(
        &self,
        logical_id: &LogicalId,
        turn_id: &str,
    ) -> Result<OfficialTurn, SwarmError>;

    /// Durably accept one input: idempotency resolution, receipt write, and the
    /// user item. The returned [`AcceptedInput::pending_turn`] is the execution
    /// the caller runs **after** the `202`.
    async fn accept_input(
        &self,
        logical_id: &LogicalId,
        submission: InputSubmission,
    ) -> Result<AcceptedInput, SwarmError>;

    /// Execute a turn previously accepted by [`SwarmService::accept_input`].
    async fn run_pending_turn(&self, pending: PendingTurn) -> Result<(), SwarmError>;

    /// Accept and execute in one call. Tests and internal callers use this;
    /// the HTTP layer uses [`SwarmService::accept_input`] plus a spawned
    /// [`SwarmService::run_pending_turn`] so `202` never waits for a terminal.
    async fn submit_input(
        &self,
        logical_id: &LogicalId,
        submission: InputSubmission,
    ) -> Result<InputOutcome, SwarmError> {
        let accepted = self.accept_input(logical_id, submission).await?;
        if let Some(pending) = accepted.pending_turn {
            self.run_pending_turn(pending).await?;
        }
        Ok(accepted.outcome)
    }

    async fn list_items(
        &self,
        logical_id: &LogicalId,
        window: ItemsWindow,
    ) -> Result<ItemPage, SwarmError>;

    /// Subscribe to an existing session; unknown sessions are `404 unknown_agent`.
    ///
    /// [`EventCursor::Tail`] means the caller supplied no cursor: the
    /// subscription carries live events only and never replays an older turn.
    async fn subscribe(
        &self,
        logical_id: &LogicalId,
        start: EventCursor,
    ) -> Result<EventSubscription, SwarmError>;

    /// Register a stream before `POST /sessions` executes the first turn.
    /// Must not create or touch session rows.
    async fn open_stream(&self, logical_id: &LogicalId) -> EventSubscription;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize, Serialize)]
    struct PatchBody {
        #[serde(default)]
        prompt_cache_options: UpdateField<PromptCacheOptions>,
        #[serde(default)]
        metadata: UpdateField<BTreeMap<String, String>>,
    }

    #[test]
    fn update_field_distinguishes_absent_null_and_value() {
        let absent: PatchBody = serde_json::from_str("{}").expect("absent");
        assert!(absent.prompt_cache_options.is_keep());
        assert!(absent.metadata.is_keep());

        let cleared: PatchBody =
            serde_json::from_str(r#"{"prompt_cache_options": null, "metadata": null}"#)
                .expect("cleared");
        assert_eq!(cleared.prompt_cache_options, UpdateField::Clear);
        assert_eq!(cleared.metadata, UpdateField::Clear);

        let set: PatchBody = serde_json::from_str(
            r#"{"prompt_cache_options": {"ttl": "12h"}, "metadata": {"team": "x"}}"#,
        )
        .expect("set");
        assert_eq!(
            set.prompt_cache_options,
            UpdateField::Set(PromptCacheOptions::new(
                crate::domain::PromptCacheTtl::TwelveHours
            ))
        );
        assert_eq!(
            set.metadata,
            UpdateField::Set([("team".to_owned(), "x".to_owned())].into())
        );
    }

    #[test]
    fn a_kept_update_field_is_omitted_and_a_cleared_one_is_null() {
        #[derive(Serialize)]
        struct Patch<'a> {
            #[serde(skip_serializing_if = "UpdateField::is_keep")]
            prompt_cache_options: &'a UpdateField<PromptCacheOptions>,
        }

        let keep = Patch {
            prompt_cache_options: &UpdateField::Keep,
        };
        assert_eq!(
            serde_json::to_value(keep).expect("serialize"),
            serde_json::json!({})
        );

        let clear = Patch {
            prompt_cache_options: &UpdateField::Clear,
        };
        assert_eq!(
            serde_json::to_value(clear).expect("serialize"),
            serde_json::json!({ "prompt_cache_options": null })
        );

        let set = Patch {
            prompt_cache_options: &UpdateField::Set(PromptCacheOptions::new(
                crate::domain::PromptCacheTtl::ThirtyMinutes,
            )),
        };
        assert_eq!(
            serde_json::to_value(set).expect("serialize"),
            serde_json::json!({ "prompt_cache_options": { "ttl": "30m" } })
        );
    }

    fn create_command(
        metadata: UpdateField<BTreeMap<String, String>>,
        prompt_cache_options: UpdateField<PromptCacheOptions>,
    ) -> CreateSessionCommand {
        CreateSessionCommand {
            logical_id: LogicalId::parse("agent-attach").expect("logical id"),
            cwd: std::path::PathBuf::from("/tmp"),
            model: None,
            instructions: None,
            mcp_tools: Vec::new(),
            metadata,
            prompt_cache_options,
            initial_input: None,
            environment: EnvironmentKind::None,
            environment_id: None,
            root_session_id: None,
            defer_initial_turn: false,
        }
    }

    #[test]
    fn create_attach_only_conflicts_on_an_explicit_different_value() {
        let stored_metadata: BTreeMap<String, String> =
            [("team".to_owned(), "alpha".to_owned())].into();
        let stored_cache_value =
            PromptCacheOptions::new(crate::domain::PromptCacheTtl::TwelveHours);
        let stored_cache = Some(stored_cache_value);

        let omitted = create_command(UpdateField::Keep, UpdateField::Keep);
        assert!(!omitted.metadata_conflicts_with(&stored_metadata));
        assert!(!omitted.prompt_cache_options_conflict_with(&stored_cache));
        assert!(omitted.metadata_for_new_row().is_empty());
        assert_eq!(omitted.prompt_cache_options_for_new_row(), None);

        let same = create_command(
            UpdateField::Set(stored_metadata.clone()),
            UpdateField::Set(stored_cache_value),
        );
        assert!(!same.metadata_conflicts_with(&stored_metadata));
        assert!(!same.prompt_cache_options_conflict_with(&stored_cache));

        let different = create_command(
            UpdateField::Set([("team".to_owned(), "beta".to_owned())].into()),
            UpdateField::Set(PromptCacheOptions::new(
                crate::domain::PromptCacheTtl::ThirtyMinutes,
            )),
        );
        assert!(different.metadata_conflicts_with(&stored_metadata));
        assert!(different.prompt_cache_options_conflict_with(&stored_cache));

        // Explicit `{}`/`null` assert "unset" and conflict with a stored value.
        let cleared = create_command(UpdateField::Clear, UpdateField::Clear);
        assert!(cleared.metadata_conflicts_with(&stored_metadata));
        assert!(cleared.prompt_cache_options_conflict_with(&stored_cache));
        assert!(cleared.metadata_for_new_row().is_empty());
        assert_eq!(cleared.prompt_cache_options_for_new_row(), None);

        // The same explicit empties match a session that has no value.
        assert!(!cleared.metadata_conflicts_with(&BTreeMap::new()));
        assert!(!cleared.prompt_cache_options_conflict_with(&None));
    }

    #[test]
    fn create_action_keeps_omission_across_the_actor_boundary() {
        let keep = create_command(UpdateField::Keep, UpdateField::Keep);
        let action = crate::actor::swarm::Create {
            logical_id: keep.logical_id.as_str().to_owned(),
            cwd: keep.cwd.to_string_lossy().into_owned(),
            model: None,
            instructions: None,
            mcp_tools: Vec::new(),
            metadata: keep.metadata,
            prompt_cache_options: keep.prompt_cache_options,
            initial_input: None,
            environment: EnvironmentKind::None,
            environment_id: None,
            root_session_id: None,
            defer_initial_turn: false,
        };
        let encoded = serde_json::to_value(&action).expect("serialize");
        assert!(
            encoded.get("metadata").is_none() && encoded.get("promptCacheOptions").is_none(),
            "a kept create field must be omitted, not `null`: {encoded}"
        );
        let decoded: crate::actor::swarm::Create =
            serde_json::from_value(encoded).expect("deserialize");
        assert!(decoded.metadata.is_keep());
        assert!(decoded.prompt_cache_options.is_keep());

        let cleared = create_command(UpdateField::Clear, UpdateField::Clear);
        let action = crate::actor::swarm::Create {
            logical_id: cleared.logical_id.as_str().to_owned(),
            cwd: cleared.cwd.to_string_lossy().into_owned(),
            model: None,
            instructions: None,
            mcp_tools: Vec::new(),
            metadata: cleared.metadata,
            prompt_cache_options: cleared.prompt_cache_options,
            initial_input: None,
            environment: EnvironmentKind::None,
            environment_id: None,
            root_session_id: None,
            defer_initial_turn: false,
        };
        let encoded = serde_json::to_value(&action).expect("serialize");
        assert_eq!(encoded["metadata"], serde_json::Value::Null);
        assert_eq!(encoded["promptCacheOptions"], serde_json::Value::Null);
        let decoded: crate::actor::swarm::Create =
            serde_json::from_value(encoded).expect("deserialize");
        assert_eq!(decoded.metadata, UpdateField::Clear);
        assert_eq!(decoded.prompt_cache_options, UpdateField::Clear);
    }
}
