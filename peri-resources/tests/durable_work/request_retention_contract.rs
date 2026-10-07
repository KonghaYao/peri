use super::*;
use peri_acp_types::store::serialize_persisted_payload;

async fn started_work() -> (
    TempDir,
    Arc<dyn SessionResources>,
    WorkAdmission,
    ReasonRequest,
) {
    let (directory, resources) = fixture().await;
    publish(resources.as_ref(), "delivery").await;
    let ticket = claim(resources.as_ref()).await;
    let loaded = snapshot(resources.as_ref()).await;
    let serialized_request = serde_json::json!({"messages": ["context".repeat(16384)]}).to_string();
    let checkpoint = ReasonRequest {
        request_digest: format!("{:x}", Sha256::digest(serialized_request.as_bytes())),
        serialized_request,
        model_ref: "test-model".into(),
        authorization_ref: "test-authorization".into(),
    };
    let receipt = resources
        .apply_work_mutation(&prepare_command(&command(
            "reason-begin",
            WorkAction::BeginReason {
                guard: guard(&loaded),
                target: target(&loaded, &ticket.work_id),
                request_id: "request".into(),
                request: checkpoint.clone(),
            },
        )))
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    (directory, resources, ticket, checkpoint)
}

#[tokio::test]
async fn in_flight_request_survives_database_reopen() {
    let (directory, resources, ticket, checkpoint) = started_work().await;
    let before = snapshot(resources.as_ref()).await;
    drop(resources);
    let reopened = SessionResourcesImpl::open(directory.path().join("work.db"))
        .await
        .unwrap();
    let loaded = snapshot(&reopened).await;
    assert_eq!(loaded.state, before.state);
    let work = &loaded.state.works[&ticket.work_id];
    assert_eq!(work.stage, WorkStage::ReasonInFlight);
    assert_eq!(work.request_id.as_deref(), Some("request"));
    assert_eq!(work.reason_request.as_ref(), Some(&checkpoint));
}

#[tokio::test]
async fn terminal_request_trimming_preserves_response_identity_and_replay_after_reopen() {
    let (directory, resources, ticket, _) = started_work().await;
    let loaded = snapshot(resources.as_ref()).await;
    let payload =
        WorkPayload::from_payload(&PersistedPayload::Message(BaseMessage::ai("done"))).unwrap();
    let response = command(
        "reason-response",
        WorkAction::CommitReasonResponseAndDispatchIntent {
            guard: guard(&loaded),
            target: target(&loaded, &ticket.work_id),
            request_id: "request".into(),
            response: payload.clone(),
            dispatch_intents: Vec::new(),
            next_work_id: None,
        },
    );
    let receipt = resources
        .apply_work_mutation(&prepare_command(&response))
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    let terminal = snapshot(resources.as_ref()).await;
    let history = resources
        .load_session_history(&"work-session".into())
        .await
        .unwrap()
        .iter()
        .map(|payload| serialize_persisted_payload(payload).unwrap())
        .collect::<Vec<_>>();
    drop(resources);
    let reopened = SessionResourcesImpl::open(directory.path().join("work.db"))
        .await
        .unwrap();
    let loaded = snapshot(&reopened).await;
    assert_eq!(loaded.state, terminal.state);
    let work = &loaded.state.works[&ticket.work_id];
    assert_eq!(work.stage, WorkStage::Settled);
    assert!(work.reason_request.is_none());
    assert_eq!(work.request_id.as_deref(), Some("request"));
    assert_eq!(work.response.as_ref(), Some(&payload));
    assert_eq!(
        reopened
            .apply_work_mutation(&prepare_command(&response))
            .await
            .unwrap(),
        receipt
    );
    assert_eq!(snapshot(&reopened).await.state, terminal.state);
    assert_eq!(
        reopened
            .load_session_history(&"work-session".into())
            .await
            .unwrap()
            .iter()
            .map(|payload| serialize_persisted_payload(payload).unwrap())
            .collect::<Vec<_>>(),
        history
    );
}

#[tokio::test]
async fn rejected_response_preserves_in_flight_state_and_checkpoint_after_reopen() {
    let (directory, resources, ticket, checkpoint) = started_work().await;
    let before = snapshot(resources.as_ref()).await;
    let response = command(
        "invalid-response",
        WorkAction::CommitReasonResponseAndDispatchIntent {
            guard: guard(&before),
            target: target(&before, &ticket.work_id),
            request_id: "different-request".into(),
            response: WorkPayload::from_payload(&PersistedPayload::Message(BaseMessage::ai(
                "must not be committed",
            )))
            .unwrap(),
            dispatch_intents: Vec::new(),
            next_work_id: None,
        },
    );
    let receipt = resources
        .apply_work_mutation(&prepare_command(&response))
        .await
        .unwrap();
    assert_eq!(
        receipt.decision,
        WorkDecision::Rejected {
            reason: WorkRejection::InvalidTransition
        }
    );
    assert_eq!(snapshot(resources.as_ref()).await.state, before.state);
    drop(resources);
    let reopened = SessionResourcesImpl::open(directory.path().join("work.db"))
        .await
        .unwrap();
    let loaded = snapshot(&reopened).await;
    assert_eq!(loaded.state, before.state);
    assert_eq!(
        loaded.state.works[&ticket.work_id].reason_request.as_ref(),
        Some(&checkpoint)
    );
    assert_eq!(
        reopened
            .apply_work_mutation(&prepare_command(&response))
            .await
            .unwrap(),
        receipt
    );
    assert_eq!(
        reopened
            .load_session_history(&"work-session".into())
            .await
            .unwrap()
            .len(),
        1
    );
}
