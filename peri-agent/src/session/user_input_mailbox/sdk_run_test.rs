use super::*;
use crate::session::test_resources::TestSession;
use peri_acp_types::identity::AttemptId;
use peri_acp_types::session::MessageQueue;
use peri_acp_types::session_resources::{work::*, ControlAttempt};

async fn fixture() -> (TestSession, Arc<UserInputMailbox>, WorkAdmission) {
    let fixture = TestSession::open().await;
    let inbox = Arc::new(SessionInbox::new(Arc::new(MessageQueue::new())));
    let mailbox = UserInputMailbox::new_durable(
        fixture.thread_id(),
        inbox,
        Arc::new(|_| {}),
        fixture.resources(),
        1,
    );
    let input_id = uuid::Uuid::now_v7().to_string();
    mailbox
        .enqueue_durable(&EnqueueUserInputRequest {
            session_id: fixture.thread_id(),
            generation: mailbox.generation().into(),
            command_id: "publication-generation".into(),
            input_id,
            content: MessageContent::text("SDK observed input"),
            original_draft: "SDK observed input".into(),
        })
        .await
        .unwrap();
    let snapshot = fixture
        .resources
        .load_session_work(&WorkQuery {
            session_id: fixture.thread_id(),
            limit: 1,
        })
        .await
        .unwrap();
    let candidate = &snapshot.candidates[0];
    let admission = WorkAdmission {
        session_id: fixture.thread_id(),
        admission_id: "sdk-exact-ticket".into(),
        instance_id: "sdk-instance".into(),
        generation_id: "sdk-generation".into(),
        lifecycle: 1,
        control_generation: snapshot.control.control_generation,
        work_id: candidate.work_id.clone(),
        work_revision: candidate.work_revision,
        execution: ControlAttempt {
            turn_id: TurnId::new(),
            attempt_id: AttemptId::new(),
        },
    };
    (fixture, mailbox, admission)
}

async fn register(fixture: &TestSession, admission: &WorkAdmission) {
    let receipt = fixture
        .resources
        .apply_work_mutation(
            &peri_acp_types::session_resources::work::PreparedWorkCommand::try_new(WorkCommand {
                session_id: fixture.thread_id(),
                recipient_lifecycle: 1,
                mutation_id: "register-sdk-ticket".into(),
                action: WorkAction::RegisterAdmission {
                    admission: admission.clone(),
                },
            })
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
}

#[tokio::test]
async fn sdk_observation_rejects_unregistered_ticket_without_local_admission() {
    let (fixture, mailbox, admission) = fixture().await;
    let before = fixture
        .resources
        .load_session_work(&WorkQuery {
            session_id: fixture.thread_id(),
            limit: 1,
        })
        .await
        .unwrap();
    assert!(matches!(
        mailbox.observe_sdk_run(&admission).await,
        Err(UserInputQueueError::DurableRejected(_))
    ));
    assert!(mailbox.active_run_ticket().is_none());
    let after = fixture
        .resources
        .load_session_work(&WorkQuery {
            session_id: fixture.thread_id(),
            limit: 1,
        })
        .await
        .unwrap();
    assert_eq!(before.state, after.state);
}

#[tokio::test]
async fn sdk_observation_starts_exact_ticket_and_settles_only_matching_terminal() {
    let (fixture, mailbox, admission) = fixture().await;
    register(&fixture, &admission).await;
    let ticket = mailbox.observe_sdk_run(&admission).await.unwrap();
    assert_eq!(ticket.id, admission.admission_id);
    assert_eq!(mailbox.observe_sdk_run(&admission).await.unwrap(), ticket);
    assert_eq!(
        mailbox
            .durable
            .as_ref()
            .unwrap()
            .snapshot_loads
            .load(std::sync::atomic::Ordering::SeqCst),
        4
    );
    assert_eq!(
        mailbox
            .durable
            .as_ref()
            .unwrap()
            .control_loads
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert!(
        mailbox.sdk_run_input_ids(&ticket).is_empty(),
        "publication is not a claim"
    );
    assert!(mailbox.attach_attempt(&ticket, CancellationToken::new()));
    assert!(
        matches!(mailbox.run_started_event(&ticket), Some(StateEvent::UserInputRunStarted { request_id, turn_id, generation, .. })
        if request_id == admission.admission_id && turn_id == admission.execution.turn_id && generation == mailbox.generation())
    );
    mailbox.finish_attempt(
        &UserInputRunTicket {
            id: "late-old-sdk-ticket".into(),
        },
        UserInputAttemptOutcome::Failed,
    );
    assert_eq!(
        mailbox.snapshot().active_request_id.as_deref(),
        Some(admission.admission_id.as_str())
    );
    assert!(mailbox.finish_sdk_run(&admission, UserInputAttemptOutcome::Completed));
    assert!(mailbox.snapshot().active_request_id.is_none());
    assert!(!mailbox.finish_sdk_run(&admission, UserInputAttemptOutcome::Failed));
    assert!(mailbox.reserve_run().is_none());
}

#[tokio::test]
async fn sdk_observation_recovers_input_ids_only_from_exact_durable_batch() {
    let (fixture, mailbox, admission) = fixture().await;
    register(&fixture, &admission).await;
    let snapshot = fixture
        .resources
        .load_session_work(&WorkQuery {
            session_id: fixture.thread_id(),
            limit: 1,
        })
        .await
        .unwrap();
    let receipt = fixture
        .resources
        .apply_work_mutation(
            &peri_acp_types::session_resources::work::PreparedWorkCommand::try_new(WorkCommand {
                session_id: fixture.thread_id(),
                recipient_lifecycle: 1,
                mutation_id: "claim-sdk-exact-batch".into(),
                action: WorkAction::ClaimBatch {
                    guard: WorkGuard {
                        expected_revision: snapshot.state.revision,
                        expected_control_generation: snapshot.control.control_generation,
                        execution: admission.execution.clone(),
                    },
                    batch_id: admission.work_id.clone(),
                    delivery_ids: snapshot.candidates[0].delivery_ids.clone(),
                },
            })
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    let ticket = mailbox.observe_sdk_run(&admission).await.unwrap();
    let persisted = fixture
        .resources
        .load_session_work(&WorkQuery {
            session_id: fixture.thread_id(),
            limit: 1,
        })
        .await
        .unwrap();
    let batch = &persisted.state.batches[&admission.work_id];
    assert_eq!(batch.execution, admission.execution);
    assert_eq!(batch.processing_delivery_ids.len(), 1);
    let delivery = &persisted.state.deliveries[&batch.processing_delivery_ids[0]];
    let identity: serde_json::Value =
        serde_json::from_str(delivery.publication.event.causation_id.as_deref().unwrap()).unwrap();
    let publication_generation = identity["publication_generation"].as_str().unwrap();
    let input_id = delivery
        .publication
        .event
        .content
        .message_id
        .as_uuid()
        .to_string();
    let draft = &persisted.state.staged_user_inputs[&input_id];
    assert_eq!(draft.command_id, "publication-generation");
    assert_ne!(publication_generation, draft.command_id);
    assert_eq!(
        delivery.publication.event.event_id,
        format!("user-input:{input_id}:{publication_generation}")
    );
    let history = fixture
        .resources
        .load_session_history(&fixture.thread_id())
        .await
        .unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(mailbox.sdk_run_input_ids(&ticket), vec![history[0].id()]);
    assert_eq!(
        history[0].id(),
        delivery.publication.event.content.message_id
    );
    assert_eq!(mailbox.sdk_run_publication_generations(&ticket).len(), 1);
    assert_eq!(
        mailbox.sdk_run_publication_generations(&ticket)[&history[0].id().as_uuid().to_string()],
        publication_generation
    );
}

#[tokio::test]
async fn sdk_observation_rejects_changed_execution_on_registered_identity() {
    let (fixture, mailbox, mut admission) = fixture().await;
    register(&fixture, &admission).await;
    admission.execution.attempt_id = AttemptId::new();
    assert!(matches!(
        mailbox.observe_sdk_run(&admission).await,
        Err(UserInputQueueError::DurableRejected(_))
    ));
    assert!(mailbox.active_run_ticket().is_none());
}

#[tokio::test]
async fn sdk_attach_after_root_binding_preserves_parent_cancel_and_is_idempotent() {
    let (fixture, mailbox, admission) = fixture().await;
    register(&fixture, &admission).await;
    let ticket = mailbox.observe_sdk_run(&admission).await.unwrap();
    let parent = CancellationToken::new();
    let child = parent.child_token();
    assert!(mailbox.attach_attempt(&ticket, parent.clone()));
    let before = mailbox.snapshot();
    assert_eq!(mailbox.observe_sdk_run(&admission).await.unwrap(), ticket);
    assert!(mailbox.attach_sdk_attempt(&admission, child.clone()));
    assert_eq!(
        serde_json::to_value(mailbox.snapshot()).unwrap(),
        serde_json::to_value(before).unwrap()
    );
    assert!(!mailbox.attach_attempt(&ticket, child.clone()));
    child.cancel();
    assert!(!parent.is_cancelled());
    assert!(mailbox.stop_attempt(&ticket.id, mailbox.generation()));
    assert!(parent.is_cancelled());
}

#[tokio::test]
async fn sdk_attach_first_binding_starts_once_and_rejects_foreign_admission() {
    let (fixture, mailbox, admission) = fixture().await;
    register(&fixture, &admission).await;
    let ticket = mailbox.observe_sdk_run(&admission).await.unwrap();
    let before = fixture
        .resources
        .load_session_work(&WorkQuery {
            session_id: fixture.thread_id(),
            limit: 1,
        })
        .await
        .unwrap();
    let parent = CancellationToken::new();
    assert!(mailbox.attach_sdk_attempt(&admission, parent.clone()));
    assert!(mailbox.run_started_event(&ticket).is_some());
    let attached = mailbox.snapshot();
    assert!(mailbox.attach_sdk_attempt(&admission, parent.child_token()));
    assert_eq!(
        serde_json::to_value(mailbox.snapshot()).unwrap(),
        serde_json::to_value(&attached).unwrap()
    );
    let mut foreign = admission.clone();
    foreign.execution.attempt_id = AttemptId::new();
    assert!(!mailbox.attach_sdk_attempt(&foreign, CancellationToken::new()));
    foreign = admission.clone();
    foreign.control_generation += 1;
    assert!(!mailbox.attach_sdk_attempt(&foreign, CancellationToken::new()));
    foreign = admission.clone();
    foreign.admission_id = "foreign-ticket".into();
    assert!(!mailbox.attach_sdk_attempt(&foreign, CancellationToken::new()));
    assert_eq!(
        serde_json::to_value(mailbox.snapshot()).unwrap(),
        serde_json::to_value(&attached).unwrap()
    );
    let after = fixture
        .resources
        .load_session_work(&WorkQuery {
            session_id: fixture.thread_id(),
            limit: 1,
        })
        .await
        .unwrap();
    assert_eq!(after.state, before.state);
    assert!(mailbox.stop_attempt(&ticket.id, mailbox.generation()));
    assert!(parent.is_cancelled());
    assert!(!mailbox.attach_sdk_attempt(&admission, CancellationToken::new()));
}

#[tokio::test]
async fn repeated_sdk_observation_rejects_changed_store_control() {
    use peri_acp_types::session_resources::{ControlAction, ControlCommand, ControlDecision};
    let (fixture, mailbox, admission) = fixture().await;
    register(&fixture, &admission).await;
    mailbox.observe_sdk_run(&admission).await.unwrap();
    let control = fixture
        .resources
        .load_session_control(&fixture.thread_id())
        .await
        .unwrap();
    let receipt = fixture
        .resources
        .apply_session_control(&ControlCommand {
            session_id: fixture.thread_id(),
            command_id: "stop-observed-run".into(),
            expected_lifecycle: control.lifecycle,
            expected_revision: control.revision,
            expected_control_generation: control.control_generation,
            action: ControlAction::Stop {
                target: admission.execution.clone(),
            },
        })
        .await
        .unwrap();
    assert_eq!(receipt.decision, ControlDecision::Accepted);
    assert!(matches!(
        mailbox.observe_sdk_run(&admission).await,
        Err(UserInputQueueError::DurableRejected(_))
    ));
    assert_eq!(
        mailbox
            .durable
            .as_ref()
            .unwrap()
            .snapshot_loads
            .load(std::sync::atomic::Ordering::SeqCst),
        4
    );
    assert_eq!(
        mailbox
            .durable
            .as_ref()
            .unwrap()
            .control_loads
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

#[tokio::test]
async fn sdk_observation_reuses_registered_snapshot_but_rechecks_store_control() {
    let (fixture, mailbox, admission) = fixture().await;
    register(&fixture, &admission).await;
    let snapshot = fixture
        .resources
        .load_session_work(&WorkQuery {
            session_id: fixture.thread_id(),
            limit: 1,
        })
        .await
        .unwrap();
    let mut foreign = snapshot.clone();
    foreign.session_id = "different-session".into();
    assert!(matches!(
        mailbox
            .observe_sdk_run_from_snapshot(&admission, &foreign)
            .await,
        Err(UserInputQueueError::DurableRejected(_))
    ));
    let ticket = mailbox
        .observe_sdk_run_from_snapshot(&admission, &snapshot)
        .await
        .unwrap();
    assert_eq!(ticket.id, admission.admission_id);
    assert_eq!(
        mailbox
            .durable
            .as_ref()
            .unwrap()
            .snapshot_loads
            .load(std::sync::atomic::Ordering::SeqCst),
        3
    );
    assert_eq!(
        mailbox
            .durable
            .as_ref()
            .unwrap()
            .control_loads
            .load(std::sync::atomic::Ordering::SeqCst),
        2
    );
    mailbox.invalidate();
    assert_eq!(
        mailbox
            .observe_sdk_run_from_snapshot(&admission, &snapshot)
            .await,
        Err(UserInputQueueError::Closed)
    );
}

#[tokio::test]
async fn sdk_attach_rejects_unobserved_and_finished_run() {
    let (fixture, mailbox, admission) = fixture().await;
    assert!(!mailbox.attach_sdk_attempt(&admission, CancellationToken::new()));
    register(&fixture, &admission).await;
    mailbox.observe_sdk_run(&admission).await.unwrap();
    assert!(mailbox.finish_sdk_run(&admission, UserInputAttemptOutcome::Completed));
    assert!(!mailbox.attach_sdk_attempt(&admission, CancellationToken::new()));
}
