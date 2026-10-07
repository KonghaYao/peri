use super::*;

#[tokio::test]
async fn abandon_exact_pending_delivery_preserves_user_first_and_terminal_obligations() {
    let (_directory, resources) = fixture().await;
    publish(resources.as_ref(), "user-first").await;
    for (delivery_id, purpose) in [
        ("cron-denied", DeliveryPurpose::Continuation),
        ("task-terminal", DeliveryPurpose::TaskTerminal),
    ] {
        let mut delivery = publication(delivery_id, MessagePolicy::ensure_processing());
        delivery.purpose = purpose;
        assert_eq!(
            resources
                .apply_work_mutation(&prepare_command(&command(
                    &format!("publish-{delivery_id}"),
                    WorkAction::PublishDelivery { delivery }
                )))
                .await
                .unwrap()
                .decision,
            WorkDecision::Accepted
        );
    }
    let loaded = snapshot(resources.as_ref()).await;
    let ticket = admission(&loaded, "mixed-ticket");
    resources
        .apply_work_mutation(&prepare_command(&command(
            "mixed-enter",
            WorkAction::RegisterAdmission { admission: ticket },
        )))
        .await
        .unwrap();
    let before = snapshot(resources.as_ref()).await;
    let abandon = command(
        "deny-only-cron",
        WorkAction::AbandonDelivery {
            guard: guard(&before),
            delivery_id: "cron-denied".into(),
            reason: "scheduled approval rejected".into(),
            evidence: "trusted-scheduled-decision:mixed-ticket".into(),
        },
    );
    let receipt = resources
        .apply_work_mutation(&prepare_command(&abandon))
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    let after = snapshot(resources.as_ref()).await;
    assert_eq!(
        after.state.obligations["cron-denied"].status,
        ObligationStatus::Abandoned
    );
    for delivery_id in ["user-first", "task-terminal"] {
        assert_eq!(
            after.state.deliveries[delivery_id],
            before.state.deliveries[delivery_id]
        );
        assert_eq!(
            after.state.obligations[delivery_id].status,
            ObligationStatus::Pending
        );
    }
    assert_eq!(
        after.state.deliveries["cron-denied"].publication,
        before.state.deliveries["cron-denied"].publication
    );
    assert!(after.state.batches.is_empty());
    assert!(after.state.works.is_empty());
    assert!(resources
        .load_session_history(&"work-session".into())
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        after.candidates[0].delivery_ids,
        vec!["user-first", "task-terminal"]
    );
    assert_eq!(
        resources
            .apply_work_mutation(&prepare_command(&abandon))
            .await
            .unwrap(),
        receipt
    );
    let missing = command(
        "deny-missing",
        WorkAction::AbandonDelivery {
            guard: guard(&after),
            delivery_id: "missing".into(),
            reason: "reject".into(),
            evidence: "trusted-decision".into(),
        },
    );
    assert_eq!(
        resources
            .apply_work_mutation(&prepare_command(&missing))
            .await
            .unwrap()
            .decision,
        WorkDecision::Rejected {
            reason: WorkRejection::Conflict
        }
    );
    let claimed = resources
        .apply_work_mutation(&prepare_command(&command(
            "claim-unrelated",
            WorkAction::ClaimBatch {
                guard: guard(&after),
                batch_id: "user-first".into(),
                delivery_ids: after.candidates[0].delivery_ids.clone(),
            },
        )))
        .await
        .unwrap();
    assert_eq!(claimed.decision, WorkDecision::Accepted);
    let claimed = snapshot(resources.as_ref()).await;
    let forbidden = command(
        "deny-already-claimed",
        WorkAction::AbandonDelivery {
            guard: guard(&claimed),
            delivery_id: "user-first".into(),
            reason: "reject".into(),
            evidence: "trusted-decision".into(),
        },
    );
    assert_eq!(
        resources
            .apply_work_mutation(&prepare_command(&forbidden))
            .await
            .unwrap()
            .decision,
        WorkDecision::Rejected {
            reason: WorkRejection::InvalidTransition
        }
    );
    assert_eq!(snapshot(resources.as_ref()).await.state, claimed.state);
}

#[tokio::test]
async fn child_resume_metadata_is_immutable_and_retained_per_lifecycle() {
    let (directory, resources) = fixture().await;
    let metadata = r#"{"initiatorSessionId":"root","initiatorLifecycle":1,"invocationId":"original-call","authorizationRef":"trusted-factory","persona":"child"}"#;
    let original = command(
        "child-metadata",
        WorkAction::BindChildResumeMetadata {
            expected_revision: 0,
            metadata_json: metadata.into(),
        },
    );
    let receipt = resources
        .apply_work_mutation(&prepare_command(&original))
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    let same = command(
        "child-metadata-same",
        WorkAction::BindChildResumeMetadata {
            expected_revision: receipt.revision,
            metadata_json: serde_json::to_string_pretty(
                &serde_json::from_str::<serde_json::Value>(metadata).unwrap(),
            )
            .unwrap(),
        },
    );
    let same_receipt = resources
        .apply_work_mutation(&prepare_command(&same))
        .await
        .unwrap();
    assert_eq!(same_receipt.decision, WorkDecision::Accepted);
    let conflict = command(
        "child-metadata-conflict",
        WorkAction::BindChildResumeMetadata {
            expected_revision: same_receipt.revision,
            metadata_json: r#"{"persona":"untrusted-replacement"}"#.into(),
        },
    );
    assert_eq!(
        resources
            .apply_work_mutation(&prepare_command(&conflict))
            .await
            .unwrap()
            .decision,
        WorkDecision::Rejected {
            reason: WorkRejection::Conflict
        }
    );
    for (id, action) in [
        ("close", ControlAction::Close),
        ("finish", ControlAction::FinishClose),
        ("reopen", ControlAction::Reopen),
    ] {
        control_action(resources.as_ref(), id, action).await;
    }
    let loaded = snapshot(resources.as_ref()).await;
    let second = WorkCommand {
        recipient_lifecycle: 2,
        ..command(
            "child-metadata-life2",
            WorkAction::BindChildResumeMetadata {
                expected_revision: loaded.state.revision,
                metadata_json:
                    r#"{"persona":"new-child","authorizationRef":"trusted-factory-life2"}"#.into(),
            },
        )
    };
    assert_eq!(
        resources
            .apply_work_mutation(&prepare_command(&second))
            .await
            .unwrap()
            .decision,
        WorkDecision::Accepted
    );
    drop(resources);
    let reopened = SessionResourcesImpl::open(directory.path().join("work.db"))
        .await
        .unwrap();
    let loaded = snapshot(&reopened).await;
    assert_eq!(loaded.state.child_resume_metadata.len(), 2);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&loaded.state.child_resume_metadata[&1]).unwrap(),
        serde_json::from_str::<serde_json::Value>(metadata).unwrap()
    );
    assert_eq!(
        reopened
            .apply_work_mutation(&prepare_command(&original))
            .await
            .unwrap(),
        receipt
    );
}

#[tokio::test]
async fn prepared_invocation_can_only_settle_with_before_effect_rejection() {
    let (directory, resources) = fixture().await;
    publish(resources.as_ref(), "delivery").await;
    let ticket = claim(resources.as_ref()).await;
    begin_reason(resources.as_ref(), &ticket.work_id).await;
    let arguments = "{}";
    let intent = InvocationIntent {
        invocation_id: "invocation".into(),
        tool_call_id: "call".into(),
        tool_name: "protected-tool".into(),
        arguments_json: arguments.into(),
        arguments_digest: format!("{:x}", Sha256::digest(arguments.as_bytes())),
        effective_tool_name: "protected-tool".into(),
        effective_arguments_json: arguments.into(),
        effective_arguments_digest: format!("{:x}", Sha256::digest(arguments.as_bytes())),
        owner_identity: "trusted-owner".into(),
        scope_id: "work-session".into(),
        scope_epoch: Some(1),
        authorization_ref: "trusted-hook".into(),
        recovery_locator: "original-call".into(),
    };
    let loaded = snapshot(resources.as_ref()).await;
    let response =
        WorkPayload::from_payload(&PersistedPayload::Message(BaseMessage::ai_with_tool_calls(
            "request protected tool",
            vec![ToolCallRequest::new(
                "call",
                "protected-tool",
                serde_json::json!({}),
            )],
        )))
        .unwrap();
    let committed = resources
        .apply_work_mutation(&prepare_command(&command(
            "intent-before-hitl",
            WorkAction::CommitReasonResponseAndDispatchIntent {
                guard: guard(&loaded),
                target: target(&loaded, &ticket.work_id),
                request_id: "request".into(),
                response,
                dispatch_intents: vec![intent],
                next_work_id: Some("act-work".into()),
            },
        )))
        .await
        .unwrap();
    assert_eq!(committed.decision, WorkDecision::Accepted);
    let loaded = snapshot(resources.as_ref()).await;
    let act_command = |id: &str, outcome| {
        command(
            id,
            WorkAction::CommitAct {
                guard: guard(&loaded),
                target: target(&loaded, "act-work"),
                results: vec![InvocationResult {
                    invocation_id: "invocation".into(),
                    outcome,
                }],
                next_work_id: Some("next-reason".into()),
            },
        )
    };
    for (id, outcome) in [
        (
            "unexecuted-result",
            InvocationOutcome::Completed {
                result: WorkPayload::from_payload(&PersistedPayload::Message(
                    BaseMessage::tool_result("call", "fabricated result"),
                ))
                .unwrap(),
            },
        ),
        (
            "empty-rejection",
            InvocationOutcome::Cancelled {
                evidence: String::new(),
            },
        ),
    ] {
        assert_eq!(
            resources
                .apply_work_mutation(&prepare_command(&act_command(id, outcome)))
                .await
                .unwrap()
                .decision,
            WorkDecision::Rejected {
                reason: WorkRejection::InvalidTransition
            }
        );
        assert_eq!(snapshot(resources.as_ref()).await.state, loaded.state);
    }
    let cancelled = act_command(
        "hitl-rejected",
        InvocationOutcome::Cancelled {
            evidence: "trusted-hook: user rejected before effect".into(),
        },
    );
    let receipt = resources
        .apply_work_mutation(&prepare_command(&cancelled))
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    let after = snapshot(resources.as_ref()).await;
    assert_eq!(
        after.state.invocations["invocation"].status,
        InvocationStatus::Settled
    );
    assert_eq!(after.state.budgets[&ticket.work_id].dispatches, 0);
    assert_eq!(
        after.state.works["next-reason"].stage,
        WorkStage::ReasonReady
    );
    let history = resources
        .load_session_history(&"work-session".into())
        .await
        .unwrap();
    assert_eq!(history.len(), 3);
    assert!(
        matches!(history.last().and_then(PersistedPayload::as_message), Some(BaseMessage::Tool { tool_call_id, .. }) if tool_call_id == "call")
    );
    let projected = after.state.invocations["invocation"]
        .settled_projection("work-session")
        .unwrap()
        .unwrap();
    assert!(projected.serialized.contains("Invocation cancelled"));
    assert_eq!(
        WorkPayload::from_payload(history.last().unwrap()).unwrap(),
        projected
    );
    drop(resources);
    let reopened = SessionResourcesImpl::open(directory.path().join("work.db"))
        .await
        .unwrap();
    assert_eq!(
        reopened
            .apply_work_mutation(&prepare_command(&cancelled))
            .await
            .unwrap(),
        receipt
    );
    let restored = reopened
        .load_session_history(&"work-session".into())
        .await
        .unwrap();
    let restored: Vec<_> = restored
        .iter()
        .map(|payload| WorkPayload::from_payload(payload).unwrap())
        .collect();
    let expected: Vec<_> = history
        .iter()
        .map(|payload| WorkPayload::from_payload(payload).unwrap())
        .collect();
    assert_eq!(restored, expected);
}

#[tokio::test]
async fn withdrawal_optional_generation_does_not_bypass_exact_claim() {
    let (_directory, resources) = fixture().await;
    publish(resources.as_ref(), "delivery").await;
    control_action(resources.as_ref(), "pause", ControlAction::Pause).await;
    let stopped = snapshot(resources.as_ref()).await;
    let reclaim = command(
        "stale-reclaim",
        WorkAction::WithdrawDelivery {
            expected_revision: stopped.state.revision,
            expected_control_generation: Some(stopped.control.control_generation),
            delivery_id: "delivery".into(),
            authorization_ref: "pause".into(),
        },
    );
    control_action(resources.as_ref(), "resume", ControlAction::Resume).await;
    let receipt = resources
        .apply_work_mutation(&prepare_command(&reclaim))
        .await
        .unwrap();
    assert_eq!(
        receipt.decision,
        WorkDecision::Rejected {
            reason: WorkRejection::StaleControlGeneration
        }
    );
    let loaded = snapshot(resources.as_ref()).await;
    let direct = command(
        "direct-withdraw",
        WorkAction::WithdrawDelivery {
            expected_revision: loaded.state.revision,
            expected_control_generation: None,
            delivery_id: "delivery".into(),
            authorization_ref: "direct-user-withdraw".into(),
        },
    );
    assert_eq!(
        resources
            .apply_work_mutation(&prepare_command(&direct))
            .await
            .unwrap()
            .decision,
        WorkDecision::Accepted
    );
    publish(resources.as_ref(), "claimed-delivery").await;
    claim(resources.as_ref()).await;
    let loaded = snapshot(resources.as_ref()).await;
    let claimed = command(
        "claimed-withdraw",
        WorkAction::WithdrawDelivery {
            expected_revision: loaded.state.revision,
            expected_control_generation: None,
            delivery_id: "claimed-delivery".into(),
            authorization_ref: "direct-user-withdraw".into(),
        },
    );
    assert!(matches!(
        resources
            .apply_work_mutation(&prepare_command(&claimed))
            .await
            .unwrap()
            .decision,
        WorkDecision::Rejected { .. }
    ));
    assert_eq!(snapshot(resources.as_ref()).await.state, loaded.state);
}

async fn control_action(resources: &dyn SessionResources, id: &str, action: ControlAction) {
    let current = resources
        .load_session_control(&"work-session".into())
        .await
        .unwrap();
    let receipt = resources
        .apply_session_control(&ControlCommand {
            session_id: "work-session".into(),
            command_id: id.into(),
            expected_lifecycle: current.lifecycle,
            expected_revision: current.revision,
            expected_control_generation: current.control_generation,
            action,
        })
        .await
        .unwrap();
    assert_eq!(
        receipt.decision,
        peri_acp_types::session_resources::ControlDecision::Accepted
    );
}

#[tokio::test]
async fn stop_cleanup_generation_is_atomic_with_resume_and_original_receipt() {
    let (_directory, resources) = fixture().await;
    publish(resources.as_ref(), "delivery").await;
    let ticket = claim(resources.as_ref()).await;
    begin_reason(resources.as_ref(), &ticket.work_id).await;
    control_action(
        resources.as_ref(),
        "stop",
        ControlAction::Stop {
            target: ticket.execution.clone(),
        },
    )
    .await;
    let stopped = snapshot(resources.as_ref()).await;
    let cleanup = command(
        "old-stop-cleanup",
        WorkAction::AbandonWork {
            expected_revision: stopped.state.revision,
            expected_control_generation: stopped.control.control_generation,
            target: target(&stopped, &ticket.work_id),
            reason: "stop exact execution".into(),
            authorization_ref: "stop".into(),
        },
    );
    control_action(resources.as_ref(), "resume", ControlAction::Resume).await;
    let before = snapshot(resources.as_ref()).await;
    let rejected = resources
        .apply_work_mutation(&prepare_command(&cleanup))
        .await
        .unwrap();
    assert_eq!(
        rejected.decision,
        WorkDecision::Rejected {
            reason: WorkRejection::StaleControlGeneration
        }
    );
    assert_eq!(snapshot(resources.as_ref()).await.state, before.state);
    let fresh = command(
        "fresh-cleanup",
        WorkAction::AbandonWork {
            expected_revision: before.state.revision,
            expected_control_generation: before.control.control_generation,
            target: target(&before, &ticket.work_id),
            reason: "authorized abandonment".into(),
            authorization_ref: "trusted-new-decision".into(),
        },
    );
    let accepted = resources
        .apply_work_mutation(&prepare_command(&fresh))
        .await
        .unwrap();
    assert_eq!(accepted.decision, WorkDecision::Accepted);
    control_action(
        resources.as_ref(),
        "pause-after-cleanup",
        ControlAction::Pause,
    )
    .await;
    assert_eq!(
        resources
            .apply_work_mutation(&prepare_command(&fresh))
            .await
            .unwrap(),
        accepted
    );
    assert_eq!(
        resources
            .apply_work_mutation(&prepare_command(&cleanup))
            .await
            .unwrap(),
        rejected
    );
}

#[tokio::test]
async fn late_terminal_retains_original_lifecycle_settlement_after_reopen() {
    let (directory, resources) = fixture().await;
    let arguments = "{}";
    let intent = InvocationIntent {
        invocation_id: "terminal-invocation".into(),
        tool_call_id: "terminal-call".into(),
        tool_name: "async-tool".into(),
        arguments_json: arguments.into(),
        arguments_digest: format!("{:x}", Sha256::digest(arguments.as_bytes())),
        effective_tool_name: "async-tool".into(),
        effective_arguments_json: arguments.into(),
        effective_arguments_digest: format!("{:x}", Sha256::digest(arguments.as_bytes())),
        owner_identity: "trusted-owner".into(),
        scope_id: "work-session".into(),
        scope_epoch: Some(1),
        authorization_ref: "trusted-owner-auth".into(),
        recovery_locator: "original-invocation".into(),
    };
    let prepared = resources
        .apply_work_mutation(&prepare_command(&command(
            "prepare-terminal",
            WorkAction::PrepareInvocation {
                expected_revision: 0,
                intent,
            },
        )))
        .await
        .unwrap();
    assert_eq!(prepared.decision, WorkDecision::Accepted);
    let binding = TaskBinding {
        invocation_id: "terminal-invocation".into(),
        owner_identity: "trusted-owner".into(),
        owner_task_id: "original-owner-task".into(),
        initiator_session_id: "work-session".into(),
        recipient_lifecycle: 1,
        recovery_locator: "original-invocation".into(),
        authorization_ref: "trusted-owner-auth".into(),
    };
    let bound = resources
        .apply_work_mutation(&prepare_command(&command(
            "bind-terminal",
            WorkAction::ReconcileTaskBinding {
                expected_revision: prepared.revision,
                binding: binding.clone(),
            },
        )))
        .await
        .unwrap();
    assert_eq!(bound.decision, WorkDecision::Accepted);
    for (id, action) in [
        ("close", ControlAction::Close),
        ("finish", ControlAction::FinishClose),
        ("reopen", ControlAction::Reopen),
    ] {
        control_action(resources.as_ref(), id, action).await;
    }
    let mut delivery = publication("late-terminal", MessagePolicy::ensure_processing());
    delivery.purpose = DeliveryPurpose::TaskTerminal;
    let ordinary = resources
        .apply_work_mutation(&prepare_command(&command(
            "ordinary-late",
            WorkAction::PublishDelivery {
                delivery: delivery.clone(),
            },
        )))
        .await
        .unwrap();
    assert_eq!(
        ordinary.decision,
        WorkDecision::Rejected {
            reason: WorkRejection::StaleLifecycle
        }
    );
    let mut forged = binding.clone();
    forged.owner_task_id = "unproved-task".into();
    let rejected = resources
        .apply_work_mutation(&prepare_command(&command(
            "forged-late",
            WorkAction::PublishTaskSettlement {
                delivery: delivery.clone(),
                binding: forged,
            },
        )))
        .await
        .unwrap();
    assert_eq!(
        rejected.decision,
        WorkDecision::Rejected {
            reason: WorkRejection::Conflict
        }
    );
    let original = command(
        "original-terminal",
        WorkAction::PublishTaskSettlement { delivery, binding },
    );
    let receipt = resources
        .apply_work_mutation(&prepare_command(&original))
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    let loaded = snapshot(resources.as_ref()).await;
    assert_eq!(loaded.control.lifecycle, 2);
    assert_eq!(
        loaded.state.deliveries["late-terminal"].recipient_lifecycle,
        1
    );
    assert_eq!(
        loaded.state.obligations["late-terminal"].status,
        ObligationStatus::Suppressed
    );
    assert!(!loaded.state.deliveries["late-terminal"].projected);
    assert!(loaded.candidates.is_empty());
    assert!(resources
        .load_session_history(&"work-session".into())
        .await
        .unwrap()
        .is_empty());
    drop(resources);
    let reopened = SessionResourcesImpl::open(directory.path().join("work.db"))
        .await
        .unwrap();
    assert_eq!(
        reopened
            .resolve_work_mutation(&prepare_command(&original))
            .await
            .unwrap(),
        WorkResolution::Applied {
            receipt: receipt.clone()
        }
    );
    assert_eq!(
        reopened
            .apply_work_mutation(&prepare_command(&original))
            .await
            .unwrap(),
        receipt
    );
    assert_eq!(snapshot(&reopened).await.state, loaded.state);
}
