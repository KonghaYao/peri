use super::*;
use async_trait::async_trait;
use peri_agent::error::AgentError;
use peri_agent::goal::{GoalController, GoalStatus, GoalViewSnapshot};
use peri_agent::llm::types::StopReason;
use peri_agent::llm::{BaseModel, LlmRequest, LlmResponse};
use peri_agent::tools::ToolContext;

struct MockController {
    has_goal: parking_lot::Mutex<bool>,
}

impl MockController {
    fn new() -> Self {
        Self {
            has_goal: parking_lot::Mutex::new(false),
        }
    }
}

#[async_trait]
impl GoalController for MockController {
    async fn create_goal(&self, objective: String) -> Result<(), String> {
        let mut guard = self.has_goal.lock();
        if *guard {
            return Err("goal 已存在".to_string());
        }
        *guard = true;
        let _ = objective;
        Ok(())
    }

    async fn complete_goal(&self) -> Result<(), String> {
        Ok(())
    }

    async fn block_goal(&self, _reason: String) -> Result<(), String> {
        Ok(())
    }

    fn snapshot(&self) -> GoalViewSnapshot {
        let guard = self.has_goal.lock();
        if *guard {
            GoalViewSnapshot {
                objective: Some("测试目标".to_string()),
                status: Some(GoalStatus::Active),
                token_budget: None,
                tokens_used: 0,
                objective_just_updated: false,
            }
        } else {
            GoalViewSnapshot::default()
        }
    }
}

#[tokio::test]
async fn test_goal_create() {
    let controller = Arc::new(MockController::new()) as Arc<dyn GoalController>;
    let tool = GoalTool::new(controller, None);

    let input = json!({"action": "create", "objective": "测试目标"});
    let ctx = ToolContext::new(&[], ".");
    let result = tool.invoke(input, ctx).await.unwrap();
    assert!(result.contains("Goal created"));
}

#[tokio::test]
async fn test_goal_create_duplicate() {
    let controller = Arc::new(MockController::new()) as Arc<dyn GoalController>;
    let tool = GoalTool::new(Arc::clone(&controller), None);

    let input = json!({"action": "create", "objective": "目标1"});
    let ctx = ToolContext::new(&[], ".");
    tool.invoke(input.clone(), ctx).await.unwrap();

    // 重复 create 返回 Err（is_error=true），让 LLM 看到工具错误而非普通文本
    let err = tool
        .invoke(input, ToolContext::new(&[], "."))
        .await
        .expect_err("重复 create 应返回 error");
    assert!(err.to_string().contains("failed to create"));
}

#[tokio::test]
async fn test_goal_get_no_goal() {
    let controller = Arc::new(MockController::new()) as Arc<dyn GoalController>;
    let tool = GoalTool::new(controller, None);

    let input = json!({"action": "get"});
    let ctx = ToolContext::new(&[], ".");
    let result = tool.invoke(input, ctx).await.unwrap();
    assert!(result.contains("No active goal"));
}

#[tokio::test]
async fn test_goal_block() {
    let controller = Arc::new(MockController::new()) as Arc<dyn GoalController>;
    let tool = GoalTool::new(controller, None);

    // 先 create
    tool.invoke(
        json!({"action": "create", "objective": "目标"}),
        ToolContext::new(&[], "."),
    )
    .await
    .unwrap();

    let result = tool
        .invoke(
            json!({"action": "block", "reason": "测试阻塞"}),
            ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("blocked"));
}

#[tokio::test]
async fn test_goal_complete_no_model_skips_verification() {
    let controller = Arc::new(MockController::new()) as Arc<dyn GoalController>;
    let tool = GoalTool::new(controller, None);

    tool.invoke(
        json!({"action": "create", "objective": "目标"}),
        ToolContext::new(&[], "."),
    )
    .await
    .unwrap();

    let result = tool
        .invoke(json!({"action": "complete"}), ToolContext::new(&[], "."))
        .await
        .unwrap();
    assert!(result.contains("verification skipped"));
}

// ===== MockBaseModel：用于 LLM 验证路径测试 =====

/// 可配置响应的 BaseModel mock
struct MockBaseModel {
    /// invoke 返回的 message 文本
    response_text: String,
    /// 若为 true，invoke 返回 Err（模拟 LLM 调用失败）
    should_error: bool,
}

impl MockBaseModel {
    fn with_response(text: impl Into<String>) -> Self {
        Self {
            response_text: text.into(),
            should_error: false,
        }
    }

    fn failing() -> Self {
        Self {
            response_text: String::new(),
            should_error: true,
        }
    }
}

#[async_trait]
impl BaseModel for MockBaseModel {
    async fn invoke(&self, _request: LlmRequest) -> peri_agent::error::AgentResult<LlmResponse> {
        if self.should_error {
            return Err(AgentError::LlmError("模拟 LLM 调用失败".to_string()));
        }
        Ok(LlmResponse {
            message: BaseMessage::ai(self.response_text.clone()),
            stop_reason: StopReason::EndTurn,
            usage: None,
            request_id: None,
        })
    }

    fn provider_name(&self) -> &str {
        "mock"
    }

    fn model_id(&self) -> &str {
        "mock-verifier"
    }
}

/// 辅助：构造带 auxiliary_model 的 GoalTool + 预先 create 目标
fn make_tool_with_model(model: MockBaseModel) -> (GoalTool, Arc<dyn GoalController>) {
    let controller = Arc::new(MockController::new()) as Arc<dyn GoalController>;
    let tool = GoalTool::new(
        Arc::clone(&controller),
        Some(Arc::new(model) as Arc<dyn BaseModel>),
    );
    (tool, controller)
}

#[tokio::test]
async fn test_goal_complete_with_model_验证通过() {
    let (tool, _controller) = make_tool_with_model(MockBaseModel::with_response(
        r#"{"achieved": true, "evidence": "所有测试通过"}"#,
    ));

    tool.invoke(
        json!({"action": "create", "objective": "完成重构"}),
        ToolContext::new(&[], "."),
    )
    .await
    .unwrap();

    let result = tool
        .invoke(json!({"action": "complete"}), ToolContext::new(&[], "."))
        .await
        .unwrap();
    assert!(
        result.contains("Goal completed"),
        "验证通过应返回完成消息，实际：{result}"
    );
    assert!(
        result.contains("所有测试通过"),
        "应包含验证证据，实际：{result}"
    );
}

#[tokio::test]
async fn test_goal_complete_with_model_验证失败() {
    let (tool, _controller) = make_tool_with_model(MockBaseModel::with_response(
        r#"{"achieved": false, "missing": "缺少边界测试"}"#,
    ));

    tool.invoke(
        json!({"action": "create", "objective": "完成重构"}),
        ToolContext::new(&[], "."),
    )
    .await
    .unwrap();

    let result = tool
        .invoke(json!({"action": "complete"}), ToolContext::new(&[], "."))
        .await
        .unwrap();
    // 验证失败：goal 保持 Active，返回"未达成"提示
    assert!(
        result.contains("Goal not yet achieved"),
        "验证失败应返回未达成提示，实际：{result}"
    );
    assert!(
        result.contains("缺少边界测试"),
        "应包含 missing 描述供 LLM 参考，实际：{result}"
    );
}

#[tokio::test]
async fn test_goal_complete_model_返回非法_json_默认未达成() {
    // LLM 返回纯文本（无 JSON），parse_verdict 宽松解析失败 → 默认 achieved=false
    let (tool, _controller) =
        make_tool_with_model(MockBaseModel::with_response("目标看起来还没完成。"));

    tool.invoke(
        json!({"action": "create", "objective": "完成重构"}),
        ToolContext::new(&[], "."),
    )
    .await
    .unwrap();

    let result = tool
        .invoke(json!({"action": "complete"}), ToolContext::new(&[], "."))
        .await
        .unwrap();
    assert!(
        result.contains("Goal not yet achieved"),
        "非法 JSON 应默认判未达成（fail-safe），实际：{result}"
    );
    assert!(
        result.contains("Failed to parse verifier LLM output"),
        "应提示解析失败原因，实际：{result}"
    );
}

#[tokio::test]
async fn test_goal_complete_model_调用失败_传播错误() {
    let (tool, _controller) = make_tool_with_model(MockBaseModel::failing());

    tool.invoke(
        json!({"action": "create", "objective": "完成重构"}),
        ToolContext::new(&[], "."),
    )
    .await
    .unwrap();

    // LLM invoke 抛错 → handle_complete 通过 ? 传播为 ToolExecutionFailed（is_error=true）
    let err = tool
        .invoke(json!({"action": "complete"}), ToolContext::new(&[], "."))
        .await
        .expect_err("LLM 调用失败应返回 error");
    assert!(
        err.to_string().contains("模拟 LLM 调用失败"),
        "错误应包含原始 LLM 错误信息，实际：{err}"
    );
}

#[tokio::test]
async fn test_goal_complete_无_goal_返回提示() {
    // 未先 create 直接 complete：应优雅返回提示而非 panic
    let controller = Arc::new(MockController::new()) as Arc<dyn GoalController>;
    let tool = GoalTool::new(controller, None);

    let result = tool
        .invoke(json!({"action": "complete"}), ToolContext::new(&[], "."))
        .await
        .unwrap();
    assert!(
        result.contains("No active goal"),
        "未 create 直接 complete 应返回提示，实际：{result}"
    );
}

#[tokio::test]
async fn test_goal_unknown_action_返回_error() {
    let controller = Arc::new(MockController::new()) as Arc<dyn GoalController>;
    let tool = GoalTool::new(controller, None);

    let err = tool
        .invoke(
            json!({"action": "unknown_action"}),
            ToolContext::new(&[], "."),
        )
        .await
        .expect_err("未知 action 应返回 error");
    assert!(
        err.to_string().contains("unknown action"),
        "错误应提示未知 action，实际：{err}"
    );
}

#[test]
fn test_parse_verdict_合法_json_提取字段() {
    let v = GoalTool::parse_verdict(r#"{"achieved": true, "evidence": "done", "missing": "x"}"#);
    assert!(v.achieved);
    assert_eq!(v.evidence, "done");
}

#[test]
fn test_parse_verdict_包裹文本_仍能提取() {
    // LLM 常在 JSON 前后添加解释性文本
    let v = GoalTool::parse_verdict(
        "根据评估...\n{\"achieved\": true, \"evidence\": \"ok\"}\n后续说明...",
    );
    assert!(v.achieved, "宽松解析应提取中间的 JSON");
    assert_eq!(v.evidence, "ok");
}

#[test]
fn test_parse_verdict_缺_achieved_字段_默认_false() {
    let v = GoalTool::parse_verdict(r#"{"evidence": "部分完成"}"#);
    assert!(!v.achieved, "缺 achieved 字段应默认 false（fail-safe）");
}

#[test]
fn test_parse_verdict_非法输入_默认未达成() {
    let v = GoalTool::parse_verdict("完全不是 JSON 的纯文本");
    assert!(!v.achieved);
    assert!(v.missing.contains("Failed to parse"));
}
