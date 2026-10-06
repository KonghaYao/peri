use super::*;

#[test]
fn claim_cannot_replace_the_sdk_admitted_processing_identity() {
    let mut facts = facts();
    apply(
        &mut facts,
        command(WorkAction::PublishDelivery {
            delivery: publication("delivery"),
        }),
    );
    admit(&mut facts);
    assert_rejected(
        &facts,
        WorkAction::ClaimBatch {
            guard: guard(&facts),
            batch_id: "unadmitted-processing".into(),
            delivery_ids: vec!["delivery".into()],
        },
        WorkRejection::StaleExecution,
    );
    assert_eq!(facts.deliveries[0].obligation, ObligationStatus::Pending);
    assert!(facts.processing.is_none());
}

#[test]
fn claim_requires_the_current_unfinished_admission() {
    let mut facts = facts();
    apply(
        &mut facts,
        command(WorkAction::PublishDelivery {
            delivery: publication("delivery"),
        }),
    );
    admit(&mut facts);
    facts.head.current_admission_id = Some("another-admission".into());
    assert_rejected(
        &facts,
        WorkAction::ClaimBatch {
            guard: guard(&facts),
            batch_id: work_id(&facts),
            delivery_ids: vec!["delivery".into()],
        },
        WorkRejection::StaleExecution,
    );
}

#[test]
fn trusted_standalone_intent_does_not_require_processing_budget() {
    let mut facts = facts();
    facts.head.limits.dispatches = 0;
    apply(
        &mut facts,
        command(WorkAction::PrepareInvocation {
            expected_revision: 0,
            intent: intent("standalone"),
        }),
    );
    let action = WorkAction::BeginDispatch {
        guard: guard(&facts),
        target: WorkTarget {
            work_id: "missing-processing".into(),
            expected_work_revision: 0,
        },
        invocation_id: "standalone".into(),
        expected_effect_revision: 0,
    };
    apply(&mut facts, command(action));
    assert_eq!(facts.effects[0].status, InvocationStatus::DispatchAccepted);
    assert_eq!(facts.head.unresolved_effects, 1);
}

fn task_binding() -> TaskBinding {
    TaskBinding {
        invocation_id: "first".into(),
        owner_identity: "owner".into(),
        owner_task_id: "owner-task".into(),
        initiator_session_id: "session".into(),
        recipient_lifecycle: 1,
        recovery_locator: "locator".into(),
        authorization_ref: "grant".into(),
    }
}

#[test]
fn task_reconciliation_cannot_cross_its_command_lifecycle() {
    let mut facts = act_ready();
    dispatch(&mut facts, "first");
    let mut binding = task_binding();
    binding.recipient_lifecycle = 2;
    facts.control.lifecycle = 2;
    facts.head.lifecycle = 2;
    facts.effects.iter_mut().for_each(|effect| {
        effect.recipient_lifecycle = 2;
    });
    assert_rejected(
        &facts,
        WorkAction::ReconcileTaskBinding {
            expected_revision: 0,
            binding,
        },
        WorkRejection::StaleLifecycle,
    );
    assert!(facts.effects.iter().all(|effect| effect.binding.is_none()));
}

#[test]
fn task_reconciliation_retains_old_lifecycle_settlement_and_write_once_binding() {
    let mut facts = act_ready();
    dispatch(&mut facts, "first");
    facts.control.lifecycle = 2;
    facts.head.lifecycle = 2;
    let action = WorkAction::ReconcileTaskBinding {
        expected_revision: 0,
        binding: task_binding(),
    };
    apply(&mut facts, command(action.clone()));
    let transition = apply(&mut facts, command(action));
    assert!(!transition
        .writes
        .iter()
        .any(|write| matches!(write, WorkWrite::Effect { .. })));
    assert_eq!(
        facts
            .effects
            .iter()
            .find(|effect| effect.invocation_id == "first")
            .unwrap()
            .binding
            .as_ref(),
        Some(&task_binding()),
    );
}
