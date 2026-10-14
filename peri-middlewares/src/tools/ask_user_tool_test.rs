//! Tests for ask_user_tool

use std::sync::Arc;

use super::*;
use peri_agent::interaction::{
    InteractionContext, InteractionResponse, QuestionAnswer, TrustedInteractionOwner,
    UnansweredCause, UserInteractionBroker,
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

#[tokio::test]
async fn test_preview_field_ignored() {
    let tool = make_tool(make_answer(&["选项A"], None));
    let result = tool
        .invoke(
            serde_json::json!({
                "questions": [{
                    "question": "What?",
                    "header": "H1",
                    "options": [{"label": "选项A", "preview": "some preview"}]
                }]
            }),
            tool_context(),
        )
        .await
        .unwrap();
    assert_eq!(
        result, "[问: H1]\n回答: 选项A",
        "preview field should not cause error"
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
