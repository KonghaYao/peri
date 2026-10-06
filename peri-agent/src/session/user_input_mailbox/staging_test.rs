use super::*;
use crate::session::test_resources::TestSession;
use peri_acp_types::session::MessageQueue;

fn mailbox(
    fixture: &TestSession,
    store: Arc<dyn SessionResources>,
) -> (Arc<UserInputMailbox>, Arc<SessionInbox>) {
    let inbox = Arc::new(SessionInbox::new(Arc::new(MessageQueue::new())));
    (
        UserInputMailbox::new_durable(
            fixture.thread_id(),
            inbox.clone(),
            Arc::new(|_| {}),
            store,
            1,
        ),
        inbox,
    )
}

fn input(mailbox: &UserInputMailbox, text: &str) -> EnqueueUserInputRequest {
    EnqueueUserInputRequest {
        session_id: mailbox.session_id.clone(),
        generation: mailbox.generation().into(),
        command_id: uuid::Uuid::now_v7().to_string(),
        input_id: uuid::Uuid::now_v7().to_string(),
        content: MessageContent::text(text),
        original_draft: format!("{text}\n完整原稿"),
    }
}

fn dispatch(mailbox: &UserInputMailbox, ids: Vec<String>) -> DispatchUserInputsRequest {
    DispatchUserInputsRequest {
        session_id: mailbox.session_id.clone(),
        generation: mailbox.generation().into(),
        command_id: uuid::Uuid::now_v7().to_string(),
        input_ids: ids,
    }
}

async fn load(fixture: &TestSession) -> WorkSnapshot {
    fixture
        .resources()
        .load_session_work(&WorkQuery {
            session_id: fixture.thread_id(),
            limit: 1,
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn initial_prompt_publishes_only_selected_input_without_cancelling_its_attachment() {
    let fixture = TestSession::open().await;
    let (mailbox, inbox) = mailbox(&fixture, fixture.resources());
    let cancel = CancellationToken::new();
    let ticket = mailbox
        .attach_external_attempt(cancel.clone(), false)
        .unwrap();
    let queued = input(&mailbox, "keep as draft");
    mailbox.enqueue_durable(&queued).await.unwrap();
    let prompt = input(&mailbox, "initial prompt");
    let receipt = mailbox
        .publish_prompt_durable(&prompt, &ticket)
        .await
        .unwrap();
    let replay = mailbox
        .publish_prompt_durable(&prompt, &ticket)
        .await
        .unwrap();
    assert_eq!(receipt.work_receipts, replay.work_receipts);
    assert!(!cancel.is_cancelled());
    assert_eq!(mailbox.active_run_ticket(), Some(ticket));
    let snapshot = load(&fixture).await;
    assert_eq!(snapshot.state.deliveries.len(), 1);
    assert_eq!(snapshot.state.obligations.len(), 1);
    assert_eq!(inbox.queue().len(), 1);
    assert_eq!(
        snapshot.state.staged_user_inputs[&queued.input_id].status,
        StagedUserInputStatus::Queued
    );
    assert_eq!(
        snapshot.state.staged_user_inputs[&prompt.input_id].status,
        StagedUserInputStatus::Published
    );
    assert_eq!(
        snapshot
            .state
            .deliveries
            .values()
            .next()
            .unwrap()
            .publication
            .event
            .event_id,
        format!(
            "user-input:{}:prompt:{}",
            prompt.input_id, prompt.command_id
        )
    );
}

#[tokio::test]
async fn initial_prompt_rejects_foreign_attachment_before_persisting_input() {
    let fixture = TestSession::open().await;
    let (mailbox, _) = mailbox(&fixture, fixture.resources());
    mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    let prompt = input(&mailbox, "foreign prompt");
    let ticket = UserInputRunTicket {
        id: uuid::Uuid::now_v7().to_string(),
    };
    assert!(matches!(
        mailbox.publish_prompt_durable(&prompt, &ticket).await,
        Err(UserInputQueueError::DurableRejected(_))
    ));
    let snapshot = load(&fixture).await;
    assert!(snapshot.state.staged_user_inputs.is_empty());
    assert!(snapshot.state.deliveries.is_empty());
    assert!(snapshot.candidates.is_empty());
}

#[tokio::test]
async fn busy_drafts_restore_in_fifo_order_without_any_inbox_obligation() {
    let fixture = TestSession::open().await;
    let (first, inbox) = mailbox(&fixture, fixture.resources());
    first
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    let first_input = input(&first, "A");
    let second_input = input(&first, "B");
    for request in [&first_input, &second_input] {
        let receipt = first.enqueue_durable(request).await.unwrap();
        assert_eq!(receipt.results[0].state, UserInputState::Queued);
        assert_eq!(receipt.work_receipts.len(), 1);
        assert!(receipt.publication_generations.is_empty());
    }
    assert!(inbox.queue().drain_all().is_empty());
    let snapshot = load(&fixture).await;
    assert!(snapshot.candidates.is_empty());
    assert!(snapshot.state.deliveries.is_empty());
    assert!(snapshot.state.obligations.is_empty());
    let (restored, restored_inbox) = mailbox(&fixture, fixture.resources());
    let snapshot = restored.refresh_durable().await.unwrap();
    assert_eq!(
        snapshot
            .items
            .iter()
            .map(|item| item.input_id.as_str())
            .collect::<Vec<_>>(),
        [
            first_input.input_id.as_str(),
            second_input.input_id.as_str()
        ]
    );
    assert!(snapshot
        .items
        .iter()
        .all(|item| item.state == UserInputState::Queued));
    assert!(restored_inbox.queue().drain_all().is_empty());
    assert!(restored.publish_next_durable().await.unwrap());
    assert!(!restored.publish_next_durable().await.unwrap());
    let published = restored_inbox.queue().drain_all();
    assert_eq!(published.len(), 1);
    assert_eq!(
        published[0].message().unwrap().id().as_uuid().to_string(),
        first_input.input_id
    );
    assert_eq!(load(&fixture).await.state.deliveries.len(), 1);
    assert_eq!(restored.snapshot().items[1].state, UserInputState::Queued);
}

#[tokio::test]
async fn selected_batch_is_atomic_and_later_draft_does_not_join_or_cancel_again() {
    let fixture = TestSession::open().await;
    let (mailbox, inbox) = mailbox(&fixture, fixture.resources());
    let cancel = CancellationToken::new();
    let ticket = mailbox
        .attach_external_attempt(cancel.clone(), false)
        .unwrap();
    let first = input(&mailbox, "A");
    let second = input(&mailbox, "B");
    let third = input(&mailbox, "C");
    for request in [&first, &second, &third] {
        mailbox.enqueue_durable(request).await.unwrap();
    }
    let single = dispatch(&mailbox, vec![second.input_id.clone()]);
    let receipt = mailbox.dispatch_durable(&single).await.unwrap();
    assert_eq!(receipt.work_receipts.len(), 1);
    assert_eq!(receipt.publication_generations.len(), 1);
    assert!(cancel.is_cancelled());
    let messages = inbox.queue().drain_all();
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].message().unwrap().id().as_uuid().to_string(),
        second.input_id
    );
    mailbox.finish_attempt(&ticket, UserInputAttemptOutcome::Interrupted);
    let all = dispatch(
        &mailbox,
        vec![first.input_id.clone(), third.input_id.clone()],
    );
    let accepted = mailbox.dispatch_durable(&all).await.unwrap();
    assert_eq!(accepted.work_receipts.len(), 1);
    assert_eq!(accepted.publication_generations.len(), 2);
    let fourth = input(&mailbox, "D");
    assert_eq!(
        mailbox.enqueue_durable(&fourth).await.unwrap().results[0].state,
        UserInputState::Queued
    );
    let messages = inbox.queue().drain_all();
    assert_eq!(messages.len(), 2);
    assert_eq!(
        messages
            .iter()
            .map(|message| message.message().unwrap().id().as_uuid().to_string())
            .collect::<Vec<_>>(),
        [first.input_id.clone(), third.input_id.clone()]
    );
    assert_eq!(load(&fixture).await.state.deliveries.len(), 3);
    let replay = mailbox.dispatch_durable(&all).await.unwrap();
    assert_eq!(accepted.work_receipts, replay.work_receipts);
    assert!(inbox.queue().drain_all().is_empty());
    assert_eq!(
        mailbox.snapshot().items.last().unwrap().state,
        UserInputState::Queued
    );
}

#[tokio::test]
async fn staged_withdrawal_survives_restart_and_old_enqueue_retry_cannot_publish() {
    let fixture = TestSession::open().await;
    let (first, _) = mailbox(&fixture, fixture.resources());
    first
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    let request = input(&first, "WITHDRAW");
    first.enqueue_durable(&request).await.unwrap();
    let withdrawal = TakeBackUserInputRequest {
        session_id: request.session_id.clone(),
        generation: request.generation.clone(),
        command_id: uuid::Uuid::now_v7().to_string(),
        input_id: request.input_id.clone(),
    };
    let accepted = first.take_back_durable(&withdrawal).await.unwrap();
    assert_eq!(accepted.work_receipts.len(), 1);
    assert_eq!(
        accepted.taken_back.unwrap().original_draft,
        request.original_draft
    );
    let (restored, inbox) = mailbox(&fixture, fixture.resources());
    let replay = restored.take_back_durable(&withdrawal).await.unwrap();
    assert_eq!(accepted.work_receipts, replay.work_receipts);
    restored.enqueue_durable(&request).await.unwrap();
    assert!(!restored.publish_next_durable().await.unwrap());
    assert!(load(&fixture).await.state.deliveries.is_empty());
    assert!(inbox.queue().drain_all().is_empty());
}

#[tokio::test]
async fn uncertain_selection_freezes_original_whole_batch_without_partial_handoff() {
    let fixture = TestSession::open().await;
    let (first, _) = mailbox(&fixture, fixture.resources());
    first
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    let first_input = input(&first, "A");
    let second_input = input(&first, "B");
    for request in [&first_input, &second_input] {
        first.enqueue_durable(request).await.unwrap();
    }
    let (read_only, inbox) = mailbox(&fixture, fixture.read_only_resources().await);
    read_only.refresh_durable().await.unwrap();
    let selection = dispatch(
        &read_only,
        vec![first_input.input_id, second_input.input_id],
    );
    assert_eq!(
        read_only.dispatch_durable(&selection).await.unwrap_err(),
        UserInputQueueError::OutcomeUnknown
    );
    let original = read_only.durable.as_ref().unwrap().operations.lock().await
        [&selection.command_id]
        .commands
        .clone();
    assert_eq!(original.len(), 1);
    let WorkAction::PublishStagedUserInputs { deliveries, .. } = &original[0].action else {
        panic!("whole selection must use one mutation");
    };
    assert_eq!(deliveries.len(), 2);
    let mut retry = selection.clone();
    retry.command_id = uuid::Uuid::now_v7().to_string();
    assert_eq!(
        read_only.dispatch_durable(&retry).await.unwrap_err(),
        UserInputQueueError::OutcomeUnknown
    );
    assert_eq!(
        read_only.dispatch_durable(&selection).await.unwrap_err(),
        UserInputQueueError::OutcomeUnknown
    );
    assert_eq!(
        read_only.durable.as_ref().unwrap().operations.lock().await[&selection.command_id].commands,
        original
    );
    assert!(load(&fixture).await.state.deliveries.is_empty());
    assert!(inbox.queue().drain_all().is_empty());
}

#[tokio::test]
async fn withdrawn_member_rejects_whole_selection_without_publishing_first_member() {
    let fixture = TestSession::open().await;
    let (mailbox, inbox) = mailbox(&fixture, fixture.resources());
    mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    let first = input(&mailbox, "A");
    let second = input(&mailbox, "B");
    for request in [&first, &second] {
        mailbox.enqueue_durable(request).await.unwrap();
    }
    mailbox
        .take_back_durable(&TakeBackUserInputRequest {
            session_id: second.session_id.clone(),
            generation: second.generation.clone(),
            command_id: uuid::Uuid::now_v7().to_string(),
            input_id: second.input_id.clone(),
        })
        .await
        .unwrap();
    let snapshot = load(&fixture).await;
    let selection_id = uuid::Uuid::now_v7().to_string();
    let deliveries = [&first, &second]
        .into_iter()
        .map(|request| {
            let command = new_publication(
                &fixture.thread_id(),
                1,
                UserInput {
                    input_id: request.input_id.clone(),
                    content: request.content.clone(),
                    original_draft: request.original_draft.clone(),
                },
                &selection_id,
                1,
            )
            .unwrap();
            let WorkAction::PublishDelivery { delivery } = command.action else {
                unreachable!()
            };
            delivery
        })
        .collect();
    let receipt = fixture
        .resources()
        .apply_work_mutation(&WorkCommand {
            session_id: fixture.thread_id(),
            recipient_lifecycle: 1,
            mutation_id: selection_id,
            action: WorkAction::PublishStagedUserInputs {
                expected_revision: snapshot.state.revision,
                expected_control_generation: snapshot.control.control_generation,
                expected_attempt: snapshot.control.attempt,
                interrupt_current: true,
                deliveries,
            },
        })
        .await
        .unwrap();
    assert!(matches!(receipt.decision, WorkDecision::Rejected { .. }));
    let current = load(&fixture).await;
    assert_eq!(current.state.revision, snapshot.state.revision);
    assert!(current.state.deliveries.is_empty());
    assert!(current.state.obligations.is_empty());
    assert_eq!(
        current.state.staged_user_inputs[&first.input_id].status,
        StagedUserInputStatus::Queued
    );
    assert!(inbox.queue().drain_all().is_empty());
}

#[tokio::test]
async fn selection_replay_after_restart_preserves_receipt_and_does_not_cancel_new_sdk_target() {
    use peri_acp_types::identity::AttemptId;
    use peri_acp_types::session_resources::ControlAttempt;
    let fixture = TestSession::open().await;
    let (first, _) = mailbox(&fixture, fixture.resources());
    first
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    let request = input(&first, "SELECTED");
    first.enqueue_durable(&request).await.unwrap();
    let selection = dispatch(&first, vec![request.input_id]);
    let original = first.dispatch_durable(&selection).await.unwrap();
    let (restored, _) = mailbox(&fixture, fixture.resources());
    let snapshot = load(&fixture).await;
    let candidate = &snapshot.candidates[0];
    let admission = WorkAdmission {
        session_id: fixture.thread_id(),
        admission_id: uuid::Uuid::now_v7().to_string(),
        instance_id: "explicit-sdk-fixture".into(),
        generation_id: "fixture-generation".into(),
        lifecycle: snapshot.control.lifecycle,
        control_generation: snapshot.control.control_generation,
        work_id: candidate.work_id.clone(),
        work_revision: candidate.work_revision,
        execution: ControlAttempt {
            turn_id: TurnId::new(),
            attempt_id: AttemptId::new(),
        },
    };
    let registered = fixture
        .resources()
        .apply_work_mutation(&WorkCommand {
            session_id: fixture.thread_id(),
            recipient_lifecycle: admission.lifecycle,
            mutation_id: uuid::Uuid::now_v7().to_string(),
            action: WorkAction::RegisterAdmission {
                admission: admission.clone(),
            },
        })
        .await
        .unwrap();
    assert_eq!(registered.decision, WorkDecision::Accepted);
    restored.observe_sdk_run(&admission).await.unwrap();
    let cancel = CancellationToken::new();
    assert!(restored.attach_sdk_attempt(&admission, cancel.clone()));
    let before = load(&fixture).await;
    let replay = restored.dispatch_durable(&selection).await.unwrap();
    assert_eq!(original.work_receipts, replay.work_receipts);
    assert!(!cancel.is_cancelled());
    let after = load(&fixture).await;
    assert_eq!(before.control, after.control);
    assert_eq!(before.state, after.state);
}

#[tokio::test]
async fn stale_control_generation_rejects_frozen_selection_without_any_publication() {
    use peri_acp_types::session_resources::{ControlAction, ControlCommand, ControlDecision};
    let fixture = TestSession::open().await;
    let (mailbox, inbox) = mailbox(&fixture, fixture.resources());
    mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    let request = input(&mailbox, "STALE");
    mailbox.enqueue_durable(&request).await.unwrap();
    let before = load(&fixture).await;
    let paused = fixture
        .resources()
        .apply_session_control(&ControlCommand {
            session_id: fixture.thread_id(),
            command_id: uuid::Uuid::now_v7().to_string(),
            expected_lifecycle: before.control.lifecycle,
            expected_revision: before.control.revision,
            expected_control_generation: before.control.control_generation,
            action: ControlAction::Pause,
        })
        .await
        .unwrap();
    assert_eq!(paused.decision, ControlDecision::Accepted);
    let publication = new_publication(
        &fixture.thread_id(),
        1,
        UserInput {
            input_id: request.input_id.clone(),
            content: request.content,
            original_draft: request.original_draft,
        },
        "original-selection",
        1,
    )
    .unwrap();
    let WorkAction::PublishDelivery { delivery } = publication.action else {
        unreachable!()
    };
    let receipt = fixture
        .resources()
        .apply_work_mutation(&WorkCommand {
            session_id: fixture.thread_id(),
            recipient_lifecycle: 1,
            mutation_id: uuid::Uuid::now_v7().to_string(),
            action: WorkAction::PublishStagedUserInputs {
                expected_revision: before.state.revision,
                expected_control_generation: before.control.control_generation,
                expected_attempt: before.control.attempt,
                interrupt_current: true,
                deliveries: vec![delivery],
            },
        })
        .await
        .unwrap();
    assert_eq!(
        receipt.decision,
        WorkDecision::Rejected {
            reason: WorkRejection::StaleControlGeneration
        }
    );
    let current = load(&fixture).await;
    assert_eq!(current.control, paused.state);
    assert_eq!(
        current.state.staged_user_inputs[&request.input_id].status,
        StagedUserInputStatus::Queued
    );
    assert!(current.state.deliveries.is_empty());
    assert!(inbox.queue().drain_all().is_empty());
}

#[tokio::test]
async fn explicit_selection_after_pause_resumes_atomically_without_fabricating_attempt() {
    use peri_acp_types::session_resources::{
        ControlAction, ControlCommand, ControlDecision, ControlStatus,
    };
    let fixture = TestSession::open().await;
    let (mailbox, inbox) = mailbox(&fixture, fixture.resources());
    let snapshot = load(&fixture).await;
    let paused = fixture
        .resources()
        .apply_session_control(&ControlCommand {
            session_id: fixture.thread_id(),
            command_id: uuid::Uuid::now_v7().to_string(),
            expected_lifecycle: snapshot.control.lifecycle,
            expected_revision: snapshot.control.revision,
            expected_control_generation: snapshot.control.control_generation,
            action: ControlAction::Pause,
        })
        .await
        .unwrap();
    assert_eq!(paused.decision, ControlDecision::Accepted);
    let request = input(&mailbox, "STOP_PRESERVED");
    assert_eq!(
        mailbox.enqueue_durable(&request).await.unwrap().results[0].state,
        UserInputState::Queued
    );
    assert!(load(&fixture).await.state.deliveries.is_empty());
    let selection = dispatch(&mailbox, vec![request.input_id]);
    mailbox.dispatch_durable(&selection).await.unwrap();
    let resumed = load(&fixture).await;
    assert_eq!(resumed.control.status, ControlStatus::Active);
    assert!(resumed.control.attempt.is_none());
    assert!(resumed.control.control_generation > paused.state.control_generation);
    assert_eq!(resumed.state.deliveries.len(), 1);
    assert_eq!(inbox.queue().drain_all().len(), 1);
    mailbox.dispatch_durable(&selection).await.unwrap();
    assert_eq!(load(&fixture).await.control, resumed.control);
    assert!(inbox.queue().drain_all().is_empty());
}
