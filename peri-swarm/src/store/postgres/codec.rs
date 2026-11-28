//! Row encoding for the Swarm state database.
//!
//! The byte layout is the one fixed in `.peri/plans/S6a-report.md` §1.2: enums
//! travel as their `as_str()` text, JSON columns as serde JSON text, `seq` and
//! turn times as `BIGINT`, `truncated` as `BOOLEAN`. Decoding never guesses: an
//! unknown enum text or a wrong column type is reported as
//! [`StoreError::Unavailable`], never as an absent row.

use std::collections::BTreeMap;

use serde_json::Value;
use sqlx::postgres::{PgArguments, PgRow};
use sqlx::query::Query;
use sqlx::{Postgres, Row};

use super::super::{
    StoreError, SwarmBindingRow, SwarmInputRow, SwarmItemRow, SwarmSessionPatch, SwarmSessionRow,
    SwarmSessionUpdate, SwarmTurnRow, SwarmTurnUsageRow,
};
use super::budget::unreadable;
use crate::domain::{
    InputState, ItemKind, McpToolSpec, PromptCacheOptions, SessionState, TurnStatus,
};

pub(super) fn parse_session_state(raw: &str) -> Result<SessionState, StoreError> {
    match raw {
        "creating" => Ok(SessionState::Creating),
        "idle" => Ok(SessionState::Idle),
        "running" => Ok(SessionState::Running),
        "recovering" => Ok(SessionState::Recovering),
        "stopped" => Ok(SessionState::Stopped),
        "failed" => Ok(SessionState::Failed),
        other => Err(unreadable(format!("state={other:?}"))),
    }
}

pub(super) fn parse_item_kind(raw: &str) -> Result<ItemKind, StoreError> {
    match raw {
        "message" => Ok(ItemKind::Message),
        "thought" => Ok(ItemKind::Thought),
        "tool_call" => Ok(ItemKind::ToolCall),
        "tool_update" => Ok(ItemKind::ToolUpdate),
        "tool_result" => Ok(ItemKind::ToolResult),
        "receipt" => Ok(ItemKind::Receipt),
        "approval" => Ok(ItemKind::Approval),
        other => Err(unreadable(format!("kind={other:?}"))),
    }
}

pub(super) fn parse_input_state(raw: &str) -> Result<InputState, StoreError> {
    match raw {
        "accepted" => Ok(InputState::Accepted),
        "handed_off" => Ok(InputState::HandedOff),
        "failed" => Ok(InputState::Failed),
        other => Err(unreadable(format!("input state={other:?}"))),
    }
}

pub(super) fn parse_turn_status(raw: &str) -> Result<TurnStatus, StoreError> {
    TurnStatus::parse(raw).ok_or_else(|| unreadable(format!("turn status={raw:?}")))
}

pub(super) fn encode<T: serde::Serialize>(
    value: &T,
    field: &'static str,
) -> Result<String, StoreError> {
    serde_json::to_string(value)
        .map_err(|error| StoreError::Unavailable(format!("cannot encode {field}: {error}")))
}

pub(super) fn decode<T: serde::de::DeserializeOwned>(
    text: &str,
    field: &'static str,
) -> Result<T, StoreError> {
    serde_json::from_str(text)
        .map_err(|error| StoreError::Unavailable(format!("cannot decode {field}: {error}")))
}

/// Canonical declaration text, or `None` for "no declared tools".
pub(super) fn mcp_json(tools: &[McpToolSpec]) -> Result<Option<String>, StoreError> {
    McpToolSpec::encode_declaration(tools)
        .map_err(|error| StoreError::Unavailable(format!("cannot encode mcp tools: {error}")))
}

pub(super) fn decode_mcp_json(text: Option<&str>) -> Result<Vec<McpToolSpec>, StoreError> {
    McpToolSpec::decode_declaration(text)
        .map_err(|error| StoreError::Unavailable(format!("cannot decode mcp tools: {error}")))
}

/// An empty metadata map is stored as `NULL`, the documented "no metadata".
pub(super) fn metadata_json(
    metadata: &BTreeMap<String, String>,
) -> Result<Option<String>, StoreError> {
    if metadata.is_empty() {
        return Ok(None);
    }
    encode(metadata, "metadata").map(Some)
}

pub(super) fn decode_metadata(text: Option<&str>) -> Result<BTreeMap<String, String>, StoreError> {
    match text {
        None => Ok(BTreeMap::new()),
        Some(text) => decode(text, "metadata"),
    }
}

pub(super) fn prompt_cache_json(
    options: Option<PromptCacheOptions>,
) -> Result<Option<String>, StoreError> {
    options
        .map(|options| encode(&options, "prompt cache options"))
        .transpose()
}

pub(super) fn decode_prompt_cache(
    text: Option<&str>,
) -> Result<Option<PromptCacheOptions>, StoreError> {
    text.map(|text| decode(text, "prompt cache options"))
        .transpose()
}

/// `seq` is a `u64` in the port and a `BIGINT` in the schema. A value outside
/// the signed range is reported, never wrapped.
pub(super) fn seq_param(value: u64, what: &'static str) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| {
        StoreError::Unavailable(format!("swarm {what} exceeds the store's 64-bit range"))
    })
}

pub(super) fn seq_field(value: i64, what: &'static str) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| unreadable(format!("{what} is negative")))
}

// ── row decoding ────────────────────────────────────────────────────────────

fn text(row: &PgRow, column: &'static str) -> Result<String, StoreError> {
    row.try_get(column)
        .map_err(|error| unreadable(format!("{column}: {error}")))
}

fn optional_text(row: &PgRow, column: &'static str) -> Result<Option<String>, StoreError> {
    row.try_get(column)
        .map_err(|error| unreadable(format!("{column}: {error}")))
}

fn int(row: &PgRow, column: &'static str) -> Result<i64, StoreError> {
    row.try_get(column)
        .map_err(|error| unreadable(format!("{column}: {error}")))
}

fn optional_int(row: &PgRow, column: &'static str) -> Result<Option<i64>, StoreError> {
    row.try_get(column)
        .map_err(|error| unreadable(format!("{column}: {error}")))
}

fn boolean(row: &PgRow, column: &'static str) -> Result<bool, StoreError> {
    row.try_get(column)
        .map_err(|error| unreadable(format!("{column}: {error}")))
}

fn optional_json(row: &PgRow, column: &'static str) -> Result<Option<Value>, StoreError> {
    optional_text(row, column)?
        .as_deref()
        .map(|text| decode(text, column))
        .transpose()
}

/// Column list: `SELECT_SESSION_SQL`.
pub(super) fn decode_session(row: &PgRow) -> Result<SwarmSessionRow, StoreError> {
    let mcp_json = optional_text(row, "mcp_json")?;
    Ok(SwarmSessionRow {
        logical_id: text(row, "logical_id")?,
        root_session_id: text(row, "root_session_id")?,
        actor_key: text(row, "actor_key")?,
        cwd: text(row, "cwd")?,
        state: parse_session_state(&text(row, "state")?)?,
        model: optional_text(row, "model")?,
        instructions: optional_text(row, "instructions")?,
        mcp_tools: decode_mcp_json(mcp_json.as_deref())?,
        environment: decode(&text(row, "environment_json")?, "environment")?,
        metadata: decode_metadata(optional_text(row, "metadata_json")?.as_deref())?,
        prompt_cache_options: decode_prompt_cache(
            optional_text(row, "prompt_cache_options_json")?.as_deref(),
        )?,
        created_at: text(row, "created_at")?,
        last_activity_at: text(row, "last_activity_at")?,
        last_item_seq: seq_field(int(row, "last_item_seq")?, "last item seq")?,
    })
}

/// Column list: the `SELECT_ITEMS_*` / `SELECT_ITEM_*` statements.
pub(super) fn decode_item(row: &PgRow) -> Result<SwarmItemRow, StoreError> {
    Ok(SwarmItemRow {
        logical_id: text(row, "logical_id")?,
        seq: seq_field(int(row, "seq")?, "seq")?,
        item_id: text(row, "item_id")?,
        kind: parse_item_kind(&text(row, "kind")?)?,
        source: decode(&text(row, "source")?, "item source")?,
        payload: decode(&text(row, "payload_json")?, "item payload")?,
        created_at: text(row, "created_at")?,
        truncated: boolean(row, "truncated")?,
    })
}

/// Column list: `SELECT_INPUT_SQL`.
pub(super) fn decode_input(row: &PgRow) -> Result<SwarmInputRow, StoreError> {
    Ok(SwarmInputRow {
        logical_id: text(row, "logical_id")?,
        input_id: text(row, "input_id")?,
        fingerprint: text(row, "fingerprint")?,
        state: parse_input_state(&text(row, "state")?)?,
        created_at: text(row, "created_at")?,
    })
}

/// Column list: `SELECT_BINDING_SQL`.
pub(super) fn decode_binding(row: &PgRow) -> Result<SwarmBindingRow, StoreError> {
    Ok(SwarmBindingRow {
        logical_env_id: text(row, "logical_env_id")?,
        logical_id: text(row, "logical_id")?,
        sandbox_id: text(row, "sandbox_id")?,
        workspace_path: text(row, "workspace_path")?,
        sandbox_root: text(row, "sandbox_root")?,
        settings_profile: optional_text(row, "settings_profile")?,
        token_file: optional_text(row, "token_file")?,
        last_run_id: optional_text(row, "last_run_id")?,
        last_used_at: text(row, "last_used_at")?,
    })
}

/// Column list: `SELECT_TURN_SQL` / the `SELECT_TURNS_*` statements.
pub(super) fn decode_turn(row: &PgRow) -> Result<SwarmTurnRow, StoreError> {
    Ok(SwarmTurnRow {
        logical_id: text(row, "logical_id")?,
        turn_id: text(row, "turn_id")?,
        status: parse_turn_status(&text(row, "status")?)?,
        created_at: int(row, "created_at")?,
        started_at: optional_int(row, "started_at")?,
        completed_at: optional_int(row, "completed_at")?,
        error: optional_json(row, "error_json")?,
        usage: optional_json(row, "usage_json")?,
        subagent_id: optional_text(row, "subagent_id")?,
    })
}

/// Column list: `SELECT_SESSION_USAGE_TURNS_SQL` /
/// `SELECT_TURN_USAGE_WINDOW_SQL`.
pub(super) fn decode_turn_usage(row: &PgRow) -> Result<SwarmTurnUsageRow, StoreError> {
    Ok(SwarmTurnUsageRow {
        logical_id: text(row, "logical_id")?,
        turn_id: text(row, "turn_id")?,
        created_at: int(row, "created_at")?,
        usage: optional_json(row, "usage_json")?,
        model: optional_text(row, "model")?,
        last_active_at: text(row, "last_activity_at")?,
    })
}

// ── parameter binding ───────────────────────────────────────────────────────
//
// Each binder follows the column order of the matching statement in
// `sql.rs`; [`super::sql_test`] pins the placeholder count, and the integration
// tests pin the round trip. Values are bound owned so a statement may outlive
// the caller's borrows.

pub(super) fn bind_session<'q>(
    query: Query<'q, Postgres, PgArguments>,
    row: &SwarmSessionRow,
) -> Result<Query<'q, Postgres, PgArguments>, StoreError> {
    let mcp = mcp_json(&row.mcp_tools)?;
    let environment = encode(&row.environment, "environment")?;
    let metadata = metadata_json(&row.metadata)?;
    let prompt_cache = prompt_cache_json(row.prompt_cache_options)?;
    Ok(query
        .bind(row.logical_id.clone())
        .bind(row.root_session_id.clone())
        .bind(row.actor_key.clone())
        .bind(row.cwd.clone())
        .bind(row.state.as_str())
        .bind(row.model.clone())
        .bind(row.instructions.clone())
        .bind(mcp)
        .bind(environment)
        .bind(metadata)
        .bind(prompt_cache)
        .bind(row.created_at.clone())
        .bind(row.last_activity_at.clone())
        .bind(seq_param(row.last_item_seq, "last item seq")?))
}

pub(super) fn bind_session_update<'q>(
    query: Query<'q, Postgres, PgArguments>,
    update: &SwarmSessionUpdate,
) -> Result<Query<'q, Postgres, PgArguments>, StoreError> {
    Ok(query
        .bind(update.logical_id.clone())
        .bind(update.state.as_str())
        .bind(seq_param(update.last_item_seq, "last item seq")?)
        .bind(update.last_activity_at.clone()))
}

pub(super) fn bind_item<'q>(
    query: Query<'q, Postgres, PgArguments>,
    row: &SwarmItemRow,
) -> Result<Query<'q, Postgres, PgArguments>, StoreError> {
    Ok(query
        .bind(row.logical_id.clone())
        .bind(seq_param(row.seq, "seq")?)
        .bind(row.item_id.clone())
        .bind(row.kind.as_str())
        .bind(encode(&row.source, "item source")?)
        .bind(encode(&row.payload, "item payload")?)
        .bind(row.created_at.clone())
        .bind(row.truncated))
}

pub(super) fn bind_input<'q>(
    query: Query<'q, Postgres, PgArguments>,
    row: &SwarmInputRow,
) -> Query<'q, Postgres, PgArguments> {
    query
        .bind(row.logical_id.clone())
        .bind(row.input_id.clone())
        .bind(row.fingerprint.clone())
        .bind(row.state.as_str())
        .bind(row.created_at.clone())
}

pub(super) fn bind_binding<'q>(
    query: Query<'q, Postgres, PgArguments>,
    row: &SwarmBindingRow,
) -> Query<'q, Postgres, PgArguments> {
    query
        .bind(row.logical_env_id.clone())
        .bind(row.logical_id.clone())
        .bind(row.sandbox_id.clone())
        .bind(row.workspace_path.clone())
        .bind(row.sandbox_root.clone())
        .bind(row.settings_profile.clone())
        .bind(row.token_file.clone())
        .bind(row.last_run_id.clone())
        .bind(row.last_used_at.clone())
}

pub(super) fn bind_turn<'q>(
    query: Query<'q, Postgres, PgArguments>,
    row: &SwarmTurnRow,
) -> Result<Query<'q, Postgres, PgArguments>, StoreError> {
    let error = row
        .error
        .as_ref()
        .map(|error| encode(error, "turn error"))
        .transpose()?;
    let usage = row
        .usage
        .as_ref()
        .map(|usage| encode(usage, "turn usage"))
        .transpose()?;
    Ok(query
        .bind(row.logical_id.clone())
        .bind(row.turn_id.clone())
        .bind(row.status.as_str())
        .bind(row.created_at)
        .bind(row.started_at)
        .bind(row.completed_at)
        .bind(error)
        .bind(usage)
        .bind(row.subagent_id.clone()))
}

pub(super) fn bind_turn_usage<'q>(
    query: Query<'q, Postgres, PgArguments>,
    logical_id: &str,
    turn_id: &str,
    created_at: i64,
    started_at: Option<i64>,
    usage: &Value,
) -> Result<Query<'q, Postgres, PgArguments>, StoreError> {
    Ok(query
        .bind(logical_id.to_owned())
        .bind(turn_id.to_owned())
        .bind(created_at)
        .bind(started_at)
        .bind(encode(usage, "turn usage")?))
}

/// Bind the five values [`super::sql::PATCH_SESSION_SQL`] consumes after the id.
///
/// `Some` replaces the stored value, `None` keeps it; the two clearable fields
/// bind `(true, None)`.
pub(super) fn patch_session_params(
    patch: &SwarmSessionPatch,
) -> Result<PatchSessionParams, StoreError> {
    let model = patch.model.clone();
    let mcp = match &patch.mcp_tools {
        Some(tools) => mcp_json(tools)?,
        None => None,
    };
    let metadata = match &patch.metadata {
        Some(metadata) => metadata_json(metadata)?,
        None => None,
    };
    let prompt_cache = match patch.prompt_cache_options {
        Some(options) => prompt_cache_json(options)?,
        None => None,
    };
    Ok(PatchSessionParams {
        logical_id: patch.logical_id.clone(),
        set_model: model.is_some(),
        model,
        set_mcp: patch.mcp_tools.is_some(),
        mcp,
        set_metadata: patch.metadata.is_some(),
        metadata,
        set_prompt_cache: patch.prompt_cache_options.is_some(),
        prompt_cache,
        set_last_activity: patch.last_activity_at.is_some(),
        last_activity_at: patch.last_activity_at.clone(),
    })
}

/// Bound values of one `sessions.update` write, in statement order.
pub(super) struct PatchSessionParams {
    pub(super) logical_id: String,
    pub(super) set_model: bool,
    pub(super) model: Option<String>,
    pub(super) set_mcp: bool,
    pub(super) mcp: Option<String>,
    pub(super) set_metadata: bool,
    pub(super) metadata: Option<String>,
    pub(super) set_prompt_cache: bool,
    pub(super) prompt_cache: Option<String>,
    pub(super) set_last_activity: bool,
    pub(super) last_activity_at: Option<String>,
}

impl PatchSessionParams {
    pub(super) fn bind<'q>(
        self,
        query: Query<'q, Postgres, PgArguments>,
    ) -> Query<'q, Postgres, PgArguments> {
        query
            .bind(self.logical_id)
            .bind(self.set_model)
            .bind(self.model)
            .bind(self.set_mcp)
            .bind(self.mcp)
            .bind(self.set_metadata)
            .bind(self.metadata)
            .bind(self.set_prompt_cache)
            .bind(self.prompt_cache)
            .bind(self.set_last_activity)
            .bind(self.last_activity_at)
    }
}

#[cfg(test)]
#[path = "codec_test.rs"]
mod tests;
