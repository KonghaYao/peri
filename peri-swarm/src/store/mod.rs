//! Persistence port for Swarm-owned facts (`design §4.3`–§4.5).
//!
//! Swarm facts live in their **own** PostgreSQL database (`design §4.1`): after
//! [`SwarmStateStore`] was added in S6c1a, bootstrap opens
//! `PERI_SWARM_STATE_STORE` directly, owns its DDL and records
//! `swarm_meta.version`. The Peri session schema never carries `swarm_*`
//! objects.
//!
//! S3 shipped the trait plus an in-memory fake; S6c1b removed the S6b adapter
//! that had mapped these rows onto a `peri-resources` port on the shared
//! session connection. There is one production implementation now.

mod memory;
mod postgres;

use async_trait::async_trait;
pub use memory::MemorySwarmStore;
pub use postgres::{
    SwarmStateError, SwarmStateStore, SwarmStateStoreConfig, SWARM_STATE_SCHEMA_VERSION,
};
use serde_json::Value;

use crate::domain::{
    EnvironmentRef, InputState, ItemKind, ItemOrder, ItemSource, ItemView, McpToolSpec,
    OfficialTurn, PromptCacheOptions, SessionState, SessionsWindow, SwarmError, TurnStatus,
    TurnsWindow,
};

/// Failure classes of the store port. `Unavailable` and `Conflict` are reported,
/// never retried silently.
#[derive(Clone, Debug, thiserror::Error)]
pub enum StoreError {
    #[error("swarm store unavailable: {0}")]
    Unavailable(String),
    #[error("swarm store conflict: {0}")]
    Conflict(String),
    #[error("swarm store operation unsupported: {0}")]
    Unsupported(String),
    #[error("swarm row not found: {0}")]
    NotFound(String),
}

/// Log once and return a transport-neutral upstream failure.
#[must_use]
pub fn store_failure(operation: &'static str, error: StoreError) -> SwarmError {
    tracing::warn!(operation, error = %error, "swarm store operation failed");
    SwarmError::upstream_unavailable(operation)
}

/// Outcome of `swarm_put_input` (`INSERT ... ON CONFLICT DO NOTHING` semantics).
#[derive(Clone, Debug, PartialEq)]
pub enum InputPut {
    Inserted,
    Existing(SwarmInputRow),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SwarmSessionRow {
    pub logical_id: String,
    pub root_session_id: String,
    pub actor_key: String,
    pub cwd: String,
    pub state: SessionState,
    pub model: Option<String>,
    /// Inline instructions are a Swarm-owned fact (`plan §4`). S6 decides the
    /// column encoding; this port must not drop them.
    pub instructions: Option<String>,
    /// Caller-declared MCP tools, persisted as canonical JSON in
    /// `swarm_sessions.mcp_json` (S8a1). No declaration is an empty list; old
    /// rows with `NULL` read back the same way.
    pub mcp_tools: Vec<McpToolSpec>,
    pub environment: EnvironmentRef,
    /// Caller metadata (`metadata_json`). `NULL`/absent reads as an empty map.
    pub metadata: std::collections::BTreeMap<String, String>,
    /// Session-default prompt cache lifetime (`prompt_cache_options_json`).
    pub prompt_cache_options: Option<PromptCacheOptions>,
    pub created_at: String,
    pub last_activity_at: String,
    pub last_item_seq: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SwarmSessionUpdate {
    pub logical_id: String,
    pub state: SessionState,
    pub last_item_seq: u64,
    pub last_activity_at: String,
}

/// One `sessions.update` write (`POST /sessions/{id}`), all fields optional.
///
/// `None` keeps the stored value. `Some(empty)` replaces metadata with an empty
/// map (the official "clear" spelling); `Some(None)` clears the prompt cache
/// option. `model` and `mcp_tools` are replaced when present.
#[derive(Clone, Debug, PartialEq)]
pub struct SwarmSessionPatch {
    pub logical_id: String,
    pub model: Option<String>,
    pub mcp_tools: Option<Vec<McpToolSpec>>,
    pub metadata: Option<std::collections::BTreeMap<String, String>>,
    pub prompt_cache_options: Option<Option<PromptCacheOptions>>,
    pub last_activity_at: Option<String>,
}

/// One persisted turn (`swarm_turns`).
///
/// Ordering is `(created_at, turn_id)`: `created_at` is the Unix second when
/// the turn opened, and `turn_id` is the unique tie-break when two turns share
/// a second. Timestamps are Unix seconds, matching the official `Turn` shape.
#[derive(Clone, Debug, PartialEq)]
pub struct SwarmTurnRow {
    pub logical_id: String,
    pub turn_id: String,
    pub status: TurnStatus,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub error: Option<Value>,
    pub usage: Option<Value>,
    pub subagent_id: Option<String>,
}

impl SwarmTurnRow {
    /// Public projection. `agent_id` is the gateway's single root convention;
    /// `subagent_id`/`usage` stay whatever the caller persisted (today both
    /// are always `None`).
    #[must_use]
    pub fn to_official(&self) -> OfficialTurn {
        OfficialTurn {
            id: self.turn_id.clone(),
            object: "agent.session.turn".to_owned(),
            agent_id: "root".to_owned(),
            session_id: self.logical_id.clone(),
            status: self.status.as_str().to_owned(),
            created_at: self.created_at,
            started_at: self.started_at,
            completed_at: self.completed_at,
            subagent_id: self.subagent_id.clone(),
            error: self.error.clone(),
            usage: self.usage.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SwarmItemRow {
    pub logical_id: String,
    pub seq: u64,
    pub item_id: String,
    pub kind: ItemKind,
    pub source: ItemSource,
    pub payload: Value,
    pub created_at: String,
    pub truncated: bool,
}

impl SwarmItemRow {
    #[must_use]
    pub fn to_view(&self) -> ItemView {
        ItemView {
            id: self.item_id.clone(),
            seq: self.seq,
            item_id: self.item_id.clone(),
            kind: self.kind,
            source: self.source.clone(),
            payload: self.payload.clone(),
            created_at: self.created_at.clone(),
            truncated: self.truncated,
        }
    }
}

/// Group raw rows into an item page.
///
/// `rows` must contain every row of the candidate items (at most `limit + 1`
/// items, in either order). The helper keeps items in page order until either
/// the item cap (`limit`) or the aggregate **row budget** (`limit` rows) is
/// reached, derives `data` from each kept item's greatest `seq`, and returns the
/// raw rows of exactly those items. Both stores call it, so memory and
/// PostgreSQL pages cannot disagree on the boundary rule.
///
/// A page's own item rows are what `/items` returns, so a page of `limit` long
/// messages would otherwise serialize thousands of rows twice (`items` and
/// `data`). The row budget stops adding a *next* item once the page already
/// holds `limit` rows. The first item is always kept, even when it alone
/// exceeds the budget: dropping it would lose its final frame, and
/// `items.list` is an item-oriented read (delta frames belong on SSE).
#[must_use]
pub(crate) fn grouped_item_page(
    rows: Vec<SwarmItemRow>,
    order: ItemOrder,
    limit: u32,
) -> SwarmItemPage {
    // `limit` is both the item cap and the aggregate row budget (`design §4.2.2`).
    let item_cap = limit as usize;
    let row_budget = limit as usize;
    let mut rows_per_item: std::collections::BTreeMap<&str, usize> =
        std::collections::BTreeMap::new();
    let mut last_rows: std::collections::BTreeMap<String, SwarmItemRow> =
        std::collections::BTreeMap::new();
    for row in &rows {
        *rows_per_item.entry(row.item_id.as_str()).or_default() += 1;
        match last_rows.get(&row.item_id) {
            Some(existing) if existing.seq >= row.seq => {}
            _ => {
                last_rows.insert(row.item_id.clone(), row.clone());
            }
        }
    }
    let mut candidates: Vec<SwarmItemRow> = last_rows.into_values().collect();
    candidates.sort_by_key(|row| row.seq);
    if order == ItemOrder::Desc {
        candidates.reverse();
    }
    let candidates_len = candidates.len();

    let mut items: Vec<SwarmItemRow> = Vec::new();
    let mut rows_in_page = 0usize;
    for item in candidates {
        if items.len() == item_cap {
            break;
        }
        if !items.is_empty() && rows_in_page >= row_budget {
            break;
        }
        rows_in_page += rows_per_item
            .get(item.item_id.as_str())
            .copied()
            .unwrap_or(0);
        items.push(item);
    }
    // More items exist beyond this page (the stores fetch `limit + 1` items, so
    // a kept set shorter than the candidate set always leaves an item behind).
    let has_more = candidates_len > items.len();
    // `nextCursor` only exists when a next page does (existing clients read it
    // as "there is more") and names the last item's greatest seq.
    let next_cursor = if has_more {
        items.last().map(|row| row.seq)
    } else {
        None
    };
    let keep: std::collections::BTreeSet<&str> =
        items.iter().map(|row| row.item_id.as_str()).collect();
    let mut rows: Vec<SwarmItemRow> = rows
        .into_iter()
        .filter(|row| keep.contains(row.item_id.as_str()))
        .collect();
    rows.sort_by_key(|row| row.seq);
    if order == ItemOrder::Desc {
        rows.reverse();
    }
    SwarmItemPage {
        rows,
        items,
        has_more,
        next_cursor,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SwarmInputRow {
    pub logical_id: String,
    pub input_id: String,
    pub fingerprint: String,
    pub state: InputState,
    pub created_at: String,
}

/// One item-grouped page of `swarm_items` (`design §4.2`).
///
/// The page unit is the item (`item_id`), not the row: a streamed assistant
/// message persists one row per delta plus its final `complete:true` row. Item
/// membership is defined by the item's greatest `seq`, so `after` (a seq or an
/// item id resolved to one) advances strictly past a whole item instead of
/// landing inside it and re-reading the same item forever.
///
/// `limit` is both the item cap and the aggregate row budget. A page therefore
/// normally holds fewer `items` (and rows) than a pure item count would allow
/// when messages have many deltas; an item whose own row count exceeds the
/// budget is still returned whole.
#[derive(Clone, Debug, PartialEq)]
pub struct SwarmItemPage {
    /// Every row of the page's items, in the requested order (`seq` is not
    /// contiguous because two streamed messages can interleave).
    pub rows: Vec<SwarmItemRow>,
    /// One row per item: the item's greatest `seq`, in page order.
    pub items: Vec<SwarmItemRow>,
    /// More items exist beyond this page.
    pub has_more: bool,
    /// Greatest `seq` of the last item in page order; `None` for an empty page.
    pub next_cursor: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SwarmBindingRow {
    pub logical_env_id: String,
    pub logical_id: String,
    pub sandbox_id: String,
    pub workspace_path: String,
    pub sandbox_root: String,
    pub settings_profile: Option<String>,
    pub token_file: Option<String>,
    pub last_run_id: Option<String>,
    pub last_used_at: String,
}

/// Async storage port consumed by the actor-side service (S4+) and S6.
///
/// Every method is scoped by `logical_id`; there is no cross-session query and
/// no generic SQL surface. Implementations must surface conflicts instead of
/// renumbering or overwriting rows.
#[async_trait]
pub trait SwarmStore: Send + Sync {
    async fn swarm_upsert_session(&self, row: SwarmSessionRow) -> Result<(), StoreError>;
    /// Atomically claim a new hosted session identity and attach its environment.
    /// An identity conflict must leave the existing environment binding intact.
    async fn swarm_create_hosted_session(
        &self,
        row: SwarmSessionRow,
        binding: SwarmBindingRow,
        require_existing: bool,
    ) -> Result<(), StoreError>;
    async fn swarm_get_session(
        &self,
        logical_id: &str,
    ) -> Result<Option<SwarmSessionRow>, StoreError>;
    async fn swarm_update_session(&self, update: SwarmSessionUpdate) -> Result<(), StoreError>;
    /// Replace the mutable `sessions.update` fields. Absent fields keep their
    /// stored value. Returns `NotFound` when the row does not exist.
    async fn swarm_patch_session(&self, patch: SwarmSessionPatch) -> Result<(), StoreError>;
    /// One keyset page over `(logical_id, order)` with metadata/agent filters.
    ///
    /// Returns at most `window.limit + 1` rows so the caller can derive
    /// `has_more` without a second query.
    async fn swarm_list_sessions(
        &self,
        window: &SessionsWindow,
    ) -> Result<Vec<SwarmSessionRow>, StoreError>;
    /// Delete every Swarm-owned row of one session.
    ///
    /// The implementation must delete `swarm_sessions`, `swarm_items`,
    /// `swarm_inputs`, `swarm_turns` and `swarm_bindings` as one unit: a
    /// partial delete that reports success is a contract violation.
    async fn swarm_delete_session(&self, logical_id: &str) -> Result<(), StoreError>;

    async fn swarm_append_item(&self, row: SwarmItemRow) -> Result<(), StoreError>;
    /// Append a contiguous run of items in one write unit.
    ///
    /// The caller has assigned consecutive `seq` values. `progress`, when
    /// present, records the session's item cursor and activity in the same
    /// unit, so a reader that observes the progress cannot be shown a cursor
    /// beyond the items that are durable. A failure must leave none of the run
    /// persisted; a seq/`item_id` conflict is reported as
    /// [`StoreError::Conflict`] so the caller can reload `MAX(seq)` and retry
    /// the whole run (a partial run would leave a hole in the log).
    async fn swarm_append_items(
        &self,
        rows: &[SwarmItemRow],
        progress: Option<SwarmSessionUpdate>,
    ) -> Result<(), StoreError>;
    /// Ascending page with `seq > after`.
    async fn swarm_items_after(
        &self,
        logical_id: &str,
        after: u64,
        limit: u32,
    ) -> Result<Vec<SwarmItemRow>, StoreError>;
    /// Descending page with `seq < before` (newest first when `before` is `None`).
    ///
    /// Row-level read used by the SSE replay and the history lookback; it is
    /// not the `/items` pagination path.
    async fn swarm_items_before(
        &self,
        logical_id: &str,
        before: Option<u64>,
        limit: u32,
    ) -> Result<Vec<SwarmItemRow>, StoreError>;
    /// Item-grouped page (`design §4.2`).
    ///
    /// `after` is an item boundary: an item belongs to an ascending page when
    /// its greatest `seq` is `> after`, and to a descending page when it is
    /// `< after`. Grouping happens in the store so a long message cannot
    /// consume the row budget and leave the page empty. `limit` caps both the
    /// item count and the page's aggregate row count; a single item larger than
    /// the budget is returned whole.
    async fn swarm_item_page(
        &self,
        logical_id: &str,
        order: ItemOrder,
        after: Option<u64>,
        limit: u32,
    ) -> Result<SwarmItemPage, StoreError>;
    async fn swarm_last_seq(&self, logical_id: &str) -> Result<u64, StoreError>;
    /// Row for `item_id`, preferring the greatest `seq` when an assistant
    /// message spans several frames. Used to resolve the official `after=id`
    /// cursor back to a seq.
    async fn swarm_get_item(
        &self,
        logical_id: &str,
        item_id: &str,
    ) -> Result<Option<SwarmItemRow>, StoreError>;

    async fn swarm_put_input(&self, row: SwarmInputRow) -> Result<InputPut, StoreError>;
    async fn swarm_get_input(
        &self,
        logical_id: &str,
        input_id: &str,
    ) -> Result<Option<SwarmInputRow>, StoreError>;

    async fn swarm_put_binding(&self, row: SwarmBindingRow) -> Result<(), StoreError>;
    async fn swarm_get_binding(
        &self,
        logical_env_id: &str,
    ) -> Result<Option<SwarmBindingRow>, StoreError>;

    /// Insert or replace one turn row.
    ///
    /// `begin_turn` writes `in_progress`; the terminal write replaces status,
    /// `started_at`/`completed_at`, `error`, `usage` and `subagent_id` while
    /// keeping the opening `created_at`.
    async fn swarm_upsert_turn(&self, row: SwarmTurnRow) -> Result<(), StoreError>;
    async fn swarm_get_turn(
        &self,
        logical_id: &str,
        turn_id: &str,
    ) -> Result<Option<SwarmTurnRow>, StoreError>;
    /// Keyset page over `(created_at, turn_id)`; `after` must already be
    /// resolved to an existing turn by the caller. Returns at most
    /// `window.limit + 1` rows.
    async fn swarm_turn_page(
        &self,
        logical_id: &str,
        window: &TurnsWindow,
    ) -> Result<Vec<SwarmTurnRow>, StoreError>;
    /// Close every non-terminal turn of a session (crash reconciliation).
    /// Returns the number of rows updated.
    async fn swarm_close_open_turns(
        &self,
        logical_id: &str,
        status: TurnStatus,
        completed_at: i64,
        error: Option<Value>,
    ) -> Result<u64, StoreError>;
    /// Whether the session still owns a non-terminal (`queued`/`in_progress`/
    /// `waiting`) turn row.
    ///
    /// A crash can leave the session row `idle` after the turn row outlived its
    /// writer, so `session.state` alone cannot answer "was this turn closed?".
    /// This is the read side of [`Self::swarm_close_open_turns`].
    async fn swarm_has_open_turn(&self, logical_id: &str) -> Result<bool, StoreError>;
}
