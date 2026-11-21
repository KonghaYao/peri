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

use std::future::Future;
use std::time::Duration;

use async_trait::async_trait;
use turso_serverless::{
    BatchStatement, Builder, Connection, Error as SdkError, TransactionBehavior, Value,
};
use url::Url;

use super::{
    InputPut, StoreError, SwarmBindingRow, SwarmInputRow, SwarmItemRow, SwarmSessionRow,
    SwarmSessionUpdate, SwarmStore,
};
use crate::domain::{EnvironmentRef, InputState, ItemKind, ItemSource, McpToolSpec, SessionState};

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

const CREATE_TABLE_SQL: [&str; 4] = [
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
];

/// Plain index: `item_id` repeats across delta/complete rows of one turn.
const CREATE_INDEX_SQL: [&str; 1] =
    ["CREATE INDEX IF NOT EXISTS swarm_items_by_id ON swarm_items (logical_id, item_id)"];

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
const UPSERT_SESSION_SQL: &str = "INSERT INTO swarm_sessions (
    logical_id, root_session_id, actor_key, cwd, state, model, instructions,
    mcp_json, environment_json, created_at, last_activity_at, last_item_seq)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
ON CONFLICT(logical_id) DO UPDATE SET
    actor_key = excluded.actor_key,
    state = excluded.state,
    model = excluded.model,
    instructions = excluded.instructions,
    mcp_json = excluded.mcp_json,
    environment_json = excluded.environment_json,
    last_activity_at = excluded.last_activity_at,
    last_item_seq = excluded.last_item_seq
WHERE swarm_sessions.root_session_id = excluded.root_session_id
  AND swarm_sessions.cwd = excluded.cwd";

const SELECT_SESSION_SQL: &str = "SELECT logical_id, root_session_id, actor_key, cwd, state,
    model, instructions, mcp_json, environment_json, created_at, last_activity_at, last_item_seq
    FROM swarm_sessions WHERE logical_id = ?1";

const UPDATE_SESSION_SQL: &str = "UPDATE swarm_sessions
    SET state = ?2, last_item_seq = ?3, last_activity_at = ?4 WHERE logical_id = ?1";

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
        sql: &'static str,
        params: Vec<Value>,
    ) -> Result<u64, StoreError> {
        within_budget(operation, self.connection.execute(sql, params)).await
    }

    async fn query_rows(
        &self,
        operation: &'static str,
        sql: &'static str,
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

/// Create the owned objects in one managed transaction. `pin_version` also
/// records [`SWARM_STATE_SCHEMA_VERSION`] when no row exists yet.
async fn apply_ddl(connection: &Connection, pin_version: bool) -> Result<(), SwarmStateError> {
    let mut statements = Vec::with_capacity(2 + CREATE_TABLE_SQL.len() + CREATE_INDEX_SQL.len());
    statements.push(batch_statement(CREATE_META_TABLE_SQL, Vec::new())?);
    for sql in CREATE_TABLE_SQL {
        statements.push(batch_statement(sql, Vec::new())?);
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
        SdkError::Constraint(message) => {
            StoreError::Conflict(format!("{operation}: {message}"))
        }
        SdkError::Readonly(message) => StoreError::Unsupported(format!(
            "{operation}: swarm state store is read only: {message}"
        )),
        SdkError::BatchStatementFailed { index, error, .. } => {
            match classify(operation, *error) {
                StoreError::Conflict(detail) => {
                    StoreError::Conflict(format!("batch statement {index}: {detail}"))
                }
                other => other,
            }
        }
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
        created_at: text_field(values, 9, "created at")?,
        last_activity_at: text_field(values, 10, "last activity at")?,
        last_item_seq: seq_field(values, 11, "last item seq")?,
    })
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
                vec![
                    Value::Text(update.logical_id.clone()),
                    Value::Text(update.state.as_str().to_owned()),
                    seq_param(update.last_item_seq, "last item seq")?,
                    Value::Text(update.last_activity_at),
                ],
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

    async fn swarm_append_item(&self, row: SwarmItemRow) -> Result<(), StoreError> {
        let affected = self
            .execute("append_item", INSERT_ITEM_SQL, item_params(&row)?)
            .await?;
        match affected {
            1 => Ok(()),
            other => Err(unexplained("append_item", other)),
        }
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
}

#[cfg(test)]
#[path = "turso_test.rs"]
mod tests;
