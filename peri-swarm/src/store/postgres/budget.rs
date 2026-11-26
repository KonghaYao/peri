//! Budgets and failure classification for the PostgreSQL state store.

use std::future::Future;
use std::time::Duration;

use super::super::StoreError;

/// One pool acquisition or one statement may take this long. Matches the
/// session store's `REQUEST_BUDGET` (`peri-resources/.../connection.rs`).
pub(super) const REQUEST_BUDGET: Duration = Duration::from_secs(20);

/// A multi-statement write may take longer than one statement: every statement
/// is its own round trip on top of the transaction setup and commit.
pub(super) fn batch_budget(statements: usize) -> Duration {
    const ROUND_TRIP_ALLOWANCE: Duration = Duration::from_millis(250);
    const MAX_BATCH_BUDGET: Duration = Duration::from_secs(300);
    let additional = u32::try_from(statements.saturating_sub(1)).unwrap_or(u32::MAX);
    (REQUEST_BUDGET + ROUND_TRIP_ALLOWANCE * additional).min(MAX_BATCH_BUDGET)
}

/// Run one auto-commit statement under [`REQUEST_BUDGET`].
///
/// A timeout is reported as an unknown outcome: the statement may still have
/// been applied server-side, so it must not be retried blindly.
pub(super) async fn within_budget<T>(
    operation: &'static str,
    future: impl Future<Output = Result<T, sqlx::Error>>,
) -> Result<T, StoreError> {
    match peri_time::timeout(REQUEST_BUDGET, future).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => {
            tracing::error!(operation, error = %error, "swarm state store request failed");
            Err(classify(operation, error))
        }
        Err(_) => Err(StoreError::Unavailable(format!(
            "{operation}: swarm state store request exceeded {REQUEST_BUDGET:?}; the outcome is \
             unknown and must not be retried blindly"
        ))),
    }
}

/// Run one explicit transaction (or any multi-step write) under the batch
/// budget.
///
/// A timeout drops the transaction, so nothing is committed; the message says
/// so instead of claiming an unknown outcome.
pub(super) async fn within_batch<T>(
    operation: &'static str,
    statements: usize,
    future: impl Future<Output = Result<T, StoreError>>,
) -> Result<T, StoreError> {
    let budget = batch_budget(statements);
    match peri_time::timeout(budget, future).await {
        Ok(result) => result,
        Err(_) => Err(StoreError::Unavailable(format!(
            "{operation}: swarm state store write exceeded {budget:?}; the transaction was rolled \
             back and nothing was applied"
        ))),
    }
}

/// Fold a SQLx failure into [`StoreError`] without inventing success.
///
/// `Conflict` is the only class the caller may act on by reading again; every
/// other failure is `Unavailable` (never `NotFound`), so a corrupt or
/// unreachable database cannot be mistaken for absent data. `Unsupported` is
/// reserved for a store that rejects writes outright (read-only transaction).
pub(super) fn classify(operation: &'static str, error: sqlx::Error) -> StoreError {
    if let sqlx::Error::Database(database) = &error {
        let code = database.code().map(|code| code.into_owned());
        if database.is_unique_violation() {
            return StoreError::Conflict(format!("{operation}: {database}"));
        }
        match code.as_deref() {
            // read_only_sql_transaction
            Some("25006") => {
                return StoreError::Unsupported(format!(
                    "{operation}: swarm state store is read only: {database}"
                ))
            }
            // query_canceled (statement_timeout): the server aborted the statement.
            Some("57014") => {
                return StoreError::Unavailable(format!(
                    "{operation}: swarm state store cancelled the statement (statement_timeout): \
                     {database}"
                ))
            }
            // undefined_table: the owned schema is incomplete or was dropped.
            Some("42P01") => {
                return StoreError::Unavailable(format!(
                    "{operation}: swarm state store schema is incomplete: {database}"
                ))
            }
            _ => {}
        }
    }
    if matches!(error, sqlx::Error::PoolTimedOut) {
        return StoreError::Unavailable(format!(
            "{operation}: no swarm state store connection became available within {REQUEST_BUDGET:?}"
        ));
    }
    StoreError::Unavailable(format!("{operation}: {error}"))
}

/// A row this build wrote cannot be read back. Never `NotFound`: a decode
/// failure is evidence of corruption, not absence.
pub(super) fn unreadable(detail: impl std::fmt::Display) -> StoreError {
    StoreError::Unavailable(format!("unreadable swarm row: {detail}"))
}

/// A write that had to affect exactly one row affected another number.
pub(super) fn unexplained(operation: &'static str, affected: u64) -> StoreError {
    StoreError::Unavailable(format!(
        "{operation}: swarm state store reported {affected} affected rows where one was required"
    ))
}

/// A read that had to return exactly one row returned another number.
pub(super) fn unexpected_row_count(operation: &'static str, rows: usize) -> StoreError {
    StoreError::Unavailable(format!(
        "{operation}: swarm state store returned {rows} rows where one was required"
    ))
}

#[cfg(test)]
#[path = "budget_test.rs"]
mod tests;
