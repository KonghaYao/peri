//! Tests for ask_user_tool

use peri_agent::tools::BaseTool as _;
use std::sync::Arc;

use super::AskUserTool;
use peri_agent::interaction::{
    InteractionContext, InteractionFailure, InteractionResponse, QuestionAnswer,
    TrustedInteractionOwner, UnansweredCause, UserInteractionBroker,
};

struct MockBroker(InteractionResponse);

#[async_trait::async_trait]
impl UserInteractionBroker for MockBroker {
    async fn request(
        &self,
        _owner: TrustedInteractionOwner,
        _ctx: InteractionContext,
    ) -> InteractionResponse {
        self.0.clone()
    }
}

fn make_answer(selected: &[&str], text: Option<&str>) -> InteractionResponse {
    InteractionResponse::Answers(vec![QuestionAnswer {
        id: "ask_user_question_0".to_string(),
        selected: selected.iter().map(|s| s.to_string()).collect(),
        text: text.map(|s| s.to_string()),
    }])
}

/// 测试用工具上下文：绑定 canonical session + 真实 turn 归属（交互归属的唯一来源）。
///
/// 生产 `ToolContext` 由 canonical dispatch 绑定（`with_attribution`）；
/// 缺归属时 `AskUserTool` 必须显式拒绝，因此本 fixture 必须给出真实形状的事实。
fn tool_context() -> peri_agent::tools::ToolContext<'static> {
    peri_agent::tools::ToolContext::new(&[], ".")
        .with_session_identity("session-test", "turn-test")
        .with_attribution(
            peri_acp_types::interaction::InteractionAttribution::model_turn(
                "session-test",
                peri_acp_types::session::ExecutionBinding {
                    turn_id: peri_acp_types::session::TurnId::new(),
                    attempt_id: peri_acp_types::identity::AttemptId::new(),
                },
            ),
        )
}

fn make_tool(response: InteractionResponse) -> AskUserTool {
    AskUserTool::new(Arc::new(MockBroker(response)))
}

fn single_question_input() -> serde_json::Value {
    serde_json::json!({
        "questions": [{
            "question": "What is your choice?",
            "header": "H1",
            "multi_select": false,
            "options": [{"label": "选项A"}, {"label": "选项B"}]
        }]
    })
}

// ── 参数解析测试 ──

#[tokio::test]
async fn test_invalid_json_returns_err() {
    let tool = make_tool(make_answer(&[], None));
    let result = tool.invoke(serde_json::Value::Null, tool_context()).await;
    assert!(result.is_err(), "null input should return Err");
}

#[tokio::test]
async fn test_missing_questions_key_returns_err() {
    let tool = make_tool(make_answer(&[], None));
    let result = tool.invoke(serde_json::json!({}), tool_context()).await;
    assert!(result.is_err(), "missing questions key should return Err");
}

#[tokio::test]
async fn test_valid_single_question_parsed() {
    let tool = make_tool(make_answer(&["选项A"], None));
    let result = tool
        .invoke(single_question_input(), tool_context())
        .await
        .unwrap();
    assert_eq!(result, "[问: H1]\n回答: 选项A");
}

// ── 单问题返回格式 ──

#[tokio::test]
async fn test_single_question_selected_answer() {
    let tool = make_tool(make_answer(&["选项A"], None));
    let result = tool
        .invoke(single_question_input(), tool_context())
        .await
        .unwrap();
    assert_eq!(result, "[问: H1]\n回答: 选项A");
}

#[tokio::test]
async fn test_single_question_text_input() {
    let tool = make_tool(make_answer(&[], Some("自定义输入")));
    let result = tool
        .invoke(single_question_input(), tool_context())
        .await
        .unwrap();
    assert_eq!(result, "[问: H1]\n回答: 自定义输入");
}

#[tokio::test]
async fn test_single_question_text_priority_over_selected() {
    let tool = make_tool(make_answer(&["选项A"], Some("自定义")));
    let result = tool
        .invoke(single_question_input(), tool_context())
        .await
        .unwrap();
    assert_eq!(
        result, "[问: H1]\n回答: 自定义",
        "non-empty text should take priority over selected"
    );
}

#[tokio::test]
async fn test_single_question_empty_selected() {
    let tool = make_tool(make_answer(&[], None));
    let result = tool
        .invoke(single_question_input(), tool_context())
        .await
        .unwrap();
    assert_eq!(
        result, "[问: H1]\n回答: ",
        "empty selected and no text should return empty answer"
    );
}

/// 用户真的提交了空内容（`accept` + 空串）：仍是合法答案，保持空语义。
#[tokio::test]
async fn test_real_empty_submission_is_valid_answer() {
    let tool = make_tool(make_answer(&[], Some("")));
    let result = tool
        .invoke(single_question_input(), tool_context())
        .await
        .unwrap();
    assert_eq!(
        result, "[问: H1]\n回答: ",
        "真实空提交必须成为合法空答案，而不是失败"
    );
}

// ── 多问题返回格式 ──

#[tokio::test]
async fn test_multi_question_format() {
    let response = InteractionResponse::Answers(vec![
        QuestionAnswer {
            id: "ask_user_question_0".into(),
            selected: vec!["v1".into()],
            text: None,
        },
        QuestionAnswer {
            id: "ask_user_question_1".into(),
            selected: vec!["v2".into()],
            text: None,
        },
    ]);
    let tool = make_tool(response);
    let result = tool
        .invoke(
            serde_json::json!({
                "questions": [
                    {"question": "Q1?", "header": "H1", "options": [{"label": "v1"}]},
                    {"question": "Q2?", "header": "H2", "options": [{"label": "v2"}]}
                ]
            }),
            tool_context(),
        )
        .await
        .unwrap();
    assert_eq!(result, "[问: H1]\n回答: v1\n\n[问: H2]\n回答: v2");
}

#[tokio::test]
async fn test_multi_question_multi_select_join() {
    // Single question with multi_select, multiple selected options
    let response = InteractionResponse::Answers(vec![QuestionAnswer {
        id: "ask_user_question_0".into(),
        selected: vec!["A".into(), "B".into()],
        text: None,
    }]);
    let tool = make_tool(response);
    let result = tool
        .invoke(
            serde_json::json!({
                "questions": [{
                    "question": "Pick all?",
                    "header": "H1",
                    "multi_select": true,
                    "options": [{"label": "A"}, {"label": "B"}]
                }]
            }),
            tool_context(),
        )
        .await
        .unwrap();
    assert_eq!(result, "[问: H1]\n回答: A, B");
}

// ── 异常响应测试 ──

#[tokio::test]
async fn test_unexpected_response_type() {
    use peri_agent::interaction::ApprovalDecision;
    let response = InteractionResponse::Decisions(vec![ApprovalDecision::Approve { source: None }]);
    let tool = make_tool(response);
    let result = tool.invoke(single_question_input(), tool_context()).await;
    assert!(result.is_err(), "non-Answers response should return Err");
}

/// 用户明确拒绝（`decline`，或 TUI Esc 的裸 cancel 经 broker 归一为 `Rejected`）：
/// 工具必须以 `ToolRejected{cause: UserRejected}` 结算，不得返回空白回答。
#[tokio::test]
async fn test_rejected_maps_to_user_rejected_tool_error() {
    let tool = make_tool(InteractionResponse::Rejected);
    let err = tool
        .invoke(single_question_input(), tool_context())
        .await
        .expect_err("用户拒绝必须返回错误，不得返回空回答");
    let agent_error = err
        .downcast_ref::<peri_agent::error::AgentError>()
        .unwrap_or_else(|| panic!("拒绝必须是 typed AgentError: {err}"));
    match agent_error {
        peri_agent::error::AgentError::ToolRejected {
            tool,
            reason,
            cause,
        } => {
            assert_eq!(tool, "AskUserQuestion");
            assert_eq!(
                *cause,
                peri_acp_types::error::ToolRejectionCause::UserRejected,
                "Esc 取消/decline 都是用户拒绝，不得归为 ApprovalUnavailable"
            );
            assert!(reason.contains("拒绝"), "拒绝原因文本必须明确: {reason}");
        }
        other => panic!("拒绝应保持 ToolRejected 语义: {other:?}"),
    }
    assert_eq!(agent_error.category_name(), "tool_rejected");
    let message = err.to_string();
    assert!(
        message.contains("Tool rejected") && message.contains("用户拒绝回答"),
        "错误文本必须声明工具被用户拒绝: {message}"
    );
    assert!(
        !message.contains("回答: "),
        "不得把拒绝伪装成空白回答: {message}"
    );
}

/// [回归测试] 客户端声明无人可作答时，工具必须如实转述原因，而不是返回伪造的
/// 空回答文本（旧行为：`[问: H1]\n回答: `）。
///
/// 参数化覆盖已知原因与未知原因：前者指明非交互客户端，后者只能说明「无人可
/// 作答」且不得声称是 `-p`。两者都只能是失败的工具结果（`ToolRejected`）。
#[tokio::test]
async fn test_unanswered_reports_declared_reason() {
    for (cause, expected, forbidden) in [
        (
            UnansweredCause::NonInteractiveClient,
            "非交互",
            "本次提问无人可作答",
        ),
        (UnansweredCause::Unknown, "本次提问无人可作答", "-p"),
    ] {
        let tool = make_tool(InteractionResponse::Unanswered { cause });
        let err = tool
            .invoke(single_question_input(), tool_context())
            .await
            .expect_err("unanswered 必须返回错误，不得伪造空回答");
        assert!(
            matches!(
                err.downcast_ref::<peri_agent::error::AgentError>(),
                Some(peri_agent::error::AgentError::ToolRejected { .. })
            ),
            "应保持 ToolRejected 语义: {err}"
        );
        let message = err.to_string();
        assert!(
            message.contains(expected),
            "{cause:?} 的错误文本必须如实说明: {message}"
        );
        assert!(
            !message.contains(forbidden),
            "{cause:?} 不得转述未声明的原因: {message}"
        );
        assert!(
            !message.contains("拒绝回答") && !message.contains("回答: "),
            "{cause:?} 不得声称为用户拒绝，也不得携带伪造的回答: {message}"
        );
    }
}

/// 提问通道失败（方法不支持 / 超时 / 传输或协议错误）：工具必须返回**可见的
/// 执行失败**（`ToolExecutionFailed`），不得伪造空回答，也不得冒充用户拒绝。
#[tokio::test]
async fn test_failed_reports_visible_tool_error() {
    for (failure, expected) in [
        (InteractionFailure::MethodUnsupported, "-32601"),
        (InteractionFailure::Timeout, "超时"),
        (
            InteractionFailure::Transport {
                detail: "JSON-RPC -32000: transport closed".to_string(),
            },
            "-32000",
        ),
    ] {
        let tool = make_tool(InteractionResponse::Failed { failure });
        let err = tool
            .invoke(single_question_input(), tool_context())
            .await
            .expect_err("通道失败必须返回错误，不得伪造空回答");
        assert!(
            matches!(
                err.downcast_ref::<peri_agent::error::AgentError>(),
                Some(peri_agent::error::AgentError::ToolExecutionFailed { .. })
            ),
            "通道失败应保持 ToolExecutionFailed 语义: {err}"
        );
        let message = err.to_string();
        assert!(
            message.contains(expected),
            "错误文本必须说明失败事实: {message}"
        );
        assert!(
            !message.contains("回答: ") && !message.contains("拒绝回答"),
            "不得携带伪造的回答，也不得声称为用户拒绝: {message}"
        );
    }
}

#[test]
#[allow(non_snake_case)]
fn test_tool_name_is_AskUserQuestion() {
    let tool = make_tool(make_answer(&[], None));
    assert_eq!(tool.name(), "AskUserQuestion");
}

#[tokio::test]
async fn test_multi_select_camel_case_input() {
    let tool = make_tool(make_answer(&["A", "B"], None));
    let result = tool
        .invoke(
            serde_json::json!({
                "questions": [{
                    "question": "Pick all?",
                    "header": "H1",
                    "multiSelect": true,
                    "options": [{"label": "A"}, {"label": "B"}]
                }]
            }),
            tool_context(),
        )
        .await
        .unwrap();
    assert_eq!(
        result, "[问: H1]\n回答: A, B",
        "multiSelect (camelCase) should work"
    );
}

/// 路线 A：schema 不得再声明未实现的 `preview` 字段。
///
/// 历史上该字段被声明但两处 parser 均静默丢弃；删除声明让契约与实现一致。
#[test]
fn test_schema_does_not_declare_preview() {
    let params = crate::ask_user::ask_user_tool_definition().parameters;
    let props =
        &params["properties"]["questions"]["items"]["properties"]["options"]["items"]["properties"];
    assert!(
        props.get("preview").is_none(),
        "schema 不得声明 preview: {params}"
    );
    assert!(props.get("label").is_some(), "选项 schema 应保留 label");
    assert!(
        props.get("description").is_some(),
        "选项 schema 应保留 description"
    );
}

/// 计数 broker：本用例要求提问路径**零交互**（被调用即失败）。
struct PanickingBroker(std::sync::atomic::AtomicUsize);

#[async_trait::async_trait]
impl UserInteractionBroker for PanickingBroker {
    async fn request(
        &self,
        _owner: TrustedInteractionOwner,
        _ctx: InteractionContext,
    ) -> InteractionResponse {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        panic!("缺少可信归属时不得发出提问交互");
    }
}

/// [回归] AskUserQuestion 缺可信归属 ⇒ 显式拒绝，零 broker 交互。
///
/// 提问归属只能从 canonical dispatch 绑定的真实 session + turn 事实铸造；缺失即
/// fail-closed，不得伪造归属、不得静默跳过（设计 §4.3）。
#[tokio::test]
async fn test_missing_trusted_owner_is_rejected_without_asking() {
    let broker = std::sync::Arc::new(PanickingBroker(std::sync::atomic::AtomicUsize::new(0)));
    let tool = AskUserTool::new(broker.clone());
    // 非 canonical 上下文：既无会话身份，也无 turn 绑定。
    let context = peri_agent::tools::ToolContext::new(&[], ".");

    let error = tool
        .invoke(single_question_input(), context)
        .await
        .expect_err("缺归属必须显式拒绝，不得提问");

    let agent_error = error
        .downcast_ref::<peri_agent::error::AgentError>()
        .expect("必须保持 AgentError 语义");
    match agent_error {
        peri_agent::error::AgentError::ToolRejected { tool, reason, .. } => {
            assert_eq!(tool, "AskUserQuestion");
            assert_eq!(reason, crate::interaction_owner::UNTRUSTED_OWNER_REASON);
        }
        other => panic!("缺归属必须显式拒绝（不是静默跳过），实际: {other:?}"),
    }
    assert_eq!(
        broker.0.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "缺归属不得产生 broker 交互"
    );
}
