use super::*;
use crate::session_resources::work::reduce_work;

fn fixture(count: usize) -> (WorkState, Vec<TaskBinding>) {
    let mut state = WorkState::default();
    let mut bindings = Vec::new();
    for index in 0..count {
        let binding = TaskBinding {
            invocation_id: format!("invocation-{index}"),
            owner_identity: "trusted-owner".into(),
            owner_task_id: format!("task-{index}"),
            initiator_session_id: "parent".into(),
            recipient_lifecycle: 1,
            recovery_locator: format!("child-{index}"),
            authorization_ref: "authorization".into(),
        };
        let arguments = "{}".to_owned();
        let digest = format!("{:x}", Sha256::digest(arguments.as_bytes()));
        let command = WorkCommand {
            session_id: "parent".into(),
            recipient_lifecycle: 1,
            mutation_id: format!("prepare-{index}"),
            action: WorkAction::PrepareInvocation {
                expected_revision: state.revision,
                intent: InvocationIntent {
                    invocation_id: binding.invocation_id.clone(),
                    tool_call_id: format!("call-{index}"),
                    tool_name: "subagent".into(),
                    arguments_json: arguments.clone(),
                    arguments_digest: digest.clone(),
                    effective_tool_name: "subagent".into(),
                    effective_arguments_json: arguments,
                    effective_arguments_digest: digest,
                    owner_identity: binding.owner_identity.clone(),
                    scope_id: "parent".into(),
                    scope_epoch: Some(1),
                    authorization_ref: binding.authorization_ref.clone(),
                    recovery_locator: binding.recovery_locator.clone(),
                },
            },
        };
        let reduced = reduce_work(&command, &ControlState::default(), &state).unwrap();
        assert_eq!(reduced.receipt.decision, WorkDecision::Accepted);
        state = reduced.state;
        bindings.push(binding);
    }
    (state, bindings)
}

fn command(binding: TaskBinding, observed_revision: u64, mutation_id: &str) -> WorkCommand {
    WorkCommand {
        session_id: "parent".into(),
        recipient_lifecycle: 1,
        mutation_id: mutation_id.into(),
        action: WorkAction::ReconcileTaskBinding {
            expected_revision: observed_revision,
            binding,
        },
    }
}

fn assert_rejected(state: &WorkState, command: &WorkCommand, reason: WorkRejection) {
    let reduced = reduce_work(command, &ControlState::default(), state).unwrap();
    assert_eq!(reduced.receipt.decision, WorkDecision::Rejected { reason });
    assert_eq!(&reduced.state, state);
    assert_eq!(reduced.receipt.revision, state.revision);
    assert!(reduced.projections.is_empty());
    assert!(reduced.events.is_empty());
    assert!(reduced.control.is_none());
}

#[test]
fn sibling_bindings_and_other_writers_accept_the_same_observed_revision() {
    for order in [[0, 1, 2, 3, 4], [4, 3, 2, 1, 0], [2, 4, 0, 3, 1]] {
        let (mut state, bindings) = fixture(5);
        let observed_revision = state.revision;
        for index in order {
            let external = WorkCommand {
                session_id: "parent".into(),
                recipient_lifecycle: 1,
                mutation_id: format!("external-{index}"),
                action: WorkAction::BindResourceOwners {
                    expected_revision: state.revision,
                    connections_json: "[]".into(),
                    authorization_ref: "authorization".into(),
                },
            };
            let external = reduce_work(&external, &ControlState::default(), &state).unwrap();
            assert_eq!(external.receipt.decision, WorkDecision::Accepted);
            state = external.state;
            let command = command(
                bindings[index].clone(),
                observed_revision,
                &format!("bind-{index}"),
            );
            let reduced = reduce_work(&command, &ControlState::default(), &state).unwrap();
            assert_eq!(reduced.receipt.decision, WorkDecision::Accepted);
            assert!(reduced.receipt.before_revision > observed_revision);
            state = reduced.state;
        }
        assert_eq!(state.task_bindings.len(), 5);
        for binding in bindings {
            assert_eq!(state.task_bindings[&binding.invocation_id], binding);
        }
    }
}

#[test]
fn observed_revision_is_a_lower_bound_and_future_revisions_are_rejected() {
    let (state, bindings) = fixture(1);
    for revision in [0, state.revision] {
        let reduced = reduce_work(
            &command(bindings[0].clone(), revision, "bind"),
            &ControlState::default(),
            &state,
        )
        .unwrap();
        assert_eq!(reduced.receipt.decision, WorkDecision::Accepted);
    }
    assert_rejected(
        &state,
        &command(bindings[0].clone(), state.revision + 1, "future"),
        WorkRejection::StaleRevision,
    );
}

#[test]
fn invocation_and_command_identity_conflicts_are_rejected_atomically() {
    let (state, bindings) = fixture(1);
    let base = command(bindings[0].clone(), 0, "bind");
    let mut conflicts = Vec::new();
    let mut wrong_session = base.clone();
    wrong_session.session_id = "other-parent".into();
    conflicts.push(wrong_session);
    let mut wrong_lifecycle = base.clone();
    wrong_lifecycle.recipient_lifecycle = 2;
    conflicts.push(wrong_lifecycle);
    for field in [
        "owner",
        "authorization",
        "locator",
        "parent",
        "lifecycle",
        "task",
    ] {
        let mut binding = bindings[0].clone();
        match field {
            "owner" => binding.owner_identity = "other-owner".into(),
            "authorization" => binding.authorization_ref = "other-authorization".into(),
            "locator" => binding.recovery_locator = "other-child".into(),
            "parent" => binding.initiator_session_id = "other-parent".into(),
            "lifecycle" => binding.recipient_lifecycle = 2,
            "task" => binding.owner_task_id.clear(),
            _ => unreachable!(),
        }
        conflicts.push(command(binding, 0, "bind"));
    }
    for conflict in conflicts {
        assert_rejected(&state, &conflict, WorkRejection::Conflict);
    }
    let mut unknown = bindings[0].clone();
    unknown.invocation_id = "unknown-invocation".into();
    assert_rejected(
        &state,
        &command(unknown, 0, "bind"),
        WorkRejection::InvalidTransition,
    );
}

#[test]
fn existing_binding_is_immutable_and_owner_task_cannot_alias_another_invocation() {
    let (state, bindings) = fixture(2);
    let reduced = reduce_work(
        &command(bindings[0].clone(), 0, "bind"),
        &ControlState::default(),
        &state,
    )
    .unwrap();
    let state = reduced.state;
    let mut replacement = bindings[0].clone();
    replacement.owner_task_id = "replacement-task".into();
    assert_rejected(
        &state,
        &command(replacement, 0, "replace"),
        WorkRejection::Conflict,
    );
    let mut alias = bindings[1].clone();
    alias.owner_task_id = bindings[0].owner_task_id.clone();
    assert_rejected(&state, &command(alias, 0, "alias"), WorkRejection::Conflict);
}

#[test]
fn identical_binding_reconciliation_is_idempotent_across_mutation_identities() {
    let (mut state, bindings) = fixture(1);
    let invocations = state.invocations.clone();
    for mutation_id in ["original-bind", "original-bind", "new-bind"] {
        let reduced = reduce_work(
            &command(bindings[0].clone(), 0, mutation_id),
            &ControlState::default(),
            &state,
        )
        .unwrap();
        assert_eq!(reduced.receipt.decision, WorkDecision::Accepted);
        state = reduced.state;
        assert_eq!(state.invocations, invocations);
        assert_eq!(state.task_bindings.len(), 1);
        assert_eq!(state.task_bindings[&bindings[0].invocation_id], bindings[0]);
    }
}

#[test]
fn saved_invocation_lifecycle_not_current_attempt_controls_late_reconciliation() {
    let (state, bindings) = fixture(1);
    let control = ControlState {
        lifecycle: 2,
        ..ControlState::default()
    };
    let reduced = reduce_work(
        &command(bindings[0].clone(), 0, "late-bind"),
        &control,
        &state,
    )
    .unwrap();
    assert_eq!(reduced.receipt.decision, WorkDecision::Accepted);
    assert_eq!(
        reduced.state.task_bindings[&bindings[0].invocation_id],
        bindings[0]
    );
}

#[test]
fn stored_binding_command_retains_original_wire_fields_and_digest() {
    let original = r#"{"sessionId":"parent","recipientLifecycle":1,"mutationId":"original-bind","action":{"kind":"reconcileTaskBinding","expected_revision":3,"binding":{"invocationId":"invocation-0","ownerIdentity":"trusted-owner","ownerTaskId":"task-0","initiatorSessionId":"parent","recipientLifecycle":1,"recoveryLocator":"child-0","authorizationRef":"authorization"}}}"#;
    let original_digest = format!("{:x}", Sha256::digest(original.as_bytes()));
    let decoded: WorkCommand = serde_json::from_str(original).unwrap();
    assert_eq!(serde_json::to_string(&decoded).unwrap(), original);
    assert_eq!(decoded.digest().unwrap(), original_digest);
    let (mut state, _) = fixture(1);
    state.revision = 10;
    let reduced = reduce_work(&decoded, &ControlState::default(), &state).unwrap();
    assert_eq!(reduced.receipt.decision, WorkDecision::Accepted);
    assert_eq!(decoded.digest().unwrap(), original_digest);
    let with_unknown = original.replace(
        "\"expected_revision\":3",
        "\"expected_revision\":3,\"unknownField\":true",
    );
    assert!(serde_json::from_str::<WorkCommand>(&with_unknown).is_err());
}
