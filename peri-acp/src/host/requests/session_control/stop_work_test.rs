use super::*;
use peri_acp_types::session_resources::work::PreparedWorkCommand;
use peri_acp_types::{
    identity::AttemptId,
    messages::{BaseMessage, ToolCallRequest},
    session::{MessagePolicy, TurnId},
    session_resources::{work::*, ControlAttempt, FrozenSnapshotBytes, NewSession, NewSessionMeta},
    store::PersistedPayload,
    workspace::SessionBinding,
};
use peri_resources::sessions::SessionResourcesImpl;
use std::sync::Arc;

struct Fixture {
    directory: tempfile::TempDir,
    resources: Arc<dyn SessionResources>,
    admission: WorkAdmission,
}

async fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let resources: Arc<dyn SessionResources> = Arc::new(
        SessionResourcesImpl::open(directory.path().join("stop.db"))
            .await
            .unwrap(),
    );
    let workspace = resources.resolve_workspace(directory.path()).await.unwrap();
    resources
        .create_session(&NewSession {
            thread_id: "stop-session".into(),
            created_at: peri_time::now_utc_rfc3339(),
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
    apply(
        resources.as_ref(),
        WorkAction::PublishDelivery {
            delivery: PublishDelivery {
                delivery_id: "owned-input".into(),
                event: WorkEvent {
                    producer_namespace: "user-input".into(),
                    event_id: "input:publication".into(),
                    event_kind: "userInput".into(),
                    causation_id: None,
                    content: WorkPayload::from_payload(&PersistedPayload::Message(
                        BaseMessage::human("process"),
                    ))
                    .unwrap(),
                },
                purpose: DeliveryPurpose::UserInput,
                policy: MessagePolicy::ensure_processing(),
            },
        },
    )
    .await;
    let snapshot = load(resources.as_ref()).await;
    let candidate = &snapshot.candidates[0];
    let admission = WorkAdmission {
        session_id: "stop-session".into(),
        admission_id: "sdk-ticket".into(),
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
    apply(
        resources.as_ref(),
        WorkAction::RegisterAdmission {
            admission: admission.clone(),
        },
    )
    .await;
    let snapshot = load(resources.as_ref()).await;
    apply(
        resources.as_ref(),
        WorkAction::ClaimBatch {
            guard: guard(&snapshot),
            batch_id: admission.work_id.clone(),
            delivery_ids: snapshot.candidates[0].delivery_ids.clone(),
        },
    )
    .await;
    Fixture {
        directory,
        resources,
        admission,
    }
}

async fn load(resources: &dyn SessionResources) -> WorkSnapshot {
    resources
        .load_session_work(&WorkQuery {
            session_id: "stop-session".into(),
            limit: 1,
        })
        .await
        .unwrap()
}

async fn apply(resources: &dyn SessionResources, action: WorkAction) -> WorkReceipt {
    let receipt = resources
        .apply_work_mutation(
            &PreparedWorkCommand::try_new(WorkCommand {
                session_id: "stop-session".into(),
                recipient_lifecycle: 1,
                mutation_id: uuid::Uuid::now_v7().to_string(),
                action,
            })
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    receipt
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

async fn stop(fixture: &Fixture) -> (ControlCommand, ControlReceipt) {
    let state = fixture
        .resources
        .load_session_control(&"stop-session".into())
        .await
        .unwrap();
    let command = ControlCommand {
        session_id: "stop-session".into(),
        command_id: "stop-original".into(),
        expected_lifecycle: state.lifecycle,
        expected_revision: state.revision,
        expected_control_generation: state.control_generation,
        action: ControlAction::Stop {
            target: fixture.admission.execution.clone(),
        },
    };
    let receipt = fixture
        .resources
        .apply_session_control(&command)
        .await
        .unwrap();
    assert_eq!(receipt.decision, ControlDecision::Accepted);
    (command, receipt)
}

#[tokio::test]
async fn stop_abandons_exact_owned_processing_work_and_replay_is_noop() {
    let fixture = fixture().await;
    let (command, receipt) = stop(&fixture).await;
    let applied = abandon_owned_work(fixture.resources.as_ref(), &command, &receipt)
        .await
        .unwrap();
    assert_eq!(applied.len(), 1);
    let snapshot = load(fixture.resources.as_ref()).await;
    assert_eq!(
        snapshot.state.works[&fixture.admission.work_id].stage,
        WorkStage::Abandoned
    );
    assert_eq!(
        snapshot.state.obligations["owned-input"].status,
        ObligationStatus::Abandoned
    );
    assert!(
        abandon_owned_work(fixture.resources.as_ref(), &command, &receipt)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        load(fixture.resources.as_ref()).await.state.revision,
        snapshot.state.revision
    );
}

#[tokio::test]
async fn old_stop_after_resume_does_not_abandon_current_work() {
    let fixture = fixture().await;
    let (command, receipt) = stop(&fixture).await;
    fixture
        .resources
        .apply_session_control(&ControlCommand {
            session_id: command.session_id.clone(),
            command_id: "resume-new-generation".into(),
            expected_lifecycle: receipt.state.lifecycle,
            expected_revision: receipt.state.revision,
            expected_control_generation: receipt.state.control_generation,
            action: ControlAction::Resume,
        })
        .await
        .unwrap();
    assert!(
        abandon_owned_work(fixture.resources.as_ref(), &command, &receipt)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        load(fixture.resources.as_ref()).await.state.works[&fixture.admission.work_id].stage,
        WorkStage::ReasonReady
    );
}

#[tokio::test]
async fn stop_does_not_abandon_batch_for_a_different_execution() {
    let fixture = fixture().await;
    let (mut command, receipt) = stop(&fixture).await;
    command.action = ControlAction::Stop {
        target: ControlAttempt {
            turn_id: TurnId::new(),
            attempt_id: AttemptId::new(),
        },
    };
    assert!(
        abandon_owned_work(fixture.resources.as_ref(), &command, &receipt)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        load(fixture.resources.as_ref()).await.state.works[&fixture.admission.work_id].stage,
        WorkStage::ReasonReady
    );
}

#[tokio::test]
async fn unknown_external_invocation_facts_survive_processing_abandonment() {
    let fixture = fixture().await;
    let snapshot = load(fixture.resources.as_ref()).await;
    let request = "{}";
    apply(
        fixture.resources.as_ref(),
        WorkAction::BeginReason {
            guard: guard(&snapshot),
            target: target(&snapshot, &fixture.admission.work_id),
            request_id: "reason-request".into(),
            request: ReasonRequest {
                serialized_request: request.into(),
                request_digest: format!("{:x}", Sha256::digest(request.as_bytes())),
                model_ref: "fixture-model".into(),
                authorization_ref: "fixture-auth".into(),
            },
        },
    )
    .await;
    let arguments = "{}";
    let intent = InvocationIntent {
        invocation_id: "unknown-invocation".into(),
        tool_call_id: "call".into(),
        tool_name: "shell".into(),
        arguments_json: arguments.into(),
        arguments_digest: format!("{:x}", Sha256::digest(arguments.as_bytes())),
        effective_tool_name: "shell".into(),
        effective_arguments_json: arguments.into(),
        effective_arguments_digest: format!("{:x}", Sha256::digest(arguments.as_bytes())),
        owner_identity: "external-owner".into(),
        scope_id: "stop-session".into(),
        scope_epoch: Some(1),
        authorization_ref: "fixture-auth".into(),
        recovery_locator: "original-owner-call".into(),
    };
    let snapshot = load(fixture.resources.as_ref()).await;
    apply(
        fixture.resources.as_ref(),
        WorkAction::CommitReasonResponseAndDispatchIntent {
            guard: guard(&snapshot),
            target: target(&snapshot, &fixture.admission.work_id),
            request_id: "reason-request".into(),
            response: WorkPayload::from_payload(&PersistedPayload::Message(
                BaseMessage::ai_with_tool_calls(
                    "dispatch",
                    vec![ToolCallRequest::new("call", "shell", serde_json::json!({}))],
                ),
            ))
            .unwrap(),
            dispatch_intents: vec![intent],
            next_work_id: Some("act-owned-work".into()),
        },
    )
    .await;
    let snapshot = load(fixture.resources.as_ref()).await;
    apply(
        fixture.resources.as_ref(),
        WorkAction::BeginDispatch {
            guard: guard(&snapshot),
            target: target(&snapshot, "act-owned-work"),
            invocation_id: "unknown-invocation".into(),
        },
    )
    .await;
    let snapshot = load(fixture.resources.as_ref()).await;
    apply(
        fixture.resources.as_ref(),
        WorkAction::OutcomeUnknown {
            expected_revision: snapshot.state.revision,
            target: target(&snapshot, "act-owned-work"),
            invocation_id: "unknown-invocation".into(),
            reason: "owner outcome could not be established".into(),
        },
    )
    .await;
    let original_invocation =
        load(fixture.resources.as_ref()).await.state.invocations["unknown-invocation"].clone();
    let (command, receipt) = stop(&fixture).await;
    abandon_owned_work(fixture.resources.as_ref(), &command, &receipt)
        .await
        .unwrap();
    let snapshot = load(fixture.resources.as_ref()).await;
    assert_eq!(
        snapshot.state.works["act-owned-work"].stage,
        WorkStage::Abandoned
    );
    assert_eq!(
        snapshot.state.invocations["unknown-invocation"],
        original_invocation
    );
    assert_eq!(
        snapshot.state.invocations["unknown-invocation"].status,
        InvocationStatus::OutcomeUnknown
    );
}

#[tokio::test]
async fn lost_abandon_ack_resolves_original_command_without_rebuilding_revision() {
    let fixture = fixture().await;
    let (command, receipt) = stop(&fixture).await;
    let applied = abandon_owned_work(fixture.resources.as_ref(), &command, &receipt)
        .await
        .unwrap();
    let revision = load(fixture.resources.as_ref()).await.state.revision;
    let connection = sqlx::SqlitePool::connect(&format!(
        "sqlite://{}",
        fixture.directory.path().join("stop.db").display()
    ))
    .await
    .unwrap();
    sqlx::query("UPDATE session_work_commands SET reconciled=0 WHERE mutation_id=?1")
        .bind(&applied[0].mutation_id)
        .execute(&connection)
        .await
        .unwrap();
    let fresh = SessionResourcesImpl::open(fixture.directory.path().join("stop.db"))
        .await
        .unwrap();
    let recovered = abandon_owned_work(&fresh, &command, &receipt)
        .await
        .unwrap();
    assert_eq!(recovered, applied);
    assert_eq!(load(&fresh).await.state.revision, revision);
    assert!(load(&fresh).await.pending_commands.is_empty());
}

async fn journal_before_effect(fixture: &Fixture, mutation: &WorkCommand) {
    let connection = sqlx::SqlitePool::connect(&format!(
        "sqlite://{}",
        fixture.directory.path().join("stop.db").display()
    ))
    .await
    .unwrap();
    sqlx::query("INSERT INTO session_work_commands(mutation_id,session_id,digest,command_json,reconciled) VALUES (?1,?2,?3,?4,0)")
        .bind(&mutation.mutation_id).bind(&mutation.session_id)
        .bind(mutation.digest().unwrap()).bind(serde_json::to_string(mutation).unwrap())
        .execute(&connection).await.unwrap();
}

fn abandon_command(
    command: &ControlCommand,
    receipt: &ControlReceipt,
    snapshot: &WorkSnapshot,
    work_id: &str,
) -> WorkCommand {
    WorkCommand {
        session_id: command.session_id.clone(),
        recipient_lifecycle: receipt.state.lifecycle,
        mutation_id: mutation_id(command, work_id),
        action: WorkAction::AbandonWork {
            expected_revision: snapshot.state.revision,
            expected_control_generation: receipt.state.control_generation,
            target: target(snapshot, work_id),
            reason: "original frozen Stop".into(),
            authorization_ref: command.command_id.clone(),
        },
    }
}

#[tokio::test]
async fn stop_before_effect_recovery_seals_not_applied_without_rebuilding() {
    let fixture = fixture().await;
    let (command, receipt) = stop(&fixture).await;
    let snapshot = load(fixture.resources.as_ref()).await;
    let original = abandon_command(&command, &receipt, &snapshot, &fixture.admission.work_id);
    journal_before_effect(&fixture, &original).await;
    let fresh = SessionResourcesImpl::open(fixture.directory.path().join("stop.db"))
        .await
        .unwrap();
    for _attempt in 0..2 {
        let error = abandon_owned_work(&fresh, &command, &receipt)
            .await
            .unwrap_err();
        assert!(error.message.contains("not applied"));
        assert_eq!(load(&fresh).await.state, snapshot.state);
    }
    let owned = fresh
        .load_work_command(&WorkCommandQuery {
            session_id: original.session_id.clone(),
            mutation_id: original.mutation_id.clone(),
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(owned.command, original);
    assert_eq!(owned.resolution, Some(WorkResolution::NotApplied));
    assert!(!owned.pending);
}

#[tokio::test]
async fn stop_replays_known_rejected_original_receipt_without_new_revision() {
    let fixture = fixture().await;
    let (command, receipt) = stop(&fixture).await;
    let snapshot = load(fixture.resources.as_ref()).await;
    let mut original = abandon_command(&command, &receipt, &snapshot, &fixture.admission.work_id);
    if let WorkAction::AbandonWork {
        expected_revision, ..
    } = &mut original.action
    {
        *expected_revision = expected_revision.saturating_sub(1);
    }
    let original = PreparedWorkCommand::try_new(original).unwrap();
    let rejected = fixture
        .resources
        .apply_work_mutation(&original)
        .await
        .unwrap();
    assert!(matches!(rejected.decision, WorkDecision::Rejected { .. }));
    let error = abandon_owned_work(fixture.resources.as_ref(), &command, &receipt)
        .await
        .unwrap_err();
    assert!(error.message.contains("rejected"));
    let owned = fixture
        .resources
        .load_work_command(&WorkCommandQuery {
            session_id: original.session_id.clone(),
            mutation_id: original.mutation_id.clone(),
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&owned.command, original.command());
    assert_eq!(
        owned.resolution,
        Some(WorkResolution::Applied { receipt: rejected })
    );
}

#[tokio::test]
async fn query_recovery_only_resolves_original_without_applying_stop() {
    let fixture = fixture().await;
    let (command, receipt) = stop(&fixture).await;
    let snapshot = load(fixture.resources.as_ref()).await;
    let original = abandon_command(&command, &receipt, &snapshot, &fixture.admission.work_id);
    journal_before_effect(&fixture, &original).await;
    let fresh = SessionResourcesImpl::open(fixture.directory.path().join("stop.db"))
        .await
        .unwrap();
    let recovered = crate::host::work_recovery::resolve_pending(
        &fresh,
        &WorkQuery {
            session_id: original.session_id.clone(),
            limit: 1,
        },
    )
    .await
    .unwrap();
    assert!(recovered.pending_commands.is_empty());
    assert_eq!(recovered.state, snapshot.state);
    let owned = fresh
        .load_work_command(&WorkCommandQuery {
            session_id: original.session_id.clone(),
            mutation_id: original.mutation_id.clone(),
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(owned.command, original);
    assert_eq!(owned.resolution, Some(WorkResolution::NotApplied));
}

#[tokio::test]
async fn root_query_recovers_descendant_original_without_retargeting() {
    let fixture = fixture().await;
    let workspace = fixture
        .resources
        .resolve_workspace(fixture.directory.path())
        .await
        .unwrap();
    fixture
        .resources
        .create_session(&NewSession {
            thread_id: "stop-child".into(),
            created_at: peri_time::now_utc_rfc3339(),
            meta: NewSessionMeta {
                title: None,
                cwd: workspace.cwd.to_string_lossy().into_owned(),
                parent_thread_id: Some("stop-session".into()),
                hidden: false,
                cancel_policy: Default::default(),
                snapshot_at_message_id: None,
            },
            binding: SessionBinding::from_workspace(&workspace),
            frozen: FrozenSnapshotBytes::new(r#"{"v":1}"#),
        })
        .await
        .unwrap();
    let original = WorkCommand {
        session_id: "stop-child".into(),
        recipient_lifecycle: 1,
        mutation_id: "original-child-publication".into(),
        action: WorkAction::PublishDelivery {
            delivery: PublishDelivery {
                delivery_id: "child-input".into(),
                event: WorkEvent {
                    producer_namespace: "user-input".into(),
                    event_id: "child:publication".into(),
                    event_kind: "userInput".into(),
                    causation_id: None,
                    content: WorkPayload::from_payload(&PersistedPayload::Message(
                        BaseMessage::human("child"),
                    ))
                    .unwrap(),
                },
                purpose: DeliveryPurpose::UserInput,
                policy: MessagePolicy::ensure_processing(),
            },
        },
    };
    journal_before_effect(&fixture, &original).await;
    let fresh = SessionResourcesImpl::open(fixture.directory.path().join("stop.db"))
        .await
        .unwrap();
    let before = load(&fresh).await;
    assert!(before.blocked);
    assert_eq!(before.pending_commands, vec![original.clone()]);
    let recovered = crate::host::work_recovery::resolve_pending(
        &fresh,
        &WorkQuery {
            session_id: "stop-session".into(),
            limit: 1,
        },
    )
    .await
    .unwrap();
    assert!(recovered.pending_commands.is_empty());
    assert_eq!(recovered.state, before.state);
    let owned = fresh
        .load_work_command(&WorkCommandQuery {
            session_id: "stop-child".into(),
            mutation_id: original.mutation_id.clone(),
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(owned.command, original);
    assert_eq!(owned.resolution, Some(WorkResolution::NotApplied));
    assert!(fresh
        .load_session_work(&WorkQuery {
            session_id: "stop-child".into(),
            limit: 1
        })
        .await
        .unwrap()
        .state
        .deliveries
        .is_empty());
}
