use super::*;
use crate::session::test_resources::TestSession;

#[tokio::test]
async fn refresh_after_claim_before_receive_notifies_once() {
    let fixture = TestSession::open().await;
    let (mailbox, _) = mailbox(&fixture, fixture.resources());
    let request = input(&mailbox);
    mailbox.enqueue_durable(&request).await.unwrap();
    let admission = admit(&fixture).await;
    let snapshot = load(&fixture).await;
    let receipt = fixture
        .resources
        .apply_work_mutation(&claim_command(&fixture, &snapshot, &admission))
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    mailbox.refresh_durable().await.unwrap();
    let input_ids = vec![MessageId::from(
        uuid::Uuid::parse_str(&request.input_id).unwrap(),
    )];
    mailbox.mark_claimed(&input_ids);
    assert_eq!(mailbox.mark_delivered(&input_ids), vec![request.input_id]);
    mailbox.refresh_durable().await.unwrap();
    mailbox.mark_claimed(&input_ids);
    assert!(mailbox.mark_delivered(&input_ids).is_empty());
}

#[tokio::test]
async fn receive_before_refresh_and_recovery_each_notify_once() {
    let fixture = TestSession::open().await;
    let (original, _) = mailbox(&fixture, fixture.resources());
    let request = input(&original);
    original.enqueue_durable(&request).await.unwrap();
    let admission = admit(&fixture).await;
    let snapshot = load(&fixture).await;
    let receipt = fixture
        .resources
        .apply_work_mutation(&claim_command(&fixture, &snapshot, &admission))
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    let input_ids = vec![MessageId::from(
        uuid::Uuid::parse_str(&request.input_id).unwrap(),
    )];
    original.mark_claimed(&input_ids);
    assert_eq!(
        original.mark_delivered(&input_ids),
        vec![request.input_id.clone()]
    );
    original.refresh_durable().await.unwrap();
    assert!(original.mark_delivered(&input_ids).is_empty());
    let (restored, _) = mailbox(&fixture, fixture.resources());
    restored.refresh_durable().await.unwrap();
    restored.mark_claimed(&input_ids);
    assert_eq!(restored.mark_delivered(&input_ids), vec![request.input_id]);
    assert!(restored.mark_delivered(&input_ids).is_empty());
}

#[tokio::test]
async fn idle_enqueue_reuses_three_snapshots_and_replay_uses_one() {
    use std::sync::atomic::Ordering;
    let fixture = TestSession::open().await;
    let (mailbox, _) = mailbox(&fixture, fixture.resources());
    let request = input(&mailbox);
    let first = mailbox.enqueue_durable(&request).await.unwrap();
    let durable = mailbox.durable.as_ref().unwrap();
    assert_eq!(durable.snapshot_loads.load(Ordering::SeqCst), 3);
    assert_eq!(first.work_receipts.len(), 2);
    let replay = mailbox.enqueue_durable(&request).await.unwrap();
    assert_eq!(durable.snapshot_loads.load(Ordering::SeqCst), 4);
    assert_eq!(replay.work_receipts, first.work_receipts);
    assert_eq!(
        replay.publication_generations,
        first.publication_generations
    );
    let snapshot = load(&fixture).await;
    assert_eq!(snapshot.state.deliveries.len(), 1);
}

#[tokio::test]
async fn notifications_are_scoped_to_publication_generation() {
    let fixture = TestSession::open().await;
    let (mailbox, _) = mailbox(&fixture, fixture.resources());
    let request = input(&mailbox);
    mailbox.enqueue_durable(&request).await.unwrap();
    let admission = admit(&fixture).await;
    let snapshot = load(&fixture).await;
    fixture
        .resources
        .apply_work_mutation(&claim_command(&fixture, &snapshot, &admission))
        .await
        .unwrap();
    mailbox.refresh_durable().await.unwrap();
    let mut snapshot = load(&fixture).await;
    let delivery = snapshot.state.deliveries.values().next().unwrap();
    let input_id = delivery.publication.event.content.message_id;
    let old_id = delivery.publication.delivery_id.clone();
    assert_eq!(
        mailbox.mark_committed_deliveries(&[(input_id, old_id.clone())]),
        vec![request.input_id.clone()]
    );
    let mut replacement = delivery.clone();
    let new_id = uuid::Uuid::now_v7().to_string();
    replacement.publication.delivery_id = new_id.clone();
    replacement.admission_sequence += 1;
    let mut identity = identity(&replacement).unwrap();
    identity.publication_generation = "replacement-generation".into();
    replacement.publication.event.causation_id = Some(serde_json::to_string(&identity).unwrap());
    snapshot
        .state
        .deliveries
        .insert(new_id.clone(), replacement);
    snapshot
        .state
        .staged_user_inputs
        .get_mut(&request.input_id)
        .unwrap()
        .publication_id = Some(new_id.clone());
    snapshot.state.revision += 1;
    mailbox.project_publications(&snapshot);
    assert!(mailbox
        .mark_committed_deliveries(&[(input_id, old_id)])
        .is_empty());
    assert_eq!(
        mailbox.mark_committed_deliveries(&[(input_id, new_id.clone())]),
        vec![request.input_id]
    );
    assert!(mailbox
        .mark_committed_deliveries(&[(input_id, new_id)])
        .is_empty());
}

#[tokio::test]
async fn invalidated_mailbox_cannot_notify_old_lifecycle() {
    let fixture = TestSession::open().await;
    let (mailbox, _) = mailbox(&fixture, fixture.resources());
    let request = input(&mailbox);
    mailbox.enqueue_durable(&request).await.unwrap();
    let admission = admit(&fixture).await;
    let snapshot = load(&fixture).await;
    fixture
        .resources
        .apply_work_mutation(&claim_command(&fixture, &snapshot, &admission))
        .await
        .unwrap();
    mailbox.refresh_durable().await.unwrap();
    let input_id = MessageId::from(uuid::Uuid::parse_str(&request.input_id).unwrap());
    mailbox.invalidate();
    assert!(mailbox.mark_delivered(&[input_id]).is_empty());
}
