use super::super::work_test_support::fixture_with_input;
use super::*;
use crate::agent::react::Reasoning;
use crate::tools::{BaseTool, ToolContext};
use std::sync::atomic::{AtomicUsize, Ordering};

#[path = "work_budget_test_support.rs"]
mod budget_support;

struct BudgetProbe(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl BaseTool for BudgetProbe {
    fn name(&self) -> &str {
        "budget_probe"
    }
    fn description(&self) -> &str {
        "bounded dispatch probe"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type":"object", "properties":{}})
    }
    async fn invoke(
        &self,
        _: serde_json::Value,
        _: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok("accepted effect outcome".into())
    }
}

#[tokio::test]
async fn mid_batch_budget_denial_persists_accepted_outcome_before_returning_budget_error() {
    let effects = Arc::new(AtomicUsize::new(0));
    let tool = Arc::new(BudgetProbe(Arc::clone(&effects)));
    let model = peri_model::OpenAiModel::new(peri_model::OpenAiConfig::new(
        "http://127.0.0.1:1".parse().unwrap(),
        "unused-key",
        "offline-model",
    ));
    let fixture = fixture_with_input(
        Arc::new(crate::agent::model_bridge::AgentModelBridge::new(Arc::new(
            model,
        ))),
        vec![tool.clone()],
        "budget settlement probe",
    )
    .await;
    super::super::receive::run_receive(super::super::ReceiveInput {
        context: fixture.context.clone(),
    })
    .await
    .unwrap();
    budget_support::install(&fixture.context, 2, 1).await;
    super::super::work_reason::prepare(
        &fixture.context,
        &[BaseMessage::human("budget probe")],
        &[tool.as_ref()],
    )
    .await
    .unwrap();
    let calls = (0..3)
        .map(|sequence| {
            ToolCall::new(
                format!("call-{sequence}"),
                "budget_probe",
                serde_json::json!({}),
            )
        })
        .collect();
    let mut reasoning = Reasoning::with_tools("budget probe", calls);
    let catalog = fixture.context.runtime.tool_catalog.snapshot();
    super::super::work_reason::commit_response(&fixture.context, &mut reasoning, &catalog)
        .await
        .unwrap();
    let error = super::super::tool_dispatch::dispatch_tools(
        &fixture.context,
        &reasoning,
        &catalog,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .err()
    .unwrap();
    assert!(matches!(
        error,
        crate::error::AgentError::WorkBudgetExhausted {
            budget: peri_acp_types::error::WorkBudgetKind::Dispatches,
            used: 1,
            limit: 1,
        }
    ));
    assert_eq!(effects.load(Ordering::SeqCst), 1);
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
        .processing(&session.admission.work_id)
        .await
        .unwrap();
    let records = session.effects(&processing).await.unwrap();
    assert_eq!(
        records
            .iter()
            .filter(|invocation| matches!(
                invocation.outcome,
                Some(InvocationOutcome::Completed { .. })
            ))
            .count(),
        1
    );
    assert_eq!(
        records.iter()
            .filter(|invocation| matches!(&invocation.outcome, Some(InvocationOutcome::Cancelled { evidence, .. }) if evidence.starts_with("before-effect:processing-stopped-before-dispatch:")))
            .count(),
        2
    );
    assert_eq!(processing.stage, WorkStage::Blocked);
    assert_ne!(processing.stage, WorkStage::ReasonReady);
    assert!(fixture
        .context
        .session
        .transcript
        .read()
        .entries()
        .iter()
        .any(|entry| matches!(entry.as_message(), Some(BaseMessage::Tool { .. }))));
    assert!(begin(&fixture.context, &reasoning.tool_calls[1])
        .await
        .is_err());
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let transcript = fixture.context.session.transcript.read();
    for call in &reasoning.tool_calls {
        assert!(transcript.entries().iter().any(|entry| {
            matches!(entry.as_message(), Some(BaseMessage::Tool { tool_call_id, .. }) if tool_call_id == &call.id)
        }));
    }
}

#[tokio::test]
async fn parallel_effects_settle_once_and_advance_one_processing_cursor() {
    let tool = Arc::new(BudgetProbe(Arc::new(AtomicUsize::new(0))));
    let model = peri_model::OpenAiModel::new(peri_model::OpenAiConfig::new(
        "http://127.0.0.1:1".parse().unwrap(),
        "unused-key",
        "offline-model",
    ));
    let fixture = fixture_with_input(
        Arc::new(crate::agent::model_bridge::AgentModelBridge::new(Arc::new(
            model,
        ))),
        vec![tool.clone()],
        "independent effect probe",
    )
    .await;
    super::super::receive::run_receive(super::super::ReceiveInput {
        context: fixture.context.clone(),
    })
    .await
    .unwrap();
    super::super::work_reason::prepare(
        &fixture.context,
        &[BaseMessage::human("probe")],
        &[tool.as_ref()],
    )
    .await
    .unwrap();
    let calls = vec![
        ToolCall::new("first", "budget_probe", serde_json::json!({})),
        ToolCall::new("second", "budget_probe", serde_json::json!({})),
    ];
    let mut reasoning = Reasoning::with_tools("probe", calls.clone());
    super::super::work_reason::commit_response(
        &fixture.context,
        &mut reasoning,
        &fixture.context.runtime.tool_catalog.snapshot(),
    )
    .await
    .unwrap();
    for call in &calls {
        begin(&fixture.context, call).await.unwrap();
    }
    let first =
        crate::agent::react::ToolResult::success(&calls[0].id, &calls[0].name, "first outcome");
    commit_results(&fixture.context, &[(calls[0].clone(), first.clone())])
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
    let partial = session
        .processing(&fixture.admission.work_id)
        .await
        .unwrap();
    assert_eq!(partial.stage, WorkStage::ActReady);
    assert_eq!(partial.remaining_effects, 1);
    commit_results(&fixture.context, &[(calls[0].clone(), first)])
        .await
        .unwrap();
    assert_eq!(
        session
            .processing(&fixture.admission.work_id)
            .await
            .unwrap(),
        partial
    );
    let second =
        crate::agent::react::ToolResult::success(&calls[1].id, &calls[1].name, "second outcome");
    commit_results(&fixture.context, &[(calls[1].clone(), second)])
        .await
        .unwrap();
    let settled = session
        .processing(&fixture.admission.work_id)
        .await
        .unwrap();
    assert_eq!(settled.processing_id, partial.processing_id);
    assert_eq!(settled.stage, WorkStage::ReasonReady);
    assert_eq!(settled.remaining_effects, 0);
    assert_eq!(settled.phase_sequence, partial.phase_sequence + 1);
    assert_eq!(settled.budget.dispatches, 2);
}
