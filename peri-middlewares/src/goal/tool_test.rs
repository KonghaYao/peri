use super::*;
use async_trait::async_trait;
use peri_agent::goal::{GoalController, GoalStatus, GoalViewSnapshot};
use peri_agent::tools::ToolContext;
use peri_model::{
    Model, ModelCapabilities, ModelError, ModelMessage, ModelRequest, ModelResponse, ModelResult,
    ModelStream, ProtocolErrorKind, StopReason,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct MockGoal {
    goal_id: String,
    objective: String,
    status: GoalStatus,
}

struct MockController {
    goal: parking_lot::Mutex<Option<MockGoal>>,
    next_id: std::sync::atomic::AtomicU64,
    /// `complete_goal_if_current` 收到的身份（按调用顺序记录）。
    checked_goal_ids: parking_lot::Mutex<Vec<String>>,
}

impl MockController {
    fn new() -> Self {
        Self {
            goal: parking_lot::Mutex::new(None),
            next_id: std::sync::atomic::AtomicU64::new(1),
            checked_goal_ids: parking_lot::Mutex::new(Vec::new()),
        }
    }

    fn checked_goal_ids(&self) -> Vec<String> {
        self.checked_goal_ids.lock().clone()
    }

    fn complete_current(&self) -> Result<(), String> {
        let mut guard = self.goal.lock();
        let goal = guard
            .as_mut()
            .ok_or_else(|| "无活跃 goal，无法 set_status".to_string())?;
        if !goal.status.can_transition_to(&GoalStatus::Complete) {
            return Err(format!("非法状态转换: {:?} → Complete", goal.status));
        }
        goal.status = GoalStatus::Complete;
        Ok(())
    }
}

#[async_trait]
impl GoalController for MockController {
    async fn create_goal(&self, objective: String) -> Result<(), String> {
        let mut guard = self.goal.lock();
        if guard.is_some() {
            return Err("goal 已存在".to_string());
        }
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *guard = Some(MockGoal {
            goal_id: format!("goal-{id}"),
            objective,
            status: GoalStatus::Active,
        });
        Ok(())
    }

    async fn complete_goal(&self) -> Result<(), String> {
        self.complete_current()
    }

    async fn complete_goal_if_current(&self, expected_goal_id: &str) -> Result<(), String> {
        self.checked_goal_ids
            .lock()
            .push(expected_goal_id.to_string());

        // 身份比对与状态写入在同一锁内，模拟生产 CAS。
        let mut guard = self.goal.lock();
        let goal = guard
            .as_mut()
            .ok_or_else(|| "无活跃 goal，无法 set_status".to_string())?;
        if goal.goal_id != expected_goal_id {
            return Err("goal 已被替换或清除，未完成本次完成请求".to_string());
        }
        if !goal.status.can_transition_to(&GoalStatus::Complete) {
            return Err(format!("非法状态转换: {:?} → Complete", goal.status));
        }
        goal.status = GoalStatus::Complete;
        Ok(())
    }

    async fn block_goal(&self, _reason: String) -> Result<(), String> {
        let mut guard = self.goal.lock();
        let goal = guard
            .as_mut()
            .ok_or_else(|| "无活跃 goal，无法 set_status".to_string())?;
        if !goal.status.can_transition_to(&GoalStatus::Blocked) {
            return Err(format!("非法状态转换: {:?} → Blocked", goal.status));
        }
        goal.status = GoalStatus::Blocked;
        Ok(())
    }

    async fn clear_goal(&self) -> Result<(), String> {
        *self.goal.lock() = None;
        Ok(())
    }

    fn snapshot(&self) -> GoalViewSnapshot {
        let guard = self.goal.lock();
        match guard.as_ref() {
            Some(goal) => GoalViewSnapshot {
                goal_id: Some(goal.goal_id.clone()),
                objective: Some(goal.objective.clone()),
                status: Some(goal.status),
                token_budget: None,
                tokens_used: 0,
                ..Default::default()
            },
            None => GoalViewSnapshot::default(),
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

// ===== MockModel：用于 LLM 验证路径测试 =====

/// 可配置响应的模型 mock（实现 peri_model::Model）
struct MockModel {
    /// complete 返回的 assistant 文本
    response_text: String,
    /// 若为 true，complete 返回 Err（模拟 LLM 调用失败）
    should_error: bool,
    /// complete 返回的停止原因
    stop_reason: StopReason,
}

impl MockModel {
    fn with_response(text: impl Into<String>) -> Self {
        Self {
            response_text: text.into(),
            should_error: false,
            stop_reason: StopReason::EndTurn,
        }
    }

    fn with_response_and_stop(text: impl Into<String>, stop_reason: StopReason) -> Self {
        Self {
            response_text: text.into(),
            should_error: false,
            stop_reason,
        }
    }

    fn failing() -> Self {
        Self {
            response_text: String::new(),
            should_error: true,
            stop_reason: StopReason::EndTurn,
        }
    }
}

#[async_trait]
impl Model for MockModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_tools: false,
            supports_reasoning: false,
            supports_vision: false,
            supports_streaming: true,
        }
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> ModelResult<ModelStream> {
        // 验证路径只走 complete()，stream() 不应被调用
        Err(ModelError::cancelled())
    }

    async fn complete(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> ModelResult<ModelResponse> {
        if self.should_error {
            return Err(ModelError::protocol(ProtocolErrorKind::Provider));
        }
        Ok(ModelResponse::new(
            ModelMessage::assistant_text(self.response_text.clone()),
            self.stop_reason.clone(),
            None,
            None,
        )?)
    }
}

/// 辅助：构造带 auxiliary_model 的 GoalTool + 预先 create 目标
fn make_tool_with_model(model: MockModel) -> (GoalTool, Arc<dyn GoalController>) {
    let controller = Arc::new(MockController::new()) as Arc<dyn GoalController>;
    let tool = GoalTool::new(
        Arc::clone(&controller),
        Some(Arc::new(model) as Arc<dyn peri_model::Model>),
    );
    (tool, controller)
}

#[tokio::test]
async fn test_goal_complete_with_model_验证通过() {
    let (tool, _controller) = make_tool_with_model(MockModel::with_response(
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
    let (tool, _controller) = make_tool_with_model(MockModel::with_response(
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
        make_tool_with_model(MockModel::with_response("目标看起来还没完成。"));

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
async fn test_goal_complete_model_空输出_报告停止原因() {
    // 复现 reasoning 模型耗尽输出预算：只有 thinking，没有正文。
    let (tool, _controller) =
        make_tool_with_model(MockModel::with_response_and_stop("", StopReason::MaxTokens));

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
        result.contains("verifier returned no text"),
        "空输出应报告真实故障，实际：{result}"
    );
    assert!(
        result.contains("MaxTokens"),
        "应包含停止原因，实际：{result}"
    );
}

#[tokio::test]
async fn test_goal_complete_model_空输出_非预算原因不误报() {
    // EndTurn 下的空响应不是输出预算故障，不能给出预算诊断。
    let (tool, _controller) = make_tool_with_model(MockModel::with_response(""));

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
        result.contains("empty response"),
        "应报告空响应，实际：{result}"
    );
    assert!(
        !result.contains("output budget was exhausted"),
        "非 MaxTokens 不应诊断为预算故障，实际：{result}"
    );
}

/// 断言验证请求不覆盖 auxiliary model 的输出预算。
struct BudgetCheckingModel;

#[async_trait]
impl Model for BudgetCheckingModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_tools: false,
            supports_reasoning: true,
            supports_vision: false,
            supports_streaming: true,
        }
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> ModelResult<ModelStream> {
        Err(ModelError::cancelled())
    }

    async fn complete(
        &self,
        request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> ModelResult<ModelResponse> {
        assert!(
            request.max_tokens.is_none(),
            "goal 验证请求必须继承 auxiliary model 的输出预算"
        );
        Ok(ModelResponse::new(
            ModelMessage::assistant_text(r#"{"achieved": true, "evidence": "ok"}"#),
            StopReason::EndTurn,
            None,
            None,
        )?)
    }
}

#[tokio::test]
async fn test_goal_complete_model_继承辅助模型输出预算() {
    let controller = Arc::new(MockController::new()) as Arc<dyn GoalController>;
    let tool = GoalTool::new(
        controller,
        Some(Arc::new(BudgetCheckingModel) as Arc<dyn peri_model::Model>),
    );

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
}

#[tokio::test]
async fn test_goal_complete_model_调用失败_传播错误() {
    let (tool, _controller) = make_tool_with_model(MockModel::failing());

    tool.invoke(
        json!({"action": "create", "objective": "完成重构"}),
        ToolContext::new(&[], "."),
    )
    .await
    .unwrap();

    // LLM complete 抛错 → handle_complete 通过 ? 传播为 ToolExecutionFailed（is_error=true）
    let err = tool
        .invoke(json!({"action": "complete"}), ToolContext::new(&[], "."))
        .await
        .expect_err("LLM 调用失败应返回 error");
    assert!(
        err.to_string().contains("model protocol error"),
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

#[tokio::test]
async fn test_goal_clear() {
    let controller = Arc::new(MockController::new()) as Arc<dyn GoalController>;
    let tool = GoalTool::new(Arc::clone(&controller), None);

    // 先 create
    tool.invoke(
        json!({"action": "create", "objective": "目标"}),
        ToolContext::new(&[], "."),
    )
    .await
    .unwrap();

    // clear
    let result = tool
        .invoke(json!({"action": "clear"}), ToolContext::new(&[], "."))
        .await
        .unwrap();
    assert!(result.contains("Goal cleared"));

    // clear 后再 get 应返回 No active goal
    let get_result = tool
        .invoke(json!({"action": "get"}), ToolContext::new(&[], "."))
        .await
        .unwrap();
    assert!(
        get_result.contains("No active goal"),
        "clear 后应无 goal，实际：{get_result}"
    );
}

#[tokio::test]
async fn test_goal_clear_no_goal_also_succeeds() {
    // 无 goal 时 clear 也应成功（幂等）
    let controller = Arc::new(MockController::new()) as Arc<dyn GoalController>;
    let tool = GoalTool::new(controller, None);

    let result = tool
        .invoke(json!({"action": "clear"}), ToolContext::new(&[], "."))
        .await
        .unwrap();
    assert!(result.contains("Goal cleared"));
}

// ===== 身份 CAS：验证期间 goal 被替换 =====

/// `ToolContext::new` 的静态空消息输入（供 `tokio::spawn` 持有 `'static` 上下文）。
const NO_MESSAGES: &[BaseMessage] = &[];

/// 受控屏障模型：`complete` 进入后等待测试释放，稳定复现验证期间的并发状态变更。
struct GatedModel {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

impl GatedModel {
    fn new() -> Self {
        Self {
            entered: Arc::new(tokio::sync::Notify::new()),
            release: Arc::new(tokio::sync::Notify::new()),
        }
    }
}

#[async_trait]
impl Model for GatedModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_tools: false,
            supports_reasoning: false,
            supports_vision: false,
            supports_streaming: false,
        }
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> ModelResult<ModelStream> {
        Err(ModelError::cancelled())
    }

    async fn complete(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> ModelResult<ModelResponse> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(ModelResponse::new(
            ModelMessage::assistant_text(
                r#"{"achieved": true, "evidence": "验证通过"}"#.to_string(),
            ),
            StopReason::EndTurn,
            None,
            None,
        )?)
    }
}

/// [回归] 验证 A 期间 clear + create B：A 的验证不得完成 B。
///
/// 修复前 `handle_complete` 无参调用 `complete_goal()`，会把当前 goal（B）置为
/// Complete；本用例在旧实现上失败。
#[tokio::test]
async fn test_goal_complete_during_replace_does_not_complete_replacement_goal() {
    let controller = Arc::new(MockController::new());
    controller.create_goal("目标 A".to_string()).await.unwrap();
    let stale_id = controller.snapshot().goal_id.clone().unwrap();

    let gate = Arc::new(GatedModel::new());
    let tool = Arc::new(GoalTool::new(
        Arc::clone(&controller) as Arc<dyn GoalController>,
        Some(Arc::clone(&gate) as Arc<dyn Model>),
    ));

    let handle = {
        let tool = Arc::clone(&tool);
        tokio::spawn(async move {
            tool.invoke(
                json!({"action": "complete"}),
                ToolContext::new(NO_MESSAGES, "."),
            )
            .await
        })
    };

    // 确认已进入辅助模型 await 窗口。
    gate.entered.notified().await;

    controller.clear_goal().await.unwrap();
    controller.create_goal("目标 B".to_string()).await.unwrap();
    let replacement_id = controller.snapshot().goal_id.clone().unwrap();
    gate.release.notify_one();

    let result = handle
        .await
        .expect("complete 任务不得 panic")
        .expect("身份漂移必须作为普通结果返回");
    assert!(
        result.contains("Goal not completed"),
        "应明确告知未完成，实际：{result}"
    );
    assert!(
        result.contains("已被替换或清除"),
        "应说明 goal 已被替换或清除，实际：{result}"
    );

    let snap = controller.snapshot();
    assert_eq!(
        snap.objective.as_deref(),
        Some("目标 B"),
        "新 goal 必须保留"
    );
    assert_eq!(
        snap.status,
        Some(GoalStatus::Active),
        "B 不得被 A 的验证置为 Complete"
    );
    assert_eq!(
        controller.checked_goal_ids(),
        vec![stale_id.clone()],
        "身份 CAS 必须收到验证开始时捕获的 A id"
    );
    assert_ne!(stale_id, replacement_id);
}

/// 同一 goal 的验证通过仍正常完成，并携带捕获的身份做 CAS。
#[tokio::test]
async fn test_goal_complete_same_goal_after_verification_completes() {
    let controller = Arc::new(MockController::new());
    controller
        .create_goal("完成重构".to_string())
        .await
        .unwrap();
    let goal_id = controller.snapshot().goal_id.clone().unwrap();

    let tool = GoalTool::new(
        Arc::clone(&controller) as Arc<dyn GoalController>,
        Some(Arc::new(MockModel::with_response(
            r#"{"achieved": true, "evidence": "所有测试通过"}"#,
        )) as Arc<dyn Model>),
    );

    let result = tool
        .invoke(json!({"action": "complete"}), ToolContext::new(&[], "."))
        .await
        .unwrap();

    assert!(result.contains("Goal completed"), "实际：{result}");
    assert_eq!(controller.snapshot().status, Some(GoalStatus::Complete));
    assert_eq!(controller.checked_goal_ids(), vec![goal_id]);
}
