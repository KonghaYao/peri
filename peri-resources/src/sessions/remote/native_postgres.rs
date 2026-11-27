//! Native PostgreSQL transport. SQLx binds parameters using PostgreSQL's binary protocol.

use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::session_resources::SessionResourceResult;
use sqlx::encode::IsNull;
use sqlx::error::BoxDynError;
use sqlx::postgres::types::Oid;
use sqlx::postgres::{
    PgArgumentBuffer, PgArguments, PgConnectOptions, PgPoolOptions, PgRow, PgTypeInfo,
};
use sqlx::query::Query;
use sqlx::{Column, Encode, PgPool, Postgres, Row, Type, TypeInfo, ValueRef};

use super::{
    failure, within_budget, Budgeted, RemoteEndpoint, RemoteFailureClass, RemoteTransport,
    SessionStoreCredential, SessionStorePoolPolicy, StatementSpec, TransportError, TransportResult,
    Value, LOCK_STORE, REQUEST_BUDGET,
};

pub(in super::super) struct PgTransport {
    pool: PgPool,
}

impl PgTransport {
    pub(in super::super) fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }
}

pub(super) async fn connect(
    endpoint: &RemoteEndpoint,
    credential: &SessionStoreCredential,
    pool_policy: SessionStorePoolPolicy,
) -> SessionResourceResult<Arc<dyn RemoteTransport>> {
    let mut options: PgConnectOptions = endpoint
        .sdk_url()
        .parse()
        .map_err(|_| RemoteFailureClass::Unsupported.into_session_resource_error())?;
    if let Some(password) = credential.expose() {
        options = options.password(password);
    }
    let pool = PgPoolOptions::new()
        // One facade shares the pool across sessions, workspace and OAuth ports.
        // A read can proceed alongside the store's serialized mutation. The cap is
        // chosen by the deployment: a single facade keeps 2, the Swarm gateway
        // raises it for many concurrent sessions. `min_connections(0)` keeps the
        // cap from reserving server slots.
        .max_connections(pool_policy.max_connections())
        .min_connections(0)
        .acquire_timeout(REQUEST_BUDGET)
        .after_connect(|conn, _| Box::pin(async move {
            // Keep catalog checks, advisory lock and unqualified relation names in one schema.
            // Bound backend work when the caller drops an in-flight query. Respect stricter DBA limits.
            sqlx::query("SELECT pg_catalog.set_config('search_path', pg_catalog.quote_ident(pg_catalog.current_schema()), false),
                        pg_catalog.set_config('statement_timeout',
                            LEAST(COALESCE(NULLIF(setting::bigint, 0), $1), $1)::text, false)
                     FROM pg_catalog.pg_settings WHERE name = 'statement_timeout'")
                .bind(REQUEST_BUDGET.as_millis() as i64)
                .execute(conn).await?;
            Ok(())
        }));
    match within_budget(
        async { pool.connect_with(options).await.map_err(before) },
        REQUEST_BUDGET,
    )
    .await
    {
        Budgeted::Done(pool) => Ok(Arc::new(PgTransport::from_pool(pool))),
        Budgeted::Failed(error) => Err(failure::from_transport_error(&error)),
        Budgeted::Exceeded => Err(RemoteFailureClass::Timeout.into_session_resource_error()),
    }
}

fn class(error: &sqlx::Error) -> RemoteFailureClass {
    use RemoteFailureClass::*;
    match error {
        sqlx::Error::Database(db) => db
            .code()
            .as_deref()
            .map(failure::sqlstate_class)
            .unwrap_or(ServerError),
        sqlx::Error::PoolTimedOut => Timeout,
        sqlx::Error::Io(_)
        | sqlx::Error::Tls(_)
        | sqlx::Error::PoolClosed
        | sqlx::Error::Protocol(_) => Transport,
        sqlx::Error::RowNotFound => NotFound,
        sqlx::Error::Encode(_)
        | sqlx::Error::Decode(_)
        | sqlx::Error::ColumnDecode { .. }
        | sqlx::Error::Configuration(_) => Unsupported,
        _ => Unknown,
    }
}

fn before(error: sqlx::Error) -> TransportError {
    TransportError::Before(class(&error))
}

// A NULL can belong to columns of different types. OID zero asks the server to infer
// the parameter type from its SQL context; the wire representation contains no bytes.
struct UntypedNull;
impl Type<Postgres> for UntypedNull {
    fn type_info() -> PgTypeInfo {
        PgTypeInfo::with_oid(Oid(0))
    }
}
impl Encode<'_, Postgres> for UntypedNull {
    fn encode_by_ref(&self, _: &mut PgArgumentBuffer) -> Result<IsNull, BoxDynError> {
        Ok(IsNull::Yes)
    }
}

fn query(spec: &StatementSpec) -> Query<'_, Postgres, PgArguments> {
    let mut query = sqlx::query(spec.sql);
    for value in &spec.params {
        query = match value {
            Value::Null => query.bind(UntypedNull),
            Value::Integer(value) => query.bind(*value),
            Value::Real(value) => query.bind(*value),
            Value::Text(value) => query.bind(value.as_str()),
            Value::Blob(value) => query.bind(value.as_slice()),
        };
    }
    query
}

fn decode_rows(rows: Vec<PgRow>) -> TransportResult<Vec<Vec<Value>>> {
    rows.into_iter()
        .map(|row| {
            row.columns()
                .iter()
                .enumerate()
                .map(|(index, column)| {
                    if row.try_get_raw(index).map_err(before)?.is_null() {
                        return Ok(Value::Null);
                    }
                    match column.type_info().name() {
                        "BOOL" => row
                            .try_get::<bool, _>(index)
                            .map(|v| Value::Integer(i64::from(v))),
                        "INT2" => row
                            .try_get::<i16, _>(index)
                            .map(|v| Value::Integer(i64::from(v))),
                        "INT4" => row
                            .try_get::<i32, _>(index)
                            .map(|v| Value::Integer(i64::from(v))),
                        "INT8" => row.try_get(index).map(Value::Integer),
                        "FLOAT4" => row
                            .try_get::<f32, _>(index)
                            .map(|v| Value::Real(f64::from(v))),
                        "FLOAT8" => row.try_get(index).map(Value::Real),
                        "BYTEA" => row.try_get(index).map(Value::Blob),
                        "TEXT" | "VARCHAR" | "NAME" | "CHAR" | "BPCHAR" | "UNKNOWN" => {
                            row.try_get(index).map(Value::Text)
                        }
                        _ => return Err(TransportError::Before(RemoteFailureClass::Unsupported)),
                    }
                    .map_err(before)
                })
                .collect()
        })
        .collect()
}

#[async_trait]
impl RemoteTransport for PgTransport {
    async fn sql_values(&self, spec: &StatementSpec) -> TransportResult<Vec<Vec<Value>>> {
        decode_rows(query(spec).fetch_all(&self.pool).await.map_err(before)?)
    }

    async fn managed_batch(&self, statements: Vec<StatementSpec>) -> TransportResult<Vec<u64>> {
        let mut tx = self
            .pool
            .begin_with("BEGIN ISOLATION LEVEL READ COMMITTED")
            .await
            .map_err(before)?;
        sqlx::query(LOCK_STORE)
            .execute(&mut *tx)
            .await
            .map_err(before)?;
        let mut counts = Vec::with_capacity(statements.len());
        for (index, spec) in statements.iter().enumerate() {
            match peri_time::timeout(REQUEST_BUDGET, query(spec).execute(&mut *tx)).await {
                Err(_) => return Err(TransportError::Uncertain(RemoteFailureClass::Timeout)),
                Ok(Ok(done)) => counts.push(done.rows_affected()),
                Ok(Err(error)) => {
                    let unique = error
                        .as_database_error()
                        .is_some_and(|db| db.is_unique_violation());
                    let class = class(&error);
                    tx.rollback()
                        .await
                        .map_err(|_| TransportError::Uncertain(RemoteFailureClass::Unknown))?;
                    return Err(TransportError::RolledBack {
                        class,
                        index: Some(index),
                        unique,
                    });
                }
            }
        }
        tx.commit().await.map_err(|error| {
            let class = class(&error);
            if error.as_database_error().is_some()
                && !matches!(
                    class,
                    RemoteFailureClass::Transport | RemoteFailureClass::Unknown
                )
            {
                TransportError::RolledBack {
                    class,
                    index: None,
                    unique: false,
                }
            } else {
                TransportError::Uncertain(class)
            }
        })?;
        Ok(counts)
    }

    async fn consistent_read(
        &self,
        statements: Vec<StatementSpec>,
    ) -> TransportResult<Vec<Vec<Vec<Value>>>> {
        let mut tx = self
            .pool
            .begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .await
            .map_err(before)?;
        let mut results = Vec::with_capacity(statements.len());
        for spec in &statements {
            results.push(decode_rows(
                peri_time::timeout(REQUEST_BUDGET, query(spec).fetch_all(&mut *tx))
                    .await
                    .map_err(|_| TransportError::Before(RemoteFailureClass::Timeout))?
                    .map_err(before)?,
            )?);
        }
        tx.commit().await.map_err(before)?;
        Ok(results)
    }

    async fn close(&self) -> TransportResult<()> {
        self.pool.close().await;
        Ok(())
    }
}

#[cfg(test)]
#[path = "native_postgres_test.rs"]
mod tests;
