//! PostgreSQL transport boundary for session storage. A batch owns its transaction.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use peri_acp_types::session_resources::SessionResourceResult;

use super::credentials::SessionStoreCredential;
use super::endpoint::RemoteEndpoint;
use super::failure::{self, RemoteFailureClass};
#[cfg(any(test, target_os = "emscripten"))]
use super::sql;
use super::sql::{StatementSpec, Value};

#[cfg(not(target_os = "emscripten"))]
#[path = "native_postgres.rs"]
mod native;
#[cfg(all(test, not(target_os = "emscripten")))]
pub(super) use native::PgTransport;

#[cfg(any(test, target_os = "emscripten"))]
#[path = "wasm_postgres.rs"]
mod wasm_postgres;

pub(super) const REQUEST_BUDGET: Duration = Duration::from_secs(20);

/// The HTTP transport submits one request per batch. Native SQLx needs a
/// round trip per statement, in addition to the transaction setup and commit.
pub(super) fn batch_budget(statements: usize) -> Duration {
    if cfg!(target_os = "emscripten") {
        return REQUEST_BUDGET;
    }
    const ROUND_TRIP_ALLOWANCE: Duration = Duration::from_millis(250);
    const MAX_BATCH_BUDGET: Duration = Duration::from_secs(300);
    let additional = u32::try_from(statements.saturating_sub(1)).unwrap_or(u32::MAX);
    (REQUEST_BUDGET + ROUND_TRIP_ALLOWANCE * additional).min(MAX_BATCH_BUDGET)
}

// Parent/child and lifecycle guards rely on mutation batches taking turns. A transaction starts
// at READ COMMITTED so each guard sees the previous writer's commit *after* taking this lock.
const LOCK_STORE: &str = "SELECT pg_catalog.pg_advisory_xact_lock(1346720329, pg_catalog.hashtext(pg_catalog.current_schema()))";

#[derive(Clone, Copy, Debug)]
pub(super) enum TransportError {
    Before(RemoteFailureClass),
    RolledBack {
        class: RemoteFailureClass,
        index: Option<usize>,
        unique: bool,
    },
    Uncertain(RemoteFailureClass),
}

impl TransportError {
    pub(super) fn class(&self) -> RemoteFailureClass {
        match *self {
            Self::Before(class) | Self::Uncertain(class) | Self::RolledBack { class, .. } => class,
        }
    }
}

pub(super) type TransportResult<T> = Result<T, TransportError>;

pub(super) enum Budgeted<T> {
    Done(T),
    Failed(TransportError),
    Exceeded,
}

pub(super) async fn within_budget<T>(
    future: impl Future<Output = TransportResult<T>>,
    budget: Duration,
) -> Budgeted<T> {
    match peri_time::timeout(budget, future).await {
        Ok(Ok(value)) => Budgeted::Done(value),
        Ok(Err(error)) => Budgeted::Failed(error),
        Err(_) => Budgeted::Exceeded,
    }
}

pub(super) async fn connect(
    endpoint: &RemoteEndpoint,
    credential: &SessionStoreCredential,
) -> SessionResourceResult<Arc<dyn RemoteTransport>> {
    #[cfg(target_os = "emscripten")]
    return wasm_postgres::connect(endpoint, credential).await;
    #[cfg(not(target_os = "emscripten"))]
    return native::connect(endpoint, credential).await;
}

#[async_trait]
pub(super) trait RemoteTransport: Send + Sync {
    async fn sql_values(&self, spec: &StatementSpec) -> TransportResult<Vec<Vec<Value>>>;
    async fn managed_batch(&self, statements: Vec<StatementSpec>) -> TransportResult<Vec<u64>>;
    async fn consistent_read(
        &self,
        statements: Vec<StatementSpec>,
    ) -> TransportResult<Vec<Vec<Vec<Value>>>>;
    async fn close(&self) -> TransportResult<()>;
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    #[test]
    fn batch_deadline_accounts_for_round_trips_and_stays_bounded() {
        assert_eq!(batch_budget(0), REQUEST_BUDGET);
        assert_eq!(batch_budget(1), REQUEST_BUDGET);
        if cfg!(target_os = "emscripten") {
            assert_eq!(batch_budget(usize::MAX), REQUEST_BUDGET);
        } else {
            assert_eq!(batch_budget(501), Duration::from_secs(145));
            assert_eq!(batch_budget(usize::MAX), Duration::from_secs(300));
        }
    }
}
