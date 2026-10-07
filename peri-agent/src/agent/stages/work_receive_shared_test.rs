use super::super::super::work_ledger::{WorkCommitError, WorkMutationBarrier};
use super::*;

fn publication(session_id: &str) -> PreparedWorkCommand {
    let content = PersistedPayload::Message(BaseMessage::human("大正文🛰".repeat(128 * 1024)));
    PreparedWorkCommand::try_new(WorkCommand {
        session_id: session_id.into(),
        recipient_lifecycle: 1,
        mutation_id: "shared-publication".into(),
        action: WorkAction::PublishDelivery {
            delivery: PublishDelivery {
                delivery_id: "shared-delivery".into(),
                event: WorkEvent {
                    producer_namespace: "shared-test".into(),
                    event_id: "shared-event".into(),
                    event_kind: "input".into(),
                    causation_id: None,
                    content: WorkPayload::from_payload(&content).unwrap(),
                },
                purpose: DeliveryPurpose::UserInput,
                policy: MessagePolicy::ensure_processing(),
            },
        },
    })
    .unwrap()
}

fn assert_shared(original: &PreparedWorkCommand, retained: &PreparedWorkCommand) {
    assert!(Arc::ptr_eq(original.encoded(), retained.encoded()));
    assert!(std::ptr::eq(original.command(), retained.command()));
    assert_eq!(original, retained);
    assert_eq!(original.digest().as_ptr(), retained.digest().as_ptr());
}

#[tokio::test]
async fn unknown_pending_error_and_replay_reuse_original_large_command_handle() {
    let bound = TestSession::open().await;
    let resources = Arc::new(PublicationResources::new(bound.resources.clone()));
    *resources.failure.lock().unwrap() = Failure::Unknown;
    let barrier = WorkMutationBarrier::new(resources.clone());
    let original = publication(&bound.thread_id);
    let error = barrier
        .commit_execution_transition(&original)
        .await
        .unwrap_err();
    let WorkCommitError::Unknown { command } = error else {
        panic!("unknown command must retain its prepared handle");
    };
    assert_shared(&original, &command);
    assert_shared(&original, &barrier.pending_command().await.unwrap());
    assert_shared(&original, &resources.commands.lock().unwrap()[0]);
    assert_shared(&original, &resources.resolved_commands.lock().unwrap()[0]);
    let mut conflicting = original.command().clone();
    let WorkAction::PublishDelivery { delivery } = &mut conflicting.action else {
        unreachable!()
    };
    delivery.event.event_kind = "different-content".into();
    let conflicting = PreparedWorkCommand::try_new(conflicting).unwrap();
    let WorkCommitError::Frozen { command } = barrier.commit(&conflicting).await.unwrap_err()
    else {
        panic!("same identity with different content must remain frozen");
    };
    assert_shared(&original, &command);
    assert_eq!(resources.commands.lock().unwrap().len(), 1);
    let WorkCommitError::Unknown { command } = barrier.commit(&original).await.unwrap_err() else {
        panic!("retry without confirmed resolution must not resign");
    };
    assert_shared(&original, &command);
    assert_eq!(resources.commands.lock().unwrap().len(), 1);
    let equivalent = PreparedWorkCommand::try_new(original.command().clone()).unwrap();
    assert!(!Arc::ptr_eq(original.encoded(), equivalent.encoded()));
    assert!(matches!(
        barrier.commit(&equivalent).await,
        Err(WorkCommitError::Unknown { .. })
    ));
    assert_shared(
        &original,
        resources.resolved_commands.lock().unwrap().last().unwrap(),
    );
    *resources.failure.lock().unwrap() = Failure::Replay;
    let receipt = barrier.commit(&original).await.unwrap();
    assert_eq!(receipt.mutation_id, original.mutation_id);
    assert!(barrier.pending_command().await.is_none());
    let attempts = resources.commands.lock().unwrap();
    assert_eq!(attempts.len(), 2);
    assert_shared(&original, &attempts[1]);
}

#[tokio::test]
async fn cancelled_submission_retains_original_preparation_before_retry() {
    let bound = TestSession::open().await;
    let resources = Arc::new(PublicationResources::new(bound.resources.clone()));
    *resources.failure.lock().unwrap() = Failure::Pending;
    let barrier = WorkMutationBarrier::new(resources.clone());
    let original = publication(&bound.thread_id);
    assert!(tokio::time::timeout(
        std::time::Duration::from_millis(10),
        barrier.commit(&original)
    )
    .await
    .is_err());
    assert_shared(&original, &barrier.pending_command().await.unwrap());
    assert_shared(&original, &resources.commands.lock().unwrap()[0]);
    *resources.failure.lock().unwrap() = Failure::Replay;
    barrier.commit(&original).await.unwrap();
    assert!(barrier.pending_command().await.is_none());
    assert_shared(&original, &resources.commands.lock().unwrap()[1]);
}

#[tokio::test]
async fn cold_journal_reload_prepares_exact_original_large_command_for_reconciliation() {
    let bound = TestSession::open().await;
    let original = publication(&bound.thread_id);
    let barrier = WorkMutationBarrier::new(bound.resources.clone());
    let receipt = barrier.commit(&original).await.unwrap();
    drop(barrier);
    let cold = bound.reopened_resources().await;
    let owned = cold
        .load_work_command(&WorkCommandQuery {
            session_id: original.session_id.clone(),
            mutation_id: original.mutation_id.clone(),
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(owned.command, *original.command());
    assert_eq!(
        owned.resolution,
        Some(WorkResolution::Applied {
            receipt: receipt.clone()
        })
    );
    let restored = PreparedWorkCommand::try_new(owned.command).unwrap();
    assert_eq!(restored, original);
    assert_eq!(restored.encoded(), original.encoded());
    assert_eq!(restored.digest(), original.digest());
    assert!(!Arc::ptr_eq(restored.encoded(), original.encoded()));
    assert_eq!(
        cold.resolve_work_mutation(&restored).await.unwrap(),
        WorkResolution::Applied { receipt }
    );
}
