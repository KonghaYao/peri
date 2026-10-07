use super::*;

#[test]
fn queued_input_records_sdk_admission_without_claiming_or_granting_new_ownership() {
    let mut facts = facts();
    let execution = facts.control.attempt.take().unwrap();
    apply(
        &mut facts,
        command(WorkAction::PublishDelivery {
            delivery: publication("delivery"),
        }),
    );
    let admission = WorkAdmission {
        session_id: "session".into(),
        admission_id: "sdk-ticket".into(),
        instance_id: "instance".into(),
        generation_id: "generation".into(),
        lifecycle: 1,
        control_generation: 0,
        work_id: work_id(&facts),
        work_revision: 0,
        execution: execution.clone(),
    };
    apply(
        &mut facts,
        command(WorkAction::RegisterAdmission { admission }),
    );
    assert_eq!(facts.control.attempt, Some(execution));
    assert_eq!(
        facts.head.current_admission_id.as_deref(),
        Some("sdk-ticket")
    );
    assert_eq!(facts.control.control_generation, 0);
    assert!(facts.processing.is_none());
    assert_eq!(facts.deliveries[0].obligation, ObligationStatus::Pending);
    assert_eq!(
        facts.admission.unwrap().initial_delivery_ids,
        Some(vec!["delivery".into()])
    );
}

#[test]
fn terminal_ack_requires_original_receiver_receipt_and_cannot_satisfy_processing_input() {
    let mut facts = claimed();
    let terminal = command(WorkAction::PublishDelivery {
        delivery: publication("terminal"),
    });
    apply(
        &mut facts,
        command(WorkAction::BindTerminalObligation {
            expected_revision: 0,
            admission_id: "admission".into(),
            command: Box::new(terminal.clone()),
        }),
    );
    let receiver = transition_work(&terminal, &facts).unwrap().receipt;
    assert_rejected(
        &facts,
        WorkAction::AcknowledgeTerminalObligation {
            expected_revision: 0,
            admission_id: "admission".into(),
            receipt: receiver.clone(),
        },
        WorkRejection::Conflict,
    );
    facts.parent_binding_receipt = Some(receiver.clone());
    apply(
        &mut facts,
        command(WorkAction::AcknowledgeTerminalObligation {
            expected_revision: 0,
            admission_id: "admission".into(),
            receipt: receiver,
        }),
    );
    assert_eq!(facts.head.terminal_obligations, 0);
    assert_eq!(facts.head.required_count, 1);
    assert_eq!(facts.deliveries[0].obligation, ObligationStatus::InProgress);
}

#[test]
fn unacknowledged_terminal_outbox_does_not_block_explicit_new_input() {
    let mut facts = claimed();
    let terminal = command(WorkAction::PublishDelivery {
        delivery: publication("terminal"),
    });
    apply(
        &mut facts,
        command(WorkAction::BindTerminalObligation {
            expected_revision: 0,
            admission_id: "admission".into(),
            command: Box::new(terminal),
        }),
    );
    let mut delivery = publication("new-input");
    apply(
        &mut facts,
        command(WorkAction::StageUserInput {
            input_id: "new-input".into(),
            content: delivery.event.content.content.clone(),
            command_id: "new-stage".into(),
            fingerprint: 11,
        }),
    );
    delivery.event.causation_id = Some(
        serde_json::to_string(&UserInputPublicationIdentity {
            input_id: "new-input".into(),
            publication_generation: "new-publication".into(),
            fingerprint: 12,
            command_id: "new-publish".into(),
            draft_binding: StagedUserInputPublicationBinding {
                draft_revision: facts.drafts[0].revision,
                draft_fingerprint: facts.drafts[0].fingerprint,
                canonical_content: delivery.event.content.content.clone(),
            },
        })
        .unwrap(),
    );
    let action = WorkAction::PublishStagedUserInputs {
        expected_revision: 0,
        expected_control_generation: 0,
        expected_attempt: facts.control.attempt.clone(),
        interrupt_current: true,
        deliveries: vec![delivery],
    };
    apply(&mut facts, command(action));
    assert_eq!(facts.head.terminal_obligations, 1);
    assert!(facts
        .terminal_obligation
        .as_ref()
        .unwrap()
        .acknowledgement
        .is_none());
    assert_eq!(facts.drafts[0].status, StagedUserInputStatus::Published);
    assert_eq!(
        facts.processing.as_ref().unwrap().stage,
        WorkStage::Abandoned
    );
    assert_eq!(facts.deliveries.len(), 2);
    assert_eq!(facts.head.required_count, 1);
    assert_eq!(
        facts
            .deliveries
            .iter()
            .find(|delivery| delivery.delivery_id == "delivery")
            .unwrap()
            .obligation,
        ObligationStatus::Abandoned,
    );
    assert_eq!(
        facts
            .deliveries
            .iter()
            .find(|delivery| delivery.delivery_id == "new-input")
            .unwrap()
            .obligation,
        ObligationStatus::Pending,
    );
}

#[test]
fn result_with_another_tool_call_identity_cannot_advance_phase() {
    let mut facts = act_ready();
    dispatch(&mut facts, "first");
    let mut outcome = result(&facts, "first");
    if let InvocationOutcome::Completed { result } = &mut outcome.outcome {
        result.tool_call_id = Some("call-another-invocation".into());
    }
    assert_rejected(
        &facts,
        WorkAction::CommitAct {
            guard: guard(&facts),
            target: target(&facts),
            results: vec![outcome],
            next_work_id: Some(work_id(&facts)),
        },
        WorkRejection::InvalidTransition,
    );
    assert_eq!(facts.processing.as_ref().unwrap().remaining_effects, 2);
}

#[test]
fn task_binding_is_write_once_and_delegation_requires_exact_parent_binding() {
    let mut parent = act_ready();
    dispatch(&mut parent, "first");
    let binding = TaskBinding {
        invocation_id: "first".into(),
        owner_identity: "owner".into(),
        owner_task_id: "owner-task".into(),
        initiator_session_id: "session".into(),
        recipient_lifecycle: 1,
        recovery_locator: "locator".into(),
        authorization_ref: "grant".into(),
    };
    let bound = apply(
        &mut parent,
        command(WorkAction::ReconcileTaskBinding {
            expected_revision: 0,
            binding: binding.clone(),
        }),
    );
    let mut changed = binding.clone();
    changed.owner_task_id = "other-task".into();
    assert_rejected(
        &parent,
        WorkAction::ReconcileTaskBinding {
            expected_revision: 0,
            binding: changed,
        },
        WorkRejection::Conflict,
    );
    let mut child = claimed();
    child.parent_binding_receipt = Some(bound.receipt.clone());
    let action = WorkAction::BindWorkDelegation {
        expected_revision: 0,
        work_id: work_id(&child),
        binding: binding.clone(),
        parent_binding_receipt: bound.receipt.clone(),
    };
    assert_rejected(&child, action.clone(), WorkRejection::Conflict);
    child.parent_effect = Some(
        parent
            .effects
            .iter()
            .find(|effect| effect.invocation_id == "first")
            .unwrap()
            .clone(),
    );
    apply(&mut child, command(action));
    assert_eq!(
        child
            .processing
            .as_ref()
            .unwrap()
            .delegation
            .as_ref()
            .unwrap()
            .delegation_id,
        "first"
    );
}

#[test]
fn recovery_descriptor_preserves_write_once_owner_authorization() {
    let mut facts = facts();
    apply(
        &mut facts,
        command(WorkAction::BindResourceOwners {
            expected_revision: 0,
            connections_json: "[]".into(),
            authorization_ref: "grant".into(),
        }),
    );
    apply(
        &mut facts,
        command(WorkAction::BindChildResumeMetadata {
            expected_revision: 0,
            metadata_json: "{\"parent\":\"parent\"}".into(),
        }),
    );
    assert_rejected(
        &facts,
        WorkAction::BindResourceOwners {
            expected_revision: 0,
            connections_json: "[]".into(),
            authorization_ref: "broader-grant".into(),
        },
        WorkRejection::Conflict,
    );
    let descriptor = facts.recovery_descriptor.unwrap();
    assert_eq!(
        descriptor.resource_owners.unwrap().authorization_ref,
        "grant"
    );
    assert!(descriptor.child_resume_metadata_json.is_some());
}
