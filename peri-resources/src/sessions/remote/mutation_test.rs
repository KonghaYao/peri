//! `mutation` 的离线测试：确定性分类、只读拒绝与未决映射。全部不联网。

use peri_acp_types::session_resources::{
    MutationOutcome as DomainOutcome, SessionResourceErrorKind,
};
use peri_acp_types::thread::ThreadId;
use turso_serverless::Error as SdkError;
use turso_serverless::Value;

use super::failure::RemoteFailureClass;
use super::mutation::{
    classify_batch_failure, ensure_result_sets, sole_row, BatchFailure, MutationOutcome,
    StoreAccess,
};

fn managed_failure(index: usize, error: SdkError) -> SdkError {
    SdkError::BatchStatementFailed {
        index,
        error: Box::new(error),
        results: Vec::new(),
    }
}

fn rollback_failure(inner: SdkError) -> SdkError {
    SdkError::BatchRollbackFailed {
        error: Box::new(inner),
        rollback_error: Box::new(SdkError::Http("transport".to_owned())),
    }
}

#[test]
fn read_only_access_refuses_before_any_request() {
    let refuse = StoreAccess::ReadOnly.ensure_writable().unwrap_err();
    assert!(matches!(
        refuse.kind(),
        SessionResourceErrorKind::ReadOnlyStore
    ));
    assert_eq!(refuse.effect(), DomainOutcome::NotApplied);
    assert!(StoreAccess::ReadWrite.ensure_writable().is_ok());
}

#[test]
fn managed_batch_failure_is_determinate_not_applied() {
    let failure = classify_batch_failure(&managed_failure(
        2,
        SdkError::Constraint("unique".to_owned()),
    ));
    assert_eq!(
        failure,
        BatchFailure::NotApplied {
            class: RemoteFailureClass::Constraint,
            index: Some(2),
        }
    );

    // 资格写（第 0 条）自身失败：整批同样回滚，且能指出是哪条被拒。
    let qualification = classify_batch_failure(&managed_failure(
        0,
        SdkError::Error("no such table".to_owned()),
    ));
    assert_eq!(
        qualification,
        BatchFailure::NotApplied {
            class: RemoteFailureClass::ServerError,
            index: Some(0),
        }
    );
}

#[test]
fn qualification_conflict_is_only_recognized_on_the_first_statement() {
    let conflict = classify_batch_failure(&managed_failure(
        0,
        SdkError::Constraint("primary key".to_owned()),
    ));
    assert_eq!(conflict, BatchFailure::QualificationConflict);

    // 效果语句上的唯一键冲突是业务冲突，不是资格占用。
    let effect = classify_batch_failure(&managed_failure(
        1,
        SdkError::Constraint("primary key".to_owned()),
    ));
    assert_eq!(
        effect,
        BatchFailure::NotApplied {
            class: RemoteFailureClass::Constraint,
            index: Some(1),
        }
    );
}

#[test]
fn rollback_failure_is_never_reported_as_not_applied() {
    let failure = classify_batch_failure(&rollback_failure(managed_failure(
        0,
        SdkError::Constraint("primary key".to_owned()),
    )));
    assert_eq!(
        failure,
        BatchFailure::Unknown {
            class: RemoteFailureClass::Unknown,
        }
    );
}

#[test]
fn transport_busy_and_timeout_stay_unresolved() {
    for error in [
        SdkError::Http("connection reset".to_owned()),
        SdkError::Busy("locked".to_owned()),
        SdkError::BusySnapshot("snapshot".to_owned()),
        SdkError::Interrupt("interrupted".to_owned()),
    ] {
        match classify_batch_failure(&error) {
            BatchFailure::Unknown { .. } => {}
            other => panic!("expected unresolved for {error:?}, got {other:?}"),
        }
    }
}

#[test]
fn client_side_rejection_is_not_applied() {
    let failure = classify_batch_failure(&SdkError::ToSqlConversionFailure(Box::new(
        std::io::Error::other("bad param"),
    )));
    assert_eq!(
        failure,
        BatchFailure::NotApplied {
            class: RemoteFailureClass::Unsupported,
            index: None,
        }
    );
}

#[test]
fn unresolved_mutation_maps_to_persistence_uncertain() {
    let thread = ThreadId::from("s-uncertain");
    let error = MutationOutcome::Unknown {
        class: RemoteFailureClass::Transport,
    }
    .failure_error(Some(&thread))
    .expect("unknown must surface as an error");
    match error.kind() {
        SessionResourceErrorKind::PersistenceUncertain { thread_id } => {
            assert_eq!(thread_id.as_ref(), Some(&thread));
        }
        other => panic!("unresolved mutation must be uncertain, got {other:?}"),
    }
    assert_eq!(error.effect(), DomainOutcome::Unknown);

    // 确定未生效与封闭都不得升级成未决。
    for outcome in [
        MutationOutcome::NotApplied {
            class: RemoteFailureClass::Constraint,
            rejected_statement: Some(1),
        },
        MutationOutcome::ClosedNeverApplied,
    ] {
        let error = outcome.failure_error(Some(&thread)).expect("not applied");
        assert_eq!(error.effect(), DomainOutcome::NotApplied);
    }
}

/// 回复少了结果集是**错误**，不是「这段查询没有数据」：分类必须是 `Internal`（既不是
/// `NotFound` 那种「确实没有这一行」，也不是 `Corrupt` 那种「记录读不出来」）。
#[test]
fn missing_result_set_is_an_error_not_absence() {
    assert!(ensure_result_sets(2, 2).is_ok());
    for (expected, actual) in [(2usize, 1usize), (1, 0), (1, 2)] {
        let error = ensure_result_sets(expected, actual).unwrap_err();
        match error.kind() {
            SessionResourceErrorKind::Internal { detail } => {
                assert!(detail.contains(&expected.to_string()));
                assert!(detail.contains(&actual.to_string()));
            }
            other => panic!("incomplete reply must be Internal, got {other:?}"),
        }
        assert!(!matches!(
            error.kind(),
            SessionResourceErrorKind::NotFound
                | SessionResourceErrorKind::Corrupt { .. }
                | SessionResourceErrorKind::Unsupported
        ));
    }
}

/// 主键查询至多一行：多给一行同样是无法解释的回复，不静默取其中一行。
#[test]
fn single_row_reads_do_not_pick_a_row_from_many() {
    assert!(sole_row(Vec::new()).unwrap().is_none());

    let one = sole_row(vec![vec![Value::Integer(7)]]).unwrap();
    assert_eq!(one, Some(vec![Value::Integer(7)]));

    let error = sole_row(vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]).unwrap_err();
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::Internal { .. }
    ));
}
