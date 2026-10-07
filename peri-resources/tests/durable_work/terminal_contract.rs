use super::*;

#[tokio::test]
async fn terminal_handoff_requires_real_parent_receipt_before_child_finish() {
    let (directory, resources) = fixture().await;
    let intent = InvocationIntent {
        invocation_id: "child-invocation".into(),
        tool_call_id: "child-call".into(),
        tool_name: "subagent".into(),
        arguments_json: "{}".into(),
        arguments_digest: format!("{:x}", Sha256::digest(b"{}")),
        effective_tool_name: "subagent".into(),
        effective_arguments_json: "{}".into(),
        effective_arguments_digest: format!("{:x}", Sha256::digest(b"{}")),
        owner_identity: "trusted-child-owner".into(),
        scope_id: "work-session".into(),
        scope_epoch: Some(1),
        authorization_ref: "trusted-delegation".into(),
        recovery_locator: "child-original-call".into(),
    };
    let prepared = resources
        .apply_work_mutation(&prepare_command(&command(
            "prepare-child",
            WorkAction::PrepareInvocation {
                expected_revision: 0,
                intent,
            },
        )))
        .await
        .unwrap();
    let binding = TaskBinding {
        invocation_id: "child-invocation".into(),
        owner_identity: "trusted-child-owner".into(),
        owner_task_id: "child-session".into(),
        initiator_session_id: "work-session".into(),
        recipient_lifecycle: 1,
        recovery_locator: "child-original-call".into(),
        authorization_ref: "trusted-delegation".into(),
    };
    let parent_binding_receipt = resources
        .apply_work_mutation(&prepare_command(&command(
            "bind-child",
            WorkAction::ReconcileTaskBinding {
                expected_revision: prepared.revision,
                binding: binding.clone(),
            },
        )))
        .await
        .unwrap();
    let workspace = resources.resolve_workspace(directory.path()).await.unwrap();
    resources
        .create_session(&NewSession {
            thread_id: "child-session".into(),
            created_at: "2026-10-05T00:00:00Z".into(),
            meta: NewSessionMeta {
                title: None,
                cwd: workspace.cwd.to_string_lossy().into_owned(),
                parent_thread_id: Some("work-session".into()),
                hidden: false,
                cancel_policy: Default::default(),
                snapshot_at_message_id: None,
            },
            binding: SessionBinding::from_workspace(&workspace),
            frozen: FrozenSnapshotBytes::new(r#"{"v":1}"#),
        })
        .await
        .unwrap();
    let child_command = |id: &str, action| WorkCommand {
        session_id: "child-session".into(),
        recipient_lifecycle: 1,
        mutation_id: id.into(),
        action,
    };
    let child_query = WorkQuery {
        session_id: "child-session".into(),
        limit: 64,
    };
    resources
        .apply_work_mutation(&prepare_command(&child_command(
            "child-input",
            WorkAction::PublishDelivery {
                delivery: publication("child-input", MessagePolicy::ensure_processing()),
            },
        )))
        .await
        .unwrap();
    let loaded = resources.load_session_work(&child_query).await.unwrap();
    let ticket = admission(&loaded, "child-admission");
    let bind_pointer = child_command(
        "child-work-delegation",
        WorkAction::BindWorkDelegation {
            expected_revision: loaded.state.revision,
            work_id: ticket.work_id.clone(),
            binding: binding.clone(),
            parent_binding_receipt,
        },
    );
    assert_eq!(
        resources
            .apply_work_mutation(&prepare_command(&bind_pointer))
            .await
            .unwrap()
            .decision,
        WorkDecision::Accepted
    );
    resources
        .apply_work_mutation(&prepare_command(&child_command(
            "child-enter",
            WorkAction::RegisterAdmission {
                admission: ticket.clone(),
            },
        )))
        .await
        .unwrap();
    let control = resources
        .load_session_control(&"child-session".into())
        .await
        .unwrap();
    resources
        .apply_session_control(&ControlCommand {
            session_id: "child-session".into(),
            command_id: "child-future-exited".into(),
            expected_lifecycle: control.lifecycle,
            expected_revision: control.revision,
            expected_control_generation: control.control_generation,
            action: ControlAction::ObserveAttempt { target: None },
        })
        .await
        .unwrap();
    let mut delivery = publication("child-terminal", MessagePolicy::ensure_processing());
    delivery.purpose = DeliveryPurpose::TaskTerminal;
    let parent_command = command(
        "original-child-terminal",
        WorkAction::PublishTaskSettlement { delivery, binding },
    );
    let loaded = resources.load_session_work(&child_query).await.unwrap();
    let bind = child_command(
        "child-terminal-obligation",
        WorkAction::BindTerminalObligation {
            expected_revision: loaded.state.revision,
            admission_id: ticket.admission_id.clone(),
            command: Box::new(parent_command.clone()),
        },
    );
    let binding_receipt = resources
        .apply_work_mutation(&prepare_command(&bind))
        .await
        .unwrap();
    assert_eq!(binding_receipt.decision, WorkDecision::Accepted);
    let finish_action = WorkAction::FinishAdmission {
        admission: ticket.clone(),
        evidence_id: "actual-child-future-exited".into(),
    };
    assert_eq!(
        resources
            .apply_work_mutation(&prepare_command(&child_command(
                "finish-too-early",
                finish_action.clone()
            )))
            .await
            .unwrap()
            .decision,
        WorkDecision::Rejected {
            reason: WorkRejection::InvalidTransition
        }
    );
    let loaded = resources.load_session_work(&child_query).await.unwrap();
    assert!(loaded.blocked);
    assert!(loaded.state.has_pending_terminal_obligations());
    for (command_id, action) in [
        ("close-child-life1", ControlAction::Close),
        ("finish-child-life1", ControlAction::FinishClose),
        ("reopen-child-life2", ControlAction::Reopen),
    ] {
        let control = resources
            .load_session_control(&"child-session".into())
            .await
            .unwrap();
        let receipt = resources
            .apply_session_control(&ControlCommand {
                session_id: "child-session".into(),
                command_id: command_id.into(),
                expected_lifecycle: control.lifecycle,
                expected_revision: control.revision,
                expected_control_generation: control.control_generation,
                action,
            })
            .await
            .unwrap();
        assert_eq!(
            receipt.decision,
            peri_acp_types::session_resources::ControlDecision::Accepted
        );
    }
    let loaded = resources.load_session_work(&child_query).await.unwrap();
    assert_eq!(loaded.control.lifecycle, 2);
    assert!(!loaded.blocked);
    assert!(!loaded.has_pending_current_work());
    assert!(loaded.state.has_pending_terminal_obligations_for(1));
    assert!(!loaded.state.has_pending_terminal_obligations_for(2));
    assert_eq!(
        loaded.state.terminal_obligations[&ticket.admission_id],
        parent_command
    );
    let forged_receipt = WorkReceipt {
        session_id: parent_command.session_id.clone(),
        mutation_id: parent_command.mutation_id.clone(),
        before_revision: 0,
        revision: 1,
        decision: WorkDecision::Accepted,
        delivery_id: Some("child-terminal".into()),
        admission_sequence: Some(1),
        batch_id: None,
        work_id: None,
        work_revision: None,
        stage: None,
    };
    let forged = child_command(
        "forged-parent-ack",
        WorkAction::AcknowledgeTerminalObligation {
            expected_revision: loaded.state.revision,
            admission_id: ticket.admission_id.clone(),
            receipt: forged_receipt,
        },
    );
    assert!(resources
        .apply_work_mutation(&prepare_command(&forged))
        .await
        .is_err());
    assert!(resources
        .load_session_work(&child_query)
        .await
        .unwrap()
        .state
        .terminal_acknowledgements
        .is_empty());
    assert_eq!(
        resources
            .resolve_work_mutation(&prepare_command(&forged))
            .await
            .unwrap(),
        WorkResolution::NotApplied
    );
    let parent_receipt = resources
        .apply_work_mutation(&prepare_command(&parent_command))
        .await
        .unwrap();
    assert_eq!(parent_receipt.decision, WorkDecision::Accepted);
    let loaded = resources.load_session_work(&child_query).await.unwrap();
    let acknowledge = child_command(
        "real-parent-ack",
        WorkAction::AcknowledgeTerminalObligation {
            expected_revision: loaded.state.revision,
            admission_id: ticket.admission_id.clone(),
            receipt: parent_receipt.clone(),
        },
    );
    assert_eq!(
        resources
            .apply_work_mutation(&prepare_command(&acknowledge))
            .await
            .unwrap()
            .decision,
        WorkDecision::Accepted
    );
    let finished = resources
        .apply_work_mutation(&prepare_command(&child_command(
            "child-settled",
            finish_action,
        )))
        .await
        .unwrap();
    assert_eq!(finished.decision, WorkDecision::Accepted);
    drop(resources);
    let reopened = SessionResourcesImpl::open(directory.path().join("work.db"))
        .await
        .unwrap();
    let loaded = reopened.load_session_work(&child_query).await.unwrap();
    assert_eq!(
        loaded.state.terminal_obligations[&ticket.admission_id],
        parent_command
    );
    assert_eq!(
        loaded.state.terminal_acknowledgements[&ticket.admission_id],
        parent_receipt
    );
    assert_eq!(
        loaded.state.admissions[&ticket.admission_id].settled_receipt,
        Some(finished)
    );
    assert!(!loaded.state.has_pending_terminal_obligations());
    assert_eq!(
        reopened
            .apply_work_mutation(&prepare_command(&bind))
            .await
            .unwrap(),
        binding_receipt
    );
}
