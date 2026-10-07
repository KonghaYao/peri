use super::*;
use crate::session::test_resources::TestSession;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::session::MessagePolicy;
use peri_acp_types::session_resources::work::{
    DeliveryPurpose, ObligationStatus, PublishDelivery, WorkAction, WorkCommand, WorkCommandQuery,
    WorkEvent, WorkGuard, WorkPayload, WorkStage, WorkTarget,
};
use peri_acp_types::session_resources::{
    ControlAction, ControlAttempt, ControlCommand, ControlDecision,
};
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
    let receipt = barrier
        .commit(
            &peri_acp_types::session_resources::work::PreparedWorkCommand::try_new(command.clone())
                .unwrap(),
        )
        .await
        .unwrap();
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
    let original = barrier
        .commit(
            &peri_acp_types::session_resources::work::PreparedWorkCommand::try_new(command.clone())
                .unwrap(),
        )
        .await
        .unwrap();
    let repeated = barrier
        .commit(
            &peri_acp_types::session_resources::work::PreparedWorkCommand::try_new(command.clone())
                .unwrap(),
        )
        .await
        .unwrap();
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
    let error = barrier
        .commit(
            &peri_acp_types::session_resources::work::PreparedWorkCommand::try_new(command.clone())
                .unwrap(),
        )
        .await
        .unwrap_err();
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

async fn entered_reason_command() -> (
    super::super::work_test_support::ProductionFixture,
    WorkMutationBarrier,
    WorkCommand,
) {
    let fixture =
        super::super::work_test_support::fixture(Arc::new(super::super::NullReactLLM), Vec::new())
            .await;
    fixture
        .context
        .work
        .ensure(&fixture.context)
        .await
        .unwrap()
        .unwrap();
    let barrier = WorkMutationBarrier::new(fixture.bound.resources());
    let snapshot = barrier
        .snapshot(&WorkQuery {
            session_id: fixture.bound.thread_id(),
            limit: 1,
        })
        .await
        .unwrap();
    let guard = WorkGuard {
        expected_revision: snapshot.state.revision,
        expected_control_generation: fixture.admission.control_generation,
        execution: fixture.admission.execution.clone(),
    };
    barrier
        .commit(
            &peri_acp_types::session_resources::work::PreparedWorkCommand::try_new(WorkCommand {
                session_id: fixture.bound.thread_id(),
                recipient_lifecycle: fixture.admission.lifecycle,
                mutation_id: uuid::Uuid::now_v7().to_string(),
                action: WorkAction::ClaimBatch {
                    guard,
                    batch_id: fixture.admission.work_id.clone(),
                    delivery_ids: vec![fixture.delivery_id.clone()],
                },
            })
            .unwrap(),
        )
        .await
        .unwrap();
    let snapshot = barrier
        .snapshot(&WorkQuery {
            session_id: fixture.bound.thread_id(),
            limit: 1,
        })
        .await
        .unwrap();
    let command = WorkCommand {
        session_id: fixture.bound.thread_id(),
        recipient_lifecycle: fixture.admission.lifecycle,
        mutation_id: uuid::Uuid::now_v7().to_string(),
        action: WorkAction::BeginReason {
            guard: WorkGuard {
                expected_revision: snapshot.state.revision,
                expected_control_generation: fixture.admission.control_generation,
                execution: fixture.admission.execution.clone(),
            },
            target: WorkTarget {
                work_id: fixture.admission.work_id.clone(),
                expected_work_revision: snapshot.state.works[&fixture.admission.work_id].revision,
            },
            request_id: uuid::Uuid::now_v7().to_string(),
            request: super::super::work_pipeline::request_checkpoint(
                &serde_json::json!({
                    "messages": [{"role": "user", "content": "exact complete body".repeat(1000)}],
                    "tools": [{"name": "probe", "parameters": {"type": "object"}}],
                    "model": "frozen-model", "endpoint": "https://fixture.invalid/model"
                }),
                "frozen-model".into(),
                fixture.admission.admission_id.clone(),
            )
            .unwrap(),
        },
    };
    (fixture, barrier, command)
}

async fn rejected_receipt(barrier: &WorkMutationBarrier, command: &WorkCommand) -> WorkReceipt {
    let WorkCommitError::Rejected { receipt } = barrier
        .commit(
            &peri_acp_types::session_resources::work::PreparedWorkCommand::try_new(command.clone())
                .unwrap(),
        )
        .await
        .unwrap_err()
    else {
        panic!("expected a definite rejection");
    };
    *receipt
}

#[tokio::test]
async fn execution_transition_refreshes_only_global_revision_and_preserves_old_rejection() {
    let (fixture, barrier, original) = entered_reason_command().await;
    let publication_receipt = barrier
        .commit(
            &peri_acp_types::session_resources::work::PreparedWorkCommand::try_new(publication(
                &original.session_id,
                original.recipient_lifecycle,
            ))
            .unwrap(),
        )
        .await
        .unwrap();
    let rejected = rejected_receipt(&barrier, &original).await;
    assert_eq!(
        rejected.decision,
        WorkDecision::Rejected {
            reason: WorkRejection::StaleRevision
        }
    );
    let accepted = barrier
        .commit_execution_transition(
            &peri_acp_types::session_resources::work::PreparedWorkCommand::try_new(
                original.clone(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(accepted.decision, WorkDecision::Accepted);
    assert_ne!(accepted.mutation_id, original.mutation_id);
    let saved = fixture
        .bound
        .resources()
        .load_work_command(&WorkCommandQuery {
            session_id: original.session_id.clone(),
            mutation_id: accepted.mutation_id.clone(),
        })
        .await
        .unwrap()
        .unwrap();
    let mut expected = original.clone();
    expected.mutation_id = accepted.mutation_id.clone();
    let WorkAction::BeginReason { guard, .. } = &mut expected.action else {
        unreachable!()
    };
    guard.expected_revision = publication_receipt.revision;
    assert_eq!(saved.command, expected);
    assert_eq!(
        saved.resolution,
        Some(WorkResolution::Applied {
            receipt: accepted.clone()
        })
    );
    let snapshot = barrier
        .snapshot(&WorkQuery {
            session_id: original.session_id.clone(),
            limit: 1,
        })
        .await
        .unwrap();
    let work = &snapshot.state.works[&fixture.admission.work_id];
    assert_eq!(work.stage, WorkStage::ReasonInFlight);
    assert_eq!(snapshot.state.budgets[&work.budget_id].reason_requests, 1);
    let WorkAction::BeginReason {
        request_id,
        request,
        ..
    } = &original.action
    else {
        unreachable!()
    };
    assert_eq!(work.request_id.as_ref(), Some(request_id));
    assert_eq!(work.reason_request.as_ref(), Some(request));
    assert_eq!(
        fixture
            .bound
            .resources()
            .resolve_work_mutation(
                &peri_acp_types::session_resources::work::PreparedWorkCommand::try_new(
                    original.clone()
                )
                .unwrap()
            )
            .await
            .unwrap(),
        WorkResolution::Applied {
            receipt: rejected.clone()
        }
    );
    assert_eq!(rejected_receipt(&barrier, &original).await, rejected);
    let repeated = barrier
        .snapshot(&WorkQuery {
            session_id: original.session_id.clone(),
            limit: 1,
        })
        .await
        .unwrap();
    assert_eq!(repeated.state, snapshot.state);
    assert!(barrier.pending_command().await.is_none());
}

#[tokio::test]
async fn execution_transition_never_refreshes_changed_target_work_revision() {
    let (fixture, barrier, original) = entered_reason_command().await;
    let mut competing = original.clone();
    competing.mutation_id = uuid::Uuid::now_v7().to_string();
    barrier
        .commit(
            &peri_acp_types::session_resources::work::PreparedWorkCommand::try_new(
                competing.clone(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let old_rejection = rejected_receipt(&barrier, &original).await;
    assert_eq!(
        old_rejection.decision,
        WorkDecision::Rejected {
            reason: WorkRejection::StaleRevision
        }
    );
    let before = barrier
        .snapshot(&WorkQuery {
            session_id: original.session_id.clone(),
            limit: 1,
        })
        .await
        .unwrap();
    let WorkCommitError::Rejected { receipt } = barrier
        .commit_execution_transition(
            &peri_acp_types::session_resources::work::PreparedWorkCommand::try_new(
                original.clone(),
            )
            .unwrap(),
        )
        .await
        .unwrap_err()
    else {
        panic!("a changed logical work must not be re-entered");
    };
    assert_eq!(
        receipt.decision,
        WorkDecision::Rejected {
            reason: WorkRejection::StaleWorkRevision
        }
    );
    assert_ne!(receipt.mutation_id, original.mutation_id);
    let saved = fixture
        .bound
        .resources()
        .load_work_command(&WorkCommandQuery {
            session_id: original.session_id.clone(),
            mutation_id: receipt.mutation_id.clone(),
        })
        .await
        .unwrap()
        .unwrap();
    let mut expected = original.clone();
    expected.mutation_id = receipt.mutation_id.clone();
    let WorkAction::BeginReason { guard, .. } = &mut expected.action else {
        unreachable!()
    };
    guard.expected_revision = before.state.revision;
    assert_eq!(saved.command, expected);
    let after = barrier
        .snapshot(&WorkQuery {
            session_id: original.session_id.clone(),
            limit: 1,
        })
        .await
        .unwrap();
    assert_eq!(after.state, before.state);
    assert_eq!(after.control, before.control);
    assert_eq!(
        fixture
            .bound
            .resources()
            .resolve_work_mutation(
                &peri_acp_types::session_resources::work::PreparedWorkCommand::try_new(
                    original.clone()
                )
                .unwrap()
            )
            .await
            .unwrap(),
        WorkResolution::Applied {
            receipt: old_rejection
        }
    );
    assert!(barrier.pending_command().await.is_none());
}

#[tokio::test]
async fn execution_transition_keeps_original_rejection_when_control_generation_or_attempt_changes()
{
    for change_generation in [true, false] {
        let (fixture, barrier, original) = entered_reason_command().await;
        barrier
            .commit(
                &peri_acp_types::session_resources::work::PreparedWorkCommand::try_new(
                    publication(&original.session_id, original.recipient_lifecycle),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let old_rejection = rejected_receipt(&barrier, &original).await;
        assert_eq!(
            old_rejection.decision,
            WorkDecision::Rejected {
                reason: WorkRejection::StaleRevision
            }
        );
        let resources = fixture.bound.resources();
        let mut control = resources
            .load_session_control(&original.session_id)
            .await
            .unwrap();
        let actions = if change_generation {
            vec![ControlAction::Pause]
        } else {
            vec![
                ControlAction::ObserveAttempt { target: None },
                ControlAction::ObserveAttempt {
                    target: Some(ControlAttempt {
                        turn_id: peri_acp_types::session::TurnId::new(),
                        attempt_id: peri_acp_types::identity::AttemptId::new(),
                    }),
                },
            ]
        };
        for action in actions {
            let receipt = resources
                .apply_session_control(&ControlCommand {
                    session_id: original.session_id.clone(),
                    command_id: uuid::Uuid::now_v7().to_string(),
                    expected_lifecycle: control.lifecycle,
                    expected_revision: control.revision,
                    expected_control_generation: control.control_generation,
                    action,
                })
                .await
                .unwrap();
            assert_eq!(receipt.decision, ControlDecision::Accepted);
            control = receipt.state;
        }
        if change_generation {
            assert_ne!(
                control.control_generation,
                fixture.admission.control_generation
            );
            assert_eq!(control.attempt.as_ref(), Some(&fixture.admission.execution));
        } else {
            assert_eq!(
                control.control_generation,
                fixture.admission.control_generation
            );
            assert_ne!(control.attempt.as_ref(), Some(&fixture.admission.execution));
        }
        let before = barrier
            .snapshot(&WorkQuery {
                session_id: original.session_id.clone(),
                limit: 1,
            })
            .await
            .unwrap();
        let WorkCommitError::Rejected { receipt } = barrier
            .commit_execution_transition(
                &peri_acp_types::session_resources::work::PreparedWorkCommand::try_new(
                    original.clone(),
                )
                .unwrap(),
            )
            .await
            .unwrap_err()
        else {
            panic!("execution identity changes must not be refreshed");
        };
        assert_eq!(*receipt, old_rejection);
        let saved = resources
            .load_work_command(&WorkCommandQuery {
                session_id: original.session_id.clone(),
                mutation_id: original.mutation_id.clone(),
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.command, original);
        assert_eq!(
            saved.resolution,
            Some(WorkResolution::Applied {
                receipt: old_rejection
            })
        );
        let after = barrier
            .snapshot(&WorkQuery {
                session_id: original.session_id.clone(),
                limit: 1,
            })
            .await
            .unwrap();
        assert_eq!(after.state, before.state);
        assert_eq!(after.control, before.control);
        assert!(barrier.pending_command().await.is_none());
    }
}
