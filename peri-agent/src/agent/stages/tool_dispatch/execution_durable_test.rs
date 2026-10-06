use super::*;
use crate::agent::react::Reasoning;
use crate::agent::stages::work_test_support::{
    fixture_with_input, native_model, read_http_body, ProductionFixture,
};
use peri_acp_types::session_resources::work::{InvocationStatus, WorkStage};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::AsyncWriteExt;

struct ResumeProbe {
    error_code: Option<EffectiveToolErrorCode>,
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl BaseTool for ResumeProbe {
    fn name(&self) -> &str {
        "resume"
    }

    fn description(&self) -> &str {
        "resume outcome probe"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    async fn invoke(
        &self,
        _: serde_json::Value,
        _: crate::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(code) = self.error_code {
            Err(Box::new(EffectiveToolError::new(
                code,
                "resume preparation rejected",
            )))
        } else {
            Err(std::io::Error::other("resume preparation rejected").into())
        }
    }
}

async fn prepare_probe(
    tool: Arc<ResumeProbe>,
) -> (ProductionFixture, Reasoning, tokio::net::TcpListener) {
    let (model, listener) = native_model().await;
    let fixture = fixture_with_input(Arc::new(model), vec![tool.clone()], "resume probe").await;
    crate::agent::stages::receive::run_receive(crate::agent::stages::ReceiveInput {
        context: fixture.context.clone(),
    })
    .await
    .unwrap();
    crate::agent::stages::work_reason::prepare(
        &fixture.context,
        &[BaseMessage::human("resume probe")],
        &[tool.as_ref()],
    )
    .await
    .unwrap();
    let mut reasoning = Reasoning::with_tools(
        "resume probe",
        vec![ToolCall::new(
            "resume-call",
            "resume",
            serde_json::json!({}),
        )],
    );
    let catalog = fixture.context.runtime.tool_catalog.snapshot();
    crate::agent::stages::work_reason::commit_response(&fixture.context, &mut reasoning, &catalog)
        .await
        .unwrap();
    (fixture, reasoning, listener)
}

#[tokio::test]
async fn known_resume_preparation_rejection_allows_parent_successor() {
    assert_known_rejection_allows_parent_successor(EffectiveToolErrorCode::ApplicationFailed).await;
}

#[tokio::test]
async fn known_resume_permission_denial_allows_parent_successor() {
    assert_known_rejection_allows_parent_successor(EffectiveToolErrorCode::PermissionDenied).await;
}

async fn assert_known_rejection_allows_parent_successor(code: EffectiveToolErrorCode) {
    let calls = Arc::new(AtomicUsize::new(0));
    let (fixture, reasoning, listener) = prepare_probe(Arc::new(ResumeProbe {
        error_code: Some(code),
        calls: calls.clone(),
    }))
    .await;
    let catalog = fixture.context.runtime.tool_catalog.snapshot();
    super::super::dispatch_tools(
        &fixture.context,
        &reasoning,
        &catalog,
        &CancellationToken::new(),
    )
    .await
    .expect("known rejection must settle and let parent continue");
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
    let invocation = snapshot.state.invocations.values().next().unwrap();
    assert_eq!(invocation.status, InvocationStatus::Settled);
    assert!(matches!(
        &invocation.outcome,
        Some(peri_acp_types::session_resources::work::InvocationOutcome::Failed { .. })
    ));
    assert!(snapshot
        .state
        .works
        .values()
        .any(|work| work.stage == WorkStage::ReasonReady));
    assert!(!snapshot
        .state
        .works
        .values()
        .any(|work| work.stage == WorkStage::Blocked));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let body = read_http_body(&mut socket).await;
        let response = concat!(
            "data: {\"id\":\"continued-parent\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"resume denied; parent continued\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"continued-parent\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let headers = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            response.len(),
        );
        socket.write_all(headers.as_bytes()).await.unwrap();
        socket.write_all(response.as_bytes()).await.unwrap();
        body
    });
    let output = crate::agent::stages::reason::run_reason(crate::agent::stages::ReasonInput {
        context: fixture.context.clone(),
        has_tool_calls: true,
    })
    .await
    .expect("parent must be able to reason after the known rejection");
    let body = server.await.unwrap();
    assert!(body["messages"].as_array().unwrap().iter().any(|message| {
        message["role"] == "tool"
            && message["content"]
                .as_str()
                .is_some_and(|content| content.contains("resume preparation rejected"))
    }));
    assert!(!output.reasoning.needs_tool_call());
    assert_eq!(
        output.reasoning.final_answer.as_deref(),
        Some("resume denied; parent continued")
    );
    crate::agent::stages::act::run_act(crate::agent::stages::ActInput {
        context: fixture.context.clone(),
        reasoning: output.reasoning,
        catalog: output.catalog,
    })
    .await
    .expect("parent final answer must commit after rejection");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn unknown_mcp_result_keeps_parent_blocked_and_cannot_replay() {
    assert_uncertain_result_keeps_parent_blocked(None).await;
}

#[tokio::test]
async fn timed_out_result_keeps_parent_blocked_and_cannot_replay() {
    assert_uncertain_result_keeps_parent_blocked(Some(EffectiveToolErrorCode::Timeout)).await;
}

#[tokio::test]
async fn cancelled_result_keeps_parent_blocked_and_cannot_replay() {
    assert_uncertain_result_keeps_parent_blocked(Some(EffectiveToolErrorCode::Cancelled)).await;
}

async fn assert_uncertain_result_keeps_parent_blocked(code: Option<EffectiveToolErrorCode>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let (fixture, reasoning, _listener) = prepare_probe(Arc::new(ResumeProbe {
        error_code: code,
        calls: calls.clone(),
    }))
    .await;
    let catalog = fixture.context.runtime.tool_catalog.snapshot();
    let targets: HashMap<String, Arc<dyn BaseTool>> = HashMap::from([(
        "resume-call".into(),
        Arc::new(ResumeProbe {
            error_code: code,
            calls: calls.clone(),
        }) as Arc<dyn BaseTool>,
    )]);
    let results = dispatch_concurrent(
        &fixture.context,
        &reasoning.tool_calls,
        &HashMap::new(),
        &targets,
        &catalog,
        &CancellationToken::new(),
        &BaseMessage::ai("resume probe"),
    )
    .await;
    assert_eq!(
        results[0].as_ref().unwrap_err().code,
        code.unwrap_or(EffectiveToolErrorCode::ToolFailed)
    );
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
    let invocation = snapshot.state.invocations.values().next().unwrap();
    assert_eq!(invocation.status, InvocationStatus::OutcomeUnknown);
    assert_eq!(
        snapshot.state.works[invocation.work_id.as_ref().unwrap()].stage,
        WorkStage::Blocked
    );
    assert!(snapshot
        .state
        .works
        .values()
        .any(|work| work.stage == WorkStage::Blocked));
    assert!(!snapshot
        .state
        .works
        .values()
        .any(|work| work.stage == WorkStage::ReasonReady));
    assert!(
        crate::agent::stages::work_dispatch::begin(&fixture.context, &reasoning.tool_calls[0])
            .await
            .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
