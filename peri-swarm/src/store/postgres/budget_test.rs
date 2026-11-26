//! Budget and failure-classification tests. No database is contacted.

use std::borrow::Cow;
use std::time::Duration;

use sqlx::error::ErrorKind;

use super::super::commit_outcome_unknown;
use super::{
    batch_budget, classify, unexpected_row_count, unexplained, unreadable, within_batch_for,
    REQUEST_BUDGET,
};
use crate::store::StoreError;

/// A synthetic driver failure with a chosen SQLSTATE.
#[derive(Debug)]
struct TestDatabaseError {
    code: &'static str,
    inner: std::io::Error,
}

impl TestDatabaseError {
    fn new(code: &'static str) -> Self {
        Self {
            code,
            inner: std::io::Error::other(format!("synthetic {code}")),
        }
    }
}

impl std::fmt::Display for TestDatabaseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "synthetic database error {}", self.code)
    }
}

impl std::error::Error for TestDatabaseError {}

impl sqlx::error::DatabaseError for TestDatabaseError {
    fn message(&self) -> &str {
        "synthetic database error"
    }

    fn code(&self) -> Option<Cow<'_, str>> {
        Some(Cow::Borrowed(self.code))
    }

    fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
        &self.inner
    }

    fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
        &mut self.inner
    }

    fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
        Box::new(self.inner)
    }

    fn kind(&self) -> ErrorKind {
        // The two codes under test: only a unique violation maps to its own kind.
        match self.code {
            "23505" => ErrorKind::UniqueViolation,
            _ => ErrorKind::Other,
        }
    }
}

fn database_error(code: &'static str) -> sqlx::Error {
    sqlx::Error::Database(Box::new(TestDatabaseError::new(code)))
}

#[test]
fn one_statement_gets_the_request_budget() {
    assert_eq!(batch_budget(0), REQUEST_BUDGET);
    assert_eq!(batch_budget(1), REQUEST_BUDGET);
}

#[test]
fn each_additional_statement_gets_one_round_trip() {
    assert_eq!(
        batch_budget(3),
        Duration::from_secs(20) + Duration::from_millis(500)
    );
    assert_eq!(
        batch_budget(9),
        Duration::from_secs(20) + Duration::from_secs(2)
    );
}

#[test]
fn the_batch_budget_is_capped() {
    assert_eq!(batch_budget(usize::MAX), Duration::from_secs(300));
    assert_eq!(batch_budget(2000), Duration::from_secs(300));
}

#[test]
fn a_unique_violation_is_a_conflict() {
    let error = classify("append_item", database_error("23505"));
    assert!(
        matches!(error, StoreError::Conflict(ref detail) if detail.contains("append_item")),
        "{error:?}"
    );
}

#[test]
fn a_read_only_transaction_is_unsupported() {
    let error = classify("append_item", database_error("25006"));
    assert!(
        matches!(error, StoreError::Unsupported(ref detail) if detail.contains("read only")),
        "{error:?}"
    );
}

#[test]
fn a_cancelled_statement_is_unavailable_and_names_the_timeout() {
    let error = classify("append_item", database_error("57014"));
    assert!(
        matches!(error, StoreError::Unavailable(ref detail) if detail.contains("statement_timeout")),
        "{error:?}"
    );
}

#[test]
fn a_missing_relation_is_unavailable_not_not_found() {
    let error = classify("get_session", database_error("42P01"));
    assert!(
        matches!(error, StoreError::Unavailable(ref detail) if detail.contains("incomplete")),
        "{error:?}"
    );
}

#[test]
fn other_server_errors_are_unavailable_and_keep_the_driver_text() {
    let error = classify("get_session", database_error("XX000"));
    assert!(
        matches!(error, StoreError::Unavailable(ref detail) if detail.contains("XX000") || detail.contains("synthetic")),
        "{error:?}"
    );
}

#[test]
fn pool_exhaustion_is_unavailable() {
    let error = classify("get_session", sqlx::Error::PoolTimedOut);
    assert!(
        matches!(error, StoreError::Unavailable(ref detail) if detail.contains("connection")),
        "{error:?}"
    );
}

#[test]
fn a_missing_row_is_never_reported_as_not_found() {
    // `NotFound` only ever comes from an explicit 0-row contract decision.
    let error = classify("get_session", sqlx::Error::RowNotFound);
    assert!(matches!(error, StoreError::Unavailable(_)), "{error:?}");
}

#[test]
fn a_protocol_failure_is_unavailable() {
    let error = classify("get_session", sqlx::Error::Protocol("bad frame".to_owned()));
    assert!(matches!(error, StoreError::Unavailable(_)), "{error:?}");
}

#[test]
fn helper_messages_name_the_operation_and_the_requirement() {
    let unreadable = unreadable("seq is not an integer");
    assert_eq!(
        unreadable.to_string(),
        "swarm store unavailable: unreadable swarm row: seq is not an integer"
    );
    let unexplained = unexplained("append_item", 0);
    assert!(
        unexplained.to_string().contains("0 affected rows"),
        "{unexplained}"
    );
    let rows = unexpected_row_count("get_session", 2);
    assert!(rows.to_string().contains("2 rows"), "{rows}");
}

#[tokio::test]
async fn a_batch_that_never_reaches_commit_promises_nothing_was_applied() {
    // The bounded block must stay uncommitted and return its transaction; a
    // timeout there is safely retryable, unlike a COMMIT timeout.
    let outcome: Result<i32, StoreError> =
        within_batch_for(Duration::from_millis(20), "append_items", async {
            peri_time::sleep(Duration::from_millis(200)).await;
            Ok(7)
        })
        .await;
    let error = outcome.expect_err("the budget must expire");
    let message = error.to_string();
    for hint in [
        "append_items",
        "did not reach COMMIT",
        "nothing was applied",
        "cannot duplicate rows",
    ] {
        assert!(message.contains(hint), "missing {hint}: {message}");
    }
    assert!(
        !message.contains("unknown"),
        "a pre-commit timeout is a known outcome: {message}"
    );
}

#[tokio::test]
async fn a_batch_inside_its_budget_returns_its_value() {
    let value = within_batch_for(Duration::from_secs(5), "append_items", async {
        Ok::<_, StoreError>("transaction")
    })
    .await
    .expect("within budget");
    assert_eq!(value, "transaction");
}

#[test]
fn a_commit_without_a_reply_reports_an_unknown_outcome() {
    // tokio polls the inner future first, so a real stuck COMMIT cannot be
    // produced deterministically in a test; the branch's contract is pinned here.
    let error = commit_outcome_unknown("append_items", Duration::from_secs(20));
    let message = error.to_string();
    for hint in [
        "append_items",
        "COMMIT was sent",
        "outcome is unknown",
        "may have committed",
        "Re-read the rows by key",
    ] {
        assert!(message.contains(hint), "missing {hint}: {message}");
    }
    assert!(
        !message.contains("nothing was applied"),
        "a sent COMMIT must not promise an empty result: {message}"
    );
}
