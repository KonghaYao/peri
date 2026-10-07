use super::super::work_test_support::{fixture_with_input, native_model, serve};
use super::super::{ReasonInput, ReceiveInput};
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct KnownTool(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl BaseTool for KnownTool {
    fn name(&self) -> &str {
        "known"
    }

    fn description(&self) -> &str {
        "known tool with observable effects"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    async fn invoke(
        &self,
        _: serde_json::Value,
        _: crate::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok("executed".into())
    }
}

#[tokio::test]
async fn completed_model_unknown_tool_reproduces_uncommitted_reason_checkpoint() {
    let (model, listener) = native_model().await;
    let fixture = fixture_with_input(Arc::new(model), Vec::new(), "unknown tool response").await;
    super::super::receive::run_receive(ReceiveInput {
        context: fixture.context.clone(),
    })
    .await
    .unwrap();
    let server = serve(
        listener,
        fixture.bound.resources(),
        fixture.bound.thread_id(),
        true,
    );
    let error = super::super::reason::run_reason(ReasonInput {
        context: fixture.context.clone(),
        has_tool_calls: false,
    })
    .await
    .err()
    .expect("current commit_response rejects the completed model's unknown tool");
    server
        .await
        .expect("model HTTP request must have completed");
    assert!(matches!(
        &error,
        AgentError::ToolNotFound(name) if name == "probe"
    ));
    let session = fixture
        .context
        .work
        .state
        .lock()
        .await
        .session
        .clone()
        .unwrap();
    let snapshot = session.snapshot().await.unwrap();
    let work = snapshot
        .state
        .works
        .values()
        .find(|work| work.stage == WorkStage::ReasonInFlight)
        .unwrap();
    assert!(work.reason_request.is_some());
    assert!(work.request_id.is_some());
    assert!(work.response.is_none());
    assert!(snapshot.state.invocations.is_empty());
    assert_eq!(
        snapshot.state.obligations[&fixture.delivery_id].status,
        ObligationStatus::InProgress
    );
    assert!(!snapshot
        .state
        .works
        .values()
        .any(|work| work.stage == WorkStage::ActReady));
    assert!(matches!(
        recover_work(&snapshot, &work.work_id).unwrap().stage,
        RecoveredStage::ReasonUncertain { .. }
    ));
    assert!(fixture.context.work.state.lock().await.request_id.is_some());
    assert!(!fixture
        .context
        .session
        .transcript
        .read()
        .entries()
        .iter()
        .any(|entry| { matches!(entry.as_message(), Some(BaseMessage::Tool { .. })) }));
}

#[tokio::test]
async fn mixed_resolved_and_unknown_calls_do_not_publish_partial_dispatch_intents() {
    let effects = Arc::new(AtomicUsize::new(0));
    let tool = Arc::new(KnownTool(effects.clone()));
    let (model, _listener) = native_model().await;
    let fixture = fixture_with_input(Arc::new(model), vec![tool.clone()], "mixed calls").await;
    super::super::receive::run_receive(ReceiveInput {
        context: fixture.context.clone(),
    })
    .await
    .unwrap();
    prepare(
        &fixture.context,
        &[BaseMessage::human("mixed calls")],
        &[tool.as_ref()],
    )
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
    let before = session.snapshot().await.unwrap();
    let mut reasoning = Reasoning::with_tools(
        "mixed calls",
        vec![
            ToolCall::new("known-call", "known", serde_json::json!({})),
            ToolCall::new("missing-call", "missing", serde_json::json!({})),
        ],
    );
    let error = commit_response(
        &fixture.context,
        &mut reasoning,
        &fixture.context.runtime.tool_catalog.snapshot(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(error.downcast_ref::<AgentError>(), Some(AgentError::ToolNotFound(name)) if name == "missing")
    );
    let after = session.snapshot().await.unwrap();
    assert_eq!(after.state, before.state);
    assert!(after.state.invocations.is_empty());
    assert!(fixture
        .context
        .work
        .state
        .lock()
        .await
        .invocations
        .is_empty());
    assert_eq!(effects.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn uncommitted_model_request_remains_blocked_for_reconciliation() {
    let (model, _listener) = native_model().await;
    let fixture =
        fixture_with_input(Arc::new(model), Vec::new(), "no confirmed model response").await;
    super::super::receive::run_receive(ReceiveInput {
        context: fixture.context.clone(),
    })
    .await
    .unwrap();
    prepare(
        &fixture.context,
        &[BaseMessage::human("no confirmed response")],
        &[],
    )
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
    let before = session.snapshot().await.unwrap();
    let request_id = fixture
        .context
        .work
        .state
        .lock()
        .await
        .request_id
        .clone()
        .unwrap();
    block_uncertain_model(&fixture.context).await.unwrap();
    let after = session.snapshot().await.unwrap();
    let work = after
        .state
        .works
        .values()
        .find(|work| work.stage == WorkStage::Blocked)
        .unwrap();
    assert!(work.response.is_none());
    assert_eq!(
        work.reason_request,
        before.state.works[&work.work_id].reason_request
    );
    assert_eq!(work.request_id.as_deref(), Some(request_id.as_str()));
    assert!(work
        .recovery_condition
        .as_ref()
        .unwrap()
        .contains(&request_id));
    assert!(fixture.context.work.state.lock().await.frozen);
    assert!(prepare(
        &fixture.context,
        &[BaseMessage::human("must not resend")],
        &[]
    )
    .await
    .is_err());
}

#[tokio::test]
async fn completed_unknown_tool_response_cannot_commit_without_a_dispatch_intent() {
    let (model, _listener) = native_model().await;
    let fixture = fixture_with_input(Arc::new(model), Vec::new(), "unknown tool contract").await;
    super::super::receive::run_receive(ReceiveInput {
        context: fixture.context.clone(),
    })
    .await
    .unwrap();
    prepare(
        &fixture.context,
        &[BaseMessage::human("unknown tool contract")],
        &[],
    )
    .await
    .unwrap();
    let state = fixture.context.work.state.lock().await;
    let session = state.session.clone().unwrap();
    let before = session.snapshot().await.unwrap();
    let command = session.command(WorkAction::CommitReasonResponseAndDispatchIntent {
        guard: session.guard(&before).unwrap(),
        target: WorkSession::target(&before, state.work_id.as_deref().unwrap()).unwrap(),
        request_id: state.request_id.clone().unwrap(),
        response: WorkPayload::from_payload(&PersistedPayload::Message(
            BaseMessage::ai_with_tool_calls(
                "unknown tool",
                vec![ToolCallRequest::new(
                    "missing-call",
                    "missing",
                    serde_json::json!({}),
                )],
            ),
        ))
        .unwrap(),
        dispatch_intents: Vec::new(),
        next_work_id: Some(uuid::Uuid::now_v7().to_string()),
    });
    drop(state);
    let receipt = fixture
        .bound
        .resources()
        .apply_work_mutation(&command)
        .await
        .unwrap();
    assert_eq!(
        receipt.decision,
        WorkDecision::Rejected {
            reason: WorkRejection::Conflict
        }
    );
    let after = session.snapshot().await.unwrap();
    assert_eq!(after.state, before.state);
}
