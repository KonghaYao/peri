use super::super::work_test_support::fixture_with_input;
use super::*;
use std::sync::Arc;

#[tokio::test]
async fn recover_ready_processing_retains_exact_claimed_delivery_identity() {
    let fixture = fixture_with_input(
        Arc::new(super::super::NullReactLLM),
        Vec::new(),
        "recovery input",
    )
    .await;
    super::super::receive::run_receive(super::super::ReceiveInput {
        context: fixture.context.clone(),
    })
    .await
    .unwrap();
    let session = fixture
        .context
        .work
        .state
        .lock()
        .await
        .session
        .clone()
        .unwrap();
    let recovered = recover_work(&session, &fixture.admission.work_id)
        .await
        .unwrap();
    assert_eq!(recovered.target.work_id, fixture.admission.work_id);
    assert_eq!(recovered.deliveries.len(), 1);
    assert_eq!(recovered.deliveries[0].delivery_id, fixture.delivery_id);
    assert!(matches!(recovered.stage, RecoveredStage::ReasonReady));
}

#[tokio::test]
async fn recover_inflight_request_returns_original_reference_not_a_new_reason() {
    let fixture = fixture_with_input(
        Arc::new(super::super::NullReactLLM),
        Vec::new(),
        "uncertain input",
    )
    .await;
    super::super::receive::run_receive(super::super::ReceiveInput {
        context: fixture.context.clone(),
    })
    .await
    .unwrap();
    let session = fixture
        .context
        .work
        .state
        .lock()
        .await
        .session
        .clone()
        .unwrap();
    let processing = session
        .processing(&fixture.admission.work_id)
        .await
        .unwrap();
    let head = session.inspect_head().await.unwrap();
    let checkpoint = super::super::work_pipeline::request_checkpoint(
        &session,
        &serde_json::json!({"exact":"request"}),
        "model".into(),
        "authorization".into(),
    )
    .await
    .unwrap();
    let command = session.command(WorkAction::BeginReason {
        guard: session.guard(&head).unwrap(),
        target: WorkSession::target(&processing),
        request_id: "original-request".into(),
        request: checkpoint.clone(),
    });
    session.ledger.commit(&command).await.unwrap();
    let before = session
        .processing(&fixture.admission.work_id)
        .await
        .unwrap();
    let recovered = recover_work(&session, &fixture.admission.work_id)
        .await
        .unwrap();
    let RecoveredStage::ReasonUncertain {
        request_id,
        request,
    } = recovered.stage
    else {
        panic!("must reconcile original request")
    };
    assert_eq!(request_id, "original-request");
    assert_eq!(request, checkpoint.payload);
    assert_eq!(
        session
            .processing(&fixture.admission.work_id)
            .await
            .unwrap(),
        before
    );
    assert_eq!(before.budget.reason_requests, 1);
}
