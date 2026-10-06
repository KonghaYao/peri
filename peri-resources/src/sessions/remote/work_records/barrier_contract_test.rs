use super::*;
use crate::sessions::work;
use peri_acp_types::messages::ToolCallRequest;
use peri_acp_types::session_resources::ControlState;

fn execution() -> peri_acp_types::session_resources::ControlAttempt {
    serde_json::from_value(serde_json::json!({
        "turnId": "00000000-0000-4000-8000-000000000003",
        "attemptId": "barrier-attempt"
    }))
    .unwrap()
}

#[tokio::test]
async fn workrecords_remote_evidence_deduplicates_bytes_and_rejects_identity_replacement() {
    let fixture = Fixture::new().await;
    let adapter = fixture.adapter().await;
    let mut write = EvidenceWrite {
        session_id: "work-session".into(),
        storage_scope: "work-session".into(),
        payload_id: "original".into(),
        encoding: 1,
        bytes: b"{}".to_vec(),
    };
    let reference = adapter.prepare_evidence(&write).await.unwrap();
    write.payload_id = "same-bytes".into();
    assert_eq!(adapter.prepare_evidence(&write).await.unwrap(), reference);
    let reopened = fixture.adapter().await;
    assert_eq!(reopened.prepare_evidence(&write).await.unwrap(), reference);
    write.payload_id = "original".into();
    write.bytes = b"changed".to_vec();
    assert!(reopened.prepare_evidence(&write).await.is_err());
    let stored = reopened
        .read_evidence(&EvidenceQuery {
            session_id: "work-session".into(),
            reference,
        })
        .await
        .unwrap();
    assert_eq!(stored.bytes, b"{}");
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM session_payloads WHERE kind='evidence'")
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn workrecords_remote_evidence_lost_ack_recovers_canonical_reference() {
    let fixture = Fixture::new().await;
    let adapter = fixture.adapter().await;
    let write = EvidenceWrite {
        session_id: "work-session".into(),
        storage_scope: "work-session".into(),
        payload_id: "unknown-evidence".into(),
        encoding: 1,
        bytes: b"{}".to_vec(),
    };
    adapter.store().await.unwrap().inject_faults(FaultPlan {
        drop_reply: Some("prepare_evidence".into()),
        drop_before_send: None,
    });
    assert!(adapter
        .prepare_evidence(&write)
        .await
        .unwrap_err()
        .is_persistence_uncertain());
    let reopened = fixture.adapter().await;
    let reference = reopened.prepare_evidence(&write).await.unwrap();
    assert_eq!(reference, write.reference().unwrap());
    let read = reopened
        .read_evidence(&EvidenceQuery {
            session_id: "work-session".into(),
            reference,
        })
        .await
        .unwrap();
    assert_eq!(read.bytes, b"{}");
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM session_payloads WHERE kind='evidence'")
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(count, 1);
}

async fn response_payload(adapter: &RemoteSessionData) -> WorkPayload {
    let payload = PersistedPayload::Message(BaseMessage::ai_with_tool_calls(
        "dispatch this call",
        vec![ToolCallRequest::new("call", "tool", serde_json::json!({}))],
    ));
    let (content, plan) =
        crate::sessions::work_store::payload::prepare(&payload, "work-session").unwrap();
    adapter
        .commit_effects(
            "prepare_barrier_payload",
            &["barrier".into()],
            super::super::specifications(plan).unwrap(),
            &"work-session".into(),
        )
        .await
        .unwrap();
    WorkPayload {
        message_id: payload.id(),
        role: "assistant".into(),
        content,
        tool_call_id: None,
    }
}

#[tokio::test]
async fn workrecords_remote_terminal_binding_reads_its_admission() {
    let fixture = Fixture::new().await;
    let adapter = fixture.adapter().await;
    let seed = WorkCommand {
        session_id: "work-session".into(),
        recipient_lifecycle: 1,
        mutation_id: "seed-terminal-admission".into(),
        action: WorkAction::PublishDelivery {
            delivery: PublishDelivery {
                delivery_id: "seed-delivery".into(),
                event: WorkEvent {
                    producer_namespace: "barrier".into(),
                    event_id: "seed".into(),
                    event_kind: "seed".into(),
                    causation_id: None,
                    content: response_payload(&adapter).await,
                },
                purpose: DeliveryPurpose::UserInput,
                policy: MessagePolicy::ensure_processing(),
            },
        },
    };
    assert_eq!(
        adapter.apply_work_mutation(&seed).await.unwrap().decision,
        WorkDecision::Accepted
    );
    let WorkPage::Availability(availability) = adapter
        .inspect_work(&WorkQuery::new("work-session", WorkSelector::Availability))
        .await
        .unwrap()
        .page
    else {
        panic!("expected availability page");
    };
    let candidate = availability.candidates.first().unwrap();
    let admission = WorkAdmission {
        session_id: "work-session".into(),
        admission_id: "admission".into(),
        instance_id: "instance".into(),
        generation_id: "generation".into(),
        lifecycle: 1,
        control_generation: 0,
        work_id: candidate.work_id.clone(),
        work_revision: candidate.work_revision,
        execution: execution(),
    };
    let register = WorkCommand {
        session_id: "work-session".into(),
        recipient_lifecycle: 1,
        mutation_id: "register".into(),
        action: WorkAction::RegisterAdmission {
            admission: admission.clone(),
        },
    };
    assert_eq!(
        adapter
            .apply_work_mutation(&register)
            .await
            .unwrap()
            .decision,
        WorkDecision::Accepted
    );
    let terminal = WorkCommand {
        session_id: "work-session".into(),
        recipient_lifecycle: 1,
        mutation_id: "publish-terminal".into(),
        action: WorkAction::PublishDelivery {
            delivery: PublishDelivery {
                delivery_id: "terminal-delivery".into(),
                event: WorkEvent {
                    producer_namespace: "barrier".into(),
                    event_id: "terminal".into(),
                    event_kind: "terminal".into(),
                    causation_id: None,
                    content: response_payload(&adapter).await,
                },
                purpose: DeliveryPurpose::TaskTerminal,
                policy: MessagePolicy::ensure_processing(),
            },
        },
    };
    let snapshot = adapter
        .inspect_work(&WorkQuery::new("work-session", WorkSelector::Head))
        .await
        .unwrap();
    let bind = WorkCommand {
        session_id: "work-session".into(),
        recipient_lifecycle: 1,
        mutation_id: "bind-terminal".into(),
        action: WorkAction::BindTerminalObligation {
            expected_revision: snapshot.head.change_seq,
            admission_id: admission.admission_id.clone(),
            command: Box::new(terminal.clone()),
        },
    };
    let receipt = adapter.apply_work_mutation(&bind).await.unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    let reopened = fixture.adapter().await;
    assert_eq!(reopened.apply_work_mutation(&bind).await.unwrap(), receipt);
    let stored = reopened
        .inspect_work(&WorkQuery::new(
            "work-session",
            WorkSelector::TerminalCommand {
                admission_id: admission.admission_id,
            },
        ))
        .await
        .unwrap();
    let WorkPage::TerminalCommands(records) = stored.page else {
        panic!("expected terminal obligation page");
    };
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].command.as_ref(), &terminal);
    assert_eq!(stored.head.terminal_obligations, 1);
}

#[tokio::test]
async fn workrecords_remote_response_without_intent_is_rejected_atomically() {
    let fixture = Fixture::new().await;
    let adapter = fixture.adapter().await;
    let control = ControlState {
        attempt: Some(execution()),
        ..ControlState::default()
    };
    sqlx::query(
        "INSERT INTO session_control_state(session_id,state_json) VALUES ('work-session',?1)",
    )
    .bind(work::encode(&control).unwrap())
    .execute(&fixture.pool)
    .await
    .unwrap();
    let processing = Processing {
        processing_id: "processing".into(),
        recipient_lifecycle: 1,
        revision: 0,
        execution: execution(),
        stage: WorkStage::ReasonInFlight,
        phase_sequence: 0,
        delivery_count: 0,
        reason_delivery_count: 0,
        budget: WorkBudget::default(),
        checkpoint: None,
        request_id: Some("request".into()),
        request: None,
        response: None,
        remaining_effects: 0,
        resume_stage: None,
        blocked_evidence: None,
        recovery_condition: None,
        delegation: None,
    };
    sqlx::query("INSERT INTO session_processing(processing_id,session_id,lifecycle,revision,phase_sequence,phase,record_json) VALUES ('processing','work-session',1,0,0,'reasonInFlight',?1)")
        .bind(work::encode(&processing).unwrap()).execute(&fixture.pool).await.unwrap();
    let command = WorkCommand {
        session_id: "work-session".into(),
        recipient_lifecycle: 1,
        mutation_id: "commit-response".into(),
        action: WorkAction::CommitReasonResponseAndDispatchIntent {
            guard: WorkGuard {
                expected_revision: 0,
                expected_control_generation: 0,
                execution: execution(),
            },
            target: WorkTarget {
                work_id: "processing".into(),
                expected_work_revision: 0,
            },
            request_id: "request".into(),
            response: response_payload(&adapter).await,
            dispatch_intents: vec![],
            next_work_id: Some("processing".into()),
        },
    };
    let receipt = adapter.apply_work_mutation(&command).await.unwrap();
    assert_eq!(
        receipt.decision,
        WorkDecision::Rejected {
            reason: WorkRejection::Conflict
        }
    );
    let reopened = fixture.adapter().await;
    assert_eq!(
        reopened.apply_work_mutation(&command).await.unwrap(),
        receipt
    );
    let stored = reopened
        .inspect_work(&WorkQuery::new(
            "work-session",
            WorkSelector::Processing {
                processing_id: "processing".into(),
            },
        ))
        .await
        .unwrap();
    let WorkPage::Processings(records) = stored.page else {
        panic!("expected processing page");
    };
    assert_eq!(records, vec![processing]);
    assert_eq!(stored.head.change_seq, 0);
    for statement in [
        "SELECT COUNT(*) FROM messages",
        "SELECT COUNT(*) FROM session_effects",
    ] {
        let count: i64 = sqlx::query_scalar(statement)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
        assert_eq!(count, 0, "{statement}");
    }
    let arguments = reopened
        .prepare_evidence(&EvidenceWrite {
            session_id: "work-session".into(),
            storage_scope: "work-session".into(),
            payload_id: "call-arguments".into(),
            encoding: 1,
            bytes: serde_json::to_vec(&serde_json::json!({})).unwrap(),
        })
        .await
        .unwrap();
    let mut valid = command.clone();
    valid.mutation_id = "commit-paired-response".into();
    let WorkAction::CommitReasonResponseAndDispatchIntent {
        dispatch_intents, ..
    } = &mut valid.action
    else {
        panic!("expected response command");
    };
    dispatch_intents.push(InvocationIntent {
        invocation_id: "invocation".into(),
        tool_call_id: "call".into(),
        tool_name: "tool".into(),
        arguments: arguments.clone(),
        arguments_digest: arguments.sha256.clone(),
        effective_tool_name: "tool".into(),
        effective_arguments: arguments.clone(),
        effective_arguments_digest: arguments.sha256,
        owner_identity: "owner".into(),
        scope_id: "scope".into(),
        scope_epoch: None,
        authorization_ref: "authorization".into(),
        recovery_locator: "recovery".into(),
    });
    assert_eq!(
        reopened.apply_work_mutation(&valid).await.unwrap().decision,
        WorkDecision::Accepted
    );
    let (messages, effects): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM messages),(SELECT COUNT(*) FROM session_effects)",
    )
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!((messages, effects), (1, 1));
}
