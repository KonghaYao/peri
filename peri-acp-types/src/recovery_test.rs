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
