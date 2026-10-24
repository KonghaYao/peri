//! H5c：wire 失败的客户端分类。只看领域字段，不依赖 message 文本。

use peri_acp::transport::types::AcpError;
use peri_acp_types::history_v2::{
    HistoryEffect, HistoryErrorData, HistoryErrorReason, HistoryRecovery,
};

use super::HistoryV2WireFailure;

fn failure(reason: HistoryErrorReason, recovery: HistoryRecovery) -> HistoryV2WireFailure {
    let data = HistoryErrorData::new(reason, recovery, HistoryEffect::unchanged());
    let error = AcpError::new(-32000, "history failure")
        .with_data(serde_json::to_value(data).expect("serialize"));
    HistoryV2WireFailure::from_error(&error)
}

#[test]
fn decodes_reason_and_recovery_from_domain_data() {
    let failure = failure(HistoryErrorReason::WindowExpired, HistoryRecovery::Reopen);
    assert_eq!(failure.code, -32000);
    assert_eq!(failure.reason, Some(HistoryErrorReason::WindowExpired));
    assert_eq!(failure.recovery, Some(HistoryRecovery::Reopen));
    assert_eq!(failure.label(), "window_expired");
    assert!(failure.requires_reopen());
}

#[test]
fn missing_domain_data_is_a_plain_failure() {
    let error = AcpError::new(-32603, "internal");
    let failure = HistoryV2WireFailure::from_error(&error);
    assert_eq!(failure.reason, None);
    assert_eq!(failure.recovery, None);
    assert_eq!(failure.label(), "history_error");
    assert!(!failure.requires_reopen());
    assert!(!failure.operation_in_progress());
    assert!(!failure.discard_only());
    assert!(!failure.negotiation_failure());
}

#[test]
fn reopen_reasons_and_hint_are_explicit() {
    for reason in [
        HistoryErrorReason::SnapshotInvalidated,
        HistoryErrorReason::WindowExpired,
        HistoryErrorReason::WindowClosed,
        HistoryErrorReason::TicketInvalid,
    ] {
        let failure = failure(reason, HistoryRecovery::None);
        assert!(failure.requires_reopen(), "{reason:?} must reopen");
    }
    // recovery 指示 reopen 时即使 reason 不在列表内也按 reopen 处理。
    let hinted = failure(
        HistoryErrorReason::BackendUnavailable,
        HistoryRecovery::Reopen,
    );
    assert!(hinted.requires_reopen());
}

#[test]
fn in_progress_op_is_not_reissued_or_discarded() {
    let failure = failure(
        HistoryErrorReason::OperationInProgress,
        HistoryRecovery::None,
    );
    assert!(failure.operation_in_progress());
    assert!(!failure.requires_reopen());
    assert!(!failure.discard_only());
}

#[test]
fn discard_only_reasons_keep_pages() {
    for reason in [
        HistoryErrorReason::InvalidCursor,
        HistoryErrorReason::CrossWindowCursor,
        HistoryErrorReason::AnchorUnavailable,
        HistoryErrorReason::RequestConflict,
        HistoryErrorReason::OperationExpired,
    ] {
        let failure = failure(reason, HistoryRecovery::Retry);
        assert!(failure.discard_only(), "{reason:?} must discard only");
        assert!(!failure.requires_reopen());
    }
}

#[test]
fn negotiation_failures_are_distinct() {
    for reason in [
        HistoryErrorReason::ProtocolNotNegotiated,
        HistoryErrorReason::UnsupportedVersion,
    ] {
        let failure = failure(reason, HistoryRecovery::Renegotiate);
        assert!(failure.negotiation_failure());
        assert!(!failure.discard_only());
    }
}

#[test]
fn label_covers_every_reason() {
    let labels = [
        (
            HistoryErrorReason::ProtocolNotNegotiated,
            "protocol_not_negotiated",
        ),
        (
            HistoryErrorReason::UnsupportedVersion,
            "unsupported_version",
        ),
        (HistoryErrorReason::InvalidCursor, "invalid_cursor"),
        (HistoryErrorReason::CrossWindowCursor, "cross_window_cursor"),
        (
            HistoryErrorReason::SnapshotInvalidated,
            "snapshot_invalidated",
        ),
        (HistoryErrorReason::WindowExpired, "window_expired"),
        (HistoryErrorReason::WindowClosed, "window_closed"),
        (HistoryErrorReason::TicketInvalid, "ticket_invalid"),
        (HistoryErrorReason::AnchorUnavailable, "anchor_unavailable"),
        (HistoryErrorReason::PermissionDenied, "permission_denied"),
        (HistoryErrorReason::NotFound, "not_found"),
        (HistoryErrorReason::IndexNotReady, "index_not_ready"),
        (HistoryErrorReason::SourceUnavailable, "source_unavailable"),
        (HistoryErrorReason::CapacityExceeded, "capacity_exceeded"),
        (HistoryErrorReason::BudgetExceeded, "budget_exceeded"),
        (
            HistoryErrorReason::BackendUnavailable,
            "backend_unavailable",
        ),
        (HistoryErrorReason::Timeout, "timeout"),
        (HistoryErrorReason::RequestConflict, "request_conflict"),
        (HistoryErrorReason::OperationExpired, "operation_expired"),
        (
            HistoryErrorReason::OperationInProgress,
            "operation_in_progress",
        ),
        (HistoryErrorReason::DeliveryExpired, "delivery_expired"),
        (HistoryErrorReason::DeliveryGap, "delivery_gap"),
        (HistoryErrorReason::Busy, "busy"),
        (HistoryErrorReason::InvalidRequest, "invalid_request"),
    ];
    for (reason, label) in labels {
        let failure = failure(reason, HistoryRecovery::None);
        assert_eq!(failure.label(), label);
    }
}
