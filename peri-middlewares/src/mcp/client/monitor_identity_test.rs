//! Pure policy tests for the builtin monitor owner-loss decision.

use std::time::Duration;

use rmcp::model::ErrorData;
use rmcp::service::ServiceError;

use super::monitor_owner_lost;

fn invalid_params() -> ErrorData {
    ErrorData::invalid_params("unknown task", None)
}

#[test]
fn closed_monitor_transport_is_a_definitive_loss() {
    assert!(monitor_owner_lost(
        true,
        false,
        &ServiceError::TransportClosed
    ));
    assert!(monitor_owner_lost(
        true,
        true,
        &ServiceError::Timeout {
            timeout: Duration::from_secs(1)
        }
    ));
}

#[test]
fn unknown_monitor_task_is_a_definitive_loss() {
    let error = ServiceError::McpError(invalid_params());
    assert!(monitor_owner_lost(true, false, &error));
}

#[test]
fn transient_monitor_errors_keep_the_bounded_retry() {
    assert!(!monitor_owner_lost(
        true,
        false,
        &ServiceError::Timeout {
            timeout: Duration::from_secs(1)
        }
    ));
    assert!(!monitor_owner_lost(
        true,
        false,
        &ServiceError::McpError(ErrorData::internal_error("owner overloaded", None))
    ));
}

#[test]
fn non_monitor_owners_keep_the_bounded_retry() {
    assert!(!monitor_owner_lost(
        false,
        true,
        &ServiceError::TransportClosed
    ));
    assert!(!monitor_owner_lost(
        false,
        false,
        &ServiceError::McpError(invalid_params())
    ));
}
