//! Persistence port for Swarm-owned facts (`design §4.3`–§4.5).
//!
//! Swarm facts live in their **own** Loom database (`design §4.1`): after
//! [`SwarmStateStore`] was added in S6c1a, bootstrap opens
//! `PERI_SWARM_STATE_STORE` directly, owns its DDL and records
//! `swarm_meta.version`. The Peri session schema never carries `swarm_*`
//! objects.
//!
//! S3 shipped the trait plus an in-memory fake; S6c1b removed the S6b adapter
//! that had mapped these rows onto a `peri-resources` port on the shared
//! session connection. There is one production implementation now.

mod memory;
mod turso;

use async_trait::async_trait;
pub use memory::MemorySwarmStore;
use serde_json::Value;
pub use turso::{
    SwarmStateError, SwarmStateStore, SwarmStateStoreConfig, SWARM_STATE_SCHEMA_VERSION,
};

use crate::domain::{
    EnvironmentRef, InputState, ItemKind, ItemOrder, ItemSource, ItemView, McpToolSpec,
    SessionState, SwarmError,
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
/// raw rows of exactly those items. Both stores call it, so memory and Loom
/// pages cannot disagree on the boundary rule.
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
    async fn swarm_get_session(
        &self,
        logical_id: &str,
    ) -> Result<Option<SwarmSessionRow>, StoreError>;
    async fn swarm_update_session(&self, update: SwarmSessionUpdate) -> Result<(), StoreError>;

    async fn swarm_append_item(&self, row: SwarmItemRow) -> Result<(), StoreError>;
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
}
