//! Independent Swarm state database on PostgreSQL.
//!
//! Swarm facts live in **their own** database (`design §4.1`). The Peri session
//! store keeps its `peri_store_meta` / canonical schema; this module never
//! probes, initializes or migrates it. It opens `PERI_SWARM_STATE_STORE` (a
//! PostgreSQL DSN) directly with SQLx, owns its DDL and records its own schema
//! identity in `swarm_meta`.
//!
//! ```text
//! Peri session store  (peri_store_meta, canonical shape)  <- Resources / AcpChild
//! Swarm state store   (swarm_meta, swarm_* tables)        <- this module
//! ```
//!
//! Opening is fail closed: an unparseable DSN, an unreachable server, a
//! non-empty database without this build's `swarm_meta`, or an unknown schema
//! version aborts the process instead of falling back to a local file or
//! silently reusing the session store. A known version with missing objects is
//! repaired by re-running the idempotent DDL; the identity row is never
//! rewritten.
//!
//! ## Statement shape
//!
//! Every statement is a `&'static str` in [`sql`] with `$n` binding; dynamic
//! values (ids, timestamps, JSON text) only travel as bound parameters. Writes
//! are either single auto-commit statements or one explicit transaction, so a
//! failure is observable and is never retried or renumbered here. A unique
//! violation is reported as [`StoreError::Conflict`] and left for the caller to
//! resolve by reading again.
//!
//! ## Field encoding
//!
//! The layout keeps the port contract fixed (`.peri/plans/S6a-report.md` §1.2):
//!
//! | [`crate::store`] row | column | encoding |
//! | --- | --- | --- |
//! | `state` / `kind` / `InputState` | `state` / `kind` / `state` | `as_str()` text |
//! | `environment` | `environment_json` | serde JSON text |
//! | `source` | `source` | serde JSON text |
//! | `payload` | `payload_json` | serde JSON text |
//! | `mcp_tools` | `mcp_json` | canonical [`McpToolSpec`](crate::domain::McpToolSpec) JSON array, `NULL` = none |
//! | `truncated` | `truncated` | `BOOLEAN` |
//! | `seq` / `last_item_seq` | `seq` / `last_item_seq` | `BIGINT` (i64 range) |
//!
//! `item_id` is deliberately **not** unique: the reader reuses one stable id for
//! a delta row and its later complete row (S6b3). `(logical_id, seq)` is the
//! primary key and `swarm_items_by_id` is a plain index.

mod budget;
mod codec;
mod item;
mod schema;
mod session;
mod sql;
mod store_impl;
mod turn;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;

pub use schema::SWARM_STATE_SCHEMA_VERSION;

use super::StoreError;
use crate::config::PostgresLocator;

/// One connection may wait for the pool or run one statement for this long.
use budget::REQUEST_BUDGET;

/// Fixed per-connection setup: keep catalog checks and unqualified relation
/// names in one schema, and bound server-side work when a caller disappears.
///
/// Same policy as the session store's PostgreSQL transport
/// (`peri-resources/src/sessions/remote/native_postgres.rs`): a stricter DBA
/// `statement_timeout` wins; the budget is only an upper bound.
const AFTER_CONNECT_SQL: &str = "SELECT pg_catalog.set_config('search_path', pg_catalog.quote_ident(pg_catalog.current_schema()), false),
        pg_catalog.set_config('statement_timeout',
            LEAST(COALESCE(NULLIF(setting::bigint, 0), $1), $1)::text, false)
     FROM pg_catalog.pg_settings WHERE name = 'statement_timeout'";

/// Where the independent Swarm state database is.
///
/// The locator carries the DSN (credentials included). Its `Debug` is redacted
/// and errors/logs only ever render [`PostgresLocator::target`].
#[derive(Clone, Debug)]
pub struct SwarmStateStoreConfig {
    pub locator: PostgresLocator,
}

/// Why the Swarm state database could not be opened.
#[derive(Debug, thiserror::Error)]
pub enum SwarmStateError {
    #[error("cannot connect to the swarm state store: {0}")]
    Connect(String),
    #[error(
        "swarm state store schema version {found} is not supported by this build (expected {expected}): \
         this build never upgrades or rewrites an existing swarm schema. Deploy the build that owns \
         version {found}, or provision an empty PostgreSQL database for PERI_SWARM_STATE_STORE \
         (see peri-swarm/README.md#升级与回退)"
    )]
    SchemaVersion { found: i64, expected: i64 },
    #[error("swarm state store schema check failed: {0}")]
    Schema(String),
}

/// Swarm state database over one shared SQLx pool.
///
/// The pool is shared by the gateway and every actor of the process. Each
/// statement runs on its own connection, so a failed auto-commit write is
/// observable without leaking a partial transaction.
pub struct SwarmStateStore {
    pool: PgPool,
}

impl std::fmt::Debug for SwarmStateStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SwarmStateStore")
            .finish_non_exhaustive()
    }
}

impl SwarmStateStore {
    /// Connect and verify (or create) the owned schema.
    ///
    /// The DSN is never rendered: failures name `postgres://host:port/db`.
    pub async fn open(config: &SwarmStateStoreConfig) -> Result<Self, SwarmStateError> {
        let target = config.locator.target();
        let options: PgConnectOptions = config.locator.as_str().parse().map_err(|error| {
            SwarmStateError::Connect(format!("{target}: unusable PostgreSQL DSN: {error}"))
        })?;
        let pool = PgPoolOptions::new()
            // The gateway and every actor append through this pool. Eight keeps a
            // burst of concurrent turns from queueing behind one connection while
            // staying modest against a server's connection limit.
            .max_connections(8)
            .min_connections(0)
            .acquire_timeout(REQUEST_BUDGET)
            .after_connect(|connection, _| {
                Box::pin(async move {
                    sqlx::query(AFTER_CONNECT_SQL)
                        .bind(REQUEST_BUDGET.as_millis() as i64)
                        .execute(connection)
                        .await?;
                    Ok(())
                })
            })
            .connect_with(options)
            .await
            .map_err(|error| SwarmStateError::Connect(format!("{target}: {error}")))?;
        let store = Self { pool };
        store.ensure_schema().await?;
        Ok(store)
    }

    /// Close the pool. `Ok` only proves every pooled connection was closed.
    pub async fn close(&self) -> Result<(), StoreError> {
        match peri_time::timeout(REQUEST_BUDGET, self.pool.close()).await {
            Ok(()) => Ok(()),
            Err(_) => Err(StoreError::Unavailable(format!(
                "close: swarm state store close exceeded {REQUEST_BUDGET:?}; the pool is marked closed"
            ))),
        }
    }

    /// The pool shared by the gateway and every actor.
    pub(super) fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Start one explicit transaction. A dropped transaction rolls back.
    pub(super) async fn begin(
        &self,
        operation: &'static str,
    ) -> Result<sqlx::Transaction<'_, sqlx::Postgres>, StoreError> {
        self.pool
            .begin()
            .await
            .map_err(|error| budget::classify(operation, error))
    }

    /// Verify the schema in one transaction: a concurrent first open either
    /// waits for this one or finds its `swarm_meta` row.
    async fn ensure_schema(&self) -> Result<(), SwarmStateError> {
        // `current_schema()` is NULL when the DSN's `search_path` names no
        // existing schema. The per-connection setup would then leave the unqualified
        // DDL nowhere to go, so fail before creating anything.
        let current: Option<String> = sqlx::query_scalar("SELECT current_schema()::text")
            .fetch_one(&self.pool)
            .await
            .map_err(|error| schema::schema_failure("current schema", error))?;
        if current.is_none() {
            return Err(SwarmStateError::Schema(
                "the DSN's search_path does not resolve to an existing schema; name an existing \
                 schema in the DSN `options` parameter or remove the `search_path` option"
                    .to_owned(),
            ));
        }
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|error| schema::schema_failure("begin schema check", error))?;
        schema::ensure_schema(&mut transaction).await?;
        transaction
            .commit()
            .await
            .map_err(|error| schema::schema_failure("commit schema check", error))
    }
}

/// Commit one explicit transaction, classifying a commit failure.
pub(super) async fn commit(
    operation: &'static str,
    transaction: sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<(), StoreError> {
    transaction
        .commit()
        .await
        .map_err(|error| budget::classify(operation, error))
}

/// Roll back one explicit transaction. A failed rollback is already reported by
/// the caller's failure path, so this only logs.
pub(super) async fn rollback(
    operation: &'static str,
    transaction: sqlx::Transaction<'_, sqlx::Postgres>,
) {
    if let Err(error) = transaction.rollback().await {
        tracing::warn!(operation, error = %error, "swarm state store rollback failed");
    }
}

#[cfg(test)]
#[path = "integration_test.rs"]
mod integration_tests;
