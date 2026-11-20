//! Persistence port for Swarm-owned facts (`design §4.3`–§4.5).
//!
//! Since S6c1a the Swarm facts live in their **own** Loom database:
//! [`SwarmStateStore`] opens `PERI_SWARM_STATE_STORE` directly, owns its DDL and
//! records `swarm_meta.version`. It is what `bootstrap` installs.
//!
//! S3 shipped the trait plus an in-memory fake, and S6b added
//! [`RemoteSwarmStore`], which maps the rows onto the `peri-resources` port
//! (`design §4.1`) sharing the Peri session connection. That adapter is retained
//! only until S6c1b removes the single-database path; `bootstrap` no longer uses
//! it.

mod memory;
mod remote;
mod turso;

pub use memory::MemorySwarmStore;
pub use remote::RemoteSwarmStore;
pub use turso::{
    SwarmStateError, SwarmStateStore, SwarmStateStoreConfig, SWARM_STATE_SCHEMA_VERSION,
};

use async_trait::async_trait;
use serde_json::Value;

use crate::domain::{
    EnvironmentRef, InputState, ItemKind, ItemSource, ItemView, McpToolSpec, SessionState,
    SwarmError,
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

#[derive(Clone, Debug, PartialEq)]
pub struct SwarmInputRow {
    pub logical_id: String,
    pub input_id: String,
    pub fingerprint: String,
    pub state: InputState,
    pub created_at: String,
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
    async fn swarm_items_before(
        &self,
        logical_id: &str,
        before: Option<u64>,
        limit: u32,
    ) -> Result<Vec<SwarmItemRow>, StoreError>;
    async fn swarm_last_seq(&self, logical_id: &str) -> Result<u64, StoreError>;

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
