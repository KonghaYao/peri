use super::*;
use crate::messages::MessageContent;
use crate::session::queue::MessageQueue;
use crate::session::transcript::MessageTranscript;
use crate::session::turn::TurnContext;

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
    let results = dispatch_concurrent(
        &ctx,
        &ready_calls,
        &raw_calls,
        &target_tools,
        &catalog,
        &cancel,
        &ai_msg,
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
    let results = dispatch_concurrent(
        &ctx,
        &ready_calls,
        &raw_calls,
        &target_tools,
        &catalog,
        &cancel,
        &ai_msg,
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
    let results = dispatch_concurrent(
        &ctx,
        &ready_calls,
        &raw_calls,
        &target_tools,
        &catalog,
        &cancel,
        &ai_msg,
    )
    .await;
    let error = results[0].as_ref().expect_err("child failure");
    let failure = error
        .subagent_failure()
        .expect("typed child failure must survive dispatch");
    assert_eq!(failure.child_thread_id(), "child-123");
    assert_eq!(failure.diagnostic().status(), Some(500));
    assert_eq!(failure.diagnostic().request_id(), Some("req-123"));
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
        settled_results: vec![(
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
    let outcome = settle_results(&ctx, approval, tool_results, false, &all_tools).await;
    // ready + settled = 2 条
    assert_eq!(outcome.results.len(), 2, "应合并 ready 和 settled 结果");
    // settled 在前，ready 在后
    assert!(outcome.results[0].1.is_error, "rejected 应是错误");
    assert!(!outcome.results[1].1.is_error, "ready 工具应成功");
    assert_eq!(outcome.results[1].1.output, "success output");
}

#[test]
fn test_post_process_result_no_registry() {
    let ctx = make_test_ctx();
    let call = ToolCall {
        id: "call_1".to_string(),
        name: "Read".to_string(),
        input: serde_json::json!({"file_path": "/tmp/x"}),
    };
    let mut result = ToolResult::error("call_1", "Read", "ENOENT: file not found");
    let all_tools: HashMap<String, std::sync::Arc<dyn BaseTool>> = HashMap::new();
    let output_before = result.output.clone();
    // error_suggest_registry 为 None（默认），不应修改 output
    post_process_result(&ctx, &call, &mut result, &all_tools);
    assert_eq!(
        result.output, output_before,
        "无 registry 时 output 不应变化，实际: {}",
        result.output
    );
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
