use std::sync::Arc;

use peri_acp_types::{
    identity::AttemptId,
    messages::{BaseMessage, ToolCallRequest},
    session::{MessagePolicy, TurnId},
    session_resources::{
        work::*, ControlAction, ControlAttempt, ControlCommand, FrozenSnapshotBytes, NewSession,
        NewSessionMeta, SessionResources,
    },
    store::PersistedPayload,
    workspace::SessionBinding,
};
use peri_resources::sessions::SessionResourcesImpl;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

#[path = "durable_work/lifecycle_contract.rs"]
mod lifecycle_contract;

#[path = "durable_work/journal_contract.rs"]
mod journal_contract;

#[path = "durable_work/terminal_contract.rs"]
mod terminal_contract;

#[path = "durable_work/reopen_contract.rs"]
mod reopen_contract;

#[path = "durable_work/delivery_query_contract.rs"]
mod delivery_query_contract;

async fn fixture() -> (TempDir, Arc<dyn SessionResources>) {
    let directory = tempfile::tempdir().unwrap();
    let resources = Arc::new(
        SessionResourcesImpl::open(directory.path().join("work.db"))
            .await
            .unwrap(),
    );
    let workspace = resources.resolve_workspace(directory.path()).await.unwrap();
    resources
        .create_session(&NewSession {
            thread_id: "work-session".into(),
            created_at: "2026-10-05T00:00:00Z".into(),
            meta: NewSessionMeta {
                title: None,
                cwd: workspace.cwd.to_string_lossy().into_owned(),
                parent_thread_id: None,
                hidden: false,
                cancel_policy: Default::default(),
                snapshot_at_message_id: None,
            },
            binding: SessionBinding::from_workspace(&workspace),
            frozen: FrozenSnapshotBytes::new(r#"{"v":1}"#),
        })
        .await
        .unwrap();
    (directory, resources)
}

fn command(mutation_id: &str, action: WorkAction) -> WorkCommand {
    WorkCommand {
        session_id: "work-session".into(),
        recipient_lifecycle: 1,
        mutation_id: mutation_id.into(),
        action,
    }
}
async fn snapshot(resources: &dyn SessionResources) -> WorkSnapshot {
    resources
        .load_session_work(&WorkQuery {
            session_id: "work-session".into(),
            limit: 64,
        })
        .await
        .unwrap()
}
fn publication(delivery_id: &str, policy: MessagePolicy) -> PublishDelivery {
    PublishDelivery {
        delivery_id: delivery_id.into(),
        event: WorkEvent {
            producer_namespace: "trusted-test".into(),
            event_id: format!("event-{delivery_id}"),
            event_kind: "request".into(),
            causation_id: None,
            content: WorkPayload::from_payload(&PersistedPayload::Message(BaseMessage::human(
                "process this",
            )))
            .unwrap(),
        },
        purpose: DeliveryPurpose::UserInput,
        policy,
    }
}
async fn publish(resources: &dyn SessionResources, delivery_id: &str) -> WorkReceipt {
    resources
        .apply_work_mutation(&command(
            &format!("publish-{delivery_id}"),
            WorkAction::PublishDelivery {
                delivery: publication(delivery_id, MessagePolicy::ensure_processing()),
            },
        ))
        .await
        .unwrap()
}
fn admission(snapshot: &WorkSnapshot, id: &str) -> WorkAdmission {
    let candidate = snapshot.candidates.first().unwrap();
    WorkAdmission {
        session_id: snapshot.session_id.clone(),
        admission_id: id.into(),
        instance_id: "sdk-instance".into(),
        generation_id: "sdk-generation".into(),
        lifecycle: snapshot.control.lifecycle,
        control_generation: snapshot.control.control_generation,
        work_id: candidate.work_id.clone(),
        work_revision: candidate.work_revision,
        execution: ControlAttempt {
            turn_id: TurnId::new(),
            attempt_id: AttemptId::new(),
        },
    }
}
fn guard(snapshot: &WorkSnapshot) -> WorkGuard {
    WorkGuard {
        expected_revision: snapshot.state.revision,
        expected_control_generation: snapshot.control.control_generation,
        execution: snapshot.control.attempt.clone().unwrap(),
    }
}
fn target(snapshot: &WorkSnapshot, work_id: &str) -> WorkTarget {
    WorkTarget {
        work_id: work_id.into(),
        expected_work_revision: snapshot.state.works[work_id].revision,
    }
}
fn request() -> ReasonRequest {
    let serialized_request =
        r#"{"messages":["process this"],"tools":[],"config":{"model":"test"}}"#.to_owned();
    ReasonRequest {
        request_digest: format!("{:x}", Sha256::digest(serialized_request.as_bytes())),
        serialized_request,
        model_ref: "test-model".into(),
        authorization_ref: "test-authorization".into(),
    }
}
async fn claim(resources: &dyn SessionResources) -> WorkAdmission {
    let loaded = snapshot(resources).await;
    let admission = admission(&loaded, "admission");
    let receipt = resources
        .apply_work_mutation(&command(
            "enter",
            WorkAction::RegisterAdmission {
                admission: admission.clone(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    let loaded = snapshot(resources).await;
    let receipt = resources
        .apply_work_mutation(&command(
            "claim",
            WorkAction::ClaimBatch {
                guard: guard(&loaded),
                batch_id: admission.work_id.clone(),
                delivery_ids: loaded.candidates[0].delivery_ids.clone(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    admission
}
async fn begin_reason(resources: &dyn SessionResources, work_id: &str) {
    let loaded = snapshot(resources).await;
    let receipt = resources
        .apply_work_mutation(&command(
            "reason-begin",
            WorkAction::BeginReason {
                guard: guard(&loaded),
                target: target(&loaded, work_id),
                request_id: "request".into(),
                request: request(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
}

#[tokio::test]
async fn durable_publish_projection_and_response_are_distinct_commitments() {
    let (directory, resources) = fixture().await;
    let publish_receipt = publish(resources.as_ref(), "delivery").await;
    let loaded = snapshot(resources.as_ref()).await;
    assert_eq!(
        loaded.state.obligations["delivery"].status,
        ObligationStatus::Pending
    );
    assert!(resources
        .load_session_history(&"work-session".into())
        .await
        .unwrap()
        .is_empty());
    let ticket = claim(resources.as_ref()).await;
    let loaded = snapshot(resources.as_ref()).await;
    assert_eq!(
        loaded.state.obligations["delivery"].status,
        ObligationStatus::InProgress
    );
    assert_eq!(
        resources
            .load_session_history(&"work-session".into())
            .await
            .unwrap()
            .len(),
        1
    );
    begin_reason(resources.as_ref(), &ticket.work_id).await;
    let loaded = snapshot(resources.as_ref()).await;
    let response = command(
        "reason-response",
        WorkAction::CommitReasonResponseAndDispatchIntent {
            guard: guard(&loaded),
            target: target(&loaded, &ticket.work_id),
            request_id: "request".into(),
            response: WorkPayload::from_payload(&PersistedPayload::Message(BaseMessage::ai(
                "done",
            )))
            .unwrap(),
            dispatch_intents: Vec::new(),
            next_work_id: None,
        },
    );
    let original = resources.apply_work_mutation(&response).await.unwrap();
    assert_eq!(original.decision, WorkDecision::Accepted);
    assert_eq!(
        snapshot(resources.as_ref()).await.state.obligations["delivery"].status,
        ObligationStatus::Satisfied
    );
    let reopened = SessionResourcesImpl::open(directory.path().join("work.db"))
        .await
        .unwrap();
    assert_eq!(
        reopened.apply_work_mutation(&response).await.unwrap(),
        original
    );
    assert_eq!(
        reopened.resolve_work_mutation(&response).await.unwrap(),
        WorkResolution::Applied { receipt: original }
    );
    assert_eq!(publish_receipt.admission_sequence, Some(1));
}

#[tokio::test]
async fn durable_pause_accepts_frozen_lifecycle_delivery_and_keeps_it_blocked() {
    let (_directory, resources) = fixture().await;
    resources
        .apply_session_control(&ControlCommand {
            session_id: "work-session".into(),
            command_id: "pause".into(),
            expected_lifecycle: 1,
            expected_revision: 0,
            expected_control_generation: 0,
            action: ControlAction::Pause,
        })
        .await
        .unwrap();
    let receipt = publish(resources.as_ref(), "paused-delivery").await;
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    let loaded = snapshot(resources.as_ref()).await;
    assert_eq!(
        loaded.state.obligations["paused-delivery"].status,
        ObligationStatus::Blocked
    );
    assert!(loaded.candidates.is_empty());
}

#[tokio::test]
async fn durable_admission_preserves_original_ticket_receipt_and_requires_finish_proof() {
    let (_directory, resources) = fixture().await;
    publish(resources.as_ref(), "delivery").await;
    let ticket = admission(&snapshot(resources.as_ref()).await, "ticket");
    let enter = command(
        "ticket-enter",
        WorkAction::RegisterAdmission {
            admission: ticket.clone(),
        },
    );
    let original = resources.apply_work_mutation(&enter).await.unwrap();
    assert_eq!(original.decision, WorkDecision::Accepted);
    let finish = command(
        "ticket-finish",
        WorkAction::FinishAdmission {
            admission: ticket.clone(),
            evidence_id: "actual-future-exited".into(),
        },
    );
    assert!(matches!(
        resources
            .apply_work_mutation(&finish)
            .await
            .unwrap()
            .decision,
        WorkDecision::Rejected { .. }
    ));
    let control = resources
        .load_session_control(&"work-session".into())
        .await
        .unwrap();
    resources
        .apply_session_control(&ControlCommand {
            session_id: "work-session".into(),
            command_id: "actual-exit".into(),
            expected_lifecycle: control.lifecycle,
            expected_revision: control.revision,
            expected_control_generation: control.control_generation,
            action: ControlAction::ObserveAttempt { target: None },
        })
        .await
        .unwrap();
    let settled = command("ticket-settle", finish.action.clone());
    assert_eq!(
        resources
            .apply_work_mutation(&settled)
            .await
            .unwrap()
            .decision,
        WorkDecision::Accepted
    );
    assert_eq!(
        resources.apply_work_mutation(&enter).await.unwrap(),
        original
    );
    let mut conflict = ticket.clone();
    conflict.generation_id = "other-generation".into();
    assert!(matches!(
        resources
            .apply_work_mutation(&command(
                "conflicting-ticket",
                WorkAction::RegisterAdmission {
                    admission: conflict
                }
            ))
            .await
            .unwrap()
            .decision,
        WorkDecision::Rejected {
            reason: WorkRejection::Conflict
        }
    ));
    assert_eq!(
        snapshot(resources.as_ref()).await.state.admissions["ticket"]
            .evidence_id
            .as_deref(),
        Some("actual-future-exited")
    );
}

#[tokio::test]
async fn durable_resource_owners_are_immutable_and_survive_close_reopen() {
    let (_directory, resources) = fixture().await;
    let binding = command(
        "owner-binding",
        WorkAction::BindResourceOwners {
            expected_revision: 0,
            connections_json: r#"{"servers":[{"name":"owner","endpoint":"trusted-declared"}]}"#
                .into(),
            authorization_ref: "trusted-acp-config".into(),
        },
    );
    let original = resources.apply_work_mutation(&binding).await.unwrap();
    assert_eq!(original.decision, WorkDecision::Accepted);
    let conflict = command(
        "owner-conflict",
        WorkAction::BindResourceOwners {
            expected_revision: original.revision,
            connections_json: "{}".into(),
            authorization_ref: "trusted-acp-config".into(),
        },
    );
    assert_eq!(
        resources
            .apply_work_mutation(&conflict)
            .await
            .unwrap()
            .decision,
        WorkDecision::Rejected {
            reason: WorkRejection::Conflict
        }
    );
    for (command_id, action) in [
        ("close", ControlAction::Close),
        ("closed", ControlAction::FinishClose),
        ("reopen", ControlAction::Reopen),
    ] {
        let current = resources
            .load_session_control(&"work-session".into())
            .await
            .unwrap();
        resources
            .apply_session_control(&ControlCommand {
                session_id: "work-session".into(),
                command_id: command_id.into(),
                expected_lifecycle: current.lifecycle,
                expected_revision: current.revision,
                expected_control_generation: current.control_generation,
                action,
            })
            .await
            .unwrap();
    }
    let loaded = snapshot(resources.as_ref()).await;
    assert_eq!(loaded.control.lifecycle, 2);
    assert!(loaded.state.resource_owners.contains_key(&1));
    let second = WorkCommand {
        recipient_lifecycle: 2,
        mutation_id: "owners-life2".into(),
        action: WorkAction::BindResourceOwners {
            expected_revision: loaded.state.revision,
            connections_json: "[]".into(),
            authorization_ref: "trusted-acp-config-life2".into(),
        },
        ..binding.clone()
    };
    assert_eq!(
        resources
            .apply_work_mutation(&second)
            .await
            .unwrap()
            .decision,
        WorkDecision::Accepted
    );
    assert_eq!(
        snapshot(resources.as_ref())
            .await
            .state
            .resource_owners
            .len(),
        2
    );
    assert_eq!(
        resources.apply_work_mutation(&binding).await.unwrap(),
        original
    );
}

#[tokio::test]
async fn durable_resolve_seals_original_id_and_conflicting_parameters_are_rejected() {
    let (_directory, resources) = fixture().await;
    let publish = command(
        "original",
        WorkAction::PublishDelivery {
            delivery: publication("delivery", MessagePolicy::ensure_processing()),
        },
    );
    assert_eq!(
        resources.resolve_work_mutation(&publish).await.unwrap(),
        WorkResolution::NotApplied
    );
    assert!(resources.apply_work_mutation(&publish).await.is_err());
    let conflicting = command(
        "original",
        WorkAction::PublishDelivery {
            delivery: publication("different", MessagePolicy::ensure_processing()),
        },
    );
    assert!(resources.resolve_work_mutation(&conflicting).await.is_err());
    assert!(snapshot(resources.as_ref())
        .await
        .state
        .deliveries
        .is_empty());
}

#[tokio::test]
async fn durable_reason_barrier_rolls_back_projection_obligation_and_intents_together() {
    let (directory, resources) = fixture().await;
    publish(resources.as_ref(), "delivery").await;
    let ticket = claim(resources.as_ref()).await;
    begin_reason(resources.as_ref(), &ticket.work_id).await;
    let connection = sqlx::SqlitePool::connect(&format!(
        "sqlite:{}",
        directory.path().join("work.db").display()
    ))
    .await
    .unwrap();
    sqlx::query("CREATE TRIGGER reject_response BEFORE INSERT ON messages WHEN NEW.role='assistant' BEGIN SELECT RAISE(ABORT,'fault response'); END").execute(&connection).await.unwrap();
    let loaded = snapshot(resources.as_ref()).await;
    let response = command(
        "fault-response",
        WorkAction::CommitReasonResponseAndDispatchIntent {
            guard: guard(&loaded),
            target: target(&loaded, &ticket.work_id),
            request_id: "request".into(),
            response: WorkPayload::from_payload(&PersistedPayload::Message(BaseMessage::ai(
                "response",
            )))
            .unwrap(),
            dispatch_intents: Vec::new(),
            next_work_id: None,
        },
    );
    assert!(resources.apply_work_mutation(&response).await.is_err());
    let after = snapshot(resources.as_ref()).await;
    assert_eq!(after.state, loaded.state);
    assert_eq!(
        after.state.obligations["delivery"].status,
        ObligationStatus::InProgress
    );
    assert_eq!(
        resources.resolve_work_mutation(&response).await.unwrap(),
        WorkResolution::NotApplied
    );
    sqlx::query("DROP TRIGGER reject_response")
        .execute(&connection)
        .await
        .unwrap();
    assert!(resources.apply_work_mutation(&response).await.is_err());
}

#[tokio::test]
async fn durable_act_handoff_has_full_intent_and_never_blindly_redispatches_unknown() {
    let (_directory, resources) = fixture().await;
    publish(resources.as_ref(), "delivery").await;
    let ticket = claim(resources.as_ref()).await;
    begin_reason(resources.as_ref(), &ticket.work_id).await;
    let args = r#"{"command":"echo test"}"#.to_owned();
    let intent = InvocationIntent {
        invocation_id: "invocation".into(),
        tool_call_id: "call".into(),
        tool_name: "shell".into(),
        arguments_digest: format!("{:x}", Sha256::digest(args.as_bytes())),
        arguments_json: args,
        effective_tool_name: "shell".into(),
        effective_arguments_json: r#"{"command":"echo test"}"#.into(),
        effective_arguments_digest: format!("{:x}", Sha256::digest(br#"{"command":"echo test"}"#)),
        owner_identity: "trusted-owner".into(),
        scope_id: "work-session".into(),
        scope_epoch: Some(1),
        authorization_ref: "test-auth".into(),
        recovery_locator: "owner-original-call".into(),
    };
    let response =
        WorkPayload::from_payload(&PersistedPayload::Message(BaseMessage::ai_with_tool_calls(
            "run",
            vec![ToolCallRequest::new(
                "call",
                "shell",
                serde_json::json!({"command":"echo test"}),
            )],
        )))
        .unwrap();
    let loaded = snapshot(resources.as_ref()).await;
    let commit = command(
        "response-intent",
        WorkAction::CommitReasonResponseAndDispatchIntent {
            guard: guard(&loaded),
            target: target(&loaded, &ticket.work_id),
            request_id: "request".into(),
            response: response.clone(),
            dispatch_intents: vec![intent.clone()],
            next_work_id: Some("act-work".into()),
        },
    );
    assert_eq!(
        resources
            .apply_work_mutation(&commit)
            .await
            .unwrap()
            .decision,
        WorkDecision::Accepted
    );
    let loaded = snapshot(resources.as_ref()).await;
    assert_eq!(loaded.state.works["act-work"].response, Some(response));
    assert_eq!(loaded.state.works["act-work"].budget_id, ticket.work_id);
    assert_eq!(
        resources
            .apply_work_mutation(&command(
                "bridge-prepare",
                WorkAction::PrepareInvocation {
                    expected_revision: loaded.state.revision,
                    intent
                }
            ))
            .await
            .unwrap()
            .decision,
        WorkDecision::Accepted
    );
    let loaded = snapshot(resources.as_ref()).await;
    let dispatch = command(
        "dispatch",
        WorkAction::BeginDispatch {
            guard: guard(&loaded),
            target: target(&loaded, "act-work"),
            invocation_id: "invocation".into(),
        },
    );
    assert_eq!(
        resources
            .apply_work_mutation(&dispatch)
            .await
            .unwrap()
            .decision,
        WorkDecision::Accepted
    );
    let loaded = snapshot(resources.as_ref()).await;
    let unknown = command(
        "outcome-unknown",
        WorkAction::OutcomeUnknown {
            expected_revision: loaded.state.revision,
            target: target(&loaded, "act-work"),
            invocation_id: "invocation".into(),
            reason: "owner reply lost".into(),
        },
    );
    assert_eq!(
        resources
            .apply_work_mutation(&unknown)
            .await
            .unwrap()
            .decision,
        WorkDecision::Accepted
    );
    let loaded = snapshot(resources.as_ref()).await;
    assert_eq!(
        loaded.state.invocations["invocation"].status,
        InvocationStatus::OutcomeUnknown
    );
    assert_eq!(loaded.state.works["act-work"].stage, WorkStage::Blocked);
    let again = command(
        "blind-redispatch",
        WorkAction::BeginDispatch {
            guard: guard(&loaded),
            target: target(&loaded, "act-work"),
            invocation_id: "invocation".into(),
        },
    );
    assert!(matches!(
        resources
            .apply_work_mutation(&again)
            .await
            .unwrap()
            .decision,
        WorkDecision::Rejected { .. }
    ));
    let result = InvocationResult {
        invocation_id: "invocation".into(),
        outcome: InvocationOutcome::Completed {
            result: WorkPayload::from_payload(&PersistedPayload::Message(
                BaseMessage::tool_result("call", "test"),
            ))
            .unwrap(),
        },
    };
    // 停下的 work（Blocked/Abandoned）只允许结算既有结果，不能再交出后继 work（44309b13 收紧）。
    let commit_act = command(
        "act-result",
        WorkAction::CommitAct {
            guard: guard(&loaded),
            target: target(&loaded, "act-work"),
            results: vec![result],
            next_work_id: Some("next-reason".into()),
        },
    );
    assert_eq!(
        resources
            .apply_work_mutation(&commit_act)
            .await
            .unwrap()
            .decision,
        WorkDecision::Rejected {
            reason: WorkRejection::InvalidTransition
        }
    );
}

#[tokio::test]
async fn durable_act_ready_commit_hands_off_successor_reason_work() {
    let (_directory, resources) = fixture().await;
    publish(resources.as_ref(), "delivery").await;
    let ticket = claim(resources.as_ref()).await;
    begin_reason(resources.as_ref(), &ticket.work_id).await;
    let args = r#"{"command":"echo test"}"#.to_owned();
    let intent = InvocationIntent {
        invocation_id: "invocation".into(),
        tool_call_id: "call".into(),
        tool_name: "shell".into(),
        arguments_digest: format!("{:x}", Sha256::digest(args.as_bytes())),
        arguments_json: args,
        effective_tool_name: "shell".into(),
        effective_arguments_json: r#"{"command":"echo test"}"#.into(),
        effective_arguments_digest: format!("{:x}", Sha256::digest(br#"{"command":"echo test"}"#)),
        owner_identity: "trusted-owner".into(),
        scope_id: "work-session".into(),
        scope_epoch: Some(1),
        authorization_ref: "test-auth".into(),
        recovery_locator: "owner-original-call".into(),
    };
    let response =
        WorkPayload::from_payload(&PersistedPayload::Message(BaseMessage::ai_with_tool_calls(
            "run",
            vec![ToolCallRequest::new(
                "call",
                "shell",
                serde_json::json!({"command":"echo test"}),
            )],
        )))
        .unwrap();
    let loaded = snapshot(resources.as_ref()).await;
    assert_eq!(
        resources
            .apply_work_mutation(&command(
                "prepare-act",
                WorkAction::CommitReasonResponseAndDispatchIntent {
                    guard: guard(&loaded),
                    target: target(&loaded, &ticket.work_id),
                    request_id: "request".into(),
                    response,
                    dispatch_intents: vec![intent],
                    next_work_id: Some("act-work".into()),
                }
            ))
            .await
            .unwrap()
            .decision,
        WorkDecision::Accepted
    );
    let loaded = snapshot(resources.as_ref()).await;
    assert_eq!(loaded.state.works["act-work"].stage, WorkStage::ActReady);
    // ActReady 上结算全部 invocation 时允许交出后继 work：act-work 收口为 Settled，
    // 后继 reason work 继承同一 budget。
    let commit_act = command(
        "act-handoff",
        WorkAction::CommitAct {
            guard: guard(&loaded),
            target: target(&loaded, "act-work"),
            results: vec![InvocationResult {
                invocation_id: "invocation".into(),
                outcome: InvocationOutcome::Cancelled {
                    evidence: "rejected before effect".into(),
                },
            }],
            next_work_id: Some("next-reason".into()),
        },
    );
    assert_eq!(
        resources
            .apply_work_mutation(&commit_act)
            .await
            .unwrap()
            .decision,
        WorkDecision::Accepted
    );
    let loaded = snapshot(resources.as_ref()).await;
    assert_eq!(loaded.state.works["act-work"].stage, WorkStage::Settled);
    assert_eq!(
        loaded.state.works["next-reason"].stage,
        WorkStage::ReasonReady
    );
    assert_eq!(loaded.state.works["next-reason"].budget_id, ticket.work_id);
}

#[tokio::test]
async fn durable_v15_upgrade_quarantines_legacy_history_not_guessed_satisfied() {
    let (directory, resources) = fixture().await;
    resources
        .append_history(
            &"work-session".into(),
            &[PersistedPayload::Message(BaseMessage::human(
                "legacy history",
            ))],
        )
        .await
        .unwrap();
    let connection = sqlx::SqlitePool::connect(&format!(
        "sqlite:{}",
        directory.path().join("work.db").display()
    ))
    .await
    .unwrap();
    sqlx::raw_sql("DROP TABLE session_work_state; DROP TABLE session_work_events; DROP TABLE session_work_receipts; PRAGMA user_version=15").execute(&connection).await.unwrap();
    drop(resources);
    let reopened = SessionResourcesImpl::open(directory.path().join("work.db"))
        .await
        .unwrap();
    let loaded = snapshot(&reopened).await;
    assert!(loaded.blocked);
    assert!(!loaded.state.legacy_unknown.is_empty());
    assert!(loaded.state.obligations.is_empty());
    assert!(loaded.candidates.is_empty());
}
