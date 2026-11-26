//! `mutation` 的离线测试：确定性分类、只读拒绝与未决映射。全部不联网。

use super::connection::TransportError;
use super::sql::Value;
use peri_acp_types::session_resources::{
    MutationOutcome as DomainOutcome, SessionResourceErrorKind,
};
use peri_acp_types::thread::ThreadId;

use super::failure::RemoteFailureClass;
use super::mutation::BatchFailure;
use super::mutation::MutationOutcome;
use super::mutation::StoreAccess;

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
fn only_a_confirmed_first_statement_unique_violation_identifies_a_replay() {
    use RemoteFailureClass::{Constraint, ServerError};
    for (class, index, unique, expected) in [
        (
            Constraint,
            Some(0),
            true,
            BatchFailure::QualificationConflict,
        ),
        (
            Constraint,
            Some(0),
            false,
            BatchFailure::NotApplied {
                class: Constraint,
                index: Some(0),
            },
        ),
        (
            Constraint,
            Some(2),
            true,
            BatchFailure::NotApplied {
                class: Constraint,
                index: Some(2),
            },
        ),
        (
            ServerError,
            None,
            false,
            BatchFailure::NotApplied {
                class: ServerError,
                index: None,
            },
        ),
    ] {
        assert_eq!(
            super::mutation::classify_batch_failure(&TransportError::RolledBack {
                class,
                index,
                unique
            }),
            expected
        );
    }
}

#[test]
fn a_lost_commit_response_stays_unknown_and_a_pre_send_failure_does_not() {
    use RemoteFailureClass::Transport;
    assert_eq!(
        super::mutation::classify_batch_failure(&TransportError::Uncertain(Transport)),
        BatchFailure::Unknown { class: Transport }
    );
    assert_eq!(
        super::mutation::classify_batch_failure(&TransportError::Before(Transport)),
        BatchFailure::NotApplied {
            class: Transport,
            index: None
        }
    );
}

#[test]
fn postgresql_diagnostics_are_classified_without_server_text() {
    use RemoteFailureClass::*;
    for (sqlstate, expected) in [
        ("23505", Constraint),
        ("23503", Constraint),
        ("40001", Busy),
        ("40P01", Busy),
        ("28P01", AuthRejected),
        ("25006", Readonly),
        ("08006", Transport),
        ("XX001", Corrupt),
    ] {
        assert_eq!(super::failure::sqlstate_class(sqlstate), expected);
    }
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
    assert!(super::mutation::ensure_result_sets(2, 2).is_ok());
    for (expected, actual) in [(2usize, 1usize), (1, 0), (1, 2)] {
        let error = super::mutation::ensure_result_sets(expected, actual).unwrap_err();
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
    assert!(super::mutation::sole_row(Vec::new()).unwrap().is_none());

    let one = super::mutation::sole_row(vec![vec![Value::Integer(7)]]).unwrap();
    assert_eq!(one, Some(vec![Value::Integer(7)]));

    let error = super::mutation::sole_row(vec![vec![Value::Integer(1)], vec![Value::Integer(2)]])
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::Internal { .. }
    ));
}
