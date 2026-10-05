use super::*;
use crate::session::test_resources::mock::MockSessionResources;

async fn observed(store: &MockSessionResources, session_id: &ThreadId) -> ControlAttempt {
    let attempt = ControlAttempt {
        turn_id: crate::session::TurnId::new(),
        attempt_id: peri_acp_types::identity::AttemptId::new(),
    };
    let current = store.load_session_control(session_id).await.unwrap();
    let receipt = store
        .apply_session_control(&ControlCommand {
            session_id: session_id.clone(),
            command_id: "fixture-enter".into(),
            expected_lifecycle: current.lifecycle,
            expected_revision: current.revision,
            expected_control_generation: current.control_generation,
            action: ControlAction::ObserveAttempt {
                target: Some(attempt.clone()),
            },
        })
        .await
        .unwrap();
    assert_eq!(receipt.decision, ControlDecision::Accepted);
    attempt
}

#[tokio::test]
async fn stopped_attempt_cleanup_clears_only_its_exact_observation() {
    let store = MockSessionResources::new();
    let session_id = "child".to_owned();
    let attempt = observed(&store, &session_id).await;
    clear_stopped_attempt(store.as_ref(), &session_id, &attempt)
        .await
        .unwrap();
    assert!(store
        .load_session_control(&session_id)
        .await
        .unwrap()
        .attempt
        .is_none());
}

#[tokio::test]
async fn stopped_attempt_cleanup_keeps_foreign_attempt_and_revision() {
    let store = MockSessionResources::new();
    let session_id = "child".to_owned();
    let mut wrong_attempt = observed(&store, &session_id).await;
    let before = store.load_session_control(&session_id).await.unwrap();
    wrong_attempt.attempt_id = peri_acp_types::identity::AttemptId::new();
    assert!(
        clear_stopped_attempt(store.as_ref(), &session_id, &wrong_attempt)
            .await
            .is_err()
    );
    assert_eq!(
        store.load_session_control(&session_id).await.unwrap(),
        before
    );
}

#[tokio::test]
async fn failed_observation_mutation_leaves_claim_cleanup_unconfirmed() {
    let store = MockSessionResources::new();
    let session_id = "child".to_owned();
    let attempt = observed(&store, &session_id).await;
    store.restrict_to_history_read_only();
    assert!(clear_stopped_attempt(store.as_ref(), &session_id, &attempt)
        .await
        .unwrap_err()
        .contains("unconfirmed"));
    assert_eq!(
        store
            .load_session_control(&session_id)
            .await
            .unwrap()
            .attempt,
        Some(attempt)
    );
}
