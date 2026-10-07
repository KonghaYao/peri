use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::{
    messages::BaseMessage,
    session::MessagePolicy,
    session_resources::{work::*, SessionResourceResult},
    store::PersistedPayload,
};
use sqlx::{
    sqlite::{SqliteArguments, SqlitePoolOptions, SqliteRow},
    Arguments, Row, Sqlite, SqlitePool, TypeInfo, ValueRef,
};
use turso_serverless::{Error as SdkError, Value};

use super::super::{
    connection::RemoteTransport,
    generation::{ConnectionFactory, ConnectionGate},
    ledger::CREATE_OP_LEDGER_SQL,
    mutation::{FaultPlan, RemoteStore, StoreAccess},
    schema::StoreId,
    session_data::RemoteSessionData,
    sql::StatementSpec,
};
use crate::sessions::data::SessionDataPort;

#[path = "session_pending_work_test.rs"]
mod pending_work;

struct SqliteTransport {
    pool: SqlitePool,
}
fn arguments(spec: &StatementSpec) -> turso_serverless::Result<SqliteArguments> {
    let mut arguments = SqliteArguments::default();
    for value in &spec.params {
        let result = match value {
            Value::Text(value) => arguments.add(value.clone()),
            Value::Integer(value) => arguments.add(*value),
            Value::Blob(value) => arguments.add(value.clone()),
            Value::Null => arguments.add(Option::<String>::None),
            _ => return Err(SdkError::Http("unsupported work test value".into())),
        };
        result.map_err(|_| SdkError::Http("work test argument encoding failed".into()))?;
    }
    Ok(arguments)
}
fn rows(values: Vec<SqliteRow>) -> turso_serverless::Result<Vec<Vec<Value>>> {
    values
        .into_iter()
        .map(|row| {
            (0..row.len())
                .map(|index| {
                    let raw = row
                        .try_get_raw(index)
                        .map_err(|_| SdkError::Http("work test decoding failed".into()))?;
                    if raw.is_null() {
                        return Ok(Value::Null);
                    }
                    match raw.type_info().name() {
                        "INTEGER" => row.try_get::<i64, _>(index).map(Value::Integer),
                        "BLOB" => row.try_get::<Vec<u8>, _>(index).map(Value::Blob),
                        _ => row.try_get::<String, _>(index).map(Value::Text),
                    }
                    .map_err(|_| SdkError::Http("work test decoding failed".into()))
                })
                .collect()
        })
        .collect()
}
fn database_error(error: sqlx::Error) -> SdkError {
    match error {
        sqlx::Error::Database(error)
            if matches!(
                error.kind(),
                sqlx::error::ErrorKind::UniqueViolation
                    | sqlx::error::ErrorKind::NotNullViolation
                    | sqlx::error::ErrorKind::CheckViolation
                    | sqlx::error::ErrorKind::ForeignKeyViolation
            ) =>
        {
            SdkError::Constraint(error.to_string())
        }
        sqlx::Error::Database(error) => SdkError::Http(error.to_string()),
        _ => SdkError::Http("work SQL failure".into()),
    }
}
#[async_trait]
impl RemoteTransport for SqliteTransport {
    async fn sql_values(&self, spec: &StatementSpec) -> turso_serverless::Result<Vec<Vec<Value>>> {
        rows(
            sqlx::query_with::<Sqlite, _>(spec.sql, arguments(spec)?)
                .fetch_all(&self.pool)
                .await
                .map_err(database_error)?,
        )
    }
    async fn managed_batch(
        &self,
        statements: Vec<StatementSpec>,
    ) -> turso_serverless::Result<Vec<u64>> {
        let mut transaction = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(database_error)?;
        let mut counts = Vec::new();
        for (index, spec) in statements.iter().enumerate() {
            match sqlx::query_with::<Sqlite, _>(spec.sql, arguments(spec)?)
                .execute(&mut *transaction)
                .await
            {
                Ok(result) => counts.push(result.rows_affected()),
                Err(error) => {
                    transaction.rollback().await.map_err(database_error)?;
                    return Err(SdkError::BatchStatementFailed {
                        index,
                        error: Box::new(database_error(error)),
                        results: Vec::new(),
                    });
                }
            }
        }
        transaction.commit().await.map_err(database_error)?;
        Ok(counts)
    }
    async fn consistent_read(
        &self,
        statements: Vec<StatementSpec>,
    ) -> turso_serverless::Result<Vec<Vec<Vec<Value>>>> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        let mut batches = Vec::new();
        for spec in statements {
            batches.push(rows(
                sqlx::query_with::<Sqlite, _>(spec.sql, arguments(&spec)?)
                    .fetch_all(&mut *transaction)
                    .await
                    .map_err(database_error)?,
            )?);
        }
        transaction.commit().await.map_err(database_error)?;
        Ok(batches)
    }
    fn is_autocommit(&self) -> turso_serverless::Result<bool> {
        Ok(true)
    }
    async fn close(&self) -> turso_serverless::Result<()> {
        Ok(())
    }
}
struct Factory {
    pool: SqlitePool,
    gate: Arc<ConnectionGate>,
}
#[async_trait]
impl ConnectionFactory for Factory {
    async fn connect(&self) -> SessionResourceResult<RemoteStore> {
        Ok(RemoteStore::new(
            Arc::new(SqliteTransport {
                pool: self.pool.clone(),
            }),
            StoreAccess::ReadWrite,
            self.gate.mint(),
            self.gate.clone(),
        ))
    }
}
struct Fixture {
    directory: tempfile::TempDir,
    pool: SqlitePool,
    id: StoreId,
}
impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(directory.path().join("remote-work.db"))
            .create_if_missing(true)
            .busy_timeout(std::time::Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .unwrap();
        for spec in super::super::session_schema::initialization_plan() {
            sqlx::query(spec.sql).execute(&pool).await.unwrap();
        }
        let id = StoreId::mint();
        for spec in super::super::schema::initialization_plan(&id, "2026-10-06T00:00:00Z") {
            sqlx::query_with::<Sqlite, _>(spec.sql, arguments(&spec).unwrap())
                .execute(&pool)
                .await
                .unwrap();
        }
        for sql in [
            CREATE_OP_LEDGER_SQL,
            "INSERT INTO machines(id,name,identity_kind) VALUES ('machine','work test','known')",
            "INSERT INTO workspaces(id,machine_id,path,path_source) VALUES ('workspace','machine','/test','discovered')",
            "INSERT INTO threads(id,created_at,updated_at,workspace_id) VALUES ('work-session','2026-10-05T00:00:00Z','2026-10-05T00:00:00Z','workspace')",
        ] {
            sqlx::query(sql).execute(&pool).await.unwrap();
        }
        Self {
            directory,
            pool,
            id,
        }
    }
    async fn adapter(&self) -> RemoteSessionData {
        let gate = Arc::new(ConnectionGate::default());
        let factory = Arc::new(Factory {
            pool: self.pool.clone(),
            gate: gate.clone(),
        });
        let store = factory.connect().await.unwrap();
        RemoteSessionData::with_connection_for_test(self.id.clone(), store, factory, gate)
    }
}
#[path = "work_records/barrier_contract_test.rs"]
mod barrier_contracts;
#[path = "work_records/contracts_test.rs"]
mod contracts;
