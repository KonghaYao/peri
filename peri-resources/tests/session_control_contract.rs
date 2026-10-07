use std::{path::Path, sync::Arc};

use peri_acp_types::{
    identity::AttemptId,
    session::TurnId,
    session_resources::{
        ControlAction, ControlAttempt, ControlCommand, ControlDecision, ControlReceipt,
        ControlRejection, ControlResolution, ControlState, ControlStatus, FrozenSnapshotBytes,
        NewSession, NewSessionMeta, SessionResourceErrorKind, SessionResources,
    },
    workspace::SessionBinding,
};
use tempfile::TempDir;

async fn open(path: &Path) -> Arc<dyn SessionResources> {
    Arc::new(
        peri_resources::sessions::SessionResourcesImpl::open(path)
            .await
            .unwrap(),
    )
}

async fn fixture() -> (TempDir, Arc<dyn SessionResources>) {
    let directory = tempfile::tempdir().unwrap();
    let resources = open(&directory.path().join("sessions.db")).await;
    let workspace = resources.resolve_workspace(directory.path()).await.unwrap();
    resources
        .create_session(&NewSession {
            thread_id: "control-session".into(),
            created_at: "2026-10-05T00:00:00Z".into(),
            meta: NewSessionMeta {
                title: None,
                cwd: workspace.cwd.to_string_lossy().into_owned(),
                parent_thread_id: None,
                hidden: false,
                cancel_policy: Default::default(),
                snapshot_at_message_id: None,
            },
            binding: SessionBinding::from_workspace(&workspace),
            frozen: FrozenSnapshotBytes::new(r#"{"v":1}"#),
        })
        .await
        .unwrap();
    (directory, resources)
}

fn command(command_id: &str, state: &ControlState, action: ControlAction) -> ControlCommand {
    ControlCommand {
        session_id: "control-session".into(),
        command_id: command_id.into(),
        expected_lifecycle: state.lifecycle,
        expected_revision: state.revision,
        expected_control_generation: state.control_generation,
        action,
    }
}

async fn apply(
    resources: &dyn SessionResources,
    command_id: &str,
    action: ControlAction,
) -> ControlReceipt {
    let state = resources
        .load_session_control(&"control-session".into())
        .await
        .unwrap();
    resources
        .apply_session_control(&command(command_id, &state, action))
        .await
        .unwrap()
}

#[tokio::test]
async fn control_replay_returns_original_receipt_after_resume_and_reopen() {
    let (directory, resources) = fixture().await;
    let pause = command("pause", &ControlState::default(), ControlAction::Pause);
    let receipt = resources.apply_session_control(&pause).await.unwrap();
    apply(resources.as_ref(), "resume", ControlAction::Resume).await;
    let reopened = open(&directory.path().join("sessions.db")).await;
    assert_eq!(
        reopened.apply_session_control(&pause).await.unwrap(),
        receipt
    );
    assert_eq!(
        reopened.resolve_session_control(&pause).await.unwrap(),
        ControlResolution::Applied { receipt }
    );
    assert_eq!(
        reopened
            .load_session_control(&pause.session_id)
            .await
            .unwrap()
            .status,
        ControlStatus::Active
    );
}

#[tokio::test]
async fn control_conflicting_id_rejects_without_state_change() {
    let (_directory, resources) = fixture().await;
    let pause = command("same", &ControlState::default(), ControlAction::Pause);
    let receipt = resources.apply_session_control(&pause).await.unwrap();
    let conflict = command("same", &receipt.state, ControlAction::Resume);
    assert!(matches!(
        resources
            .apply_session_control(&conflict)
            .await
            .unwrap_err()
            .kind(),
        SessionResourceErrorKind::Conflict { .. }
    ));
    assert!(matches!(
        resources
            .resolve_session_control(&conflict)
            .await
            .unwrap_err()
            .kind(),
        SessionResourceErrorKind::Conflict { .. }
    ));
    assert_eq!(
        resources
            .load_session_control(&pause.session_id)
            .await
            .unwrap(),
        receipt.state
    );
}

#[tokio::test]
async fn control_stale_revision_rejection_is_durable_and_replayable() {
    let (directory, resources) = fixture().await;
    apply(resources.as_ref(), "first", ControlAction::Pause).await;
    let stale = command("stale", &ControlState::default(), ControlAction::Resume);
    let receipt = resources.apply_session_control(&stale).await.unwrap();
    assert_eq!(
        receipt.decision,
        ControlDecision::Rejected {
            reason: ControlRejection::StaleRevision
        }
    );
    let reopened = open(&directory.path().join("sessions.db")).await;
    assert_eq!(
        reopened.apply_session_control(&stale).await.unwrap(),
        receipt
    );
}

#[tokio::test]
async fn control_stale_generation_rejects_even_at_current_revision() {
    let (_directory, resources) = fixture().await;
    let paused = apply(resources.as_ref(), "pause", ControlAction::Pause).await;
    let mut stale = command("old-generation", &paused.state, ControlAction::Resume);
    stale.expected_control_generation = 0;
    assert_eq!(
        resources
            .apply_session_control(&stale)
            .await
            .unwrap()
            .decision,
        ControlDecision::Rejected {
            reason: ControlRejection::StaleControlGeneration
        }
    );
}

#[tokio::test]
async fn control_stop_requires_both_turn_and_attempt() {
    let (_directory, resources) = fixture().await;
    let target = ControlAttempt {
        turn_id: TurnId::new(),
        attempt_id: AttemptId::new(),
    };
    let observed = apply(
        resources.as_ref(),
        "observe",
        ControlAction::ObserveAttempt {
            target: Some(target.clone()),
        },
    )
    .await;
    let wrong = ControlAttempt {
        turn_id: TurnId::new(),
        attempt_id: target.attempt_id.clone(),
    };
    let stale = command(
        "wrong-turn",
        &observed.state,
        ControlAction::Stop { target: wrong },
    );
    assert_eq!(
        resources
            .apply_session_control(&stale)
            .await
            .unwrap()
            .decision,
        ControlDecision::Rejected {
            reason: ControlRejection::StaleAttempt
        }
    );
    let stopped = apply(resources.as_ref(), "stop", ControlAction::Stop { target }).await;
    assert_eq!(stopped.state.status, ControlStatus::Paused);
}

#[tokio::test]
async fn control_observe_cannot_replace_an_existing_attempt_without_cas_clear() {
    let (_directory, resources) = fixture().await;
    let first = ControlAttempt {
        turn_id: TurnId::new(),
        attempt_id: AttemptId::new(),
    };
    let observed = apply(
        resources.as_ref(),
        "first-attempt",
        ControlAction::ObserveAttempt {
            target: Some(first),
        },
    )
    .await;
    let next = ControlAttempt {
        turn_id: TurnId::new(),
        attempt_id: AttemptId::new(),
    };
    let replace = command(
        "replace-attempt",
        &observed.state,
        ControlAction::ObserveAttempt {
            target: Some(next.clone()),
        },
    );
    let rejected = resources.apply_session_control(&replace).await.unwrap();
    assert_eq!(
        rejected.decision,
        ControlDecision::Rejected {
            reason: ControlRejection::StaleAttempt
        }
    );
    assert_eq!(rejected.state, observed.state);
    apply(
        resources.as_ref(),
        "clear-attempt",
        ControlAction::ObserveAttempt { target: None },
    )
    .await;
    let recorded = apply(
        resources.as_ref(),
        "next-attempt",
        ControlAction::ObserveAttempt {
            target: Some(next.clone()),
        },
    )
    .await;
    assert_eq!(recorded.state.attempt, Some(next));
}

#[tokio::test]
async fn control_close_is_atomic_with_admission_intent_and_separate_finish() {
    let (directory, resources) = fixture().await;
    let close = command("close", &ControlState::default(), ControlAction::Close);
    let closing = resources.apply_session_control(&close).await.unwrap();
    assert_eq!(closing.state.status, ControlStatus::Closing);
    let reopened = open(&directory.path().join("sessions.db")).await;
    assert!(reopened
        .is_session_closing(&close.session_id)
        .await
        .unwrap());
    assert_eq!(
        reopened
            .load_session_control(&close.session_id)
            .await
            .unwrap(),
        closing.state
    );
    let closed = apply(reopened.as_ref(), "finish", ControlAction::FinishClose).await;
    assert_eq!(closed.state.status, ControlStatus::Closed);
    assert_eq!(
        reopened.apply_session_control(&close).await.unwrap(),
        closing
    );
    let active = apply(reopened.as_ref(), "reopen", ControlAction::Reopen).await;
    assert_eq!(active.state.lifecycle, 2);
    assert_eq!(active.state.status, ControlStatus::Active);
    assert!(!reopened
        .is_session_closing(&close.session_id)
        .await
        .unwrap());
    let old = command("old-lifecycle", &closed.state, ControlAction::Pause);
    assert_eq!(
        reopened.apply_session_control(&old).await.unwrap().decision,
        ControlDecision::Rejected {
            reason: ControlRejection::StaleLifecycle
        }
    );
}

#[tokio::test]
async fn control_tombstone_can_finish_after_canonical_session_delete() {
    let (directory, resources) = fixture().await;
    let closing = apply(resources.as_ref(), "close", ControlAction::Close).await;
    resources
        .delete_session_tree(&closing.session_id)
        .await
        .unwrap();
    let finish = command("finish-deleted", &closing.state, ControlAction::FinishClose);
    let closed = resources.apply_session_control(&finish).await.unwrap();
    assert_eq!(closed.state.status, ControlStatus::Closed);
    let reopened = open(&directory.path().join("sessions.db")).await;
    assert_eq!(
        reopened
            .load_session_control(&closing.session_id)
            .await
            .unwrap(),
        closed.state
    );
    assert_eq!(
        reopened.apply_session_control(&finish).await.unwrap(),
        closed
    );
}

#[tokio::test]
async fn control_resolution_seals_original_id_against_late_apply() {
    let (directory, resources) = fixture().await;
    let original = command("late", &ControlState::default(), ControlAction::Pause);
    assert_eq!(
        resources.resolve_session_control(&original).await.unwrap(),
        ControlResolution::NotApplied
    );
    let reopened = open(&directory.path().join("sessions.db")).await;
    assert!(matches!(
        reopened
            .apply_session_control(&original)
            .await
            .unwrap_err()
            .kind(),
        SessionResourceErrorKind::Conflict { .. }
    ));
    assert_eq!(
        reopened
            .load_session_control(&original.session_id)
            .await
            .unwrap(),
        ControlState::default()
    );
}

#[tokio::test]
async fn control_two_store_instances_cas_accept_only_one_revision() {
    let (directory, resources) = fixture().await;
    let other = open(&directory.path().join("sessions.db")).await;
    let pause = command("race-pause", &ControlState::default(), ControlAction::Pause);
    let close = command("race-close", &ControlState::default(), ControlAction::Close);
    let (first, second) = tokio::join!(
        resources.apply_session_control(&pause),
        other.apply_session_control(&close)
    );
    let receipts = [first.unwrap(), second.unwrap()];
    assert_eq!(
        receipts
            .iter()
            .filter(|receipt| receipt.decision == ControlDecision::Accepted)
            .count(),
        1
    );
    assert_eq!(
        resources
            .load_session_control(&pause.session_id)
            .await
            .unwrap()
            .revision,
        1
    );
}

#[tokio::test]
async fn control_read_only_store_refuses_apply_and_resolution() {
    let (directory, _resources) = fixture().await;
    let readonly = peri_resources::sessions::SessionResourcesImpl::open_existing_read_only(
        &directory.path().join("sessions.db"),
    )
    .await
    .unwrap();
    let pause = command("readonly", &ControlState::default(), ControlAction::Pause);
    assert!(matches!(
        readonly
            .apply_session_control(&pause)
            .await
            .unwrap_err()
            .kind(),
        SessionResourceErrorKind::ReadOnlyStore
    ));
    assert!(matches!(
        readonly
            .resolve_session_control(&pause)
            .await
            .unwrap_err()
            .kind(),
        SessionResourceErrorKind::ReadOnlyStore
    ));
}

#[tokio::test]
async fn control_upgrade_from_v14_preserves_existing_close_intent() {
    use sqlx::Connection;

    let (directory, resources) = fixture().await;
    resources
        .mark_session_closing(&"control-session".into())
        .await
        .unwrap();
    let options =
        sqlx::sqlite::SqliteConnectOptions::new().filename(directory.path().join("sessions.db"));
    let mut connection = sqlx::SqliteConnection::connect_with(&options)
        .await
        .unwrap();
    for sql in [
        "DROP TABLE session_control_receipts",
        "DROP TABLE session_control_state",
        "PRAGMA user_version = 14",
    ] {
        sqlx::query(sql).execute(&mut connection).await.unwrap();
    }
    connection.close().await.unwrap();
    let upgraded = open(&directory.path().join("sessions.db")).await;
    let state = upgraded
        .load_session_control(&"control-session".into())
        .await
        .unwrap();
    assert_eq!(state.status, ControlStatus::Closing);
    assert!(upgraded
        .is_session_closing(&"control-session".into())
        .await
        .unwrap());
    let close = command("legacy-close", &state, ControlAction::Close);
    assert_eq!(
        upgraded
            .apply_session_control(&close)
            .await
            .unwrap()
            .state
            .status,
        ControlStatus::Closing
    );
}
