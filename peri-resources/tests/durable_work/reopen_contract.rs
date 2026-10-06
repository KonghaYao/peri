use super::*;

async fn control_action(resources: &dyn SessionResources, id: &str, action: ControlAction) {
    let state = resources
        .load_session_control(&"work-session".into())
        .await
        .unwrap();
    let receipt = resources
        .apply_session_control(&ControlCommand {
            session_id: "work-session".into(),
            command_id: id.into(),
            expected_lifecycle: state.lifecycle,
            expected_revision: state.revision,
            expected_control_generation: state.control_generation,
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
async fn reopen_retains_old_ready_and_blocked_work_without_admitting_it_into_new_life() {
    for blocked in [false, true] {
        let (_directory, resources) = fixture().await;
        publish(resources.as_ref(), "D1").await;
        let old_ticket = claim(resources.as_ref()).await;
        if blocked {
            let loaded = snapshot(resources.as_ref()).await;
            resources
                .apply_work_mutation(&command(
                    "block-D1",
                    WorkAction::BlockWork {
                        expected_revision: loaded.state.revision,
                        target: target(&loaded, &old_ticket.work_id),
                        reason: "old paused responsibility".into(),
                        recovery_condition: "old scope evidence".into(),
                    },
                ))
                .await
                .unwrap();
        }
        let old = snapshot(resources.as_ref()).await;
        let old_work = old.state.works[&old_ticket.work_id].clone();
        let old_projection = old.state.deliveries["D1"].projection.message_id;
        for (id, action) in [
            (
                "exit-old-loop",
                ControlAction::ObserveAttempt { target: None },
            ),
            ("close-life1", ControlAction::Close),
            ("finish-life1", ControlAction::FinishClose),
            ("reopen-life2", ControlAction::Reopen),
        ] {
            control_action(resources.as_ref(), id, action).await;
        }
        let reopened = snapshot(resources.as_ref()).await;
        assert_eq!(reopened.control.lifecycle, 2);
        assert!(!reopened.blocked);
        assert!(reopened.candidates.is_empty());
        assert!(!reopened.has_pending_current_work());
        assert!(reopened.state.has_pending_work_for(1));
        assert_eq!(reopened.state.works[&old_ticket.work_id], old_work);
        assert!(reopened
            .validate_work_lifecycle(&old_ticket.work_id)
            .is_err());
        let next_publication = publication("D2", MessagePolicy::ensure_processing());
        let next_projection = next_publication.event.content.message_id;
        let life2 = |id: &str, action| WorkCommand {
            recipient_lifecycle: 2,
            ..command(id, action)
        };
        assert_eq!(
            resources
                .apply_work_mutation(&life2(
                    "publish-D2",
                    WorkAction::PublishDelivery {
                        delivery: next_publication
                    }
                ))
                .await
                .unwrap()
                .decision,
            WorkDecision::Accepted
        );
        let loaded = snapshot(resources.as_ref()).await;
        assert_eq!(loaded.candidates[0].delivery_ids, vec!["D2"]);
        let ticket = admission(&loaded, "new-life-ticket");
        let mut stale = ticket.clone();
        stale.admission_id = "forged-old-work-ticket".into();
        stale.work_id = old_ticket.work_id.clone();
        stale.work_revision = old_work.revision;
        assert!(loaded.validate_admission(&stale).is_err());
        assert!(matches!(
            resources
                .apply_work_mutation(&life2(
                    "reject-old-work-admission",
                    WorkAction::RegisterAdmission { admission: stale }
                ))
                .await
                .unwrap()
                .decision,
            WorkDecision::Rejected { .. }
        ));
        assert_eq!(
            resources
                .apply_work_mutation(&life2(
                    "enter-new-life",
                    WorkAction::RegisterAdmission {
                        admission: ticket.clone()
                    }
                ))
                .await
                .unwrap()
                .decision,
            WorkDecision::Accepted
        );
        let loaded = snapshot(resources.as_ref()).await;
        assert_eq!(
            resources
                .apply_work_mutation(&life2(
                    "claim-new-life",
                    WorkAction::ClaimBatch {
                        guard: guard(&loaded),
                        batch_id: ticket.work_id.clone(),
                        delivery_ids: vec!["D2".into()]
                    }
                ))
                .await
                .unwrap()
                .decision,
            WorkDecision::Accepted
        );
        let loaded = snapshot(resources.as_ref()).await;
        let old_reason = life2(
            "forged-newlife-old-reason",
            WorkAction::BeginReason {
                guard: guard(&loaded),
                target: target(&loaded, &old_ticket.work_id),
                request_id: "old-reason".into(),
                request: request(),
            },
        );
        assert_eq!(
            resources
                .apply_work_mutation(&old_reason)
                .await
                .unwrap()
                .decision,
            WorkDecision::Rejected {
                reason: WorkRejection::StaleLifecycle
            }
        );
        assert_eq!(snapshot(resources.as_ref()).await.state, loaded.state);
        let serialized_request = serde_json::json!({"messages":[{"sourceMessageId":next_projection.as_uuid().to_string(),"content":"new lifecycle input"}]}).to_string();
        let reason = ReasonRequest {
            request_digest: format!("{:x}", Sha256::digest(serialized_request.as_bytes())),
            serialized_request,
            model_ref: "fixture-model".into(),
            authorization_ref: "current-life-authority".into(),
        };
        assert_eq!(
            resources
                .apply_work_mutation(&life2(
                    "reason-new-life",
                    WorkAction::BeginReason {
                        guard: guard(&loaded),
                        target: target(&loaded, &ticket.work_id),
                        request_id: "new-reason".into(),
                        request: reason
                    }
                ))
                .await
                .unwrap()
                .decision,
            WorkDecision::Accepted
        );
        let after = snapshot(resources.as_ref()).await;
        let current = &after.state.works[&ticket.work_id];
        let batch = &after.state.batches[&current.batch_id];
        assert_eq!(batch.recipient_lifecycle, 2);
        assert_eq!(batch.processing_delivery_ids, vec!["D2"]);
        assert!(current
            .reason_request
            .as_ref()
            .unwrap()
            .serialized_request
            .contains(&next_projection.as_uuid().to_string()));
        assert!(!current
            .reason_request
            .as_ref()
            .unwrap()
            .serialized_request
            .contains(&old_projection.as_uuid().to_string()));
        assert_eq!(after.state.works[&old_ticket.work_id], old_work);
        assert_eq!(
            after.state.batches[&old_work.batch_id].recipient_lifecycle,
            1
        );
    }
}
