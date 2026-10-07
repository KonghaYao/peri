use super::*;
use peri_acp_types::identity::AttemptId;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::session::{MessagePolicy, TurnId};
use peri_acp_types::session_resources::work::{
    reduce_work, DeliveryPurpose, PublishDelivery, WorkAction, WorkCommand, WorkDecision,
    WorkEvent, WorkGuard, WorkQuery, WorkState,
};
use peri_acp_types::session_resources::ControlAttempt;

fn claimed_snapshot() -> WorkSnapshot {
    let mut control = peri_acp_types::session_resources::ControlState::default();
    let execution = ControlAttempt {
        turn_id: TurnId::new(),
        attempt_id: AttemptId::new(),
    };
    control.attempt = Some(execution.clone());
    let payload = PersistedPayload::Message(BaseMessage::human("pending input"));
    let published = reduce_work(
        &peri_acp_types::session_resources::work::PreparedWorkCommand::try_new(WorkCommand {
            session_id: "session".into(),
            recipient_lifecycle: control.lifecycle,
            mutation_id: "publish".into(),
            action: WorkAction::PublishDelivery {
                delivery: PublishDelivery {
                    delivery_id: "delivery".into(),
                    event: WorkEvent {
                        producer_namespace: "recovery-test".into(),
                        event_id: payload.id().as_uuid().to_string(),
                        event_kind: "user-input".into(),
                        causation_id: None,
                        content: WorkPayload::from_payload(&payload).unwrap(),
                    },
                    purpose: DeliveryPurpose::UserInput,
                    policy: MessagePolicy::ensure_processing(),
                },
            },
        })
        .unwrap(),
        &control,
        WorkState::default(),
    )
    .unwrap();
    assert_eq!(published.receipt.decision, WorkDecision::Accepted);
    let published_state = published.state.unwrap();
    let claimed = reduce_work(
        &peri_acp_types::session_resources::work::PreparedWorkCommand::try_new(WorkCommand {
            session_id: "session".into(),
            recipient_lifecycle: control.lifecycle,
            mutation_id: "claim".into(),
            action: WorkAction::ClaimBatch {
                guard: WorkGuard {
                    expected_revision: published_state.revision,
                    expected_control_generation: control.control_generation,
                    execution,
                },
                batch_id: "work".into(),
                delivery_ids: vec!["delivery".into()],
            },
        })
        .unwrap(),
        &control,
        published_state,
    )
    .unwrap();
    assert_eq!(claimed.receipt.decision, WorkDecision::Accepted);
    WorkSnapshot::from_state(
        &WorkQuery {
            session_id: "session".into(),
            limit: 64,
        },
        control,
        claimed.state.unwrap(),
    )
}

#[test]
fn claimed_projection_does_not_mean_input_is_satisfied() {
    let snapshot = claimed_snapshot();
    let recovered = recover_work(&snapshot, "work").unwrap();
    assert!(matches!(recovered.stage, RecoveredStage::ReasonReady));
    assert_eq!(recovered.projection.len(), 1);
    assert_eq!(recovered.delivery_ids, vec!["delivery"]);
    assert_eq!(recovered.processing_delivery_ids, vec!["delivery"]);
    assert_eq!(
        snapshot.state.obligations["delivery"].status,
        peri_acp_types::session_resources::work::ObligationStatus::InProgress
    );
    assert_eq!(recovered.budget_id, snapshot.state.works["work"].budget_id);
}

#[test]
fn legacy_unknown_prevents_stage_replay_even_with_a_canonical_projection() {
    let mut snapshot = claimed_snapshot();
    snapshot
        .state
        .legacy_unknown
        .insert("legacy-message".into(), "no processing evidence".into());
    assert!(matches!(
        recover_work(&snapshot, "work"),
        Err(WorkRecoveryError::LegacyUnknown { .. })
    ));
}

#[test]
fn changed_projection_version_is_not_a_recoverable_exact_batch() {
    let mut snapshot = claimed_snapshot();
    snapshot
        .state
        .deliveries
        .get_mut("delivery")
        .unwrap()
        .projection_version += 1;
    assert!(matches!(
        recover_work(&snapshot, "work"),
        Err(WorkRecoveryError::InvalidProjection)
    ));
}

#[test]
fn missing_work_is_not_recreated_from_its_projection() {
    let mut snapshot = claimed_snapshot();
    snapshot.state.works.clear();
    assert!(matches!(
        recover_work(&snapshot, "work"),
        Err(WorkRecoveryError::MissingWork)
    ));
}

#[test]
fn reopened_lifecycle_cannot_recover_old_batch_even_with_complete_projection() {
    let mut snapshot = claimed_snapshot();
    let original = snapshot.state.clone();
    snapshot.control.lifecycle += 1;
    snapshot.control.control_generation += 1;
    snapshot.control.attempt = None;
    assert!(matches!(
        recover_work(&snapshot, "work"),
        Err(WorkRecoveryError::UnconfirmedLifecycle)
    ));
    assert_eq!(snapshot.state, original);
}

#[test]
fn missing_batch_lifecycle_evidence_cannot_be_reconstructed_from_transcript() {
    let mut snapshot = claimed_snapshot();
    snapshot.state.batches.clear();
    assert!(matches!(
        recover_work(&snapshot, "work"),
        Err(WorkRecoveryError::UnconfirmedLifecycle)
    ));
}
