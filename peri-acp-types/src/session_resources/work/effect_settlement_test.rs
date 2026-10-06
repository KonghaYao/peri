use super::*;

#[test]
fn before_effect_cancellation_commits_a_paired_result_without_spending_dispatch_budget() {
    let mut facts = act_ready();
    let action = WorkAction::CommitAct {
        guard: guard(&facts),
        target: target(&facts),
        results: vec![InvocationResult {
            invocation_id: "first".into(),
            expected_effect_revision: 0,
            outcome: InvocationOutcome::Cancelled {
                evidence: "before-effect:user-rejected:first".into(),
                result: payload("first", "tool"),
            },
        }],
        next_work_id: None,
    };
    let transition = apply(&mut facts, command(action));
    assert_eq!(facts.processing.as_ref().unwrap().remaining_effects, 1);
    assert_eq!(facts.processing.as_ref().unwrap().budget.dispatches, 0);
    assert_eq!(facts.head.unresolved_effects, 1);
    assert_eq!(
        facts
            .effects
            .iter()
            .find(|effect| effect.invocation_id == "first")
            .unwrap()
            .status,
        InvocationStatus::Settled,
    );
    assert_eq!(transition.writes.iter().filter(|write| matches!(
        write, WorkWrite::Transcript { payload } if payload.tool_call_id.as_deref() == Some("call-first")
    )).count(), 1);
    dispatch(&mut facts, "second");
    let action = WorkAction::CommitAct {
        guard: guard(&facts),
        target: target(&facts),
        results: vec![result(&facts, "second")],
        next_work_id: Some(work_id(&facts)),
    };
    apply(&mut facts, command(action));
    assert_eq!(
        facts.processing.as_ref().unwrap().stage,
        WorkStage::ReasonReady
    );
    assert_eq!(facts.processing.as_ref().unwrap().budget.dispatches, 1);
}

#[test]
fn prepared_cancellation_requires_evidence_and_the_exact_tool_call() {
    let facts = act_ready();
    for (evidence, tool_call_id) in [("", "call-first"), ("rejected", "call-another")] {
        let mut result_payload = payload("first", "tool");
        result_payload.tool_call_id = Some(tool_call_id.into());
        assert_rejected(
            &facts,
            WorkAction::CommitAct {
                guard: guard(&facts),
                target: target(&facts),
                results: vec![InvocationResult {
                    invocation_id: "first".into(),
                    expected_effect_revision: 0,
                    outcome: InvocationOutcome::Cancelled {
                        evidence: evidence.into(),
                        result: result_payload,
                    },
                }],
                next_work_id: None,
            },
            WorkRejection::InvalidTransition,
        );
    }
}

#[test]
fn early_task_terminal_is_accepted_without_settling_the_tool_invocation() {
    let mut facts = act_ready();
    dispatch(&mut facts, "first");
    let binding = TaskBinding {
        invocation_id: "first".into(),
        owner_identity: "owner".into(),
        owner_task_id: "owner-task".into(),
        initiator_session_id: "session".into(),
        recipient_lifecycle: 1,
        recovery_locator: "locator".into(),
        authorization_ref: "grant".into(),
    };
    apply(
        &mut facts,
        command(WorkAction::ReconcileTaskBinding {
            expected_revision: 0,
            binding: binding.clone(),
        }),
    );
    let mut delivery = publication("early-terminal");
    delivery.purpose = DeliveryPurpose::TaskTerminal;
    let action = WorkAction::PublishTaskSettlement {
        delivery,
        binding: binding.clone(),
    };
    apply(&mut facts, command(action.clone()));
    let revision = facts.processing.as_ref().unwrap().revision;
    let replay = apply(&mut facts, command(action.clone()));
    assert!(!replay
        .writes
        .iter()
        .any(|write| matches!(write, WorkWrite::Delivery { .. })));
    assert_eq!(facts.deliveries.len(), 2);
    assert_eq!(facts.head.required_count, 1);
    assert_eq!(facts.head.unresolved_effects, 2);
    assert_eq!(facts.processing.as_ref().unwrap().remaining_effects, 2);
    assert_eq!(facts.processing.as_ref().unwrap().revision, revision);
    let mut changed = action;
    if let WorkAction::PublishTaskSettlement { binding, .. } = &mut changed {
        binding.owner_task_id = "another-task".into();
    }
    assert_rejected(&facts, changed, WorkRejection::InvalidTransition);
}

#[test]
fn dispatch_budget_denial_blocks_without_accepting_the_effect_or_incrementing_usage() {
    for limit in [0, 1, u64::MAX] {
        let mut facts = act_ready();
        facts.head.limits.dispatches = limit;
        facts.processing.as_mut().unwrap().budget.dispatches = limit;
        dispatch(&mut facts, "first");
        let processing = facts.processing.as_ref().unwrap();
        assert_eq!(processing.stage, WorkStage::Blocked);
        assert_eq!(processing.resume_stage, Some(WorkStage::ActReady));
        assert_eq!(processing.budget.dispatches, limit);
        assert_eq!(
            processing.blocked_evidence.as_deref(),
            Some("dispatch budget exhausted")
        );
        assert!(facts
            .effects
            .iter()
            .all(|effect| effect.status == InvocationStatus::Prepared));
        assert_eq!(processing.remaining_effects, 2);
        assert_eq!(facts.head.unresolved_effects, 2);
    }
}
