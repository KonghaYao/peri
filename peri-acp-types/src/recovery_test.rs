//! 恢复契约的类型与 serde 兼容：分类映射、报告容器与 MQ 来源字面量。

use super::*;

fn resource(kind: SessionResourceErrorKind) -> SessionResourceError {
    SessionResourceError::new(kind)
}

#[test]
fn recovery_error_preserves_the_storage_classification() {
    for (kind, expected) in [
        (
            SessionResourceErrorKind::NotFound,
            RecoveryErrorKind::NotFound,
        ),
        (
            SessionResourceErrorKind::Corrupt {
                detail: "cyclic thread ancestry".to_owned(),
            },
            RecoveryErrorKind::Corrupt,
        ),
        (
            SessionResourceErrorKind::ReadOnlyStore,
            RecoveryErrorKind::ReadOnly,
        ),
        (
            SessionResourceErrorKind::Unsupported,
            RecoveryErrorKind::Unsupported,
        ),
        (
            SessionResourceErrorKind::Unavailable {
                detail: "store is unavailable".to_owned(),
            },
            RecoveryErrorKind::Unavailable,
        ),
    ] {
        let error = RecoveryError::from(resource(kind));
        assert_eq!(error.kind(), expected);
        assert!(error.thread_id().is_none());
    }
}

#[test]
fn recovery_error_keeps_the_original_source_instead_of_a_string() {
    let error = RecoveryError::new(
        RecoveryErrorKind::Corrupt,
        Some("thread-a".to_owned()),
        resource(SessionResourceErrorKind::Corrupt {
            detail: "thread ancestry is missing".to_owned(),
        }),
    );
    assert!(matches!(
        error.source().kind(),
        SessionResourceErrorKind::Corrupt { .. }
    ));
    assert_eq!(error.thread_id().map(String::as_str), Some("thread-a"));
    assert_eq!(
        error.to_string(),
        "corrupt (thread-a): session data is corrupt: thread ancestry is missing"
    );
}

#[test]
fn superseded_is_not_a_storage_failure() {
    let error = RecoveryError::superseded(&"thread-a".to_owned());
    assert_eq!(error.kind(), RecoveryErrorKind::Superseded);
}

#[test]
fn report_defaults_to_an_empty_result() {
    let report = RecoveryReport::default();
    assert!(report.recovered.is_empty());
    assert!(report.reused.is_empty());
    assert!(report.skipped.is_empty());
    assert!(report.failures.is_empty());
    assert!(
        matches!(
            report.peripheral_tasks,
            crate::ports::PeripheralTasks::Unknown { .. }
        ),
        "未查询不能默认为「没有外围任务」: {:?}",
        report.peripheral_tasks
    );
}

fn report_with(failures: Vec<RecoveryFailure>) -> RecoveryReport {
    RecoveryReport {
        root_thread_id: "root".to_owned(),
        entered_thread_id: "entered".to_owned(),
        recovered: vec!["root".to_owned()],
        reused: vec!["reused-child".to_owned()],
        skipped: vec![RecoverySkip {
            thread_id: "idle-child".to_owned(),
            reason: RecoverySkipReason::Idle,
        }],
        failures,
        peripheral_tasks: Default::default(),
    }
}

#[test]
fn recovery_view_from_report_marks_partial() {
    let view = RecoveryView::from_report(&report_with(vec![RecoveryFailure {
        thread_id: "broken-child".to_owned(),
        kind: RecoveryErrorKind::Unavailable,
        detail: "store is unavailable".to_owned(),
    }]));
    assert_eq!(view.status, RecoveryViewStatus::Partial);
    assert!(!view.execution_blocked, "partial 不阻断进入会话执行");
    assert_eq!(view.recovered, vec!["root".to_owned()]);
    assert_eq!(view.reused, vec!["reused-child".to_owned()]);
    assert_eq!(view.failures.len(), 1);
    assert!(!view.failures[0].execution_blocked);
    assert!(view.error.is_none());

    let json = serde_json::to_value(&view).unwrap();
    assert_eq!(json["status"], "partial");
    assert_eq!(json["rootThreadId"], "root");
    assert_eq!(json["enteredThreadId"], "entered");
    assert_eq!(json["skipped"][0]["threadId"], "idle-child");
    assert_eq!(json["skipped"][0]["reason"], "idle");
    assert_eq!(json["recovered"][0], "root");
}

#[test]
fn recovery_view_from_report_without_failures_is_ok() {
    let view = RecoveryView::from_report(&report_with(Vec::new()));
    assert_eq!(view.status, RecoveryViewStatus::Ok);
    assert!(!view.execution_blocked);
    assert!(view.should_attach());
}

#[test]
fn idle_report_is_not_attached_to_the_wire() {
    let report = RecoveryReport {
        root_thread_id: "root".to_owned(),
        entered_thread_id: "root".to_owned(),
        skipped: vec![RecoverySkip {
            thread_id: "root".to_owned(),
            reason: RecoverySkipReason::Idle,
        }],
        ..Default::default()
    };
    let view = RecoveryView::from_report(&report);
    assert_eq!(view.status, RecoveryViewStatus::Ok);
    assert!(!view.should_attach(), "普通 idle load 不产生恢复字段");
}

#[test]
fn recovery_view_from_error_marks_blocked_kinds() {
    for (kind, blocked) in [
        (RecoveryErrorKind::Corrupt, true),
        (RecoveryErrorKind::ReadOnly, true),
        (RecoveryErrorKind::NotFound, true),
        (RecoveryErrorKind::Unavailable, false),
        (RecoveryErrorKind::Superseded, false),
        (RecoveryErrorKind::Unsupported, false),
    ] {
        let error = RecoveryError::new(
            kind,
            Some("thread-a".to_owned()),
            SessionResourceError::new(SessionResourceErrorKind::Corrupt {
                detail: "broken ancestry".to_owned(),
            }),
        );
        let view = RecoveryView::from_error(&"thread-a".to_owned(), &error);
        assert_eq!(view.status, RecoveryViewStatus::Failed);
        assert_eq!(view.execution_blocked, blocked, "{kind:?}");
        assert_eq!(view.error.as_ref().unwrap().execution_blocked, blocked);
        assert_eq!(view.entered_thread_id, "thread-a");
        assert_eq!(view.root_thread_id.as_deref(), Some("thread-a"));
    }
}

#[test]
fn recovery_view_roundtrips_on_the_wire() {
    let view = RecoveryView::from_report(&report_with(vec![RecoveryFailure {
        thread_id: "broken-child".to_owned(),
        kind: RecoveryErrorKind::Corrupt,
        detail: "missing parent".to_owned(),
    }]));
    let encoded = serde_json::to_value(&view).unwrap();
    let decoded: RecoveryView = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded, view);
    assert_eq!(decoded.failures[0].kind, RecoveryErrorKind::Corrupt);
    assert!(decoded.failures[0].execution_blocked);
}
