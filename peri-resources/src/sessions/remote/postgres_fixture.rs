//! A private PostgreSQL temporary schema per test. The server removes it when
//! the single test connection closes, including after a failed assertion.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use sqlx::{postgres::PgPoolOptions, PgPool};

use super::connection::{PgTransport, RemoteTransport, TransportResult};
use super::sql::{StatementSpec, Value};

pub(super) async fn test_pool() -> PgPool {
    let url = std::env::var("PERI_TEST_POSTGRES_URL")
        .expect("set PERI_TEST_POSTGRES_URL to a test database");
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .max_lifetime(None)
        .idle_timeout(None)
        .connect(&url)
        .await
        .expect("connect to test PostgreSQL database");
    sqlx::raw_sql(
        "CREATE TEMP TABLE peri_fixture (id integer);
         SET search_path TO pg_temp;
         CREATE FUNCTION pg_temp.reject_fixture_write() RETURNS trigger LANGUAGE plpgsql AS
             $$ BEGIN RAISE EXCEPTION 'injected constraint failure' USING ERRCODE = '23514'; END $$"
    ).execute(&pool).await.unwrap();
    pool
}

pub(super) struct PgTestTransport {
    pub(super) pool: PgPool,
    inner: PgTransport,
    pub(super) writes: AtomicUsize,
    pub(super) read_batches: AtomicUsize,
    pub(super) before_batch: Mutex<Option<String>>,
}

impl PgTestTransport {
    pub(super) fn for_test(pool: PgPool) -> Self {
        Self {
            inner: PgTransport::from_pool(pool.clone()),
            pool,
            writes: AtomicUsize::new(0),
            read_batches: AtomicUsize::new(0),
            before_batch: Mutex::new(None),
        }
    }
}

#[async_trait::async_trait]
impl RemoteTransport for PgTestTransport {
    async fn sql_values(&self, spec: &StatementSpec) -> TransportResult<Vec<Vec<Value>>> {
        self.inner.sql_values(spec).await
    }

    async fn managed_batch(&self, statements: Vec<StatementSpec>) -> TransportResult<Vec<u64>> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        let before = self.before_batch.lock().unwrap().take();
        if let Some(sql) = before {
            sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
                .execute(&self.pool)
                .await
                .expect("fixture intervention");
        }
        self.inner.managed_batch(statements).await
    }

    async fn consistent_read(
        &self,
        statements: Vec<StatementSpec>,
    ) -> TransportResult<Vec<Vec<Vec<Value>>>> {
        self.read_batches.fetch_add(1, Ordering::SeqCst);
        self.inner.consistent_read(statements).await
    }

    async fn close(&self) -> TransportResult<()> {
        Ok(()) // The fixture owns the shared server connection.
    }
}
