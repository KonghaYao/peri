//! Loom-backed [`SwarmStore`] over the S6a `peri-resources` port.
//!
//! This adapter is the only place that knows the port's row shapes
//! (`.peri/plans/S6a-report.md` §1.2). It never opens a local database and never
//! falls back to one: every call goes through
//! [`peri_resources::SwarmStorePort`], which is the remote Loom connection when
//! the deployment opened one (`Resources::swarm_store()`), and the explicit
//! `Unsupported` port otherwise.
//!
//! ## Field mapping
//!
//! | [`crate::store`] row | port row | encoding |
//! | --- | --- | --- |
//! | `state` / `kind` / `InputState` | same-name `String` | [`SessionState::as_str`] etc. |
//! | `environment: EnvironmentRef` | `environment_json` | serde JSON text |
//! | `source: ItemSource` | `source_json` | serde JSON text |
//! | `payload: Value` | `payload_json` | serde JSON text |
//! | `instructions`, `model`, `truncated`, `seq`, timestamps | same-name field | as-is |
//!
//! JSON text is the same precedent as `thread_mq.payload_json`: `peri-resources`
//! stores the bytes, `peri-swarm` owns the shape.
//!
//! ## Error classes
//!
//! [`StoreError`] has four classes, so the port's richer vocabulary is folded
//! without inventing success:
//!
//! | port kind | [`StoreError`] |
//! | --- | --- |
//! | `Conflict` | `Conflict` (kept, never renumbered) |
//! | `NotFound` | `NotFound` |
//! | `Unsupported` / `ReadOnlyStore` | `Unsupported` |
//! | everything else (unavailable, timeout, uncertain, corrupt, internal) | `Unavailable` |
//!
//! `PersistenceUncertain` becomes `Unavailable` with the port's own wording
//! ("outcome is unknown"), so callers must not read it as "not applied". A row
//! that cannot be decoded is `Unavailable` too: a corrupt row is not an absence
//! (`NotFound`) and not a supported operation.

use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::session_resources::{SessionResourceError, SessionResourceErrorKind};
use peri_resources::{
    SwarmBindingRow as PortBindingRow, SwarmInputPut as PortInputPut,
    SwarmInputRow as PortInputRow, SwarmItemRow as PortItemRow, SwarmSessionRow as PortSessionRow,
    SwarmSessionUpdate as PortSessionUpdate, SwarmStorePort,
};

use super::{
    InputPut, StoreError, SwarmBindingRow, SwarmInputRow, SwarmItemRow, SwarmSessionRow,
    SwarmSessionUpdate, SwarmStore,
};
use crate::domain::{EnvironmentRef, InputState, ItemKind, ItemSource, McpToolSpec, SessionState};

/// [`SwarmStore`] backed by the deployment's remote Loom port.
pub struct RemoteSwarmStore {
    port: Arc<dyn SwarmStorePort>,
}

impl RemoteSwarmStore {
    #[must_use]
    pub fn new(port: Arc<dyn SwarmStorePort>) -> Self {
        Self { port }
    }
}

impl std::fmt::Debug for RemoteSwarmStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteSwarmStore")
            .finish_non_exhaustive()
    }
}

// ── error mapping ───────────────────────────────────────────────────────────

fn map_port_error(operation: &'static str, error: SessionResourceError) -> StoreError {
    let detail = error.to_string();
    match error.kind() {
        SessionResourceErrorKind::Conflict { detail } => StoreError::Conflict(detail.clone()),
        SessionResourceErrorKind::NotFound => StoreError::NotFound(operation.to_owned()),
        SessionResourceErrorKind::Unsupported | SessionResourceErrorKind::ReadOnlyStore => {
            StoreError::Unsupported(format!("{operation}: {detail}"))
        }
        _ => {
            if error.is_persistence_uncertain() {
                tracing::warn!(
                    operation,
                    "remote swarm store outcome is unknown; the write must not be retried blindly"
                );
            }
            StoreError::Unavailable(format!("{operation}: {detail}"))
        }
    }
}

fn corrupt(field: &'static str, raw: &str) -> StoreError {
    StoreError::Unavailable(format!("unreadable swarm row: {field}={raw:?}"))
}

// ── enum encoding ───────────────────────────────────────────────────────────

fn parse_session_state(raw: &str) -> Result<SessionState, StoreError> {
    match raw {
        "idle" => Ok(SessionState::Idle),
        "running" => Ok(SessionState::Running),
        "recovering" => Ok(SessionState::Recovering),
        "stopped" => Ok(SessionState::Stopped),
        "failed" => Ok(SessionState::Failed),
        other => Err(corrupt("state", other)),
    }
}

fn parse_item_kind(raw: &str) -> Result<ItemKind, StoreError> {
    match raw {
        "message" => Ok(ItemKind::Message),
        "thought" => Ok(ItemKind::Thought),
        "tool_call" => Ok(ItemKind::ToolCall),
        "tool_update" => Ok(ItemKind::ToolUpdate),
        "tool_result" => Ok(ItemKind::ToolResult),
        "receipt" => Ok(ItemKind::Receipt),
        other => Err(corrupt("kind", other)),
    }
}

fn parse_input_state(raw: &str) -> Result<InputState, StoreError> {
    match raw {
        "accepted" => Ok(InputState::Accepted),
        "handed_off" => Ok(InputState::HandedOff),
        "failed" => Ok(InputState::Failed),
        other => Err(corrupt("input state", other)),
    }
}

// ── field mapping ───────────────────────────────────────────────────────────

fn encode<T: serde::Serialize>(value: &T, field: &'static str) -> Result<String, StoreError> {
    serde_json::to_string(value)
        .map_err(|error| StoreError::Unavailable(format!("cannot encode {field}: {error}")))
}

fn decode<T: serde::de::DeserializeOwned>(
    text: &str,
    field: &'static str,
) -> Result<T, StoreError> {
    serde_json::from_str(text)
        .map_err(|error| StoreError::Unavailable(format!("cannot decode {field}: {error}")))
}

fn to_port_session(row: SwarmSessionRow) -> Result<PortSessionRow, StoreError> {
    let mcp_json = McpToolSpec::encode_declaration(&row.mcp_tools)
        .map_err(|error| StoreError::Unavailable(format!("cannot encode mcp tools: {error}")))?;
    Ok(PortSessionRow {
        logical_id: row.logical_id,
        root_session_id: row.root_session_id,
        actor_key: row.actor_key,
        cwd: row.cwd,
        state: row.state.as_str().to_owned(),
        model: row.model,
        instructions: row.instructions,
        mcp_json,
        environment_json: encode(&row.environment, "environment")?,
        created_at: row.created_at,
        last_activity_at: row.last_activity_at,
        last_item_seq: row.last_item_seq,
    })
}

fn from_port_session(row: PortSessionRow) -> Result<SwarmSessionRow, StoreError> {
    let mcp_tools = McpToolSpec::decode_declaration(row.mcp_json.as_deref())
        .map_err(|error| StoreError::Unavailable(format!("cannot decode mcp tools: {error}")))?;
    Ok(SwarmSessionRow {
        logical_id: row.logical_id,
        root_session_id: row.root_session_id,
        actor_key: row.actor_key,
        cwd: row.cwd,
        state: parse_session_state(&row.state)?,
        model: row.model,
        instructions: row.instructions,
        mcp_tools,
        environment: decode::<EnvironmentRef>(&row.environment_json, "environment")?,
        created_at: row.created_at,
        last_activity_at: row.last_activity_at,
        last_item_seq: row.last_item_seq,
    })
}

fn to_port_session_update(update: SwarmSessionUpdate) -> PortSessionUpdate {
    PortSessionUpdate {
        logical_id: update.logical_id,
        state: update.state.as_str().to_owned(),
        last_item_seq: update.last_item_seq,
        last_activity_at: update.last_activity_at,
    }
}

fn to_port_item(row: SwarmItemRow) -> Result<PortItemRow, StoreError> {
    Ok(PortItemRow {
        logical_id: row.logical_id,
        seq: row.seq,
        item_id: row.item_id,
        kind: row.kind.as_str().to_owned(),
        source_json: encode(&row.source, "item source")?,
        payload_json: encode(&row.payload, "item payload")?,
        created_at: row.created_at,
        truncated: row.truncated,
    })
}

fn from_port_item(row: PortItemRow) -> Result<SwarmItemRow, StoreError> {
    Ok(SwarmItemRow {
        logical_id: row.logical_id,
        seq: row.seq,
        item_id: row.item_id,
        kind: parse_item_kind(&row.kind)?,
        source: decode::<ItemSource>(&row.source_json, "item source")?,
        payload: decode(&row.payload_json, "item payload")?,
        created_at: row.created_at,
        truncated: row.truncated,
    })
}

fn to_port_input(row: SwarmInputRow) -> PortInputRow {
    PortInputRow {
        logical_id: row.logical_id,
        input_id: row.input_id,
        fingerprint: row.fingerprint,
        state: row.state.as_str().to_owned(),
        created_at: row.created_at,
    }
}

fn from_port_input(row: PortInputRow) -> Result<SwarmInputRow, StoreError> {
    Ok(SwarmInputRow {
        logical_id: row.logical_id,
        input_id: row.input_id,
        fingerprint: row.fingerprint,
        state: parse_input_state(&row.state)?,
        created_at: row.created_at,
    })
}

fn to_port_binding(row: SwarmBindingRow) -> PortBindingRow {
    PortBindingRow {
        logical_env_id: row.logical_env_id,
        logical_id: row.logical_id,
        sandbox_id: row.sandbox_id,
        workspace_path: row.workspace_path,
        sandbox_root: row.sandbox_root,
        settings_profile: row.settings_profile,
        token_file: row.token_file,
        last_run_id: row.last_run_id,
        last_used_at: row.last_used_at,
    }
}

fn from_port_binding(row: PortBindingRow) -> SwarmBindingRow {
    SwarmBindingRow {
        logical_env_id: row.logical_env_id,
        logical_id: row.logical_id,
        sandbox_id: row.sandbox_id,
        workspace_path: row.workspace_path,
        sandbox_root: row.sandbox_root,
        settings_profile: row.settings_profile,
        token_file: row.token_file,
        last_run_id: row.last_run_id,
        last_used_at: row.last_used_at,
    }
}

// ── trait implementation ────────────────────────────────────────────────────

#[async_trait]
impl SwarmStore for RemoteSwarmStore {
    async fn swarm_upsert_session(&self, row: SwarmSessionRow) -> Result<(), StoreError> {
        let row = to_port_session(row)?;
        self.port
            .swarm_upsert_session(row)
            .await
            .map_err(|error| map_port_error("upsert_session", error))
    }

    async fn swarm_get_session(
        &self,
        logical_id: &str,
    ) -> Result<Option<SwarmSessionRow>, StoreError> {
        self.port
            .swarm_get_session(logical_id)
            .await
            .map_err(|error| map_port_error("get_session", error))?
            .map(from_port_session)
            .transpose()
    }

    async fn swarm_update_session(&self, update: SwarmSessionUpdate) -> Result<(), StoreError> {
        self.port
            .swarm_update_session(to_port_session_update(update))
            .await
            .map_err(|error| map_port_error("update_session", error))
    }

    async fn swarm_append_item(&self, row: SwarmItemRow) -> Result<(), StoreError> {
        let row = to_port_item(row)?;
        self.port
            .swarm_append_item(row)
            .await
            .map_err(|error| map_port_error("append_item", error))
    }

    async fn swarm_items_after(
        &self,
        logical_id: &str,
        after: u64,
        limit: u32,
    ) -> Result<Vec<SwarmItemRow>, StoreError> {
        let rows = self
            .port
            .swarm_items_after(logical_id, after, limit)
            .await
            .map_err(|error| map_port_error("items_after", error))?;
        rows.into_iter().map(from_port_item).collect()
    }

    async fn swarm_items_before(
        &self,
        logical_id: &str,
        before: Option<u64>,
        limit: u32,
    ) -> Result<Vec<SwarmItemRow>, StoreError> {
        let rows = self
            .port
            .swarm_items_before(logical_id, before, limit)
            .await
            .map_err(|error| map_port_error("items_before", error))?;
        rows.into_iter().map(from_port_item).collect()
    }

    async fn swarm_last_seq(&self, logical_id: &str) -> Result<u64, StoreError> {
        self.port
            .swarm_last_seq(logical_id)
            .await
            .map_err(|error| map_port_error("last_seq", error))
    }

    async fn swarm_put_input(&self, row: SwarmInputRow) -> Result<InputPut, StoreError> {
        let put = self
            .port
            .swarm_put_input(to_port_input(row))
            .await
            .map_err(|error| map_port_error("put_input", error))?;
        match put {
            PortInputPut::Inserted => Ok(InputPut::Inserted),
            PortInputPut::Existing(row) => Ok(InputPut::Existing(from_port_input(row)?)),
        }
    }

    async fn swarm_get_input(
        &self,
        logical_id: &str,
        input_id: &str,
    ) -> Result<Option<SwarmInputRow>, StoreError> {
        self.port
            .swarm_get_input(logical_id, input_id)
            .await
            .map_err(|error| map_port_error("get_input", error))?
            .map(from_port_input)
            .transpose()
    }

    async fn swarm_put_binding(&self, row: SwarmBindingRow) -> Result<(), StoreError> {
        self.port
            .swarm_put_binding(to_port_binding(row))
            .await
            .map_err(|error| map_port_error("put_binding", error))
    }

    async fn swarm_get_binding(
        &self,
        logical_env_id: &str,
    ) -> Result<Option<SwarmBindingRow>, StoreError> {
        Ok(self
            .port
            .swarm_get_binding(logical_env_id)
            .await
            .map_err(|error| map_port_error("get_binding", error))?
            .map(from_port_binding))
    }
}

#[cfg(test)]
#[path = "remote_test.rs"]
mod tests;
