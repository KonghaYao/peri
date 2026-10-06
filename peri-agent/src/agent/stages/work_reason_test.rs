use super::*;
use peri_acp_types::session::ExecutionFailure;

#[path = "work_budget_test_support.rs"]
mod budget_support;

fn budget_state(kind: WorkBudgetKind, used: u64) -> WorkState {
    let mut state = WorkState::default();
    state.limits.reason_requests = 64;
    state.limits.dispatches = 256;
    state.limits.recoveries = 8;
    let reason = match kind {
        WorkBudgetKind::ReasonRequests => "reason budget exhausted",
        WorkBudgetKind::Dispatches => "dispatch budget exhausted",
        WorkBudgetKind::Recoveries => "recovery budget exhausted",
    };
    let mut budget = WorkBudget::default();
    match kind {
        WorkBudgetKind::ReasonRequests => budget.reason_requests = used,
        WorkBudgetKind::Dispatches => budget.dispatches = used,
        WorkBudgetKind::Recoveries => budget.recoveries = used,
    }
    state.budgets.insert("batch".into(), budget);
    state.works.insert(
        "work".into(),
        WorkRecord {
            work_id: "work".into(),
            revision: 489,
            budget_id: "batch".into(),
            batch_id: "batch".into(),
            stage: WorkStage::Blocked,
            resume_stage: Some(WorkStage::ReasonReady),
            request_id: None,
            reason_request: None,
            response: None,
            invocation_ids: Vec::new(),
            reason: Some(reason.into()),
            recovery_condition: Some("explicit budget reset authorization".into()),
        },
    );
    state
}

#[test]
fn reason_budget_block_is_typed_and_preserves_durable_state() {
    let state = budget_state(WorkBudgetKind::ReasonRequests, 64);
    let before = state.clone();
    let error = blocked_budget_error(&state, "work").unwrap();
    assert!(matches!(
        error,
        AgentError::WorkBudgetExhausted {
            budget: WorkBudgetKind::ReasonRequests,
            used: 64,
            limit: 64,
        }
    ));
    assert_eq!(state, before);
    let failure = ExecutionFailure::from_agent_error(&error);
    assert!(failure.public_message.contains("reason requests (64/64)"));
    assert!(failure
        .public_message
        .contains("explicit budget reset authorization"));
}

#[test]
fn dispatch_budget_block_is_typed() {
    let state = budget_state(WorkBudgetKind::Dispatches, 256);
    assert!(matches!(
        blocked_budget_error(&state, "work"),
        Some(AgentError::WorkBudgetExhausted {
            budget: WorkBudgetKind::Dispatches,
            used: 256,
            limit: 256,
        })
    ));
}

#[test]
fn recovery_budget_block_is_typed() {
    let state = budget_state(WorkBudgetKind::Recoveries, 8);
    assert!(matches!(
        blocked_budget_error(&state, "work"),
        Some(AgentError::WorkBudgetExhausted {
            budget: WorkBudgetKind::Recoveries,
            used: 8,
            limit: 8,
        })
    ));
}

#[test]
fn unrelated_block_does_not_become_budget_error() {
    let mut state = budget_state(WorkBudgetKind::ReasonRequests, 64);
    state.works.get_mut("work").unwrap().reason = Some("model request uncertain".into());
    assert!(blocked_budget_error(&state, "work").is_none());
}

#[test]
fn unexhausted_budget_does_not_become_budget_error() {
    let state = budget_state(WorkBudgetKind::ReasonRequests, 63);
    assert!(blocked_budget_error(&state, "work").is_none());
}

#[test]
fn unblocked_work_does_not_become_budget_error() {
    let mut state = budget_state(WorkBudgetKind::ReasonRequests, 64);
    state.works.get_mut("work").unwrap().stage = WorkStage::ReasonReady;
    assert!(blocked_budget_error(&state, "work").is_none());
}

#[tokio::test]
async fn reason_budget_reducer_blocks_before_request_and_maps_exact_limit() {
    let fixture =
        super::super::work_test_support::fixture(Arc::new(super::super::NullReactLLM), Vec::new())
            .await;
    super::super::receive::run_receive(super::super::ReceiveInput {
        context: fixture.context.clone(),
    })
    .await
    .unwrap();
    let session = fixture
        .context
        .work
        .ensure(&fixture.context)
        .await
        .unwrap()
        .unwrap();
    let snapshot = session.snapshot().await.unwrap();
    let work_id = fixture
        .context
        .work
        .state
        .lock()
        .await
        .work_id
        .clone()
        .unwrap();
    let target = WorkSession::target(&snapshot, &work_id).unwrap();
    let request = request_checkpoint(
        &serde_json::json!({"messages": []}),
        "model".into(),
        "auth".into(),
    )
    .unwrap();
    let command = session.command(WorkAction::BeginReason {
        guard: session.guard(&snapshot).unwrap(),
        target,
        request_id: "must-not-send".into(),
        request,
    });
    let mut state = snapshot.state.clone();
    state.limits.reason_requests = 64;
    let budget_id = state.works[&work_id].budget_id.clone();
    state.budgets.get_mut(&budget_id).unwrap().reason_requests = 64;
    let reduction = reduce_work(&command, &snapshot.control, &state).unwrap();
    assert_eq!(reduction.receipt.stage, Some(WorkStage::Blocked));
    assert!(reduction.state.works[&work_id].request_id.is_none());
    assert_eq!(reduction.state.budgets[&budget_id].reason_requests, 64);
    let error = budget_exhaustion(&state, &work_id, WorkBudgetKind::ReasonRequests).unwrap();
    let converted = AgentError::from(anyhow::Error::new(error));
    assert!(matches!(
        converted,
        AgentError::WorkBudgetExhausted {
            budget: WorkBudgetKind::ReasonRequests,
            used: 64,
            limit: 64,
        }
    ));
}

struct NeverInvokedTool;

#[async_trait::async_trait]
impl BaseTool for NeverInvokedTool {
    fn name(&self) -> &str {
        "budget_probe"
    }
    fn description(&self) -> &str {
        "budget test probe"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    async fn invoke(
        &self,
        _: serde_json::Value,
        _: crate::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        panic!("budget regression must not invoke external effects")
    }
}

#[tokio::test]
async fn dispatch_budget_denial_survives_frozen_boundary_and_result_commit_without_effect() {
    let model = peri_model::OpenAiModel::new(peri_model::OpenAiConfig::new(
        "http://127.0.0.1:1".parse().unwrap(),
        "unused-key",
        "offline-model",
    ));
    let fixture = super::super::work_test_support::fixture_with_input(
        Arc::new(crate::agent::model_bridge::AgentModelBridge::new(Arc::new(
            model,
        ))),
        vec![Arc::new(NeverInvokedTool)],
        "dispatch budget probe",
    )
    .await;
    super::super::receive::run_receive(super::super::ReceiveInput {
        context: fixture.context.clone(),
    })
    .await
    .unwrap();
    budget_support::install(&fixture.context, 2, 3).await;
    let tool = NeverInvokedTool;
    prepare(
        &fixture.context,
        &[BaseMessage::human("dispatch probe")],
        &[&tool],
    )
    .await
    .unwrap();
    let calls = (0..4)
        .map(|sequence| {
            ToolCall::new(
                format!("dispatch-{sequence}"),
                "budget_probe",
                serde_json::json!({}),
            )
        })
        .collect::<Vec<_>>();
    let mut reasoning = Reasoning::with_tools("dispatch probe", calls.clone());
    commit_response(
        &fixture.context,
        &mut reasoning,
        &fixture.context.runtime.tool_catalog.snapshot(),
    )
    .await
    .unwrap();
    for call in &calls[..3] {
        assert!(super::super::work_dispatch::begin(&fixture.context, call)
            .await
            .unwrap()
            .is_some());
    }
    let error = AgentError::from(
        super::super::work_dispatch::begin(&fixture.context, &calls[3])
            .await
            .err()
            .unwrap(),
    );
    assert!(matches!(
        error,
        AgentError::WorkBudgetExhausted {
            budget: WorkBudgetKind::Dispatches,
            used: 3,
            limit: 3,
        }
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
    let before = session.snapshot().await.unwrap();
    let error = AgentError::from(
        fixture
            .context
            .work
            .ensure(&fixture.context)
            .await
            .err()
            .unwrap(),
    );
    assert!(matches!(
        error,
        AgentError::WorkBudgetExhausted {
            budget: WorkBudgetKind::Dispatches,
            ..
        }
    ));
    let error = AgentError::from(
        super::super::work_dispatch::commit_results(&fixture.context, &[])
            .await
            .err()
            .unwrap(),
    );
    assert!(matches!(
        error,
        AgentError::WorkBudgetExhausted {
            budget: WorkBudgetKind::Dispatches,
            ..
        }
    ));
    let after = session.snapshot().await.unwrap();
    assert_eq!(after.state, before.state);
    assert_eq!(after.control, before.control);
    assert_eq!(
        after
            .state
            .invocations
            .values()
            .filter(|record| record.status == InvocationStatus::DispatchAccepted)
            .count(),
        3
    );
    assert_eq!(
        after
            .state
            .invocations
            .values()
            .filter(|record| record.status == InvocationStatus::Prepared)
            .count(),
        1
    );
}

#[tokio::test]
async fn reason_budget_denial_survives_prepare_and_cold_boundary_without_reset() {
    let model = peri_model::OpenAiModel::new(peri_model::OpenAiConfig::new(
        "http://127.0.0.1:1".parse().unwrap(),
        "unused-key",
        "offline-model",
    ));
    let fixture = super::super::work_test_support::fixture_with_input(
        Arc::new(crate::agent::model_bridge::AgentModelBridge::new(Arc::new(
            model,
        ))),
        vec![Arc::new(NeverInvokedTool)],
        "reason budget probe",
    )
    .await;
    super::super::receive::run_receive(super::super::ReceiveInput {
        context: fixture.context.clone(),
    })
    .await
    .unwrap();
    budget_support::install(&fixture.context, 2, 3).await;
    let messages = vec![BaseMessage::human("budget probe")];
    let tool = NeverInvokedTool;
    for sequence in 0..2 {
        assert!(prepare(&fixture.context, &messages, &[&tool])
            .await
            .unwrap()
            .is_some());
        let call = ToolCall::new(
            format!("probe-{sequence}"),
            "budget_probe",
            serde_json::json!({}),
        );
        let mut reasoning = Reasoning::with_tools("probe", vec![call.clone()]);
        commit_response(
            &fixture.context,
            &mut reasoning,
            &fixture.context.runtime.tool_catalog.snapshot(),
        )
        .await
        .unwrap();
        let mut result = crate::agent::react::ToolResult::error(
            &call.id,
            &call.name,
            "user rejected before effect",
        );
        result.effective_error_code = Some(crate::tools::EffectiveToolErrorCode::UserRejected);
        super::super::work_dispatch::commit_results(&fixture.context, &[(call, result)])
            .await
            .unwrap();
    }
    let error = AgentError::from(
        prepare(&fixture.context, &messages, &[&tool])
            .await
            .err()
            .unwrap(),
    );
    assert!(matches!(
        error,
        AgentError::WorkBudgetExhausted {
            budget: WorkBudgetKind::ReasonRequests,
            used: 2,
            limit: 2,
        }
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
    let before = session.snapshot().await.unwrap();
    let mut cold = fixture.context.clone();
    cold.work = Arc::new(WorkBoundary::default());
    let error = match cold.work.ensure(&cold).await {
        Err(error) => AgentError::from(error),
        Ok(_) => panic!("blocked budget must deny a fresh boundary"),
    };
    assert!(matches!(
        error,
        AgentError::WorkBudgetExhausted {
            budget: WorkBudgetKind::ReasonRequests,
            used: 2,
            limit: 2,
        }
    ));
    let error = super::super::receive::run_receive(super::super::ReceiveInput { context: cold })
        .await
        .err()
        .unwrap();
    assert!(matches!(error, AgentError::WorkBudgetExhausted { .. }));
    let after = session.snapshot().await.unwrap();
    assert_eq!(after.state, before.state);
    assert_eq!(after.control, before.control);
    let mut old_lifecycle = before;
    old_lifecycle.control.lifecycle += 1;
    assert!(snapshot_budget_error(&old_lifecycle).is_none());
}
