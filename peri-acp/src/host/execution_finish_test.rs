use super::*;
use crate::host::executor_flow_tests::execution_fixture::new_resources;
use peri_acp_types::{
    identity::AttemptId,
    messages::BaseMessage,
    session::{MessagePolicy, TurnId},
    session_resources::{
        ControlAction, ControlAttempt, ControlCommand, ControlDecision,
        work::{
            DeliveryPurpose, PublishDelivery, WorkEvent, WorkInspection, WorkPayload, WorkQuery,
        },
    },
    store::PersistedPayload,
};
use std::sync::Arc;

async fn snapshot(resources: &dyn SessionResources, admission: &WorkAdmission) -> WorkInspection {
    resources
        .inspect_work(&WorkQuery {
            session_id: admission.session_id.clone(),
            selector: peri_acp_types::session_resources::work::WorkSelector::Admission {
                admission_id: admission.admission_id.clone(),
            },
            limit: 1,
            cursor: None,
        })
        .await
        .unwrap()
}

async fn registered_admission() -> (
    Arc<dyn SessionResources>,
    Arc<tempfile::TempDir>,
    WorkAdmission,
) {
    let session_id = uuid::Uuid::now_v7().to_string();
    let (resources, directory) = new_resources(&session_id).await;
    let control = resources.load_session_control(&session_id).await.unwrap();
    let publication = WorkCommand {
        session_id: session_id.clone(),
        recipient_lifecycle: control.lifecycle,
        mutation_id: "finish-fixture-input".into(),
        action: WorkAction::PublishDelivery {
            delivery: PublishDelivery {
                delivery_id: uuid::Uuid::now_v7().to_string(),
                event: WorkEvent {
                    producer_namespace: "execution-finish-test".into(),
                    event_id: "required-user-input".into(),
                    event_kind: "userInput".into(),
                    causation_id: None,
                    content: crate::host::work_query::test_payload(
                        resources.as_ref(),
                        &session_id,
                        &PersistedPayload::Message(BaseMessage::human(
                            "required input for execution settlement",
                        )),
                    )
                    .await,
                },
                purpose: DeliveryPurpose::UserInput,
                policy: MessagePolicy::ensure_processing(),
            },
        },
    };
    assert_eq!(
        resources
            .apply_work_mutation(&publication)
            .await
            .unwrap()
            .decision,
        WorkDecision::Accepted
    );
    let published = resources
        .inspect_work(&WorkQuery {
            session_id: session_id.clone(),
            selector: peri_acp_types::session_resources::work::WorkSelector::Availability,
            limit: 1,
            cursor: None,
        })
        .await
        .unwrap();
    assert_eq!(
        crate::host::work_query::availability(&published)
            .unwrap()
            .candidates
            .len(),
        1
    );
    let candidate = &crate::host::work_query::availability(&published)
        .unwrap()
        .candidates[0];
    let admission = WorkAdmission {
        session_id,
        admission_id: uuid::Uuid::now_v7().to_string(),
        instance_id: "finish-fixture-instance".into(),
        generation_id: "finish-fixture-generation".into(),
        lifecycle: published.control.lifecycle,
        control_generation: published.control.control_generation,
        work_id: candidate.work_id.clone(),
        work_revision: candidate.work_revision,
        execution: ControlAttempt {
            turn_id: TurnId::new(),
            attempt_id: AttemptId::new(),
        },
    };
    let registration = WorkCommand {
        session_id: admission.session_id.clone(),
        recipient_lifecycle: admission.lifecycle,
        mutation_id: "finish-fixture-registration".into(),
        action: WorkAction::RegisterAdmission {
            admission: admission.clone(),
        },
    };
    assert_eq!(
        resources
            .apply_work_mutation(&registration)
            .await
            .unwrap()
            .decision,
        WorkDecision::Accepted
    );
    let registered = snapshot(resources.as_ref(), &admission).await;
    assert_eq!(
        registered.control.attempt,
        Some(admission.execution.clone())
    );
    let record = &crate::host::work_query::admission(&registered, &admission.admission_id)
        .unwrap()
        .unwrap();
    assert_eq!(record.admission, admission);
    assert!(!record.entering_mutation_id.is_empty());
    assert!(record.leaving_evidence_id.is_none());
    assert!(record.leaving_evidence_id.is_none());
    (resources, directory, admission)
}

#[tokio::test]
async fn finish_admission_commits_attempt_clear_and_settlement_evidence_and_replays_idempotently() {
    let (resources, directory, admission) = registered_admission().await;
    let before = snapshot(resources.as_ref(), &admission).await;
    let evidence = finish_admission(resources.as_ref(), &admission)
        .await
        .unwrap();
    assert_eq!(
        evidence,
        format!("execution-exit:{}", admission.admission_id)
    );
    let finished = snapshot(resources.as_ref(), &admission).await;
    assert!(finished.control.attempt.is_none());
    assert_eq!(finished.control.revision, before.control.revision + 1);
    assert_eq!(
        finished.control.control_generation,
        before.control.control_generation
    );
    assert_eq!(finished.head.change_seq, before.head.change_seq + 1);
    let record = &crate::host::work_query::admission(&finished, &admission.admission_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        record.leaving_evidence_id.as_deref(),
        Some(evidence.as_str())
    );
    let journal = crate::host::work_query::command(
        resources.as_ref(),
        &admission.session_id,
        &command(&admission, 0).mutation_id,
    )
    .await
    .unwrap()
    .unwrap();
    let Some(WorkResolution::Applied { receipt: settled }) = journal.resolution else {
        panic!("settlement receipt required")
    };
    assert_eq!(settled.decision, WorkDecision::Accepted);
    assert_eq!(settled.mutation_id, command(&admission, 0).mutation_id);
    assert_eq!(
        resources
            .resolve_work_mutation(&command(&admission, 0))
            .await
            .unwrap(),
        WorkResolution::Applied { receipt: settled }
    );
    assert_eq!(
        finish_admission(resources.as_ref(), &admission)
            .await
            .unwrap(),
        evidence
    );
    let replayed = snapshot(resources.as_ref(), &admission).await;
    assert_eq!(replayed.control, finished.control);
    assert_eq!(replayed.page, finished.page);

    drop(resources);
    let reopened =
        peri_resources::sessions::SessionResourcesImpl::open(directory.path().join("execution.db"))
            .await
            .unwrap();
    assert!(reconcile_finish(&reopened, &admission).await.unwrap());
    assert_eq!(
        finish_admission(&reopened, &admission).await.unwrap(),
        evidence
    );
    let replayed = snapshot(&reopened, &admission).await;
    assert_eq!(replayed.control, finished.control);
    assert_eq!(replayed.page, finished.page);
}

#[tokio::test]
async fn reconcile_finish_retries_known_not_applied_with_new_stable_mutation() {
    let (resources, _directory, admission) = registered_admission().await;
    let original = command(&admission, 0);
    let before = snapshot(resources.as_ref(), &admission).await;
    assert_eq!(
        resources.resolve_work_mutation(&original).await.unwrap(),
        WorkResolution::NotApplied
    );
    let journal = crate::host::work_query::command(
        resources.as_ref(),
        &admission.session_id,
        &original.mutation_id.clone(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(journal.command, original);
    assert_eq!(journal.resolution, Some(WorkResolution::NotApplied));
    let unresolved = snapshot(resources.as_ref(), &admission).await;
    assert_eq!(unresolved.control, before.control);
    assert_eq!(unresolved.page, before.page);

    assert!(
        reconcile_finish(resources.as_ref(), &admission)
            .await
            .unwrap()
    );
    let retry = command(&admission, 1);
    assert_ne!(retry.mutation_id, original.mutation_id);
    assert_eq!(retry.action, original.action);
    let finished = snapshot(resources.as_ref(), &admission).await;
    assert!(finished.control.attempt.is_none());
    let record = &crate::host::work_query::admission(&finished, &admission.admission_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        record.leaving_evidence_id,
        Some(format!("execution-exit:{}", admission.admission_id))
    );
    let journal = crate::host::work_query::command(
        resources.as_ref(),
        &admission.session_id,
        &retry.mutation_id,
    )
    .await
    .unwrap()
    .unwrap();
    let Some(WorkResolution::Applied { receipt: settled }) = journal.resolution else {
        panic!("settlement receipt required")
    };
    assert_eq!(settled.decision, WorkDecision::Accepted);
    assert_eq!(settled.mutation_id, retry.mutation_id);
    assert_eq!(
        resources.resolve_work_mutation(&retry).await.unwrap(),
        WorkResolution::Applied { receipt: settled }
    );
    assert_eq!(
        resources.resolve_work_mutation(&original).await.unwrap(),
        WorkResolution::NotApplied
    );
    assert!(
        reconcile_finish(resources.as_ref(), &admission)
            .await
            .unwrap()
    );
    let replayed = snapshot(resources.as_ref(), &admission).await;
    assert_eq!(replayed.control, finished.control);
    assert_eq!(replayed.page, finished.page);
    assert!(
        crate::host::work_query::command(
            resources.as_ref(),
            &admission.session_id,
            &command(&admission, 2).mutation_id
        )
        .await
        .unwrap()
        .is_none()
    );
}

#[tokio::test]
async fn finish_admission_rejects_original_execution_after_another_attempt_is_observed() {
    let (resources, _directory, admission) = registered_admission().await;
    let other_execution = ControlAttempt {
        turn_id: TurnId::new(),
        attempt_id: AttemptId::new(),
    };
    assert_ne!(other_execution, admission.execution);
    for (command_id, target) in [
        ("finish-identity-observe-none", None),
        (
            "finish-identity-observe-other",
            Some(other_execution.clone()),
        ),
    ] {
        let before = snapshot(resources.as_ref(), &admission).await;
        let observed = resources
            .apply_session_control(&ControlCommand {
                session_id: admission.session_id.clone(),
                command_id: command_id.into(),
                expected_lifecycle: before.control.lifecycle,
                expected_revision: before.control.revision,
                expected_control_generation: before.control.control_generation,
                action: ControlAction::ObserveAttempt {
                    target: target.clone(),
                },
            })
            .await
            .unwrap();
        assert_eq!(observed.decision, ControlDecision::Accepted);
        assert_eq!(observed.state.attempt, target);
    }
    let before = snapshot(resources.as_ref(), &admission).await;
    assert_eq!(before.control.attempt, Some(other_execution));
    assert!(
        crate::host::work_query::admission(&before, &admission.admission_id)
            .unwrap()
            .unwrap()
            .leaving_evidence_id
            .is_none()
    );
    for _ in 0..2 {
        assert!(
            finish_admission(resources.as_ref(), &admission)
                .await
                .is_err()
        );
        let after = snapshot(resources.as_ref(), &admission).await;
        assert_eq!(after.control, before.control);
        assert_eq!(after.page, before.page);
        let record = &crate::host::work_query::admission(&after, &admission.admission_id)
            .unwrap()
            .unwrap();
        assert!(record.leaving_evidence_id.is_none());
        assert!(record.leaving_evidence_id.is_none());
    }
    assert!(matches!(
        resources.resolve_work_mutation(&command(&admission, 0)).await.unwrap(),
        WorkResolution::Applied { receipt }
            if matches!(receipt.decision, WorkDecision::Rejected { .. })
    ));
}

#[tokio::test]
async fn reconcile_finish_can_progress_past_previous_known_not_applied_retries() {
    let (resources, _directory, admission) = registered_admission().await;
    for retry in 0..5 {
        assert_eq!(
            resources
                .resolve_work_mutation(&command(&admission, retry))
                .await
                .unwrap(),
            WorkResolution::NotApplied
        );
    }
    assert!(
        reconcile_finish(resources.as_ref(), &admission)
            .await
            .unwrap()
    );
    let finished = snapshot(resources.as_ref(), &admission).await;
    assert!(finished.control.attempt.is_none());
    let journal = crate::host::work_query::command(
        resources.as_ref(),
        &admission.session_id,
        &command(&admission, 5).mutation_id,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        journal.resolution,
        Some(WorkResolution::Applied { .. })
    ));
    assert!(
        reconcile_finish(resources.as_ref(), &admission)
            .await
            .unwrap()
    );
    assert_eq!(
        snapshot(resources.as_ref(), &admission).await.page,
        finished.page
    );
}

#[tokio::test]
async fn reconcile_finish_without_journal_does_not_infer_settlement_from_absent_attempt() {
    let (resources, _directory, admission) = registered_admission().await;
    let registered = snapshot(resources.as_ref(), &admission).await;
    let cleared = resources
        .apply_session_control(&ControlCommand {
            session_id: admission.session_id.clone(),
            command_id: "finish-fixture-observe-no-attempt".into(),
            expected_lifecycle: registered.control.lifecycle,
            expected_revision: registered.control.revision,
            expected_control_generation: registered.control.control_generation,
            action: ControlAction::ObserveAttempt { target: None },
        })
        .await
        .unwrap();
    assert_eq!(cleared.decision, ControlDecision::Accepted);
    let before = snapshot(resources.as_ref(), &admission).await;
    assert!(before.control.attempt.is_none());
    for _ in 0..2 {
        assert!(
            !reconcile_finish(resources.as_ref(), &admission)
                .await
                .unwrap()
        );
        let after = snapshot(resources.as_ref(), &admission).await;
        assert_eq!(after.control, before.control);
        assert_eq!(after.page, before.page);
        let record = &crate::host::work_query::admission(&after, &admission.admission_id)
            .unwrap()
            .unwrap();
        assert!(record.leaving_evidence_id.is_none());
        assert!(record.leaving_evidence_id.is_none());
    }
    for retry in 0..3 {
        assert!(
            crate::host::work_query::command(
                resources.as_ref(),
                &admission.session_id,
                &command(&admission, retry).mutation_id
            )
            .await
            .unwrap()
            .is_none()
        );
    }
}
