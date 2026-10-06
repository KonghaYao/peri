use super::*;
use crate::session::test_resources::TestSession;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::session::MessagePolicy;
use peri_acp_types::session_resources::work::{
    DeliveryPurpose, ObligationStatus, PublishDelivery, WorkAction, WorkEvent, WorkPayload,
};
use peri_acp_types::session_resources::{ControlAction, ControlCommand};
use peri_acp_types::store::PersistedPayload;

fn publication(session_id: &str, lifecycle: u64) -> WorkCommand {
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
                    content: WorkPayload::from_payload(&payload).unwrap(),
                },
                purpose: DeliveryPurpose::UserInput,
                policy: MessagePolicy::ensure_processing(),
            },
        },
    }
}

#[tokio::test]
async fn paused_recipient_still_accepts_durable_publication() {
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
    let command = publication(session.thread_id.as_str(), control.lifecycle);
    let receipt = barrier.commit(&command).await.unwrap();
    let snapshot = barrier
        .snapshot(&WorkQuery {
            session_id: command.session_id,
            limit: 64,
        })
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    let delivery_id = receipt.delivery_id.unwrap();
    assert!(snapshot.state.deliveries.contains_key(&delivery_id));
    assert_eq!(
        snapshot.state.obligations[&delivery_id].status,
        ObligationStatus::Blocked
    );
    assert!(snapshot.blocked);
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
    let command = publication(session.thread_id.as_str(), control.lifecycle);
    let original = barrier.commit(&command).await.unwrap();
    let repeated = barrier.commit(&command).await.unwrap();
    assert_eq!(original, repeated);
    let snapshot = barrier
        .snapshot(&WorkQuery {
            session_id: command.session_id,
            limit: 64,
        })
        .await
        .unwrap();
    assert_eq!(snapshot.state.deliveries.len(), 1);
    assert_eq!(snapshot.state.obligations.len(), 1);
    assert_eq!(snapshot.state.revision, original.revision);
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
    let command = publication(session.thread_id.as_str(), control.lifecycle + 1);
    let error = barrier.commit(&command).await.unwrap_err();
    assert!(matches!(error, WorkCommitError::Rejected { .. }));
    let snapshot = barrier
        .snapshot(&WorkQuery {
            session_id: command.session_id,
            limit: 64,
        })
        .await
        .unwrap();
    assert!(snapshot.state.deliveries.is_empty());
    assert!(snapshot.state.obligations.is_empty());
}
