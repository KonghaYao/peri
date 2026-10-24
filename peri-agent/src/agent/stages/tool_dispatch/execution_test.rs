use super::*;
use crate::agent::stages::{
    ExecutionEntryOrigin, InvocationContext, SessionInvocationSource, StageContext,
};
use crate::messages::MessageContent;
use crate::session::queue::MessageQueue;
use crate::session::transcript::MessageTranscript;
use crate::session::turn::TurnContext;
use crate::tools::InvocationOrigin;
use peri_acp_types::error::ToolRejectionCause;
use peri_acp_types::interaction::UnansweredCause;

#[test]
fn completed_application_failure_retains_typed_error_without_unknown_classification() {
    let received = EffectiveToolError::new(
        EffectiveToolErrorCode::ApplicationFailed,
        "FileNotFound: missing-file.txt",
    );
    let effective = effective_tool_error_from_boxed(Box::new(received));
    assert_eq!(effective.code, EffectiveToolErrorCode::ApplicationFailed);
    assert_eq!(effective.message, "FileNotFound: missing-file.txt");
    assert!(execution_for_effective_error(effective.code).is_none());
}

#[test]
fn unclassified_tool_errors_retain_failure_classification() {
    for message in ["FileNotFound", "-32603", "transport disconnected"] {
        let effective = effective_tool_error_from_boxed(std::io::Error::other(message).into());
        assert_eq!(effective.code, EffectiveToolErrorCode::ToolFailed);
    }
}

#[test]
fn boxed_known_agent_rejections_preserve_failure_classification() {
    for (error, expected) in [
        (
            AgentError::ToolNotFound("resume".into()),
            EffectiveToolErrorCode::UnknownTool,
        ),
        (
            AgentError::ToolRejected {
                tool: "resume".into(),
                reason: "denied".into(),
                // 用户/客户端已作出拒绝决定（本用例只校验错误分类）。
                cause: peri_acp_types::error::ToolRejectionCause::UserRejected,
            },
            EffectiveToolErrorCode::UserRejected,
        ),
    ] {
        let effective = effective_tool_error_from_boxed(Box::new(error));
        assert_eq!(effective.code, expected);
    }
}

/// 用户**已作出**的拒绝决定 ⇒ `USER_REJECTED`（分类不变）。
#[test]
fn user_rejection_keeps_user_rejected_classification() {
    let effective = effective_tool_error(AgentError::ToolRejected {
        tool: "Write".into(),
        reason: "用户拒绝".into(),
        cause: ToolRejectionCause::UserRejected,
    });
    assert_eq!(effective.code, EffectiveToolErrorCode::UserRejected);
    assert_eq!(
        effective.unanswered_cause(),
        None,
        "用户拒绝不得携带「无人可答」原因"
    );
}

/// 无人可答 ⇒ `PERMISSION_DENIED` + typed 原因随错误过边界（裁决 R9）。
#[test]
fn approval_unavailable_is_permission_denied_with_typed_cause() {
    let effective = effective_tool_error(AgentError::ToolRejected {
        tool: "Write".into(),
        reason: UnansweredCause::NoOnlineEndpoint.reason_text().to_string(),
        cause: ToolRejectionCause::ApprovalUnavailable(UnansweredCause::NoOnlineEndpoint),
    });
    assert_eq!(effective.code, EffectiveToolErrorCode::PermissionDenied);
    assert_ne!(effective.code, EffectiveToolErrorCode::UserRejected);
    assert_eq!(
        effective.unanswered_cause(),
        Some(UnansweredCause::NoOnlineEndpoint),
        "「没有人可以决定」必须以 typed 原因随错误过边界"
    );
}

/// 本端策略拒绝（hook / 分类器 / 审批机制缺失）既不是用户拒绝，也不是无人可答。
#[test]
fn policy_denial_is_permission_denied_without_unanswered_cause() {
    let effective = effective_tool_error(AgentError::ToolRejected {
        tool: "Write".into(),
        reason: "Hook blocked".into(),
        cause: ToolRejectionCause::PolicyDenied,
    });
    assert_eq!(effective.code, EffectiveToolErrorCode::PermissionDenied);
    assert_ne!(effective.code, EffectiveToolErrorCode::UserRejected);
    assert_eq!(effective.unanswered_cause(), None);
}

/// 三种原因经**同一张**分类表（模型面结算与协议错误面共用权威）。
#[test]
fn rejection_codes_have_a_single_authority() {
    for (cause, expected) in [
        (
            ToolRejectionCause::UserRejected,
            EffectiveToolErrorCode::UserRejected,
        ),
        (
            ToolRejectionCause::ApprovalUnavailable(UnansweredCause::OwnerUnmatched),
            EffectiveToolErrorCode::PermissionDenied,
        ),
        (
            ToolRejectionCause::PolicyDenied,
            EffectiveToolErrorCode::PermissionDenied,
        ),
    ] {
        assert_eq!(rejection_error_code(cause), expected);
        assert_eq!(
            effective_error_code(&AgentError::ToolRejected {
                tool: "Write".into(),
                reason: "x".into(),
                cause,
            }),
            expected
        );
    }
}

#[test]
fn boxed_interruption_retains_cancelled_classification() {
    let effective = effective_tool_error_from_boxed(Box::new(AgentError::Interrupted));
    assert_eq!(effective.code, EffectiveToolErrorCode::Cancelled);
}

#[test]
fn boxed_unclassified_agent_error_retains_tool_failed_classification() {
    let effective = effective_tool_error_from_boxed(Box::new(AgentError::Other(anyhow::anyhow!(
        "resume preparation rejected"
    ))));
    assert_eq!(effective.code, EffectiveToolErrorCode::ToolFailed);
}

struct OutputTool {
    name: String,
    output: String,
}

struct FailingSubagentTool;

#[async_trait::async_trait]
impl BaseTool for FailingSubagentTool {
    fn name(&self) -> &str {
        "SubAgent"
    }
    fn description(&self) -> &str {
        "test child failure"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: crate::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Err(Box::new(crate::session::subagent::SubagentFailure::new(
            "child-123",
            "worker",
            crate::error::AgentError::ModelError(peri_model::ModelError::http_status(
                500,
                "provider.example",
                Some("req-123"),
            )),
        )))
    }
}

#[async_trait::async_trait]
impl BaseTool for OutputTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "test output"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: crate::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.output.clone())
    }
}

fn make_test_ctx() -> StageContext {
    let turn = TurnContext::new(
        std::sync::Arc::from("/tmp"),
        std::sync::Arc::new(CancellationToken::new()),
    );
    let transcript = std::sync::Arc::new(parking_lot::RwLock::new(MessageTranscript::new()));
    let queue = MessageQueue::new();
    StageContext::new(turn, transcript, queue)
}

/// 调用期上下文：模型批次入口（Act 侧投影的真实形状）。
fn make_invocation(ctx: &StageContext) -> InvocationContext {
    InvocationContext::from_stage(
        ctx,
        ExecutionEntryOrigin::ModelBatch,
        ctx.session.turn.cancel_token.as_ref().clone(),
    )
}

#[tokio::test]
async fn same_name_calls_have_distinct_invocations_and_original_model_ids() {
    type Identities = Arc<parking_lot::Mutex<Vec<(String, InvocationId, ToolCallId)>>>;
    struct IdentityProbe(Identities);
    #[async_trait::async_trait]
    impl BaseTool for IdentityProbe {
        fn name(&self) -> &str {
            "Probe"
        }
        fn description(&self) -> &str {
            "identity probe"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn invoke(
            &self,
            _: serde_json::Value,
            context: crate::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            self.0.lock().push((
                context.session_id.unwrap(),
                context.invocation_id.unwrap(),
                context.tool_call_id.unwrap(),
            ));
            Ok("ok".into())
        }
    }
    let context = make_test_ctx();
    context
        .session
        .session_context
        .write()
        .insert("session_id".into(), "current-session".into());
    let captured: Identities = Arc::default();
    let tool: Arc<dyn BaseTool> = Arc::new(IdentityProbe(captured.clone()));
    let calls = vec![
        ToolCall::new("model-call-1", "Probe", serde_json::json!({})),
        ToolCall::new("model-call-2", "Probe", serde_json::json!({})),
    ];
    let targets = HashMap::from([
        ("model-call-1".into(), tool.clone()),
        ("model-call-2".into(), tool),
    ]);
    let invocation = make_invocation(&context);
    let turn = invocation
        .require_turn("test identity")
        .expect("active turn");
    let output = dispatch_concurrent(
        &invocation,
        Some(turn),
        &calls,
        &HashMap::new(),
        &targets,
        &context.runtime.tool_catalog.snapshot(),
        &CancellationToken::new(),
        &BaseMessage::ai("call"),
        None,
    )
    .await;
    assert!(output.iter().all(Result::is_ok));
    let identities = captured.lock();
    assert_eq!(identities.len(), 2);
    // 契约基线：同一批同名调用的 canonical 执行身份互异，
    // 且模型 tool-call ID 原样保留，两个身份不得互相替代。
    assert_ne!(identities[0].1, identities[1].1);
    for (session, invocation, model_call) in identities.iter() {
        assert_eq!(session, "current-session");
        assert_ne!(invocation.as_str(), model_call.as_str());
        assert!(uuid::Uuid::parse_str(invocation.as_str()).is_ok());
    }
    let model_ids: std::collections::HashSet<_> = identities
        .iter()
        .map(|identity| identity.2.as_str())
        .collect();
    assert_eq!(
        model_ids,
        std::collections::HashSet::from(["model-call-1", "model-call-2"])
    );
}

#[tokio::test]
async fn test_dispatch_concurrent_single_tool_succeeds() {
    let ctx = make_test_ctx();
    let tool = std::sync::Arc::new(OutputTool {
        name: "Read".to_string(),
        output: "ok".to_string(),
    });
    let mut all_tools: HashMap<String, std::sync::Arc<dyn BaseTool>> = HashMap::new();
    all_tools.insert("Read".to_string(), tool);
    let cancel = CancellationToken::new();
    let ai_msg = BaseMessage::ai(MessageContent::text("thinking...".to_string()));
    let ready_calls = vec![ToolCall {
        id: "call_1".to_string(),
        name: "Read".to_string(),
        input: serde_json::json!({"file_path": "/tmp/test.txt"}),
    }];
    let mut target_tools: HashMap<String, std::sync::Arc<dyn BaseTool>> = HashMap::new();
    target_tools.insert(
        "call_1".to_string(),
        Arc::clone(all_tools.get("Read").unwrap()),
    );
    let raw_calls = HashMap::new();
    let catalog = ctx.runtime.tool_catalog.snapshot();
    let invocation = make_invocation(&ctx);
    let turn = invocation
        .require_turn("test dispatch")
        .expect("active turn");
    let results = dispatch_concurrent(
        &invocation,
        Some(turn),
        &ready_calls,
        &raw_calls,
        &target_tools,
        &catalog,
        &cancel,
        &ai_msg,
        None,
    )
    .await;
    assert_eq!(results.len(), 1);
    assert!(results[0].is_ok(), "工具应成功执行");
    assert_eq!(results[0].as_ref().unwrap().text, "ok");
}

#[tokio::test]
async fn test_dispatch_concurrent_cancelled() {
    let ctx = make_test_ctx();
    let tool = std::sync::Arc::new(OutputTool {
        name: "Read".to_string(),
        output: "ok".to_string(),
    });
    let mut all_tools: HashMap<String, std::sync::Arc<dyn BaseTool>> = HashMap::new();
    all_tools.insert("Read".to_string(), tool);
    let cancel = CancellationToken::new();
    cancel.cancel(); // 提前触发取消
    let ai_msg = BaseMessage::ai(MessageContent::text("thinking...".to_string()));
    let ready_calls = vec![ToolCall {
        id: "call_1".to_string(),
        name: "Read".to_string(),
        input: serde_json::json!({}),
    }];
    let mut target_tools: HashMap<String, std::sync::Arc<dyn BaseTool>> = HashMap::new();
    target_tools.insert(
        "call_1".to_string(),
        Arc::clone(all_tools.get("Read").unwrap()),
    );
    let raw_calls = HashMap::new();
    let catalog = ctx.runtime.tool_catalog.snapshot();
    let invocation = make_invocation(&ctx);
    let turn = invocation
        .require_turn("test dispatch")
        .expect("active turn");
    let results = dispatch_concurrent(
        &invocation,
        Some(turn),
        &ready_calls,
        &raw_calls,
        &target_tools,
        &catalog,
        &cancel,
        &ai_msg,
        None,
    )
    .await;
    assert_eq!(results.len(), 1);
    assert!(results[0].is_err(), "取消后应返回错误");
    let err = results[0].as_ref().unwrap_err().to_string();
    assert!(
        err.contains("interrupted by user"),
        "错误信息应包含取消描述，实际: {err}"
    );
}

#[tokio::test]
async fn test_dispatch_concurrent_preserves_typed_subagent_failure() {
    let ctx = make_test_ctx();
    let tool = std::sync::Arc::new(FailingSubagentTool);
    let mut target_tools: HashMap<String, std::sync::Arc<dyn BaseTool>> = HashMap::new();
    target_tools.insert("call_1".to_string(), tool);
    let cancel = CancellationToken::new();
    let ai_msg = BaseMessage::ai(MessageContent::text("thinking...".to_string()));
    let ready_calls = vec![ToolCall {
        id: "call_1".to_string(),
        name: "SubAgent".to_string(),
        input: serde_json::json!({}),
    }];
    let raw_calls = HashMap::new();
    let catalog = ctx.runtime.tool_catalog.snapshot();
    let invocation = make_invocation(&ctx);
    let turn = invocation
        .require_turn("test dispatch")
        .expect("active turn");
    let results = dispatch_concurrent(
        &invocation,
        Some(turn),
        &ready_calls,
        &raw_calls,
        &target_tools,
        &catalog,
        &cancel,
        &ai_msg,
        None,
    )
    .await;
    let error = results[0].as_ref().expect_err("child failure");
    let failure = error
        .subagent_failure()
        .expect("typed child failure must survive dispatch");
    assert_eq!(failure.child_thread_id(), "child-123");
    assert_eq!(
        failure.diagnostic().expect("model diagnostic").status(),
        Some(500)
    );
    assert_eq!(
        failure.diagnostic().expect("model diagnostic").request_id(),
        Some("req-123")
    );
    assert!(!error.to_string().contains("provider body"));
}

#[tokio::test]
async fn test_settle_results_mixed_ready_settled() {
    let ctx = make_test_ctx();
    let approval = ApprovalOutcome {
        ready_calls: vec![ToolCall {
            id: "call_ready".to_string(),
            name: "Read".to_string(),
            input: serde_json::json!({}),
        }],
        settled_results: vec![SettledInvocation::unreported(
            ToolCall {
                id: "call_rejected".to_string(),
                name: "Bash".to_string(),
                input: serde_json::json!({}),
            },
            ToolResult::error("call_rejected", "Bash", "HITL rejected"),
        )],
    };
    let tool_results: Vec<Result<crate::tools::ToolOutput, EffectiveToolError>> =
        vec![Ok(crate::tools::ToolOutput::from_legacy("success output"))];
    let all_tools: HashMap<String, std::sync::Arc<dyn BaseTool>> = HashMap::new();
    let invocation = make_invocation(&ctx);
    let outcome = settle_results(&invocation, approval, tool_results, false, &all_tools).await;
    // ready + settled = 2 条
    assert_eq!(outcome.results.len(), 2, "应合并 ready 和 settled 结果");
    // settled 在前，ready 在后
    assert!(outcome.results[0].result.is_error, "rejected 应是错误");
    assert!(!outcome.results[1].result.is_error, "ready 工具应成功");
    assert_eq!(outcome.results[1].result.output, "success output");
}

#[test]
fn test_post_process_result_truncates_by_tool_output_limit() {
    struct LimitedTool;
    #[async_trait::async_trait]
    impl BaseTool for LimitedTool {
        fn name(&self) -> &str {
            "Limited"
        }
        fn description(&self) -> &str {
            "test output limit"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn invoke(
            &self,
            _input: serde_json::Value,
            _ctx: crate::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok(String::new())
        }
        fn output_char_limit(&self) -> Option<usize> {
            Some(60)
        }
    }
    let call = ToolCall {
        id: "call_1".to_string(),
        name: "Limited".to_string(),
        input: serde_json::json!({}),
    };
    let mut result = ToolResult::error("call_1", "Limited", "x".repeat(200));
    let mut all_tools: HashMap<String, std::sync::Arc<dyn BaseTool>> = HashMap::new();
    all_tools.insert("call_1".to_string(), std::sync::Arc::new(LimitedTool));
    post_process_result(&call, &mut result, &all_tools);
    assert_eq!(result.output.chars().count(), 60, "{}", result.output);
    assert!(
        result.output.ends_with("[Output truncated at 60 chars]"),
        "{}",
        result.output
    );
    assert!(result.output.starts_with('x'));

    // 未登记 output 上限的工具：输出保持不变
    let mut untouched = ToolResult::error("call_2", "Other", "kept as-is");
    let empty_tools: HashMap<String, std::sync::Arc<dyn BaseTool>> = HashMap::new();
    post_process_result(&call, &mut untouched, &empty_tools);
    assert_eq!(untouched.output, "kept as-is");
}

#[test]
fn test_effective_cancel_and_timeout_errors_keep_typed_execution_facts() {
    let cancelled = execution_for_effective_error(EffectiveToolErrorCode::Cancelled)
        .expect("cancel must carry evidence");
    assert_eq!(
        cancelled.status,
        crate::tools::ToolExecutionStatus::Cancelled
    );
    let timed_out = execution_for_effective_error(EffectiveToolErrorCode::Timeout)
        .expect("timeout must carry evidence");
    assert_eq!(
        timed_out.status,
        crate::tools::ToolExecutionStatus::TimedOut
    );
    assert!(execution_for_effective_error(EffectiveToolErrorCode::ToolFailed).is_none());
}

/// idle 执行入口的输入（session 级事实 + typed 来源）：生产路径由
/// session-owned canonical 入口提供，测试从同一个夹具投影。
fn idle_source(stage: &StageContext, origin: InvocationOrigin) -> SessionInvocationSource {
    SessionInvocationSource {
        session_id: stage.project_session_id(),
        run_id: None,
        agent_id: stage.session.agent_id,
        cwd: std::sync::Arc::clone(&stage.session.turn.cwd),
        messages: Some(std::sync::Arc::new(stage.visible_messages())),
        context_usage: None,
        session_resources: None,
        resolver: std::sync::Arc::clone(&stage.runtime.tool_invocation_resolver),
        chain: std::sync::Arc::clone(&stage.runtime.middleware_chain),
        queue: stage.session.queue.clone(),
        origin,
        cancel: CancellationToken::new(),
        post_hook_skips: None,
    }
}

fn attached_app_origin() -> InvocationOrigin {
    InvocationOrigin::App {
        app_id: "app-idle".into(),
        attachment_id: Some(peri_acp_types::interaction::AttachmentId::new("att-idle")),
        connection_epoch: Some(peri_acp_types::interaction::ConnectionEpoch::initial()),
        server_generation: 4,
    }
}

/// [回归] idle（无活跃 turn）入口：**模型来源**依赖 turn 的准入规则显式返回 typed
/// 不可用，不是静默跳过、不是泛化 `Other` 错误（设计 §4.1）。
///
/// 错误必须原样携带入口名与已知会话身份，供 idle 入口判别与诊断。
#[tokio::test]
async fn idle_entry_returns_typed_not_available_without_turn() {
    let stage = make_test_ctx();
    stage
        .session
        .session_context
        .write()
        .insert("session_id".into(), "session-idle".into());
    let invocation = InvocationContext::from_source(idle_source(
        &stage,
        InvocationOrigin::Model {
            tool_call_id: ToolCallId::new("call-1"),
        },
    ));
    assert!(
        invocation.session_id().is_some(),
        "idle 入口仍持有真实会话身份（缺的是 turn 事实）"
    );

    let error = match collect_tool_results(
        &invocation,
        vec![ToolCall::new("call-1", "Probe", serde_json::json!({}))],
        &HashMap::new(),
        &HashMap::new(),
        &stage.runtime.tool_catalog.snapshot(),
        &CancellationToken::new(),
        Some(crate::messages::MessageId::new()),
        &BaseMessage::ai("call"),
        None,
    )
    .await
    {
        Err(error) => error,
        Ok(_) => panic!("模型来源缺 turn 的准入必须显式不可用，不得静默跳过"),
    };

    let AgentError::NotAvailableWithoutTurn { entry, session_id } = error else {
        panic!("必须是 typed 不可用错误（不是 Other / 静默跳过），实际: {error:?}");
    };
    assert_eq!(entry, "tool dispatch");
    assert_eq!(session_id.as_deref(), Some("session-idle"));
}

/// [回归] App 来源的 idle 执行：`turn: None` 下管线**照常执行**（准入规则与执行
/// 都不需要 turn 事实），但 ① 不发任何 render 事件、② 不写 `turn_generation`、
/// ③ 工具拿到的归属是 App 归属（不是伪造的模型归属）。
#[tokio::test]
async fn idle_app_invocation_runs_without_turn_facts_or_model_ownership() {
    type Seen =
        Arc<parking_lot::Mutex<Vec<Option<peri_acp_types::interaction::InteractionAttribution>>>>;
    struct AttributionProbe(Seen);
    #[async_trait::async_trait]
    impl BaseTool for AttributionProbe {
        fn name(&self) -> &str {
            "Probe"
        }
        fn description(&self) -> &str {
            "attribution probe"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn invoke(
            &self,
            _: serde_json::Value,
            context: crate::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            assert!(
                context.turn_generation.is_none(),
                "无 turn 的执行不得写 turn_generation（也不得伪造）"
            );
            self.0.lock().push(context.attribution.clone());
            Ok("ok".into())
        }
    }

    let stage = make_test_ctx();
    stage
        .session
        .session_context
        .write()
        .insert("session_id".into(), "session-idle".into());
    let invocation = InvocationContext::from_source(idle_source(&stage, attached_app_origin()));
    assert!(
        !invocation.has_render_events(),
        "idle 执行按构造没有 render 事件端口"
    );

    let seen: Seen = Arc::default();
    let tool: Arc<dyn BaseTool> = Arc::new(AttributionProbe(seen.clone()));
    let outcome = collect_tool_results(
        &invocation,
        vec![ToolCall::new("call-1", "Probe", serde_json::json!({}))],
        &HashMap::new(),
        &HashMap::from([("call-1".to_string(), tool)]),
        &stage.runtime.tool_catalog.snapshot(),
        &CancellationToken::new(),
        None,
        &BaseMessage::ai("call"),
        None,
    )
    .await
    .expect("App 来源的 idle 执行必须照常完成");

    assert_eq!(outcome.results.len(), 1);
    assert!(!outcome.results[0].result.is_error);
    assert_eq!(
        seen.lock().as_slice(),
        &[Some(
            peri_acp_types::interaction::InteractionAttribution::app_attachment(
                "app-idle",
                "att-idle",
                peri_acp_types::interaction::ConnectionEpoch::initial()
            )
        )],
        "idle 执行的归属必须是 App 归属（模型 turn 事实缺失时不得伪造）"
    );
}
