//! Independent Swarm state database (S6c1a).
//!
//! User decision (S6c1a): Swarm facts live in **their own** Loom logical
//! database. The Peri session store keeps its existing `peri_store_meta` /
//! canonical schema; this module never probes, initializes or migrates it. It
//! opens the `PERI_SWARM_STATE_STORE` locator directly with `turso_serverless`,
//! owns its DDL and records its own schema version in `swarm_meta`.
//!
//! ```text
//! Peri session store  (peri_store_meta, canonical shape)  <- Resources / AcpChild
//! Swarm state store   (swarm_meta, swarm_* tables)        <- this module
//! ```
//!
//! Opening is fail closed: a missing credential, an unreachable locator or an
//! unknown `swarm_meta.version` aborts the process instead of falling back to a
//! local file or silently reusing the session store. A known version with
//! missing objects is repaired by re-running the idempotent DDL; the version row
//! is never rewritten.
//!
//! ## Statement shape
//!
//! Every statement is a `&'static str` with positional `?` binding; dynamic
//! values (ids, timestamps, JSON text) only travel as bound parameters. Writes
//! are single auto-commit statements, so a failure is observable and is never
//! retried or renumbered here. `Constraint` is reported as
//! [`StoreError::Conflict`] and left for the caller to resolve by reading again.
//!
//! A transport failure or an expired request budget is `Unavailable` with an
//! explicit "outcome is unknown" note: it is not evidence that the write did not
//! apply, and it must not be retried blindly.
//!
//! ## Field encoding
//!
//! The byte layout is the one fixed in `.peri/plans/S6a-report.md` §1.2:
//!
//! | [`crate::store`] row | column | encoding |
//! | --- | --- | --- |
//! | `state` / `kind` / `InputState` | `state` / `kind` / `state` | `as_str()` text |
//! | `environment` | `environment_json` | serde JSON text |
//! | `source` | `source` | serde JSON text |
//! | `payload` | `payload_json` | serde JSON text |
//! | `mcp_tools` | `mcp_json` | canonical [`McpToolSpec`] JSON array, `NULL` = none |
//! | `truncated` | `truncated` | `INTEGER` 0/1 |
//! | `seq` / `last_item_seq` | `seq` / `last_item_seq` | `INTEGER` (i64 range) |
//!
//! `item_id` is deliberately **not** unique: the reader reuses one stable id for
//! a delta row and its later complete row (S6b3). `(logical_id, seq)` is the
//! primary key and `swarm_items_by_id` is a plain index.

use std::collections::BTreeMap;
use std::future::Future;
use std::time::Duration;

use async_trait::async_trait;
use turso_serverless::{
    BatchStatement, Builder, Connection, Error as SdkError, TransactionBehavior, Value,
};
use url::Url;

use super::{
    grouped_item_page, InputPut, StoreError, SwarmBindingRow, SwarmInputRow, SwarmItemPage,
    SwarmItemRow, SwarmSessionPatch, SwarmSessionRow, SwarmSessionUpdate, SwarmStore, SwarmTurnRow,
};
use crate::domain::{
    EnvironmentRef, InputState, ItemKind, ItemOrder, ItemSource, McpToolSpec, PromptCacheOptions,
    SessionState, SessionsWindow, TurnStatus, TurnsWindow,
};

/// Version recorded in `swarm_meta.version` by this build.
///
/// Bump only together with an explicit, additive migration. An unknown version
/// fails closed; this store never upgrades or rewrites an existing schema.
pub const SWARM_STATE_SCHEMA_VERSION: i64 = 1;

/// Per-request budget. The SDK exposes no client timeout of its own.
const REQUEST_BUDGET: Duration = Duration::from_secs(20);

// ── owned schema ────────────────────────────────────────────────────────────

/// One row: the schema version this database was created/verified with.
const CREATE_META_TABLE_SQL: &str =
    "CREATE TABLE IF NOT EXISTS swarm_meta (version INTEGER NOT NULL)";

const CREATE_TABLE_SQL: [&str; 5] = [
    "CREATE TABLE IF NOT EXISTS swarm_sessions (
        logical_id       TEXT PRIMARY KEY,
        root_session_id  TEXT NOT NULL UNIQUE,
        actor_key        TEXT NOT NULL,
        cwd              TEXT NOT NULL,
        state            TEXT NOT NULL,
        model            TEXT,
        instructions     TEXT,
        mcp_json         TEXT,
        environment_json TEXT NOT NULL,
        metadata_json    TEXT,
        prompt_cache_options_json TEXT,
        created_at       TEXT NOT NULL,
        last_activity_at TEXT NOT NULL,
        last_item_seq    INTEGER NOT NULL DEFAULT 0
    )",
    "CREATE TABLE IF NOT EXISTS swarm_items (
        logical_id   TEXT NOT NULL,
        seq          INTEGER NOT NULL,
        item_id      TEXT NOT NULL,
        kind         TEXT NOT NULL,
        source       TEXT NOT NULL,
        payload_json TEXT NOT NULL,
        created_at   TEXT NOT NULL,
        truncated    INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (logical_id, seq)
    )",
    "CREATE TABLE IF NOT EXISTS swarm_inputs (
        logical_id   TEXT NOT NULL,
        input_id     TEXT NOT NULL,
        fingerprint  TEXT NOT NULL,
        state        TEXT NOT NULL,
        created_at   TEXT NOT NULL,
        PRIMARY KEY (logical_id, input_id)
    )",
    "CREATE TABLE IF NOT EXISTS swarm_bindings (
        logical_env_id   TEXT PRIMARY KEY,
        logical_id       TEXT NOT NULL,
        sandbox_id       TEXT NOT NULL,
        workspace_path   TEXT NOT NULL,
        sandbox_root     TEXT NOT NULL,
        settings_profile TEXT,
        token_file       TEXT,
        last_run_id      TEXT,
        last_used_at     TEXT NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS swarm_turns (
        logical_id   TEXT NOT NULL,
        turn_id      TEXT NOT NULL,
        status       TEXT NOT NULL,
        created_at   INTEGER NOT NULL,
        started_at   INTEGER,
        completed_at INTEGER,
        error_json   TEXT,
        usage_json   TEXT,
        subagent_id  TEXT,
        PRIMARY KEY (logical_id, turn_id)
    )",
];

/// Plain index: `item_id` repeats across delta/complete rows of one turn.
const CREATE_INDEX_SQL: [&str; 2] = [
    "CREATE INDEX IF NOT EXISTS swarm_items_by_id ON swarm_items (logical_id, item_id)",
    "CREATE INDEX IF NOT EXISTS swarm_turns_by_created ON swarm_turns (logical_id, created_at, turn_id)",
];

/// Additive columns introduced after `swarm_meta.version=1`.
///
/// `CREATE TABLE IF NOT EXISTS` cannot add a column to an existing table, so a
/// database opened by an older build is repaired with `ALTER TABLE ... ADD
/// COLUMN` before the ordinary idempotent DDL runs. The version row is not
/// bumped: the change is additive, old readers keep working, and the new
/// columns are nullable.
const MIGRATION_COLUMNS: [(&str, &str); 2] = [
    (
        "metadata_json",
        "ALTER TABLE swarm_sessions ADD COLUMN metadata_json TEXT",
    ),
    (
        "prompt_cache_options_json",
        "ALTER TABLE swarm_sessions ADD COLUMN prompt_cache_options_json TEXT",
    ),
];

/// One `PRAGMA table_info` target per migrated table. Each statement is
/// static; `apply_ddl` only runs the matching `ALTER` when its column is
/// missing.
const SESSIONS_TABLE_INFO_SQL: &str = "PRAGMA table_info(swarm_sessions)";

/// Pin the version exactly once. `WHERE NOT EXISTS` keeps concurrent first
/// opens from inserting a second row.
const PIN_VERSION_SQL: &str =
    "INSERT INTO swarm_meta (version) SELECT ?1 WHERE NOT EXISTS (SELECT 1 FROM swarm_meta)";

const META_TABLE_EXISTS_SQL: &str =
    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'swarm_meta'";

/// Adopted-schema guard: a database that already has Swarm tables but no
/// `swarm_meta` is not ours to initialize (it may be a Peri session store).
const SESSIONS_TABLE_EXISTS_SQL: &str =
    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'swarm_sessions'";

const SELECT_VERSION_SQL: &str = "SELECT version FROM swarm_meta";

// ── statements ──────────────────────────────────────────────────────────────

/// Same identity rule as the S3 in-memory fake: only `root_session_id` and
/// `cwd` may repeat for one `logical_id`, otherwise the update affects 0 rows
/// and the caller sees a conflict. A second `logical_id` claiming the same
/// `root_session_id` fails on the unique constraint, also a conflict.
macro_rules! insert_session {
    ($conflict:literal) => {
        concat!(
            "INSERT INTO swarm_sessions (
    logical_id, root_session_id, actor_key, cwd, state, model, instructions,
    mcp_json, environment_json, metadata_json, prompt_cache_options_json,
    created_at, last_activity_at, last_item_seq)
SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14",
            $conflict
        )
    };
}
const INSERT_SESSION_SQL: &str = insert_session!("");
// Immediately follows the guarded binding UPDATE on the same SQLite connection.
const INSERT_ATTACHED_SESSION_SQL: &str = insert_session!(" WHERE changes() = 1");
const UPSERT_SESSION_SQL: &str = insert_session!(
    "
ON CONFLICT(logical_id) DO UPDATE SET
    actor_key = excluded.actor_key,
    state = excluded.state,
    model = excluded.model,
    instructions = excluded.instructions,
    mcp_json = excluded.mcp_json,
    environment_json = excluded.environment_json,
    metadata_json = excluded.metadata_json,
    prompt_cache_options_json = excluded.prompt_cache_options_json,
    last_activity_at = excluded.last_activity_at,
    last_item_seq = excluded.last_item_seq
WHERE swarm_sessions.root_session_id = excluded.root_session_id
  AND swarm_sessions.cwd = excluded.cwd"
);

const SELECT_SESSION_SQL: &str = "SELECT logical_id, root_session_id, actor_key, cwd, state,
    model, instructions, mcp_json, environment_json, metadata_json,
    prompt_cache_options_json, created_at, last_activity_at, last_item_seq
    FROM swarm_sessions WHERE logical_id = ?1";

const UPDATE_SESSION_SQL: &str = "UPDATE swarm_sessions
    SET state = ?2, last_item_seq = ?3, last_activity_at = ?4 WHERE logical_id = ?1";

/// `sessions.update`. Each `?n IS NULL` keeps the stored value; booleans decide
/// the two fields whose official spelling can be either "replace" or "clear".
const PATCH_SESSION_SQL: &str = "UPDATE swarm_sessions SET
    model = CASE WHEN ?2 THEN ?3 ELSE model END,
    mcp_json = CASE WHEN ?4 THEN ?5 ELSE mcp_json END,
    metadata_json = CASE WHEN ?6 THEN ?7 ELSE metadata_json END,
    prompt_cache_options_json = CASE WHEN ?8 THEN ?9 ELSE prompt_cache_options_json END,
    last_activity_at = CASE WHEN ?10 THEN ?11 ELSE last_activity_at END
    WHERE logical_id = ?1";

/// `sessions.list` base statement; metadata predicates are inserted by
/// [`session_list_sql`]. `?1` is the exclusive id cursor (`''` = none), `?2` is
/// `limit + 1`.
const SESSION_LIST_ASC_PREFIX: &str = "SELECT logical_id, root_session_id, actor_key, cwd, state,
    model, instructions, mcp_json, environment_json, metadata_json,
    prompt_cache_options_json, created_at, last_activity_at, last_item_seq
    FROM swarm_sessions s
    WHERE (?1 = '' OR s.logical_id > ?1)";

const SESSION_LIST_DESC_PREFIX: &str = "SELECT logical_id, root_session_id, actor_key, cwd, state,
    model, instructions, mcp_json, environment_json, metadata_json,
    prompt_cache_options_json, created_at, last_activity_at, last_item_seq
    FROM swarm_sessions s
    WHERE (?1 = '' OR s.logical_id < ?1)";

const SESSION_LIST_ASC_SUFFIX: &str = " ORDER BY s.logical_id ASC LIMIT ?2";
const SESSION_LIST_DESC_SUFFIX: &str = " ORDER BY s.logical_id DESC LIMIT ?2";

const DELETE_SESSION_SQL: &str = "DELETE FROM swarm_sessions WHERE logical_id = ?1";
const DELETE_ITEMS_SQL: &str = "DELETE FROM swarm_items WHERE logical_id = ?1";
const DELETE_INPUTS_SQL: &str = "DELETE FROM swarm_inputs WHERE logical_id = ?1";
const DELETE_TURNS_SQL: &str = "DELETE FROM swarm_turns WHERE logical_id = ?1";
// The binding records the most recent session to attach. Before deleting that
// session, give another user of the same environment the binding in this transaction.
const REASSIGN_BINDINGS_SQL: &str = "UPDATE swarm_bindings SET logical_id = COALESCE((
    SELECT s.logical_id FROM swarm_sessions s
    WHERE s.logical_id <> ?1 AND json_extract(s.environment_json, '$.type') = 'openai_hosted'
      AND json_extract(s.environment_json, '$.id') = swarm_bindings.logical_env_id
    ORDER BY s.logical_id LIMIT 1
), logical_id) WHERE logical_id = ?1";
const DELETE_BINDINGS_SQL: &str = "DELETE FROM swarm_bindings WHERE logical_id = ?1";

const UPSERT_TURN_SQL: &str = "INSERT INTO swarm_turns
    (logical_id, turn_id, status, created_at, started_at, completed_at,
     error_json, usage_json, subagent_id)
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
    ON CONFLICT(logical_id, turn_id) DO UPDATE SET
        status = excluded.status,
        started_at = excluded.started_at,
        completed_at = excluded.completed_at,
        error_json = excluded.error_json,
        usage_json = excluded.usage_json,
        subagent_id = excluded.subagent_id";

const SELECT_TURN_SQL: &str = "SELECT logical_id, turn_id, status, created_at, started_at,
    completed_at, error_json, usage_json, subagent_id
    FROM swarm_turns WHERE logical_id = ?1 AND turn_id = ?2";

const SELECT_TURNS_ASC_SQL: &str = "SELECT logical_id, turn_id, status, created_at,
    started_at, completed_at, error_json, usage_json, subagent_id
    FROM swarm_turns WHERE logical_id = ?1
    ORDER BY created_at ASC, turn_id ASC LIMIT ?2";

const SELECT_TURNS_ASC_AFTER_SQL: &str = "SELECT logical_id, turn_id, status, created_at,
    started_at, completed_at, error_json, usage_json, subagent_id
    FROM swarm_turns WHERE logical_id = ?1 AND
    (created_at > ?2 OR (created_at = ?2 AND turn_id > ?3))
    ORDER BY created_at ASC, turn_id ASC LIMIT ?4";

const SELECT_TURNS_DESC_SQL: &str = "SELECT logical_id, turn_id, status, created_at,
    started_at, completed_at, error_json, usage_json, subagent_id
    FROM swarm_turns WHERE logical_id = ?1
    ORDER BY created_at DESC, turn_id DESC LIMIT ?2";

const SELECT_TURNS_DESC_AFTER_SQL: &str = "SELECT logical_id, turn_id, status, created_at,
    started_at, completed_at, error_json, usage_json, subagent_id
    FROM swarm_turns WHERE logical_id = ?1 AND
    (created_at < ?2 OR (created_at = ?2 AND turn_id < ?3))
    ORDER BY created_at DESC, turn_id DESC LIMIT ?4";

const CLOSE_OPEN_TURNS_SQL: &str = "UPDATE swarm_turns
    SET status = ?2, completed_at = ?3, error_json = ?4
    WHERE logical_id = ?1 AND status IN ('queued', 'in_progress', 'waiting')";

/// Crash-recovery probe: does the session still own a non-terminal turn row?
/// Predicate matches [`TurnStatus::is_open`].
const HAS_OPEN_TURN_SQL: &str = "SELECT EXISTS(SELECT 1 FROM swarm_turns
    WHERE logical_id = ?1 AND status IN ('queued', 'in_progress', 'waiting'))";

const INSERT_ITEM_SQL: &str = "INSERT INTO swarm_items
    (logical_id, seq, item_id, kind, source, payload_json, created_at, truncated)
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)";

const SELECT_ITEMS_AFTER_SQL: &str = "SELECT logical_id, seq, item_id, kind, source,
    payload_json, created_at, truncated FROM swarm_items
    WHERE logical_id = ?1 AND seq > ?2 ORDER BY seq ASC LIMIT ?3";

const SELECT_ITEMS_BEFORE_SQL: &str = "SELECT logical_id, seq, item_id, kind, source,
    payload_json, created_at, truncated FROM swarm_items
    WHERE logical_id = ?1 AND seq < ?2 ORDER BY seq DESC LIMIT ?3";

const SELECT_ITEMS_TAIL_SQL: &str = "SELECT logical_id, seq, item_id, kind, source,
    payload_json, created_at, truncated FROM swarm_items
    WHERE logical_id = ?1 ORDER BY seq DESC LIMIT ?2";

const SELECT_ITEM_BY_ID_SQL: &str = "SELECT logical_id, seq, item_id, kind, source,
    payload_json, created_at, truncated FROM swarm_items
    WHERE logical_id = ?1 AND item_id = ?2 ORDER BY seq DESC LIMIT 1";

// Item-grouped pagination (`design §4.2`). `page` selects items by their
// greatest `seq` — never by rows — so a cursor resolved from an item id cannot
// land inside the same item and repeat it. The outer query then returns every
// row of the selected items; the caller derives `data` from each item's last
// row. The join carries `logical_id` so a caller-supplied input id that repeats
// across sessions can never pull another session's rows.
const SELECT_ITEM_PAGE_ASC_SQL: &str = "WITH page AS (
    SELECT logical_id, item_id, MAX(seq) AS last_seq FROM swarm_items
    WHERE logical_id = ?1 GROUP BY logical_id, item_id HAVING MAX(seq) > ?2
    ORDER BY last_seq ASC LIMIT ?3)
  SELECT i.logical_id, i.seq, i.item_id, i.kind, i.source, i.payload_json,
    i.created_at, i.truncated
  FROM swarm_items i
  JOIN page p ON i.logical_id = p.logical_id AND i.item_id = p.item_id
  ORDER BY i.seq ASC";

const SELECT_ITEM_PAGE_DESC_SQL: &str = "WITH page AS (
    SELECT logical_id, item_id, MAX(seq) AS last_seq FROM swarm_items
    WHERE logical_id = ?1 GROUP BY logical_id, item_id HAVING MAX(seq) < ?2
    ORDER BY last_seq DESC LIMIT ?3)
  SELECT i.logical_id, i.seq, i.item_id, i.kind, i.source, i.payload_json,
    i.created_at, i.truncated
  FROM swarm_items i
  JOIN page p ON i.logical_id = p.logical_id AND i.item_id = p.item_id
  ORDER BY i.seq DESC";

const SELECT_ITEM_PAGE_TAIL_SQL: &str = "WITH page AS (
    SELECT logical_id, item_id, MAX(seq) AS last_seq FROM swarm_items
    WHERE logical_id = ?1 GROUP BY logical_id, item_id
    ORDER BY last_seq DESC LIMIT ?2)
  SELECT i.logical_id, i.seq, i.item_id, i.kind, i.source, i.payload_json,
    i.created_at, i.truncated
  FROM swarm_items i
  JOIN page p ON i.logical_id = p.logical_id AND i.item_id = p.item_id
  ORDER BY i.seq DESC";

const SELECT_LAST_SEQ_SQL: &str =
    "SELECT COALESCE(MAX(seq), 0) FROM swarm_items WHERE logical_id = ?1";

const INSERT_INPUT_SQL: &str = "INSERT INTO swarm_inputs
    (logical_id, input_id, fingerprint, state, created_at)
    VALUES (?1, ?2, ?3, ?4, ?5)
    ON CONFLICT(logical_id, input_id) DO NOTHING";

const SELECT_INPUT_SQL: &str = "SELECT logical_id, input_id, fingerprint, state, created_at
    FROM swarm_inputs WHERE logical_id = ?1 AND input_id = ?2";

const UPSERT_BINDING_SQL: &str = "INSERT INTO swarm_bindings
    (logical_env_id, logical_id, sandbox_id, workspace_path, sandbox_root,
     settings_profile, token_file, last_run_id, last_used_at)
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
    ON CONFLICT(logical_env_id) DO UPDATE SET
        logical_id = excluded.logical_id,
        sandbox_id = excluded.sandbox_id,
        workspace_path = excluded.workspace_path,
        sandbox_root = excluded.sandbox_root,
        settings_profile = excluded.settings_profile,
        token_file = excluded.token_file,
        last_run_id = excluded.last_run_id,
        last_used_at = excluded.last_used_at";

const ATTACH_BINDING_SQL: &str = "UPDATE swarm_bindings
    SET logical_id = ?2, last_used_at = ?9, last_run_id = COALESCE(?8, last_run_id)
    WHERE logical_env_id = ?1 AND sandbox_id = ?3 AND workspace_path = ?4";
const SELECT_BINDING_SQL: &str = "SELECT logical_env_id, logical_id, sandbox_id, workspace_path,
    sandbox_root, settings_profile, token_file, last_run_id, last_used_at
    FROM swarm_bindings WHERE logical_env_id = ?1";

// ── opening ─────────────────────────────────────────────────────────────────

/// Where the Swarm state database is and which variable holds its token.
///
/// The locator must not carry credentials (the bootstrap validator rejects
/// them); the token is resolved by name and never stored in this config.
#[derive(Clone, Debug)]
pub struct SwarmStateStoreConfig {
    pub locator: Url,
    /// **Name** of the environment variable holding the bearer token.
    pub credential_env: String,
}

/// Why the Swarm state database could not be opened.
#[derive(Debug, thiserror::Error)]
pub enum SwarmStateError {
    #[error("swarm state store credential variable {variable} is {reason}")]
    Credential {
        variable: String,
        reason: &'static str,
    },
    #[error("cannot connect to the swarm state store: {0}")]
    Connect(String),
    #[error(
        "swarm state store schema version {found} is not supported by this build (expected {expected})"
    )]
    SchemaVersion { found: i64, expected: i64 },
    #[error("swarm state store schema check failed: {0}")]
    Schema(String),
}

/// Swarm state database over one direct `turso_serverless` connection.
///
/// The connection is shared by the gateway and every actor of the process; the
/// driver serializes statements on it. Closing is the process's job.
pub struct SwarmStateStore {
    connection: Connection,
}

impl std::fmt::Debug for SwarmStateStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SwarmStateStore")
            .finish_non_exhaustive()
    }
}

impl SwarmStateStore {
    /// Resolve the credential by name and open the state database.
    ///
    /// The token value is only handed to the SDK; neither errors nor `Debug`
    /// render it.
    pub async fn open(config: &SwarmStateStoreConfig) -> Result<Self, SwarmStateError> {
        let credential =
            resolve_credential(|name| std::env::var(name).ok(), &config.credential_env)?;
        Self::connect(config.locator.as_str(), &credential).await
    }

    async fn connect(locator: &str, credential: &str) -> Result<Self, SwarmStateError> {
        let database = Builder::new_remote(locator)
            .with_auth_token(credential)
            .build()
            .await
            .map_err(|error| SwarmStateError::Connect(error.to_string()))?;
        let connection = database
            .connect()
            .map_err(|error| SwarmStateError::Connect(error.to_string()))?;
        ensure_schema(&connection).await?;
        Ok(Self { connection })
    }

    /// Close the connection. `Ok` only proves the local transport surface
    /// stopped; the driver swallows remote close errors (same fact as the
    /// session store's `Connection::close`).
    pub async fn close(&self) -> Result<(), StoreError> {
        within_budget("close", self.connection.close()).await
    }

    async fn execute(
        &self,
        operation: &'static str,
        sql: &str,
        params: Vec<Value>,
    ) -> Result<u64, StoreError> {
        within_budget(operation, self.connection.execute(sql, params)).await
    }

    async fn query_rows(
        &self,
        operation: &'static str,
        sql: &str,
        params: Vec<Value>,
    ) -> Result<Vec<Vec<Value>>, StoreError> {
        let mut rows = within_budget(operation, self.connection.query(sql, params)).await?;
        let mut collected = Vec::new();
        loop {
            match within_budget(operation, rows.next()).await? {
                Some(row) => collected.push(columns_of(&row)?),
                None => return Ok(collected),
            }
        }
    }
}

fn session_list_sql(order: ItemOrder, filters: usize) -> String {
    let (prefix, suffix) = match order {
        ItemOrder::Asc => (SESSION_LIST_ASC_PREFIX, SESSION_LIST_ASC_SUFFIX),
        ItemOrder::Desc => (SESSION_LIST_DESC_PREFIX, SESSION_LIST_DESC_SUFFIX),
    };
    let mut sql = String::from(prefix);
    for index in 0..filters {
        // `?1`/`?2` are the cursor and limit; each metadata pair binds two
        // parameters. `json_each` compares the stored key and value as text, so
        // no caller string becomes SQL or a JSON path.
        let key_param = 3 + index * 2;
        let value_param = key_param + 1;
        sql.push_str(&format!(
            " AND EXISTS (SELECT 1 FROM json_each(s.metadata_json) m \
             WHERE m.key = ?{key_param} AND m.value = ?{value_param})"
        ));
    }
    sql.push_str(suffix);
    sql
}

fn resolve_credential(
    lookup: impl Fn(&str) -> Option<String>,
    variable: &str,
) -> Result<String, SwarmStateError> {
    match lookup(variable) {
        Some(value) if value.trim().is_empty() => Err(SwarmStateError::Credential {
            variable: variable.to_owned(),
            reason: "set but empty",
        }),
        Some(value) => Ok(value),
        None => Err(SwarmStateError::Credential {
            variable: variable.to_owned(),
            reason: "not set",
        }),
    }
}

// ── schema check ────────────────────────────────────────────────────────────

/// Verify (and, for a fresh database, create) the owned schema.
///
/// Order matters: an existing `swarm_meta` is read **before** any DDL, so a
/// database written by a newer build is rejected without being touched.
async fn ensure_schema(connection: &Connection) -> Result<(), SwarmStateError> {
    let has_meta = table_exists(connection, META_TABLE_EXISTS_SQL).await?;
    if !has_meta {
        if table_exists(connection, SESSIONS_TABLE_EXISTS_SQL).await? {
            return Err(SwarmStateError::Schema(
                "database already has swarm_sessions without swarm_meta; refusing to adopt an existing schema"
                    .to_owned(),
            ));
        }
        apply_ddl(connection, true).await?;
    }
    let found = read_version(connection).await?;
    require_supported_version(found)?;
    if has_meta {
        // Same version: re-apply the idempotent DDL to repair missing objects.
        // The version row is not rewritten by this path.
        apply_ddl(connection, false).await?;
    }
    Ok(())
}

/// Fail closed on any version this build does not own. There is no upgrade or
/// downgrade path here: a newer Swarm schema must be served by its own build.
fn require_supported_version(found: i64) -> Result<(), SwarmStateError> {
    if found == SWARM_STATE_SCHEMA_VERSION {
        return Ok(());
    }
    Err(SwarmStateError::SchemaVersion {
        found,
        expected: SWARM_STATE_SCHEMA_VERSION,
    })
}

async fn table_exists(connection: &Connection, sql: &'static str) -> Result<bool, SwarmStateError> {
    let rows = query_schema_rows(connection, sql).await?;
    match rows.as_slice() {
        [row] => match row.as_slice() {
            [Value::Integer(count)] => Ok(*count > 0),
            _ => Err(SwarmStateError::Schema(
                "sqlite_master count is not a single integer".to_owned(),
            )),
        },
        _ => Err(SwarmStateError::Schema(
            "sqlite_master lookup did not return one row".to_owned(),
        )),
    }
}

async fn read_version(connection: &Connection) -> Result<i64, SwarmStateError> {
    let rows = query_schema_rows(connection, SELECT_VERSION_SQL).await?;
    match rows.as_slice() {
        [row] => match row.as_slice() {
            [Value::Integer(version)] => Ok(*version),
            _ => Err(SwarmStateError::Schema(
                "swarm_meta.version is not a single integer".to_owned(),
            )),
        },
        [] => Err(SwarmStateError::Schema(
            "swarm_meta has no version row".to_owned(),
        )),
        rows => Err(SwarmStateError::Schema(format!(
            "swarm_meta has {} rows; exactly one is required",
            rows.len()
        ))),
    }
}

async fn query_schema_rows(
    connection: &Connection,
    sql: &'static str,
) -> Result<Vec<Vec<Value>>, SwarmStateError> {
    let mut rows = within_budget("schema", connection.query(sql, ()))
        .await
        .map_err(|error| SwarmStateError::Schema(error.to_string()))?;
    let mut collected = Vec::new();
    loop {
        match within_budget("schema", rows.next())
            .await
            .map_err(|error| SwarmStateError::Schema(error.to_string()))?
        {
            Some(row) => collected.push(
                columns_of(&row).map_err(|error| SwarmStateError::Schema(error.to_string()))?,
            ),
            None => return Ok(collected),
        }
    }
}

/// `ALTER TABLE` statements needed for a table with `existing` columns.
///
/// Pure so the legacy→current migration decision is testable without a Loom
/// server. An empty result means the table already carries every new column;
/// a fresh database takes the `CREATE TABLE` path and passes an empty slice
/// too (the table does not exist yet, so `ALTER` must not run).
fn migration_statements(existing: &[String]) -> Vec<&'static str> {
    if existing.is_empty() {
        return Vec::new();
    }
    MIGRATION_COLUMNS
        .iter()
        .filter(|(column, _)| !existing.iter().any(|name| name == column))
        .map(|(_, sql)| *sql)
        .collect()
}

/// Column names reported by `PRAGMA table_info`. `None` means the table does
/// not exist yet.
async fn session_columns(connection: &Connection) -> Result<Option<Vec<String>>, SwarmStateError> {
    let rows = query_schema_rows(connection, SESSIONS_TABLE_INFO_SQL).await?;
    if rows.is_empty() {
        return Ok(None);
    }
    let mut columns = Vec::with_capacity(rows.len());
    for row in rows {
        match row.get(1) {
            Some(Value::Text(name)) => columns.push(name.clone()),
            _ => {
                return Err(SwarmStateError::Schema(
                    "PRAGMA table_info did not return a text column name".to_owned(),
                ));
            }
        }
    }
    Ok(Some(columns))
}

/// Create the owned objects in one managed transaction. `pin_version` also
/// records [`SWARM_STATE_SCHEMA_VERSION`] when no row exists yet.
///
/// `PRAGMA table_info` runs before the transaction, so two replicas opening the
/// same legacy database can both decide a column is missing. The loser's
/// `ALTER TABLE ... ADD COLUMN` fails with `duplicate column`; that is a lost
/// migration race, not a schema error. When a re-read confirms every migrated
/// column is now present, the batch is retried once (it is idempotent, and the
/// retry also pins the version row a concurrent fresh open may have raced).
async fn apply_ddl(connection: &Connection, pin_version: bool) -> Result<(), SwarmStateError> {
    match apply_ddl_once(connection, pin_version).await {
        Ok(()) => Ok(()),
        Err(error) => {
            // A failed `PRAGMA` leaves the decision at `Propagate`: the
            // original failure is what the caller must see.
            let existing = session_columns(connection).await.ok().flatten();
            match migration_retry_decision(&error, existing.as_deref()) {
                MigrationRetry::Propagate => Err(error),
                MigrationRetry::Retry => match apply_ddl_once(connection, pin_version).await {
                    Ok(()) => Ok(()),
                    Err(retry_error) => {
                        tracing::warn!(
                            error = %retry_error,
                            "swarm schema retry after a duplicate-column race failed"
                        );
                        Err(error)
                    }
                },
            }
        }
    }
}

async fn apply_ddl_once(connection: &Connection, pin_version: bool) -> Result<(), SwarmStateError> {
    let existing = session_columns(connection).await?;
    let mut statements = Vec::with_capacity(4 + CREATE_TABLE_SQL.len() + CREATE_INDEX_SQL.len());
    statements.push(batch_statement(CREATE_META_TABLE_SQL, Vec::new())?);
    for sql in CREATE_TABLE_SQL {
        statements.push(batch_statement(sql, Vec::new())?);
    }
    // Existing table: add the post-v1 nullable columns. The decision is made
    // from `PRAGMA table_info` before the batch, so a second open adds nothing.
    if let Some(existing) = existing.as_deref() {
        for sql in migration_statements(existing) {
            statements.push(batch_statement(sql, Vec::new())?);
        }
    }
    for sql in CREATE_INDEX_SQL {
        statements.push(batch_statement(sql, Vec::new())?);
    }
    if pin_version {
        statements.push(batch_statement(
            PIN_VERSION_SQL,
            vec![Value::Integer(SWARM_STATE_SCHEMA_VERSION)],
        )?);
    }
    within_budget(
        "schema",
        connection.transactional_batch(statements, TransactionBehavior::Immediate),
    )
    .await
    .map_err(|error| SwarmStateError::Schema(error.to_string()))?;
    Ok(())
}

fn batch_statement(
    sql: &'static str,
    params: Vec<Value>,
) -> Result<BatchStatement, SwarmStateError> {
    BatchStatement::new(sql, params)
        .map_err(|error| SwarmStateError::Schema(format!("cannot build schema statement: {error}")))
}

/// Outcome of a failed schema batch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MigrationRetry {
    /// A concurrent replica added the columns first; retry the idempotent batch
    /// once (this also pins the version row, which the loser did not write).
    Retry,
    /// Not a lost migration race, or the migrated columns are still missing:
    /// surface the original failure instead of retrying blindly.
    Propagate,
}

/// Decide whether a failed `apply_ddl_once` lost a migration race.
///
/// Pure so the decision is testable with a constructed error; the actual
/// retry lives in [`apply_ddl`]. Only SQLite's `duplicate column` text counts as
/// a race: every other schema failure (missing table, permission, transport)
/// must propagate unchanged.
fn migration_retry_decision(
    error: &SwarmStateError,
    existing: Option<&[String]>,
) -> MigrationRetry {
    let SwarmStateError::Schema(message) = error else {
        return MigrationRetry::Propagate;
    };
    if !message.to_ascii_lowercase().contains("duplicate column") {
        return MigrationRetry::Propagate;
    }
    match existing {
        Some(columns) if migration_statements(columns).is_empty() => MigrationRetry::Retry,
        _ => MigrationRetry::Propagate,
    }
}

// ── error classification ────────────────────────────────────────────────────

async fn within_budget<T>(
    operation: &'static str,
    future: impl Future<Output = turso_serverless::Result<T>>,
) -> Result<T, StoreError> {
    match peri_time::timeout(REQUEST_BUDGET, future).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => {
            tracing::error!(operation, error = %error, "swarm state store request failed");
            Err(classify(operation, error))
        }
        Err(_) => Err(StoreError::Unavailable(format!(
            "{operation}: swarm state store request exceeded {REQUEST_BUDGET:?}; the outcome is unknown and must not be retried blindly"
        ))),
    }
}

/// Fold the driver's vocabulary into [`StoreError`] without inventing success.
///
/// `Conflict` is the only class the caller may act on by reading again; every
/// non-constraint failure is `Unavailable` (never `NotFound`), so a corrupt or
/// unreachable database cannot be mistaken for absent data.
fn classify(operation: &'static str, error: SdkError) -> StoreError {
    match error {
        SdkError::Constraint(message) => StoreError::Conflict(format!("{operation}: {message}")),
        SdkError::Readonly(message) => StoreError::Unsupported(format!(
            "{operation}: swarm state store is read only: {message}"
        )),
        SdkError::BatchStatementFailed { index, error, .. } => match classify(operation, *error) {
            StoreError::Conflict(detail) => {
                StoreError::Conflict(format!("batch statement {index}: {detail}"))
            }
            other => other,
        },
        SdkError::BatchRollbackFailed {
            error,
            rollback_error,
        } => StoreError::Unavailable(format!(
            "{operation}: batch rollback failed after {error}: {rollback_error}; the outcome is unknown"
        )),
        other => StoreError::Unavailable(format!("{operation}: {other}")),
    }
}

fn columns_of(row: &turso_serverless::Row) -> Result<Vec<Value>, StoreError> {
    (0..row.column_count())
        .map(|index| {
            row.get_value(index)
                .map_err(|error| classify("decode", error))
        })
        .collect()
}

// ── field mapping ───────────────────────────────────────────────────────────

fn parse_session_state(raw: &str) -> Result<SessionState, StoreError> {
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

fn parse_item_kind(raw: &str) -> Result<ItemKind, StoreError> {
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

fn parse_input_state(raw: &str) -> Result<InputState, StoreError> {
    match raw {
        "accepted" => Ok(InputState::Accepted),
        "handed_off" => Ok(InputState::HandedOff),
        "failed" => Ok(InputState::Failed),
        other => Err(unreadable(format!("input state={other:?}"))),
    }
}

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

fn mcp_json(tools: &[McpToolSpec]) -> Result<Option<String>, StoreError> {
    McpToolSpec::encode_declaration(tools)
        .map_err(|error| StoreError::Unavailable(format!("cannot encode mcp tools: {error}")))
}

fn decode_mcp_json(text: Option<&str>) -> Result<Vec<McpToolSpec>, StoreError> {
    McpToolSpec::decode_declaration(text)
        .map_err(|error| StoreError::Unavailable(format!("cannot decode mcp tools: {error}")))
}

/// An empty metadata map is stored as `NULL`, the documented "no metadata".
fn metadata_json(metadata: &BTreeMap<String, String>) -> Result<Value, StoreError> {
    if metadata.is_empty() {
        return Ok(Value::Null);
    }
    encode(metadata, "metadata").map(Value::Text)
}

fn decode_metadata(text: Option<&str>) -> Result<BTreeMap<String, String>, StoreError> {
    match text {
        None => Ok(BTreeMap::new()),
        Some(text) => decode(text, "metadata"),
    }
}

fn prompt_cache_json(options: Option<PromptCacheOptions>) -> Result<Value, StoreError> {
    match options {
        None => Ok(Value::Null),
        Some(options) => encode(&options, "prompt cache options").map(Value::Text),
    }
}

fn decode_prompt_cache(text: Option<&str>) -> Result<Option<PromptCacheOptions>, StoreError> {
    text.map(|text| decode(text, "prompt cache options"))
        .transpose()
}

fn parse_turn_status(raw: &str) -> Result<TurnStatus, StoreError> {
    TurnStatus::parse(raw).ok_or_else(|| unreadable(format!("turn status={raw:?}")))
}

fn turn_params(row: &SwarmTurnRow) -> Result<Vec<Value>, StoreError> {
    Ok(vec![
        Value::Text(row.logical_id.clone()),
        Value::Text(row.turn_id.clone()),
        Value::Text(row.status.as_str().to_owned()),
        Value::Integer(row.created_at),
        row.started_at.map_or(Value::Null, Value::Integer),
        row.completed_at.map_or(Value::Null, Value::Integer),
        row.error.as_ref().map_or(Ok(Value::Null), |error| {
            encode(error, "turn error").map(Value::Text)
        })?,
        row.usage.as_ref().map_or(Ok(Value::Null), |usage| {
            encode(usage, "turn usage").map(Value::Text)
        })?,
        optional_text(row.subagent_id.as_deref()),
    ])
}

fn decode_turn(values: &[Value]) -> Result<SwarmTurnRow, StoreError> {
    let error = optional_text_field(values, 6, "turn error")?;
    let usage = optional_text_field(values, 7, "turn usage")?;
    Ok(SwarmTurnRow {
        logical_id: text_field(values, 0, "logical id")?,
        turn_id: text_field(values, 1, "turn id")?,
        status: parse_turn_status(&text_field(values, 2, "turn status")?)?,
        created_at: int_field(values, 3, "turn created at")?,
        started_at: optional_int_field(values, 4, "turn started at")?,
        completed_at: optional_int_field(values, 5, "turn completed at")?,
        error: error
            .as_deref()
            .map(|text| decode(text, "turn error"))
            .transpose()?,
        usage: usage
            .as_deref()
            .map(|text| decode(text, "turn usage"))
            .transpose()?,
        subagent_id: optional_text_field(values, 8, "subagent id")?,
    })
}

fn session_params(row: &SwarmSessionRow) -> Result<Vec<Value>, StoreError> {
    Ok(vec![
        Value::Text(row.logical_id.clone()),
        Value::Text(row.root_session_id.clone()),
        Value::Text(row.actor_key.clone()),
        Value::Text(row.cwd.clone()),
        Value::Text(row.state.as_str().to_owned()),
        optional_text(row.model.as_deref()),
        optional_text(row.instructions.as_deref()),
        optional_text(mcp_json(&row.mcp_tools)?.as_deref()),
        Value::Text(encode(&row.environment, "environment")?),
        metadata_json(&row.metadata)?,
        prompt_cache_json(row.prompt_cache_options)?,
        Value::Text(row.created_at.clone()),
        Value::Text(row.last_activity_at.clone()),
        seq_param(row.last_item_seq, "last item seq")?,
    ])
}

/// Column order: `SELECT_SESSION_SQL`.
fn decode_session(values: &[Value]) -> Result<SwarmSessionRow, StoreError> {
    let mcp_json = optional_text_field(values, 7, "mcp")?;
    Ok(SwarmSessionRow {
        logical_id: text_field(values, 0, "logical id")?,
        root_session_id: text_field(values, 1, "root session id")?,
        actor_key: text_field(values, 2, "actor key")?,
        cwd: text_field(values, 3, "cwd")?,
        state: parse_session_state(&text_field(values, 4, "state")?)?,
        model: optional_text_field(values, 5, "model")?,
        instructions: optional_text_field(values, 6, "instructions")?,
        mcp_tools: decode_mcp_json(mcp_json.as_deref())?,
        environment: decode::<EnvironmentRef>(
            &text_field(values, 8, "environment")?,
            "environment",
        )?,
        metadata: decode_metadata(optional_text_field(values, 9, "metadata")?.as_deref())?,
        prompt_cache_options: decode_prompt_cache(
            optional_text_field(values, 10, "prompt cache options")?.as_deref(),
        )?,
        created_at: text_field(values, 11, "created at")?,
        last_activity_at: text_field(values, 12, "last activity at")?,
        last_item_seq: seq_field(values, 13, "last item seq")?,
    })
}

fn session_update_params(update: &SwarmSessionUpdate) -> Result<Vec<Value>, StoreError> {
    Ok(vec![
        Value::Text(update.logical_id.clone()),
        Value::Text(update.state.as_str().to_owned()),
        seq_param(update.last_item_seq, "last item seq")?,
        Value::Text(update.last_activity_at.clone()),
    ])
}

fn item_params(row: &SwarmItemRow) -> Result<Vec<Value>, StoreError> {
    Ok(vec![
        Value::Text(row.logical_id.clone()),
        seq_param(row.seq, "seq")?,
        Value::Text(row.item_id.clone()),
        Value::Text(row.kind.as_str().to_owned()),
        Value::Text(encode(&row.source, "item source")?),
        Value::Text(encode(&row.payload, "item payload")?),
        Value::Text(row.created_at.clone()),
        Value::Integer(i64::from(row.truncated)),
    ])
}

/// Column order: the `SELECT_ITEMS_*` statements.
fn decode_item(values: &[Value]) -> Result<SwarmItemRow, StoreError> {
    Ok(SwarmItemRow {
        logical_id: text_field(values, 0, "logical id")?,
        seq: seq_field(values, 1, "seq")?,
        item_id: text_field(values, 2, "item id")?,
        kind: parse_item_kind(&text_field(values, 3, "kind")?)?,
        source: decode::<ItemSource>(&text_field(values, 4, "source")?, "item source")?,
        payload: decode(&text_field(values, 5, "payload")?, "item payload")?,
        created_at: text_field(values, 6, "created at")?,
        truncated: int_field(values, 7, "truncated")? != 0,
    })
}

fn input_params(row: &SwarmInputRow) -> Vec<Value> {
    vec![
        Value::Text(row.logical_id.clone()),
        Value::Text(row.input_id.clone()),
        Value::Text(row.fingerprint.clone()),
        Value::Text(row.state.as_str().to_owned()),
        Value::Text(row.created_at.clone()),
    ]
}

/// Column order: `SELECT_INPUT_SQL`.
fn decode_input(values: &[Value]) -> Result<SwarmInputRow, StoreError> {
    Ok(SwarmInputRow {
        logical_id: text_field(values, 0, "logical id")?,
        input_id: text_field(values, 1, "input id")?,
        fingerprint: text_field(values, 2, "fingerprint")?,
        state: parse_input_state(&text_field(values, 3, "input state")?)?,
        created_at: text_field(values, 4, "created at")?,
    })
}

fn binding_params(row: &SwarmBindingRow) -> Vec<Value> {
    vec![
        Value::Text(row.logical_env_id.clone()),
        Value::Text(row.logical_id.clone()),
        Value::Text(row.sandbox_id.clone()),
        Value::Text(row.workspace_path.clone()),
        Value::Text(row.sandbox_root.clone()),
        optional_text(row.settings_profile.as_deref()),
        optional_text(row.token_file.as_deref()),
        optional_text(row.last_run_id.as_deref()),
        Value::Text(row.last_used_at.clone()),
    ]
}

/// Column order: `SELECT_BINDING_SQL`.
fn decode_binding(values: &[Value]) -> Result<SwarmBindingRow, StoreError> {
    Ok(SwarmBindingRow {
        logical_env_id: text_field(values, 0, "logical environment id")?,
        logical_id: text_field(values, 1, "logical id")?,
        sandbox_id: text_field(values, 2, "sandbox id")?,
        workspace_path: text_field(values, 3, "workspace path")?,
        sandbox_root: text_field(values, 4, "sandbox root")?,
        settings_profile: optional_text_field(values, 5, "settings profile")?,
        token_file: optional_text_field(values, 6, "token file")?,
        last_run_id: optional_text_field(values, 7, "last run id")?,
        last_used_at: text_field(values, 8, "last used at")?,
    })
}

fn text_field(values: &[Value], index: usize, what: &'static str) -> Result<String, StoreError> {
    text_at(values, index)
        .map(str::to_owned)
        .ok_or_else(|| unreadable(format!("{what} is not text")))
}

fn optional_text_field(
    values: &[Value],
    index: usize,
    what: &'static str,
) -> Result<Option<String>, StoreError> {
    match values.get(index) {
        Some(Value::Null) => Ok(None),
        Some(Value::Text(text)) => Ok(Some(text.clone())),
        _ => Err(unreadable(format!("{what} is not text"))),
    }
}

fn int_field(values: &[Value], index: usize, what: &'static str) -> Result<i64, StoreError> {
    int_at(values, index).ok_or_else(|| unreadable(format!("{what} is not an integer")))
}

fn optional_int_field(
    values: &[Value],
    index: usize,
    what: &'static str,
) -> Result<Option<i64>, StoreError> {
    match values.get(index) {
        Some(Value::Null) | None => Ok(None),
        Some(Value::Integer(value)) => Ok(Some(*value)),
        _ => Err(unreadable(format!("{what} is not an integer"))),
    }
}

fn seq_field(values: &[Value], index: usize, what: &'static str) -> Result<u64, StoreError> {
    int_field(values, index, what)?
        .try_into()
        .map_err(|_| unreadable(format!("{what} is negative")))
}

fn seq_param(value: u64, what: &'static str) -> Result<Value, StoreError> {
    i64::try_from(value).map(Value::Integer).map_err(|_| {
        StoreError::Unavailable(format!("swarm {what} exceeds the store's 64-bit range"))
    })
}

fn text_at(values: &[Value], index: usize) -> Option<&str> {
    match values.get(index) {
        Some(Value::Text(text)) => Some(text.as_str()),
        _ => None,
    }
}

fn int_at(values: &[Value], index: usize) -> Option<i64> {
    match values.get(index) {
        Some(Value::Integer(value)) => Some(*value),
        _ => None,
    }
}

fn optional_text(value: Option<&str>) -> Value {
    value.map_or(Value::Null, |text| Value::Text(text.to_owned()))
}

fn unreadable(detail: impl Into<String>) -> StoreError {
    StoreError::Unavailable(format!("unreadable swarm row: {}", detail.into()))
}

fn unexplained(operation: &'static str, affected: u64) -> StoreError {
    StoreError::Unavailable(format!(
        "{operation}: swarm state store reported {affected} affected rows where one was required"
    ))
}

fn unexpected_row_count(operation: &'static str, rows: usize) -> StoreError {
    StoreError::Unavailable(format!(
        "{operation}: swarm state store returned {rows} rows where one was required"
    ))
}

// ── trait implementation ────────────────────────────────────────────────────

#[async_trait]
impl SwarmStore for SwarmStateStore {
    async fn swarm_upsert_session(&self, row: SwarmSessionRow) -> Result<(), StoreError> {
        let affected = self
            .execute("upsert_session", UPSERT_SESSION_SQL, session_params(&row)?)
            .await?;
        match affected {
            1 => Ok(()),
            0 => Err(StoreError::Conflict(
                "swarm session identity conflict: cwd or rootSessionId differs".to_owned(),
            )),
            other => Err(unexplained("upsert_session", other)),
        }
    }

    async fn swarm_create_hosted_session(
        &self,
        row: SwarmSessionRow,
        binding: SwarmBindingRow,
        require_existing: bool,
    ) -> Result<(), StoreError> {
        let (binding_sql, session_sql) = if require_existing {
            (ATTACH_BINDING_SQL, INSERT_ATTACHED_SESSION_SQL)
        } else {
            (UPSERT_BINDING_SQL, INSERT_SESSION_SQL)
        };
        // Updating the binding and inserting the session form one write transaction.
        // A session identity conflict rolls back the binding update. A missing or
        // replaced shared environment leaves both statements without effect.
        let statements = [
            BatchStatement::new(binding_sql, binding_params(&binding))
                .map_err(|e| classify("create_hosted_session", e))?,
            BatchStatement::new(session_sql, session_params(&row)?)
                .map_err(|e| classify("create_hosted_session", e))?,
        ];
        let results = within_budget(
            "create_hosted_session",
            self.connection
                .transactional_batch(statements, TransactionBehavior::Immediate),
        )
        .await?;
        if results
            .last()
            .is_none_or(|result| result.rows_affected() != 1)
        {
            return Err(StoreError::NotFound(binding.logical_env_id));
        }
        Ok(())
    }

    async fn swarm_get_session(
        &self,
        logical_id: &str,
    ) -> Result<Option<SwarmSessionRow>, StoreError> {
        let rows = self
            .query_rows(
                "get_session",
                SELECT_SESSION_SQL,
                vec![Value::Text(logical_id.to_owned())],
            )
            .await?;
        match rows.as_slice() {
            [] => Ok(None),
            [row] => Ok(Some(decode_session(row)?)),
            rows => Err(unexpected_row_count("get_session", rows.len())),
        }
    }

    async fn swarm_update_session(&self, update: SwarmSessionUpdate) -> Result<(), StoreError> {
        let affected = self
            .execute(
                "update_session",
                UPDATE_SESSION_SQL,
                session_update_params(&update)?,
            )
            .await?;
        match affected {
            1 => Ok(()),
            0 => Err(StoreError::NotFound(format!(
                "swarm session {} does not exist",
                update.logical_id
            ))),
            other => Err(unexplained("update_session", other)),
        }
    }

    async fn swarm_patch_session(&self, patch: SwarmSessionPatch) -> Result<(), StoreError> {
        let set_mcp = patch.mcp_tools.is_some();
        let model = patch.model;
        let mcp = match patch.mcp_tools {
            Some(tools) => mcp_json(&tools)?,
            None => None,
        };
        let set_metadata = patch.metadata.is_some();
        let metadata = match patch.metadata {
            Some(metadata) => Some(metadata_json(&metadata)?),
            None => None,
        };
        let set_prompt_cache = patch.prompt_cache_options.is_some();
        let prompt_cache = match patch.prompt_cache_options {
            Some(options) => Some(prompt_cache_json(options)?),
            None => None,
        };
        let params = vec![
            Value::Text(patch.logical_id.clone()),
            Value::Integer(i64::from(model.is_some())),
            optional_text(model.as_deref()),
            Value::Integer(i64::from(set_mcp)),
            optional_text(mcp.as_deref()),
            Value::Integer(i64::from(set_metadata)),
            metadata.unwrap_or(Value::Null),
            Value::Integer(i64::from(set_prompt_cache)),
            prompt_cache.unwrap_or(Value::Null),
            Value::Integer(i64::from(patch.last_activity_at.is_some())),
            optional_text(patch.last_activity_at.as_deref()),
        ];
        let affected = self
            .execute("patch_session", PATCH_SESSION_SQL, params)
            .await?;
        match affected {
            1 => Ok(()),
            0 => Err(StoreError::NotFound(format!(
                "swarm session {} does not exist",
                patch.logical_id
            ))),
            other => Err(unexplained("patch_session", other)),
        }
    }

    async fn swarm_list_sessions(
        &self,
        window: &SessionsWindow,
    ) -> Result<Vec<SwarmSessionRow>, StoreError> {
        if window
            .agent_id
            .as_deref()
            .is_some_and(|agent_id| agent_id != "root")
        {
            // The gateway has one root agent identity; a different id selects
            // no session rather than being ignored.
            return Ok(Vec::new());
        }
        let sql = session_list_sql(window.order, window.metadata.len());
        let mut params = vec![
            Value::Text(window.after.clone().unwrap_or_default()),
            Value::Integer(i64::from(window.limit.saturating_add(1))),
        ];
        for (key, value) in &window.metadata {
            params.push(Value::Text(key.clone()));
            params.push(Value::Text(value.clone()));
        }
        let rows = self.query_rows("list_sessions", &sql, params).await?;
        rows.iter().map(|row| decode_session(row)).collect()
    }

    async fn swarm_delete_session(&self, logical_id: &str) -> Result<(), StoreError> {
        // One managed transaction: a partial delete can never be reported as
        // success. Statements are ordered child-first for readability; the
        // transaction is what makes the set atomic.
        let statements = [
            DELETE_ITEMS_SQL,
            DELETE_INPUTS_SQL,
            DELETE_TURNS_SQL,
            REASSIGN_BINDINGS_SQL,
            DELETE_BINDINGS_SQL,
            DELETE_SESSION_SQL,
        ]
        .into_iter()
        .map(|sql| batch_statement(sql, vec![Value::Text(logical_id.to_owned())]))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| StoreError::Unavailable(error.to_string()))?;
        within_budget(
            "delete_session",
            self.connection
                .transactional_batch(statements, TransactionBehavior::Immediate),
        )
        .await?;
        Ok(())
    }

    async fn swarm_append_item(&self, row: SwarmItemRow) -> Result<(), StoreError> {
        let affected = self
            .execute("append_item", INSERT_ITEM_SQL, item_params(&row)?)
            .await?;
        match affected {
            1 => Ok(()),
            other => Err(unexplained("append_item", other)),
        }
    }

    async fn swarm_append_items(
        &self,
        rows: &[SwarmItemRow],
        progress: Option<SwarmSessionUpdate>,
    ) -> Result<(), StoreError> {
        let has_progress = progress.is_some();
        let session_id = progress
            .as_ref()
            .map(|update| update.logical_id.clone())
            .unwrap_or_default();
        let mut statements = Vec::with_capacity(rows.len() + usize::from(has_progress));
        for row in rows {
            statements.push(
                BatchStatement::new(INSERT_ITEM_SQL, item_params(row)?)
                    .map_err(|error| classify("append_items", error))?,
            );
        }
        if let Some(update) = progress {
            statements.push(
                BatchStatement::new(UPDATE_SESSION_SQL, session_update_params(&update)?)
                    .map_err(|error| classify("append_items", error))?,
            );
        }
        if statements.is_empty() {
            return Ok(());
        }
        // One transaction: a viewer never sees an item the session cursor has
        // not reached, and a failed run leaves no hole for the retry to skip.
        let results = within_budget(
            "append_items",
            self.connection
                .transactional_batch(statements, TransactionBehavior::Immediate),
        )
        .await?;
        for (index, result) in results.iter().enumerate() {
            if result.rows_affected() == 1 {
                continue;
            }
            // The last statement is the progress update when one was sent; a
            // zero there means the session row disappeared mid-turn.
            return Err(if has_progress && index + 1 == results.len() {
                StoreError::NotFound(format!("swarm session {session_id} does not exist"))
            } else {
                unexplained("append_items", result.rows_affected())
            });
        }
        Ok(())
    }

    async fn swarm_items_after(
        &self,
        logical_id: &str,
        after: u64,
        limit: u32,
    ) -> Result<Vec<SwarmItemRow>, StoreError> {
        let rows = self
            .query_rows(
                "items_after",
                SELECT_ITEMS_AFTER_SQL,
                vec![
                    Value::Text(logical_id.to_owned()),
                    seq_param(after, "cursor")?,
                    Value::Integer(i64::from(limit)),
                ],
            )
            .await?;
        rows.iter().map(|row| decode_item(row)).collect()
    }

    async fn swarm_items_before(
        &self,
        logical_id: &str,
        before: Option<u64>,
        limit: u32,
    ) -> Result<Vec<SwarmItemRow>, StoreError> {
        // Two static statements: `Some(before)` binds the cursor, the tail read
        // must not bind `u64::MAX` (it does not fit an i64).
        let (sql, params) = match before {
            Some(before) => (
                SELECT_ITEMS_BEFORE_SQL,
                vec![
                    Value::Text(logical_id.to_owned()),
                    seq_param(before, "cursor")?,
                    Value::Integer(i64::from(limit)),
                ],
            ),
            None => (
                SELECT_ITEMS_TAIL_SQL,
                vec![
                    Value::Text(logical_id.to_owned()),
                    Value::Integer(i64::from(limit)),
                ],
            ),
        };
        let rows = self.query_rows("items_before", sql, params).await?;
        rows.iter().map(|row| decode_item(row)).collect()
    }

    async fn swarm_item_page(
        &self,
        logical_id: &str,
        order: ItemOrder,
        after: Option<u64>,
        limit: u32,
    ) -> Result<SwarmItemPage, StoreError> {
        // `limit + 1` items are selected so `has_more` needs no second query;
        // the extra item's rows are dropped by `grouped_item_page`.
        let probe = limit.saturating_add(1);
        let (sql, params) = match (order, after) {
            (ItemOrder::Asc, after) => (
                SELECT_ITEM_PAGE_ASC_SQL,
                vec![
                    Value::Text(logical_id.to_owned()),
                    seq_param(after.unwrap_or(0), "cursor")?,
                    Value::Integer(i64::from(probe)),
                ],
            ),
            (ItemOrder::Desc, Some(after)) => (
                SELECT_ITEM_PAGE_DESC_SQL,
                vec![
                    Value::Text(logical_id.to_owned()),
                    seq_param(after, "cursor")?,
                    Value::Integer(i64::from(probe)),
                ],
            ),
            (ItemOrder::Desc, None) => (
                SELECT_ITEM_PAGE_TAIL_SQL,
                vec![
                    Value::Text(logical_id.to_owned()),
                    Value::Integer(i64::from(probe)),
                ],
            ),
        };
        let rows = self.query_rows("item_page", sql, params).await?;
        let rows = rows
            .iter()
            .map(|row| decode_item(row))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(grouped_item_page(rows, order, limit))
    }

    async fn swarm_last_seq(&self, logical_id: &str) -> Result<u64, StoreError> {
        let rows = self
            .query_rows(
                "last_seq",
                SELECT_LAST_SEQ_SQL,
                vec![Value::Text(logical_id.to_owned())],
            )
            .await?;
        match rows.as_slice() {
            [row] => seq_field(row, 0, "last item seq"),
            rows => Err(unexpected_row_count("last_seq", rows.len())),
        }
    }

    async fn swarm_get_item(
        &self,
        logical_id: &str,
        item_id: &str,
    ) -> Result<Option<SwarmItemRow>, StoreError> {
        let rows = self
            .query_rows(
                "get_item",
                SELECT_ITEM_BY_ID_SQL,
                vec![
                    Value::Text(logical_id.to_owned()),
                    Value::Text(item_id.to_owned()),
                ],
            )
            .await?;
        match rows.as_slice() {
            [] => Ok(None),
            [row] => decode_item(row).map(Some),
            rows => Err(unexpected_row_count("get_item", rows.len())),
        }
    }

    async fn swarm_put_input(&self, row: SwarmInputRow) -> Result<InputPut, StoreError> {
        let affected = self
            .execute("put_input", INSERT_INPUT_SQL, input_params(&row))
            .await?;
        match affected {
            1 => Ok(InputPut::Inserted),
            0 => {
                let existing = self
                    .swarm_get_input(&row.logical_id, &row.input_id)
                    .await?
                    .ok_or_else(|| {
                        StoreError::Unavailable(
                            "put_input: conflicting input receipt is not readable".to_owned(),
                        )
                    })?;
                Ok(InputPut::Existing(existing))
            }
            other => Err(unexplained("put_input", other)),
        }
    }

    async fn swarm_get_input(
        &self,
        logical_id: &str,
        input_id: &str,
    ) -> Result<Option<SwarmInputRow>, StoreError> {
        let rows = self
            .query_rows(
                "get_input",
                SELECT_INPUT_SQL,
                vec![
                    Value::Text(logical_id.to_owned()),
                    Value::Text(input_id.to_owned()),
                ],
            )
            .await?;
        match rows.as_slice() {
            [] => Ok(None),
            [row] => Ok(Some(decode_input(row)?)),
            rows => Err(unexpected_row_count("get_input", rows.len())),
        }
    }

    async fn swarm_put_binding(&self, row: SwarmBindingRow) -> Result<(), StoreError> {
        let affected = self
            .execute("put_binding", UPSERT_BINDING_SQL, binding_params(&row))
            .await?;
        match affected {
            1 => Ok(()),
            other => Err(unexplained("put_binding", other)),
        }
    }

    async fn swarm_get_binding(
        &self,
        logical_env_id: &str,
    ) -> Result<Option<SwarmBindingRow>, StoreError> {
        let rows = self
            .query_rows(
                "get_binding",
                SELECT_BINDING_SQL,
                vec![Value::Text(logical_env_id.to_owned())],
            )
            .await?;
        match rows.as_slice() {
            [] => Ok(None),
            [row] => Ok(Some(decode_binding(row)?)),
            rows => Err(unexpected_row_count("get_binding", rows.len())),
        }
    }

    async fn swarm_upsert_turn(&self, row: SwarmTurnRow) -> Result<(), StoreError> {
        let affected = self
            .execute("upsert_turn", UPSERT_TURN_SQL, turn_params(&row)?)
            .await?;
        match affected {
            1 => Ok(()),
            other => Err(unexplained("upsert_turn", other)),
        }
    }

    async fn swarm_get_turn(
        &self,
        logical_id: &str,
        turn_id: &str,
    ) -> Result<Option<SwarmTurnRow>, StoreError> {
        let rows = self
            .query_rows(
                "get_turn",
                SELECT_TURN_SQL,
                vec![
                    Value::Text(logical_id.to_owned()),
                    Value::Text(turn_id.to_owned()),
                ],
            )
            .await?;
        match rows.as_slice() {
            [] => Ok(None),
            [row] => decode_turn(row).map(Some),
            rows => Err(unexpected_row_count("get_turn", rows.len())),
        }
    }

    async fn swarm_turn_page(
        &self,
        logical_id: &str,
        window: &TurnsWindow,
    ) -> Result<Vec<SwarmTurnRow>, StoreError> {
        // The service rejects an unknown cursor; a missing row here simply
        // selects nothing.
        let after = match window.after.as_deref() {
            Some(after) => self.swarm_get_turn(logical_id, after).await?,
            None => None,
        };
        let probe = Value::Integer(i64::from(window.limit.saturating_add(1)));
        let (sql, params) = match (window.order, after) {
            (ItemOrder::Asc, Some(after)) => (
                SELECT_TURNS_ASC_AFTER_SQL,
                vec![
                    Value::Text(logical_id.to_owned()),
                    Value::Integer(after.created_at),
                    Value::Text(after.turn_id),
                    probe,
                ],
            ),
            (ItemOrder::Asc, None) => (
                SELECT_TURNS_ASC_SQL,
                vec![Value::Text(logical_id.to_owned()), probe],
            ),
            (ItemOrder::Desc, Some(after)) => (
                SELECT_TURNS_DESC_AFTER_SQL,
                vec![
                    Value::Text(logical_id.to_owned()),
                    Value::Integer(after.created_at),
                    Value::Text(after.turn_id),
                    probe,
                ],
            ),
            (ItemOrder::Desc, None) => (
                SELECT_TURNS_DESC_SQL,
                vec![Value::Text(logical_id.to_owned()), probe],
            ),
        };
        let rows = self.query_rows("turn_page", sql, params).await?;
        rows.iter().map(|row| decode_turn(row)).collect()
    }

    async fn swarm_close_open_turns(
        &self,
        logical_id: &str,
        status: TurnStatus,
        completed_at: i64,
        error: Option<serde_json::Value>,
    ) -> Result<u64, StoreError> {
        self.execute(
            "close_open_turns",
            CLOSE_OPEN_TURNS_SQL,
            vec![
                Value::Text(logical_id.to_owned()),
                Value::Text(status.as_str().to_owned()),
                Value::Integer(completed_at),
                match error {
                    Some(error) => encode(&error, "turn error").map(Value::Text)?,
                    None => Value::Null,
                },
            ],
        )
        .await
    }

    async fn swarm_has_open_turn(&self, logical_id: &str) -> Result<bool, StoreError> {
        let rows = self
            .query_rows(
                "has_open_turn",
                HAS_OPEN_TURN_SQL,
                vec![Value::Text(logical_id.to_owned())],
            )
            .await?;
        match rows.as_slice() {
            [row] => match row.as_slice() {
                [Value::Integer(open)] => Ok(*open != 0),
                _ => Err(unreadable("has_open_turn result is not one integer")),
            },
            rows => Err(unexpected_row_count("has_open_turn", rows.len())),
        }
    }
}

#[cfg(test)]
#[path = "turso_test.rs"]
mod tests;
