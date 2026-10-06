use super::*;
use crate::identity::AttemptId;
use crate::messages::{BaseMessage, MessageContent};
use crate::session::TurnId;

fn attempt() -> ControlAttempt {
    ControlAttempt {
        turn_id: TurnId::new(),
        attempt_id: AttemptId::new(),
    }
}

fn delivery(delivery_id: &str) -> PublishDelivery {
    PublishDelivery {
        delivery_id: delivery_id.into(),
        event: WorkEvent {
            producer_namespace: "test".into(),
            event_id: delivery_id.into(),
            event_kind: "input".into(),
            causation_id: None,
            content: WorkPayload::from_payload(&PersistedPayload::Message(BaseMessage::Human {
                id: MessageId::new(),
                content: MessageContent::text("fresh input"),
            }))
            .unwrap(),
        },
        purpose: DeliveryPurpose::UserInput,
        policy: MessagePolicy::ensure_processing(),
    }
}

fn superseded_state() -> WorkState {
    let mut state = WorkState::default();
    let execution = attempt();
    state.batches.insert(
        "old-batch".into(),
        ProcessingBatch {
            batch_id: "old-batch".into(),
            delivery_ids: vec!["old-delivery".into()],
            processing_delivery_ids: vec!["old-delivery".into()],
            projection_versions: BTreeMap::new(),
            execution: execution.clone(),
            recipient_lifecycle: 1,
        },
    );
    state.works.insert(
        "successor".into(),
        WorkRecord {
            work_id: "successor".into(),
            revision: 1,
            budget_id: "budget".into(),
            batch_id: "old-batch".into(),
            stage: WorkStage::Abandoned,
            resume_stage: None,
            request_id: None,
            reason_request: None,
            response: None,
            invocation_ids: Vec::new(),
            reason: Some("explicit user input superseded processing".into()),
            recovery_condition: None,
        },
    );
    state.admissions.insert(
        "old-admission".into(),
        AdmissionRecord {
            admission: WorkAdmission {
                session_id: "session".into(),
                admission_id: "old-admission".into(),
                instance_id: "old-instance".into(),
                generation_id: "old-generation".into(),
                lifecycle: 1,
                control_generation: 0,
                work_id: "old-delivery".into(),
                work_revision: 0,
                execution,
            },
            entering_receipt: None,
            settled_receipt: None,
            evidence_id: None,
        },
    );
    let binding = TaskBinding {
        invocation_id: "invocation".into(),
        owner_identity: "owner".into(),
        owner_task_id: "task".into(),
        initiator_session_id: "parent".into(),
        recipient_lifecycle: 1,
        recovery_locator: "owner-locator".into(),
        authorization_ref: "authorization".into(),
    };
    state
        .task_bindings
        .insert("invocation".into(), binding.clone());
    let mut terminal = delivery("terminal");
    terminal.purpose = DeliveryPurpose::TaskTerminal;
    state.terminal_obligations.insert(
        "old-admission".into(),
        WorkCommand {
            session_id: "parent".into(),
            recipient_lifecycle: 1,
            mutation_id: "terminal-command".into(),
            action: WorkAction::PublishTaskSettlement {
                delivery: terminal,
                binding,
            },
        },
    );
    state
}

fn snapshot(state: WorkState) -> WorkSnapshot {
    WorkSnapshot::from_state(
        &WorkQuery {
            session_id: "session".into(),
            limit: 1,
        },
        ControlState::default(),
        state,
    )
}

fn recovered_terminal_state() -> WorkState {
    let mut state = superseded_state();
    let mut source = state.works["successor"].clone();
    source.work_id = "recovery-source".into();
    source.stage = WorkStage::Settled;
    state.works.insert(source.work_id.clone(), source);
    let admission = &mut state.admissions.get_mut("old-admission").unwrap().admission;
    admission.work_id = "recovery-source".into();
    admission.execution = attempt();
    state
}

/// [回归测试] B terminal pending 的恢复 batch 保留 A execution；idle 显式选择后解除阻挡而不伪造 ACK。
#[test]
fn recovered_terminal_pending_idle_selection_releases_fresh_admission_without_ack() {
    let control = ControlState::default();
    let mut state = recovered_terminal_state();
    state.works.get_mut("successor").unwrap().stage = WorkStage::Blocked;
    assert_ne!(
        state.batches["old-batch"].execution,
        state.admissions["old-admission"].admission.execution
    );
    assert!(state.has_pending_terminal_obligations_for(1));
    assert!(snapshot(state.clone()).blocked);
    let fresh = delivery("fresh-input");
    let input = crate::session::UserInput {
        input_id: fresh.event.content.message_id.as_uuid().to_string(),
        content: MessageContent::text("fresh input"),
        original_draft: "fresh input".into(),
    };
    let staged = reduce_work(
        &WorkCommand {
            session_id: "session".into(),
            recipient_lifecycle: 1,
            mutation_id: "stage-fresh".into(),
            action: WorkAction::StageUserInput {
                input_json: serde_json::to_string(&input).unwrap(),
                command_id: "stage-fresh".into(),
                fingerprint: 1,
            },
        },
        &control,
        &state,
    )
    .unwrap();
    assert_eq!(staged.receipt.decision, WorkDecision::Accepted);
    let selected = reduce_work(
        &WorkCommand {
            session_id: "session".into(),
            recipient_lifecycle: 1,
            mutation_id: "select-fresh".into(),
            action: WorkAction::PublishStagedUserInputs {
                expected_revision: staged.state.revision,
                expected_control_generation: control.control_generation,
                expected_attempt: None,
                interrupt_current: true,
                deliveries: vec![fresh],
            },
        },
        &control,
        &staged.state,
    )
    .unwrap();
    assert_eq!(selected.receipt.decision, WorkDecision::Accepted);
    assert_eq!(
        selected.state.works["successor"].stage,
        WorkStage::Abandoned
    );
    assert_eq!(
        selected.state.works["recovery-source"],
        state.works["recovery-source"]
    );
    assert!(!selected.state.has_pending_terminal_obligations_for(1));
    assert!(selected.state.has_pending_terminal_obligations());
    assert_eq!(
        selected.state.terminal_obligations,
        state.terminal_obligations
    );
    assert_eq!(selected.state.task_bindings, state.task_bindings);
    assert_eq!(selected.state.admissions, state.admissions);
    assert_eq!(selected.state.batches, state.batches);
    assert!(selected.state.terminal_acknowledgements.is_empty());
    let snapshot = snapshot(selected.state);
    assert!(!snapshot.blocked);
    assert_eq!(snapshot.candidates.len(), 1);
    assert_eq!(snapshot.candidates[0].work_id, "fresh-input");
    snapshot
        .validate_admission(&WorkAdmission {
            session_id: "session".into(),
            admission_id: "fresh-admission".into(),
            instance_id: "fresh-instance".into(),
            generation_id: "fresh-generation".into(),
            lifecycle: 1,
            control_generation: 0,
            work_id: "fresh-input".into(),
            work_revision: 0,
            execution: attempt(),
        })
        .unwrap();
}

/// [回归测试] 恢复 admission 必须有真实 work/batch lineage；同 execution 的无关 batch 不构成证据。
#[test]
fn recovered_terminal_missing_or_live_associations_remain_blocking() {
    for variant in 0..7 {
        let mut state = recovered_terminal_state();
        match variant {
            0 => {
                state.works.remove("recovery-source");
            }
            1 => state.batches.clear(),
            2 => {
                state
                    .batches
                    .get_mut("old-batch")
                    .unwrap()
                    .recipient_lifecycle = 2
            }
            3 => state.works.get_mut("successor").unwrap().stage = WorkStage::ReasonReady,
            4 => state.works.get_mut("successor").unwrap().stage = WorkStage::Settled,
            5 => state.works.get_mut("recovery-source").unwrap().batch_id = "missing-batch".into(),
            6 => {
                state.works.remove("recovery-source");
                let mut unrelated = state.batches["old-batch"].clone();
                unrelated.batch_id = "unrelated-batch".into();
                unrelated.execution = state.admissions["old-admission"]
                    .admission
                    .execution
                    .clone();
                state.batches.insert(unrelated.batch_id.clone(), unrelated);
            }
            _ => unreachable!(),
        }
        assert!(
            state.has_pending_terminal_obligations_for(1),
            "variant {variant}"
        );
        assert!(snapshot(state).blocked, "variant {variant}");
    }
}

#[test]
fn superseded_successor_releases_fresh_admission_without_acknowledging_terminal() {
    let state = superseded_state();
    assert!(!state.has_pending_terminal_obligations_for(1));
    assert!(!state.has_pending_work_for(1));
    assert!(state.has_pending_terminal_obligations());
    let terminal = state.terminal_obligations.clone();
    let bindings = state.task_bindings.clone();
    let control = ControlState::default();
    let reduction = reduce_work(
        &WorkCommand {
            session_id: "session".into(),
            recipient_lifecycle: 1,
            mutation_id: "fresh-publication".into(),
            action: WorkAction::PublishDelivery {
                delivery: delivery("fresh-input"),
            },
        },
        &control,
        &state,
    )
    .unwrap();
    assert_eq!(reduction.receipt.decision, WorkDecision::Accepted);
    let snapshot = snapshot(reduction.state);
    assert!(!snapshot.blocked);
    assert_eq!(snapshot.candidates.len(), 1);
    assert_eq!(snapshot.candidates[0].work_id, "fresh-input");
    snapshot
        .validate_admission(&WorkAdmission {
            session_id: "session".into(),
            admission_id: "fresh-admission".into(),
            instance_id: "fresh-instance".into(),
            generation_id: "fresh-generation".into(),
            lifecycle: 1,
            control_generation: 0,
            work_id: "fresh-input".into(),
            work_revision: 0,
            execution: attempt(),
        })
        .unwrap();
    assert_eq!(snapshot.state.terminal_obligations, terminal);
    assert_eq!(snapshot.state.task_bindings, bindings);
    assert!(snapshot.state.terminal_acknowledgements.is_empty());
}

#[test]
fn incomplete_or_unsuperseded_terminal_associations_remain_blocking() {
    for variant in 0..7 {
        let mut state = superseded_state();
        match variant {
            0 => state.admissions.clear(),
            1 => state.batches.clear(),
            2 => state.works.clear(),
            3 => state
                .batches
                .get_mut("old-batch")
                .unwrap()
                .processing_delivery_ids
                .clear(),
            4 => {
                state
                    .batches
                    .get_mut("old-batch")
                    .unwrap()
                    .recipient_lifecycle = 2
            }
            5 => state.works.get_mut("successor").unwrap().stage = WorkStage::Settled,
            6 => {
                let mut live = state.works["successor"].clone();
                live.work_id = "live-successor".into();
                live.stage = WorkStage::ReasonReady;
                state.works.insert(live.work_id.clone(), live);
            }
            _ => unreachable!(),
        }
        assert!(
            state.has_pending_terminal_obligations_for(1),
            "variant {variant}"
        );
        assert!(state.has_pending_work_for(1), "variant {variant}");
        let snapshot = snapshot(state);
        assert!(snapshot.blocked, "variant {variant}");
        assert!(snapshot.candidates.is_empty(), "variant {variant}");
    }
}

#[test]
fn another_batch_of_the_same_execution_must_not_still_be_live() {
    let mut state = superseded_state();
    let mut batch = state.batches["old-batch"].clone();
    batch.batch_id = "other-batch".into();
    batch.delivery_ids = vec!["other-delivery".into()];
    batch.processing_delivery_ids = vec!["other-delivery".into()];
    state.batches.insert(batch.batch_id.clone(), batch);
    let mut work = state.works["successor"].clone();
    work.work_id = "other-work".into();
    work.batch_id = "other-batch".into();
    work.stage = WorkStage::ReasonReady;
    state.works.insert(work.work_id.clone(), work);
    let mut admission = state.admissions["old-admission"].clone();
    admission.admission.admission_id = "other-admission".into();
    admission.admission.work_id = "other-work".into();
    state.admissions.insert("other-admission".into(), admission);
    assert!(state.has_pending_terminal_obligations_for(1));
    assert!(state.has_pending_work_for(1));
    state.works.get_mut("other-work").unwrap().stage = WorkStage::Settled;
    assert!(!state.has_pending_terminal_obligations_for(1));
    assert!(!state.has_pending_work_for(1));
    state
        .legacy_unknown
        .insert("legacy".into(), "unresolved legacy evidence".into());
    assert!(snapshot(state).blocked);
}

/// [回归测试] 一个 initial admission 的 processing delivery 被多个 batch 声明时，终态 work 也不能消除关联歧义。
#[test]
fn ambiguous_initial_admission_batch_association_remains_blocking() {
    let mut state = superseded_state();
    let mut batch = state.batches["old-batch"].clone();
    batch.batch_id = "ambiguous-batch".into();
    state.batches.insert(batch.batch_id.clone(), batch);
    let mut work = state.works["successor"].clone();
    work.work_id = "ambiguous-work".into();
    work.batch_id = "ambiguous-batch".into();
    work.stage = WorkStage::Settled;
    state.works.insert(work.work_id.clone(), work);
    assert!(state
        .admission_batch(&state.admissions["old-admission"].admission)
        .is_none());
    assert!(state.has_pending_terminal_obligations_for(1));
    assert!(state.has_pending_work_for(1));
    assert!(snapshot(state).blocked);
}
