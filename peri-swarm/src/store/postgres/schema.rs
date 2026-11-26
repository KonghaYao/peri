//! Owned schema, version metadata and the fail-closed initializer.
//!
//! `swarm_meta` is a single-row identity table. It records the logical schema
//! version (`1`), the storage engine (`postgres`) and the contract
//! (`peri.swarm.postgres/v1`). A database is only initialized when it carries
//! **no relation at all**; anything else without this build's `swarm_meta` is
//! refused, so a misdirected DSN can never adopt the Peri session store.
//!
//! Every future schema change must bump [`SWARM_STATE_SCHEMA_VERSION`], run its
//! additive migration and update the version row **in the same transaction that
//! holds the advisory lock**. There is no downgrade path: an unknown (higher)
//! version fails closed, and this build never rewrites an identity row.

use std::collections::{BTreeMap, BTreeSet};

use sqlx::postgres::Postgres;
use sqlx::Row;

use super::budget::classify;
use super::SwarmStateError;
use crate::store::StoreError;

/// Logical schema version recorded in `swarm_meta.version`.
///
/// Bump only together with an explicit, additive migration.
pub const SWARM_STATE_SCHEMA_VERSION: i64 = 1;

/// Storage engine recorded in `swarm_meta.engine`. A PostgreSQL database can
/// never be a Loom/SQLite database, so this makes a hand-copied file obvious.
pub(super) const SWARM_STATE_ENGINE: &str = "postgres";

/// Contract recorded in `swarm_meta.contract`.
pub(super) const SWARM_STATE_CONTRACT: &str = "peri.swarm.postgres/v1";

/// Serializes initialization between replicas of one schema. The first key
/// differs from the session store's (`1346720329`), so the two initializers
/// never block each other.
const LOCK_SCHEMA_SQL: &str = "SELECT pg_catalog.pg_advisory_xact_lock(1346720330, \
     pg_catalog.hashtext(pg_catalog.current_schema()))";

/// Every relation in the schema this connection writes to. Any relation means
/// the database already has an owner; only a relation-free schema may be
/// initialized.
const RELATIONS_SQL: &str = "SELECT c.relname::text FROM pg_catalog.pg_class c
    JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
    WHERE n.nspname = current_schema() AND c.relkind IN ('r', 'p', 'v', 'm', 'S', 'f')
    ORDER BY c.relname";

/// Tables (not views/sequences) of the schema: the shape check's input.
const TABLES_SQL: &str = "SELECT c.relname::text FROM pg_catalog.pg_class c
    JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
    WHERE n.nspname = current_schema() AND c.relkind IN ('r', 'p')
    ORDER BY c.relname";

const COLUMNS_SQL: &str = "SELECT table_name::text, column_name::text
    FROM information_schema.columns
    WHERE table_schema = current_schema() ORDER BY table_name, ordinal_position";

const SELECT_META_SQL: &str =
    "SELECT version, engine, contract FROM swarm_meta WHERE singleton = 0";

const PIN_VERSION_SQL: &str =
    "INSERT INTO swarm_meta (singleton, version, engine, contract, created_at)
    VALUES (0, $1, $2, $3, $4)";

const CREATE_META_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS swarm_meta (
    singleton  SMALLINT PRIMARY KEY CHECK (singleton = 0),
    version    BIGINT NOT NULL,
    engine     TEXT   NOT NULL,
    contract   TEXT   NOT NULL,
    created_at TEXT   NOT NULL
)";

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
        last_item_seq    BIGINT NOT NULL DEFAULT 0
    )",
    "CREATE TABLE IF NOT EXISTS swarm_items (
        logical_id   TEXT NOT NULL,
        seq          BIGINT NOT NULL,
        item_id      TEXT NOT NULL,
        kind         TEXT NOT NULL,
        source       TEXT NOT NULL,
        payload_json TEXT NOT NULL,
        created_at   TEXT NOT NULL,
        truncated    BOOLEAN NOT NULL DEFAULT FALSE,
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
        created_at   BIGINT NOT NULL,
        started_at   BIGINT,
        completed_at BIGINT,
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

/// Columns this build reads and writes. Subset semantics: an additive column
/// from a later build is tolerated, a missing one is not.
const EXPECTED_COLUMNS: [(&str, &[&str]); 6] = [
    (
        "swarm_meta",
        &["singleton", "version", "engine", "contract", "created_at"],
    ),
    (
        "swarm_sessions",
        &[
            "logical_id",
            "root_session_id",
            "actor_key",
            "cwd",
            "state",
            "model",
            "instructions",
            "mcp_json",
            "environment_json",
            "metadata_json",
            "prompt_cache_options_json",
            "created_at",
            "last_activity_at",
            "last_item_seq",
        ],
    ),
    (
        "swarm_items",
        &[
            "logical_id",
            "seq",
            "item_id",
            "kind",
            "source",
            "payload_json",
            "created_at",
            "truncated",
        ],
    ),
    (
        "swarm_inputs",
        &[
            "logical_id",
            "input_id",
            "fingerprint",
            "state",
            "created_at",
        ],
    ),
    (
        "swarm_bindings",
        &[
            "logical_env_id",
            "logical_id",
            "sandbox_id",
            "workspace_path",
            "sandbox_root",
            "settings_profile",
            "token_file",
            "last_run_id",
            "last_used_at",
        ],
    ),
    (
        "swarm_turns",
        &[
            "logical_id",
            "turn_id",
            "status",
            "created_at",
            "started_at",
            "completed_at",
            "error_json",
            "usage_json",
            "subagent_id",
        ],
    ),
];

/// Version identity read back from `swarm_meta`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct MetaRow {
    pub(super) version: i64,
    pub(super) engine: String,
    pub(super) contract: String,
}

/// Verify (and, for a relation-free schema, create) the owned schema.
///
/// Order matters: the advisory lock is taken **before** the catalog read, so two
/// replicas opening the same empty database cannot both decide to initialize
/// it. An existing `swarm_meta` is validated before any DDL runs, so a database
/// written by a newer build is rejected without being touched.
pub(super) async fn ensure_schema(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
) -> Result<(), SwarmStateError> {
    sqlx::query(LOCK_SCHEMA_SQL)
        .execute(&mut **transaction)
        .await
        .map_err(|error| schema_failure("lock schema", error))?;
    let relations = relation_names(transaction, RELATIONS_SQL).await?;
    if relations.iter().any(|name| name == "swarm_meta") {
        let meta = read_meta(transaction).await?;
        require_supported_meta(&meta)?;
        // Same version: re-apply the idempotent DDL to repair missing objects.
        // The identity row is not rewritten by this path.
        apply_ddl(transaction).await?;
    } else if !relations.is_empty() {
        return Err(adopt_refusal(&relations));
    } else {
        apply_ddl(transaction).await?;
        pin_version(transaction).await?;
    }
    verify_shape(transaction).await
}

/// Fail closed on any identity this build does not own.
fn require_supported_meta(meta: &MetaRow) -> Result<(), SwarmStateError> {
    if meta.engine != SWARM_STATE_ENGINE || meta.contract != SWARM_STATE_CONTRACT {
        return Err(SwarmStateError::Schema(format!(
            "swarm_meta records engine={:?} contract={:?}; this build owns engine={:?} \
             contract={:?}. Point PERI_SWARM_STATE_STORE at the database this build created, or \
             restore it from a backup",
            meta.engine, meta.contract, SWARM_STATE_ENGINE, SWARM_STATE_CONTRACT
        )));
    }
    if meta.version != SWARM_STATE_SCHEMA_VERSION {
        return Err(SwarmStateError::SchemaVersion {
            found: meta.version,
            expected: SWARM_STATE_SCHEMA_VERSION,
        });
    }
    Ok(())
}

/// A schema that already has content is never treated as a fresh database.
fn adopt_refusal(relations: &[String]) -> SwarmStateError {
    if relations.iter().any(|name| name == "peri_store_meta") {
        return SwarmStateError::Schema(format!(
            "the target database already holds the Peri session store (relations: {}); \
             PERI_SWARM_STATE_STORE must name a different database than \
             PERI_SWARM_SESSION_STORE",
            relation_list(relations)
        ));
    }
    SwarmStateError::Schema(format!(
        "the target schema is not empty (relations: {}) but has no swarm_meta; refusing to \
         initialize over an existing schema. Point PERI_SWARM_STATE_STORE at an empty dedicated \
         database, or restore the intended Swarm state database from a backup",
        relation_list(relations)
    ))
}

fn relation_list(relations: &[String]) -> String {
    const SHOWN: usize = 8;
    let mut names: Vec<&str> = relations.iter().take(SHOWN).map(String::as_str).collect();
    if relations.len() > SHOWN {
        names.push("…");
    }
    names.join(", ")
}

/// Columns this build requires but `actual` lacks, one `table.column` per entry.
fn missing_columns(actual: &BTreeMap<String, BTreeSet<String>>) -> Vec<String> {
    let mut missing = Vec::new();
    for (table, columns) in EXPECTED_COLUMNS {
        let present = actual.get(table);
        for column in columns {
            if !present.is_some_and(|present| present.contains(*column)) {
                missing.push(format!("{table}.{column}"));
            }
        }
    }
    missing
}

async fn relation_names(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    sql: &'static str,
) -> Result<Vec<String>, SwarmStateError> {
    let rows = sqlx::query(sql)
        .fetch_all(&mut **transaction)
        .await
        .map_err(|error| schema_failure("read catalog", error))?;
    rows.iter()
        .map(|row| {
            row.try_get::<String, _>(0)
                .map_err(|error| SwarmStateError::Schema(format!("catalog name: {error}")))
        })
        .collect()
}

async fn read_meta(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
) -> Result<MetaRow, SwarmStateError> {
    let rows = sqlx::query(SELECT_META_SQL)
        .fetch_all(&mut **transaction)
        .await
        .map_err(|error| schema_failure("read swarm_meta", error))?;
    let row =
        match rows.as_slice() {
            [row] => row,
            [] => return Err(SwarmStateError::Schema(
                "swarm_meta has no identity row; this build never re-creates an identity it did \
                 not write. Restore the Swarm state database from a backup, or drop and re-create \
                 the empty database"
                    .to_owned(),
            )),
            rows => {
                return Err(SwarmStateError::Schema(format!(
                    "swarm_meta has {} identity rows; exactly one is required",
                    rows.len()
                )))
            }
        };
    Ok(MetaRow {
        version: row
            .try_get("version")
            .map_err(|error| SwarmStateError::Schema(format!("swarm_meta.version: {error}")))?,
        engine: row
            .try_get("engine")
            .map_err(|error| SwarmStateError::Schema(format!("swarm_meta.engine: {error}")))?,
        contract: row
            .try_get("contract")
            .map_err(|error| SwarmStateError::Schema(format!("swarm_meta.contract: {error}")))?,
    })
}

async fn apply_ddl(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
) -> Result<(), SwarmStateError> {
    let mut statements = Vec::with_capacity(1 + CREATE_TABLE_SQL.len() + CREATE_INDEX_SQL.len());
    statements.push(CREATE_META_TABLE_SQL);
    statements.extend(CREATE_TABLE_SQL);
    statements.extend(CREATE_INDEX_SQL);
    for sql in statements {
        sqlx::query(sql)
            .execute(&mut **transaction)
            .await
            .map_err(|error| schema_failure("apply ddl", error))?;
    }
    Ok(())
}

async fn pin_version(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
) -> Result<(), SwarmStateError> {
    sqlx::query(PIN_VERSION_SQL)
        .bind(SWARM_STATE_SCHEMA_VERSION)
        .bind(SWARM_STATE_ENGINE)
        .bind(SWARM_STATE_CONTRACT)
        .bind(peri_time::now_utc_rfc3339())
        .execute(&mut **transaction)
        .await
        .map_err(|error| schema_failure("pin schema version", error))?;
    Ok(())
}

/// The six owned tables and the columns this build needs must exist after the
/// idempotent DDL; otherwise the database was tampered with and the process
/// must not start against it.
async fn verify_shape(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
) -> Result<(), SwarmStateError> {
    let tables = relation_names(transaction, TABLES_SQL).await?;
    let mut problems: Vec<String> = EXPECTED_COLUMNS
        .iter()
        .map(|(table, _)| *table)
        .filter(|table| !tables.iter().any(|present| present == table))
        .map(|table| format!("{table} (table)"))
        .collect();
    let rows = sqlx::query(COLUMNS_SQL)
        .fetch_all(&mut **transaction)
        .await
        .map_err(|error| schema_failure("read columns", error))?;
    let mut actual: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for row in &rows {
        let table: String = row
            .try_get(0)
            .map_err(|error| SwarmStateError::Schema(format!("column table: {error}")))?;
        let column: String = row
            .try_get(1)
            .map_err(|error| SwarmStateError::Schema(format!("column name: {error}")))?;
        actual.entry(table).or_default().insert(column);
    }
    problems.extend(missing_columns(&actual));
    if problems.is_empty() {
        return Ok(());
    }
    Err(SwarmStateError::Schema(format!(
        "swarm state schema is incomplete after the idempotent DDL (missing: {}); the database was \
         modified outside this build. Drop and re-create the empty state database, or restore it \
         from a backup",
        problems.join(", ")
    )))
}

/// Map a SQLx failure of the schema phase into [`SwarmStateError`].
///
/// The inner text is the driver's own diagnostic (never the DSN); nothing here
/// renders a credential.
pub(super) fn schema_failure(operation: &'static str, error: sqlx::Error) -> SwarmStateError {
    match classify(operation, error) {
        StoreError::Unavailable(detail)
        | StoreError::Conflict(detail)
        | StoreError::Unsupported(detail)
        | StoreError::NotFound(detail) => SwarmStateError::Schema(detail),
    }
}

#[cfg(test)]
#[path = "schema_test.rs"]
mod tests;
