//! Service port consumed by the HTTP layer.
//!
//! S3 ships [`InMemorySwarmService`] as a deterministic fake. S4 replaces it
//! with an ACP-backed actor service that implements the same trait; the HTTP
//! layer never branches on which implementation is present.

mod in_memory;

pub use in_memory::{InMemorySwarmService, ServiceCall, ServiceConfig, TurnPolicy};

use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::broadcast;

use crate::domain::{
    Cursor, EnvironmentKind, InputOutcome, InputSubmission, ItemPage, ItemsWindow, LogicalId,
    McpToolSpec, OfficialEvent, OfficialMessageItem, OfficialSession, OfficialTurn, SessionView,
    StreamEvent, SwarmError, TurnStatus,
};

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
}

/// Create response payload shared by the in-memory fake and the actor.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSessionResult {
    pub session: SessionView,
    pub created: bool,
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
/// stream instead of being invented by the replay. The remaining gaps are
/// listed in `docs/verification/peri-swarm-official-sdk-2026-11-21.md`.
pub struct OfficialReplay {
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
    #[must_use]
    pub fn new(row: &crate::store::SwarmSessionRow, live_open_turn: bool) -> Self {
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
    /// reconnect cursor points inside it). Its `turn.created` is not repeated:
    /// the client received it before the cursor, and the derived id would be
    /// the same anyway.
    pub fn render(
        &self,
        items: &[crate::domain::ItemView],
        seed: Option<(&str, i64)>,
    ) -> Vec<StreamEvent> {
        let mut turns: Vec<ReplayTurn> = Vec::new();
        let mut current: Option<usize> = None;
        if let Some((turn_id, created_at)) = seed {
            turns.push(ReplayTurn {
                turn_id: turn_id.to_owned(),
                created_at,
                completed_at: None,
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
pub async fn replay_backlog(
    store: &dyn crate::store::SwarmStore,
    session: &crate::store::SwarmSessionRow,
    cursor: Cursor,
    live_open_turn: bool,
) -> Result<Vec<StreamEvent>, SwarmError> {
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
    // A window that starts inside a turn (the cursor is past its opening user
    // item) has no opener row, so the assistant frames would have no turn to
    // attach to. Seed the turn from the newest user item before the cursor; the
    // derived id is the one the client already received, so `turn.created` is
    // not repeated.
    let seed = if starts_inside_a_turn(&items) {
        previous_user_item(store, &session.logical_id, cursor.value())
            .await?
            .map(|item| {
                (
                    crate::domain::turn_id_for_item(&item.item_id),
                    crate::domain::official::unix_from_rfc3339(&item.created_at),
                )
            })
    } else {
        None
    };
    let replay = OfficialReplay::new(session, live_open_turn);
    Ok(replay.render(
        &items,
        seed.as_ref().map(|(turn_id, at)| (turn_id.as_str(), *at)),
    ))
}

/// Whether the first official-relevant row is an assistant final frame, i.e.
/// the window begins after its turn's opening user item.
fn starts_inside_a_turn(items: &[crate::domain::ItemView]) -> bool {
    for item in items {
        if item.kind != crate::domain::ItemKind::Message {
            continue;
        }
        match item.payload.get("role").and_then(|role| role.as_str()) {
            Some("user") => return false,
            Some("assistant") if item.payload.get("complete") == Some(&json!(true)) => return true,
            _ => {}
        }
    }
    false
}

/// Newest user message at or before `up_to`, scanning backwards.
async fn previous_user_item(
    store: &dyn crate::store::SwarmStore,
    logical_id: &str,
    up_to: u64,
) -> Result<Option<crate::domain::ItemView>, SwarmError> {
    // The reconnect cursor is the last delivered seq, so its row is included.
    let mut before = Some(up_to.saturating_add(1));
    while let Some(cursor) = before {
        let page = store
            .swarm_items_before(logical_id, Some(cursor), ItemsWindow::MAX_LIMIT)
            .await
            .map_err(|error| crate::store::store_failure("items_before", error))?;
        let Some(oldest_seq) = page.last().map(|row| row.seq) else {
            return Ok(None);
        };
        if let Some(row) = page.into_iter().find(|row| {
            let view = row.to_view();
            view.kind == crate::domain::ItemKind::Message
                && view.payload.get("role").and_then(|role| role.as_str()) == Some("user")
        }) {
            return Ok(Some(row.to_view()));
        }
        // The page is descending, so the oldest row is the next exclusive bound.
        before = (oldest_seq < cursor).then_some(oldest_seq);
    }
    Ok(None)
}

/// Typed port between HTTP and the session runtime (S4 actor / S3 fake).
#[async_trait]
pub trait SwarmService: Send + Sync {
    async fn create_session(
        &self,
        command: CreateSessionCommand,
    ) -> Result<CreateSessionResult, SwarmError>;

    async fn get_session(&self, logical_id: &LogicalId) -> Result<SessionView, SwarmError>;

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
    async fn subscribe(
        &self,
        logical_id: &LogicalId,
        cursor: Cursor,
    ) -> Result<EventSubscription, SwarmError>;

    /// Register a stream before `POST /sessions` executes the first turn.
    /// Must not create or touch session rows.
    async fn open_stream(&self, logical_id: &LogicalId) -> EventSubscription;
}
