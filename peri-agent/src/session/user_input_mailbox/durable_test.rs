use super::*;
use crate::session::test_resources::TestSession;
use peri_acp_types::identity::AttemptId;
use peri_acp_types::session::{MessageQueue, MessageRequirement};
use peri_acp_types::session_resources::ControlAttempt;

fn mailbox(
    fixture: &TestSession,
    store: Arc<dyn SessionResources>,
) -> (Arc<UserInputMailbox>, Arc<SessionInbox>) {
    let inbox = Arc::new(SessionInbox::new(Arc::new(MessageQueue::new())));
    let mailbox = UserInputMailbox::new_durable(
        fixture.thread_id(),
        inbox.clone(),
        Arc::new(|_| {}),
        store,
        1,
    );
    (mailbox, inbox)
}

fn input(mailbox: &UserInputMailbox) -> EnqueueUserInputRequest {
    EnqueueUserInputRequest {
        session_id: mailbox.session_id.clone(),
        generation: mailbox.generation().into(),
        command_id: uuid::Uuid::now_v7().to_string(),
        input_id: uuid::Uuid::now_v7().to_string(),
        content: MessageContent::text("process durable input"),
        original_draft: "@image draft\nprocess durable input".into(),
    }
}

async fn load(fixture: &TestSession) -> WorkInspection {
    fixture
        .resources
        .inspect_work(&WorkQuery::new(
            fixture.thread_id(),
            WorkSelector::Availability,
        ))
        .await
        .unwrap()
}

fn candidates(inspection: &WorkInspection) -> &[WorkCandidate] {
    let WorkPage::Availability(availability) = &inspection.page else {
        panic!("availability page required")
    };
    &availability.candidates
}

async fn input_delivery(fixture: &TestSession, input_id: &str) -> Delivery {
    let inspected = fixture
        .resources
        .inspect_work(&WorkQuery::new(
            fixture.thread_id(),
            WorkSelector::Draft {
                input_id: input_id.into(),
            },
        ))
        .await
        .unwrap();
    let WorkPage::Drafts(drafts) = inspected.page else {
        panic!("draft page required")
    };
    delivery(fixture, drafts[0].publication_id.as_ref().unwrap()).await
}

async fn delivery(fixture: &TestSession, delivery_id: &str) -> Delivery {
    let inspected = fixture
        .resources
        .inspect_work(&WorkQuery::new(
            fixture.thread_id(),
            WorkSelector::Delivery {
                delivery_id: delivery_id.into(),
            },
        ))
        .await
        .unwrap();
    let WorkPage::Deliveries(mut deliveries) = inspected.page else {
        panic!("delivery page required")
    };
    deliveries.pop().unwrap()
}

fn takeback(
    mailbox: &UserInputMailbox,
    input: &EnqueueUserInputRequest,
) -> TakeBackUserInputRequest {
    TakeBackUserInputRequest {
        session_id: mailbox.session_id.clone(),
        generation: mailbox.generation().into(),
        command_id: uuid::Uuid::now_v7().to_string(),
        input_id: input.input_id.clone(),
    }
}

async fn admit(fixture: &TestSession) -> WorkAdmission {
    let snapshot = load(fixture).await;
    let candidate = &candidates(&snapshot)[0];
    let admission = WorkAdmission {
        session_id: fixture.thread_id(),
        admission_id: uuid::Uuid::now_v7().to_string(),
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
    let receipt = fixture
        .resources
        .apply_work_mutation(&WorkCommand {
            session_id: fixture.thread_id(),
            recipient_lifecycle: 1,
            mutation_id: uuid::Uuid::now_v7().to_string(),
            action: WorkAction::RegisterAdmission {
                admission: admission.clone(),
            },
        })
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    admission
}

fn claim_command(
    fixture: &TestSession,
    snapshot: &WorkInspection,
    admission: &WorkAdmission,
) -> WorkCommand {
    WorkCommand {
        session_id: fixture.thread_id(),
        recipient_lifecycle: 1,
        mutation_id: uuid::Uuid::now_v7().to_string(),
        action: WorkAction::ClaimBatch {
            guard: WorkGuard {
                expected_revision: snapshot.head.change_seq,
                expected_control_generation: snapshot.control.control_generation,
                execution: admission.execution.clone(),
            },
            batch_id: admission.work_id.clone(),
            delivery_ids: candidates(snapshot)[0].delivery_ids.clone(),
        },
    }
}

#[tokio::test]
async fn accepted_has_recoverable_required_receipt_and_distinct_delivery_identity() {
    let fixture = TestSession::open().await;
    let (mailbox, inbox) = mailbox(&fixture, fixture.resources());
    let request = input(&mailbox);
    let receipt = mailbox.enqueue_durable(&request).await.unwrap();
    let snapshot = load(&fixture).await;
    assert_eq!(receipt.work_receipts.len(), 2);
    assert_eq!(receipt.work_receipts[0].decision, WorkDecision::Accepted);
    let delivery = input_delivery(&fixture, &request.input_id).await;
    assert_eq!(
        delivery.publication.policy.requirement,
        MessageRequirement::Required
    );
    assert_eq!(
        delivery
            .publication
            .event
            .content
            .message_id
            .as_uuid()
            .to_string(),
        request.input_id
    );
    assert_eq!(
        delivery.publication.event.event_id,
        format!(
            "user-input:{}:{}",
            request.input_id, receipt.publication_generations[&request.input_id]
        )
    );
    assert_ne!(delivery.publication.delivery_id, request.input_id);
    assert!(delivery.projection.is_none());
    let messages = inbox.queue().drain_all();
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].delivery_id.unwrap().as_uuid().to_string(),
        delivery.publication.delivery_id
    );
    assert_eq!(
        messages[0].message().unwrap().id().as_uuid().to_string(),
        request.input_id
    );
    assert!(
        mailbox.reserve_run().is_none(),
        "Peri must not own SDK admission"
    );
}

#[tokio::test]
async fn busy_enqueue_stages_draft_until_idle_without_interrupting_execution() {
    let fixture = TestSession::open().await;
    let (mailbox, inbox) = mailbox(&fixture, fixture.resources());
    let cancel = CancellationToken::new();
    let ticket = mailbox
        .attach_external_attempt(cancel.clone(), false)
        .unwrap();
    let request = input(&mailbox);
    let receipt = mailbox.enqueue_durable(&request).await.unwrap();
    assert_eq!(receipt.results[0].state, UserInputState::Queued);
    assert_eq!(receipt.work_receipts.len(), 1);
    assert!(!cancel.is_cancelled());
    assert!(inbox.queue().drain_all().is_empty());
    let staged = load(&fixture).await;
    assert_eq!(staged.head.required_count, 0);
    assert_eq!(staged.head.unresolved_effects, 0);
    assert!(candidates(&staged).is_empty());
    assert_eq!(mailbox.snapshot().items.len(), 1);
    mailbox.enter_idle_durable().await.unwrap();
    mailbox.finish_attempt(&ticket, UserInputAttemptOutcome::Completed);
    assert_eq!(inbox.queue().drain_all().len(), 1);
    assert_eq!(load(&fixture).await.head.required_count, 1);
    assert!(mailbox.reserve_run().is_none());
}

#[tokio::test]
async fn restart_replay_returns_original_publication_receipt() {
    let fixture = TestSession::open().await;
    let (first, _) = mailbox(&fixture, fixture.resources());
    let request = input(&first);
    let accepted = first.enqueue_durable(&request).await.unwrap();
    let (restarted, inbox) = mailbox(&fixture, fixture.resources());
    let replay = restarted.enqueue_durable(&request).await.unwrap();
    assert_eq!(accepted.work_receipts, replay.work_receipts);
    assert_eq!(
        accepted.publication_generations,
        replay.publication_generations
    );
    assert_eq!(load(&fixture).await.head.required_count, 1);
    assert_eq!(inbox.queue().drain_all().len(), 1);
}

#[tokio::test]
async fn withdraw_then_resend_new_generation_cannot_revive_old_event() {
    let fixture = TestSession::open().await;
    let (mailbox, inbox) = mailbox(&fixture, fixture.resources());
    let old = input(&mailbox);
    let old_receipt = mailbox.enqueue_durable(&old).await.unwrap();
    let withdrawal = mailbox
        .take_back_durable(&takeback(&mailbox, &old))
        .await
        .unwrap();
    assert_eq!(
        withdrawal.taken_back.unwrap().original_draft,
        old.original_draft
    );
    assert!(inbox.queue().drain_all().is_empty());
    let old_delivery = input_delivery(&fixture, &old.input_id).await;
    let mut new = old.clone();
    new.command_id = uuid::Uuid::now_v7().to_string();
    let new_receipt = mailbox.enqueue_durable(&new).await.unwrap();
    assert_ne!(
        old_receipt.publication_generations,
        new_receipt.publication_generations
    );
    mailbox.enqueue_durable(&old).await.unwrap();
    let archived = delivery(&fixture, &old_delivery.delivery_id).await;
    assert!(archived.disposition.is_some());
    assert_eq!(archived.obligation, ObligationStatus::Abandoned);
    let current = input_delivery(&fixture, &new.input_id).await;
    assert!(current.disposition.is_none());
    assert_ne!(current.delivery_id, archived.delivery_id);
    assert_eq!(inbox.queue().drain_all().len(), 1);
}

#[tokio::test]
async fn durable_claim_blocks_withdrawal_and_keeps_canonical_message_identity() {
    let fixture = TestSession::open().await;
    let (mailbox, _) = mailbox(&fixture, fixture.resources());
    let request = input(&mailbox);
    mailbox.enqueue_durable(&request).await.unwrap();
    let admission = admit(&fixture).await;
    let snapshot = load(&fixture).await;
    let command = claim_command(&fixture, &snapshot, &admission);
    assert_eq!(
        fixture
            .resources
            .apply_work_mutation(&command)
            .await
            .unwrap()
            .decision,
        WorkDecision::Accepted
    );
    assert!(matches!(
        mailbox
            .take_back_durable(&takeback(&mailbox, &request))
            .await,
        Err(UserInputQueueError::DurableRejected(_))
    ));
    let snapshot = load(&fixture).await;
    let delivery = input_delivery(&fixture, &request.input_id).await;
    assert!(delivery.projection.is_some());
    assert_eq!(
        delivery
            .publication
            .event
            .content
            .message_id
            .as_uuid()
            .to_string(),
        request.input_id
    );
    assert!(
        mailbox.snapshot().items.is_empty(),
        "canonical Delivered must leave the pending queue"
    );
    assert_eq!(
        mailbox
            .state
            .lock()
            .records
            .iter()
            .find(|record| record.input.input_id == request.input_id)
            .unwrap()
            .state,
        UserInputState::Delivered
    );
    let history = fixture
        .resources
        .load_session_history(&fixture.thread_id())
        .await
        .unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].id().as_uuid().to_string(), request.input_id);
    assert!(
        matches!(&history[0], PersistedPayload::Message(BaseMessage::Human { content, .. }) if content == &request.content)
    );
}

#[tokio::test]
async fn withdrawal_wins_before_claim_cas_without_processing_projection() {
    let fixture = TestSession::open().await;
    let (mailbox, _) = mailbox(&fixture, fixture.resources());
    let request = input(&mailbox);
    mailbox.enqueue_durable(&request).await.unwrap();
    let admission = admit(&fixture).await;
    let snapshot = load(&fixture).await;
    let command = claim_command(&fixture, &snapshot, &admission);
    mailbox
        .take_back_durable(&takeback(&mailbox, &request))
        .await
        .unwrap();
    let receipt = fixture
        .resources
        .apply_work_mutation(&command)
        .await
        .unwrap();
    assert!(matches!(receipt.decision, WorkDecision::Rejected { .. }));
    assert!(input_delivery(&fixture, &request.input_id)
        .await
        .projection
        .is_none());
}

#[tokio::test]
async fn store_write_failure_never_accepts_or_hands_off_and_freezes_original_command() {
    let fixture = TestSession::open().await;
    let (mailbox, inbox) = mailbox(&fixture, fixture.read_only_resources().await);
    let original = input(&mailbox);
    assert_eq!(
        mailbox.enqueue_durable(&original).await.unwrap_err(),
        UserInputQueueError::OutcomeUnknown
    );
    let frozen = {
        let operations = mailbox.durable.as_ref().unwrap().operations.lock().await;
        operations[&original.command_id].commands[0].clone()
    };
    let mut retry = original.clone();
    retry.command_id = uuid::Uuid::now_v7().to_string();
    assert_eq!(
        mailbox.enqueue_durable(&retry).await.unwrap_err(),
        UserInputQueueError::OutcomeUnknown
    );
    assert_eq!(
        mailbox.enqueue_durable(&original).await.unwrap_err(),
        UserInputQueueError::OutcomeUnknown
    );
    let operations = mailbox.durable.as_ref().unwrap().operations.lock().await;
    assert_eq!(operations[&original.command_id].commands[0], frozen);
    assert!(inbox.queue().drain_all().is_empty());
    assert_eq!(load(&fixture).await.head.required_count, 0);
}

#[tokio::test]
async fn withdrawn_receipt_replays_after_mailbox_restart() {
    let fixture = TestSession::open().await;
    let (first, _) = mailbox(&fixture, fixture.resources());
    let request = input(&first);
    first.enqueue_durable(&request).await.unwrap();
    let withdrawal = takeback(&first, &request);
    let original = first.take_back_durable(&withdrawal).await.unwrap();
    let (restarted, inbox) = mailbox(&fixture, fixture.resources());
    let replay = restarted.take_back_durable(&withdrawal).await.unwrap();
    assert_eq!(replay.work_receipts, original.work_receipts);
    assert_eq!(
        replay.taken_back.unwrap().original_draft,
        request.original_draft
    );
    assert!(inbox.queue().drain_all().is_empty());
}

#[tokio::test]
async fn stop_withdrawal_is_durable_before_return_to_draft() {
    let fixture = TestSession::open().await;
    let (mailbox, inbox) = mailbox(&fixture, fixture.resources());
    let request = input(&mailbox);
    mailbox.enqueue_durable(&request).await.unwrap();
    let original_delivery = input_delivery(&fixture, &request.input_id).await;
    mailbox
        .reclaim_unclaimed_durable("stop-command", 0)
        .await
        .unwrap();
    assert_eq!(
        mailbox.refresh_durable().await.unwrap().items[0].state,
        UserInputState::Queued
    );
    let snapshot = load(&fixture).await;
    assert_eq!(
        delivery(&fixture, &original_delivery.delivery_id)
            .await
            .obligation,
        ObligationStatus::Abandoned
    );
    assert!(inbox.queue().drain_all().is_empty());
    let dispatch = DispatchUserInputsRequest {
        session_id: fixture.thread_id(),
        generation: mailbox.generation().into(),
        command_id: "new-send".into(),
        input_ids: vec![request.input_id],
    };
    assert_eq!(
        mailbox.dispatch_durable(&dispatch).await.unwrap().results[0].state,
        UserInputState::Dispatching
    );
    assert_ne!(
        input_delivery(&fixture, &dispatch.input_ids[0])
            .await
            .delivery_id,
        original_delivery.delivery_id
    );
}

#[tokio::test]
async fn pending_commands_keep_original_authorization_without_revision_refresh() {
    let fixture = TestSession::open().await;
    let (mailbox, _) = mailbox(&fixture, fixture.read_only_resources().await);
    let request = input(&mailbox);
    assert_eq!(
        mailbox.enqueue_durable(&request).await.unwrap_err(),
        UserInputQueueError::OutcomeUnknown
    );
    let operations = mailbox.durable.as_ref().unwrap().operations.lock().await;
    let operation = &operations[&request.command_id];
    let frozen = operation.commands.clone();
    assert!(operation.attempted);
    assert!(operation.uncertain);
    drop(operations);
    assert_eq!(
        mailbox.enqueue_durable(&request).await.unwrap_err(),
        UserInputQueueError::OutcomeUnknown
    );
    let operations = mailbox.durable.as_ref().unwrap().operations.lock().await;
    assert_eq!(operations[&request.command_id].commands, frozen);
}
