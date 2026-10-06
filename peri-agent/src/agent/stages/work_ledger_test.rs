use super::*;
use crate::session::test_resources::TestSession;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::session::MessagePolicy;
use peri_acp_types::session_resources::work::{
    DeliveryPurpose, ObligationStatus, PublishDelivery, WorkAction, WorkEvent, WorkPage,
    WorkSelector,
};
use peri_acp_types::session_resources::{ControlAction, ControlCommand, ControlDecision};
use peri_acp_types::store::PersistedPayload;

async fn publication(
    resources: &dyn SessionResources,
    session_id: &str,
    lifecycle: u64,
) -> WorkCommand {
    let payload = PersistedPayload::Message(BaseMessage::human("reliable input"));
    WorkCommand {
        session_id: session_id.into(),
        recipient_lifecycle: lifecycle,
        mutation_id: uuid::Uuid::now_v7().to_string(),
        action: WorkAction::PublishDelivery {
            delivery: PublishDelivery {
                delivery_id: uuid::Uuid::now_v7().to_string(),
                event: WorkEvent {
                    producer_namespace: "pipeline-test".into(),
                    event_id: payload.id().as_uuid().to_string(),
                    event_kind: "user-input".into(),
                    causation_id: None,
                    content: super::super::prepare_work_payload(resources, session_id, &payload)
                        .await
                        .unwrap(),
                },
                purpose: DeliveryPurpose::UserInput,
                policy: MessagePolicy::ensure_processing(),
            },
        },
    }
}

#[tokio::test]
async fn paused_recipient_keeps_publication_pending_without_claiming_it() {
    let session = TestSession::open().await;
    let control = session
        .resources
        .load_session_control(&session.thread_id)
        .await
        .unwrap();
    session
        .resources
        .apply_session_control(&ControlCommand {
            session_id: session.thread_id.clone(),
            command_id: uuid::Uuid::now_v7().to_string(),
            expected_lifecycle: control.lifecycle,
            expected_revision: control.revision,
            expected_control_generation: control.control_generation,
            action: ControlAction::Pause,
        })
        .await
        .unwrap();
    let barrier = WorkMutationBarrier::new(session.resources.clone());
    let command = publication(
        session.resources.as_ref(),
        session.thread_id.as_str(),
        control.lifecycle,
    )
    .await;
    let receipt = barrier.commit(&command).await.unwrap();
    let snapshot = barrier
        .inspect(&WorkQuery::new(&command.session_id, WorkSelector::Inbox))
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    let delivery_id = receipt.delivery_id.unwrap();
    let WorkPage::Deliveries(deliveries) = &snapshot.page else {
        panic!("expected deliveries")
    };
    assert!(deliveries
        .iter()
        .any(|delivery| delivery.delivery_id == delivery_id));
    assert_eq!(
        deliveries
            .iter()
            .find(|delivery| delivery.delivery_id == delivery_id)
            .unwrap()
            .obligation,
        ObligationStatus::Pending
    );
    assert_eq!(
        snapshot.control.status,
        peri_acp_types::session_resources::ControlStatus::Paused
    );
    let delivery = deliveries
        .iter()
        .find(|delivery| delivery.delivery_id == delivery_id)
        .unwrap();
    assert!(delivery.processing_id.is_none());
    assert!(delivery.projection.is_none());
    assert_eq!(snapshot.head.required_count, 1);
}

#[tokio::test]
async fn publication_retry_returns_original_receipt_and_one_obligation() {
    let session = TestSession::open().await;
    let control = session
        .resources
        .load_session_control(&session.thread_id)
        .await
        .unwrap();
    let barrier = WorkMutationBarrier::new(session.resources.clone());
    let command = publication(
        session.resources.as_ref(),
        session.thread_id.as_str(),
        control.lifecycle,
    )
    .await;
    let original = barrier.commit(&command).await.unwrap();
    let repeated = barrier.commit(&command).await.unwrap();
    assert_eq!(original, repeated);
    let snapshot = barrier
        .inspect(&WorkQuery::new(&command.session_id, WorkSelector::Inbox))
        .await
        .unwrap();
    assert!(matches!(snapshot.page, WorkPage::Deliveries(ref deliveries) if deliveries.len() == 1));
    assert_eq!(snapshot.head.required_count, 1);
    assert_eq!(snapshot.head.change_seq, original.revision);
    assert!(barrier.pending_command().await.is_none());
}

#[tokio::test]
async fn stale_lifecycle_rejection_does_not_create_an_obligation() {
    let session = TestSession::open().await;
    let control = session
        .resources
        .load_session_control(&session.thread_id)
        .await
        .unwrap();
    let barrier = WorkMutationBarrier::new(session.resources.clone());
    let command = publication(
        session.resources.as_ref(),
        session.thread_id.as_str(),
        control.lifecycle + 1,
    )
    .await;
    let error = barrier.commit(&command).await.unwrap_err();
    assert!(matches!(error, WorkCommitError::Rejected { .. }));
    let snapshot = barrier
        .inspect(&WorkQuery::new(&command.session_id, WorkSelector::Inbox))
        .await
        .unwrap();
    assert!(matches!(snapshot.page, WorkPage::Deliveries(ref deliveries) if deliveries.is_empty()));
    assert_eq!(snapshot.head.required_count, 0);
}

#[tokio::test]
async fn mismatched_receipt_keeps_original_command_frozen() {
    let session = TestSession::open().await;
    let barrier = WorkMutationBarrier::new(session.resources());
    let command = publication(session.resources().as_ref(), &session.thread_id(), 1).await;
    let receipt = barrier.commit(&command).await.unwrap();
    let mut wrong = receipt;
    wrong.mutation_id = "different-mutation".into();
    let mut pending = Some(command.clone());
    assert!(matches!(
        WorkMutationBarrier::confirm(&command, wrong, &mut pending),
        Err(WorkCommitError::InvalidReceipt { .. })
    ));
    assert_eq!(pending, Some(command));
}
