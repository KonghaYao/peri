//! Tests for `broker/transport_broker.rs` 提取的 elicitation 纯函数
//! （`build_elicitation_params` / `parse_elicitation_response`）。
//!
//! TUI/notify（mpsc）与 stdio 两路共用同一 `AcpTransportBroker` 及这两处
//! 逻辑（行为零变化复用面），纯函数单测直接锁住 schema 构造
//! （单选 oneOf / 多选 array+items.anyOf / 自由文本 + option description 注入）
//! 与响应解析语义（accept/decline/cancel/未知 action/解析失败兜底）。

use peri_acp_types::interaction::{
    InteractionFailure, InteractionResponse, QuestionItem, QuestionOption, UnansweredCause,
};
use serde_json::json;

use crate::broker::transport_broker::AcpTransportBroker;
use agent_client_protocol::schema::v1::RequestPermissionResponse;
use agent_client_protocol::schema::v1::SessionId;
use peri_acp_types::interaction::ApprovalDecision;
use peri_acp_types::interaction::ApprovalItem;
use peri_acp_types::interaction::InteractionContext;
use peri_acp_types::interaction::TrustedInteractionOwner;
use peri_acp_types::interaction::UserInteractionBroker;
use std::sync::Arc;
use std::time::Duration;

/// 测试用 session-owned 交互门：与生产同语义（session 持有、broker 借用）。
fn interaction_gate() -> Arc<tokio::sync::Mutex<()>> {
    Arc::new(tokio::sync::Mutex::new(()))
}

/// 测试用可信归属（模型 turn owner）——本文件只验证 endpoint 行为，
/// 归属值不参与裁决。
fn owner() -> TrustedInteractionOwner {
    TrustedInteractionOwner::model(
        "s1",
        peri_acp_types::session::ExecutionBinding {
            turn_id: peri_acp_types::session::TurnId::new(),
            attempt_id: peri_acp_types::identity::AttemptId::new(),
        },
    )
}

/// 三形态问题集（与 stdio 双端测试同构）：单选 / 多选 / 自由文本。
fn sample_questions() -> Vec<QuestionItem> {
    vec![
        QuestionItem {
            id: "choice".into(),
            question: "选择部署环境？".into(),
            header: "部署环境".into(),
            options: vec![
                QuestionOption {
                    label: "生产".into(),
                    description: Some("生产环境".into()),
                },
                QuestionOption {
                    label: "测试".into(),
                    description: None,
                },
            ],
            multi_select: false,
        },
        QuestionItem {
            id: "multi".into(),
            question: "选择功能？".into(),
            header: "功能".into(),
            options: vec![
                QuestionOption {
                    label: "a".into(),
                    description: Some("desc-a".into()),
                },
                QuestionOption {
                    label: "b".into(),
                    description: None,
                },
            ],
            multi_select: true,
        },
        QuestionItem {
            id: "text".into(),
            question: "补充说明？".into(),
            header: "补充".into(),
            options: vec![],
            multi_select: false,
        },
    ]
}

// ─── build_elicitation_params：schema 构造 ───

#[test]
fn test_build_elicitation_params_single_select_form() {
    let params = crate::broker::transport_broker::build_elicitation_params(
        &sample_questions(),
        SessionId::new("s1"),
    );

    // 顶层协议形状：mode=form + sessionId scope + message
    assert_eq!(params["mode"], "form");
    assert_eq!(params["sessionId"], "s1");
    assert_eq!(
        params["message"],
        "Please provide the requested information"
    );

    // 单选：type=string + oneOf（const/title）+ title/description
    let choice = &params["requestedSchema"]["properties"]["choice"];
    assert_eq!(choice["type"], "string");
    assert_eq!(choice["title"], "部署环境");
    assert_eq!(choice["description"], "选择部署环境？");
    assert_eq!(choice["oneOf"][0]["const"], "生产");
    assert_eq!(choice["oneOf"][0]["title"], "生产");
    assert_eq!(choice["oneOf"][0]["description"], "生产环境");
    assert_eq!(choice["oneOf"][1]["const"], "测试");
}

#[test]
fn test_build_elicitation_params_multi_select_and_free_text() {
    let params = crate::broker::transport_broker::build_elicitation_params(
        &sample_questions(),
        SessionId::new("s1"),
    );
    let props = &params["requestedSchema"]["properties"];

    // 多选：type=array + items.anyOf，option description 注入 items 层
    let multi = &props["multi"];
    assert_eq!(multi["type"], "array");
    assert_eq!(multi["title"], "功能");
    assert_eq!(multi["items"]["anyOf"][0]["const"], "a");
    assert_eq!(multi["items"]["anyOf"][0]["description"], "desc-a");
    assert!(multi["items"]["anyOf"][1].get("description").is_none());

    // 自由文本：type=string，无 oneOf/anyOf
    let text = &props["text"];
    assert_eq!(text["type"], "string");
    assert_eq!(text["title"], "补充");
    assert_eq!(text["description"], "补充说明？");
    assert!(text.get("oneOf").is_none());
}

#[test]
fn test_build_elicitation_params_empty_questions() {
    let params =
        crate::broker::transport_broker::build_elicitation_params(&[], SessionId::new("s1"));
    assert_eq!(params["mode"], "form");
    assert_eq!(params["sessionId"], "s1");
    assert_eq!(
        params["requestedSchema"]["properties"],
        json!({}),
        "空问题集 → 空 schema"
    );
}

// ─── parse_elicitation_response：accept ───

#[test]
fn test_parse_accept_maps_text_and_selected() {
    let response = crate::broker::transport_broker::parse_elicitation_response(
        json!({
            "action": "accept",
            "content": {
                "choice": "生产",
                "multi": ["a", "b"],
                "text": "自由补充",
            }
        }),
        sample_questions(),
    );

    let InteractionResponse::Answers(answers) = response else {
        panic!("accept 应返回 Answers: {response:?}");
    };
    assert_eq!(answers.len(), 3);
    // 单选 → text；多选 → selected；自由文本 → text
    assert_eq!(answers[0].id, "choice");
    assert_eq!(answers[0].text.as_deref(), Some("生产"));
    assert!(answers[0].selected.is_empty());
    assert_eq!(answers[1].id, "multi");
    assert_eq!(answers[1].selected, vec!["a", "b"]);
    assert!(answers[1].text.is_none());
    assert_eq!(answers[2].id, "text");
    assert_eq!(answers[2].text.as_deref(), Some("自由补充"));
}

#[test]
fn test_parse_accept_boolean_and_integer_to_text() {
    // map_elicitation_answer 的 Boolean/Integer 分支 → text（字符串化）。
    let response = crate::broker::transport_broker::parse_elicitation_response(
        json!({
            "action": "accept",
            "content": { "choice": true, "text": 42 }
        }),
        sample_questions(),
    );
    let InteractionResponse::Answers(answers) = response else {
        panic!("accept 应返回 Answers: {response:?}");
    };
    assert_eq!(answers[0].text.as_deref(), Some("true"));
    assert_eq!(answers[2].text.as_deref(), Some("42"));
}

#[test]
fn test_parse_accept_missing_content_keeps_none() {
    // content 缺失/缺 q_id → text=None（不兜底空串；空串只来自真实的空提交）。
    let response = crate::broker::transport_broker::parse_elicitation_response(
        json!({ "action": "accept" }),
        sample_questions(),
    );
    let InteractionResponse::Answers(answers) = response else {
        panic!("accept 应返回 Answers: {response:?}");
    };
    for a in &answers {
        assert!(a.text.is_none(), "缺 content → text=None: {a:?}");
        assert!(a.selected.is_empty());
    }
}

#[test]
fn test_parse_accept_empty_submission_is_valid_empty_answer() {
    // 用户真的提交了空内容：accept 仍是合法答案，保持空语义，不得升级为失败。
    let response = crate::broker::transport_broker::parse_elicitation_response(
        json!({
            "action": "accept",
            "content": { "choice": "", "multi": [], "text": "" }
        }),
        sample_questions(),
    );
    let InteractionResponse::Answers(answers) = response else {
        panic!("真实空提交必须保持 Answers: {response:?}");
    };
    assert_eq!(answers.len(), 3);
    assert_eq!(answers[0].text.as_deref(), Some(""));
    assert!(answers[1].selected.is_empty());
    assert_eq!(answers[2].text.as_deref(), Some(""));
}

// ─── parse_elicitation_response：decline / cancel / 兜底 ───

#[test]
fn test_parse_decline_rejected() {
    let response = crate::broker::transport_broker::parse_elicitation_response(
        json!({ "action": "decline" }),
        sample_questions(),
    );
    assert!(
        matches!(response, InteractionResponse::Rejected),
        "decline 应返回 Rejected: {response:?}"
    );
}

/// [回归测试] 裸 `cancel`（无 `_meta`：TUI Esc、owner 失效 / session/prompt
/// 取消等生命周期结算）是用户明确撤销提问 ⇒ 必须产出 `Rejected`。旧行为回落
/// 空 `Answers`，让模型把「取消」误读成「用户提交了空答案」。
#[test]
fn test_parse_bare_cancel_is_rejected() {
    let response = crate::broker::transport_broker::parse_elicitation_response(
        json!({ "action": "cancel" }),
        sample_questions(),
    );
    assert!(
        matches!(response, InteractionResponse::Rejected),
        "裸 cancel 必须是明确拒绝，不得回落空 Answers: {response:?}"
    );
}

/// [回归测试] 非交互客户端（`-p`）的 cancel 用 wire 字面键值声明了原因时，必须
/// 产出 `Unanswered` —— 若落回空 `Answers`（或降级成 `Rejected`），工具会把
/// 「无人可作答」说成用户回答或用户拒绝。参数化覆盖字面 wire 键/值、未知取值与
/// 取值形态畸形的声明：三种都表示「已声明无人可作答」，都必须产出 `Unanswered`。
#[test]
fn test_parse_cancel_with_unanswered_cause() {
    for (label, cause_value) in [
        ("字面 wire 键/值", json!("non_interactive_client")),
        ("更新的客户端", json!("some_future_client")),
        ("取值畸形", json!({"nested": "object"})),
    ] {
        for wire in [
            json!({ "action": "cancel", "_meta": { "peri.elicitationUnanswered": cause_value } }),
            json!({ "action": "cancel", "_meta": { "peri.elicitationUnanswered": cause_value, "other": 1 } }),
        ] {
            let response = crate::broker::transport_broker::parse_elicitation_response(
                wire.clone(),
                sample_questions(),
            );
            assert!(
                matches!(response, InteractionResponse::Unanswered { .. }),
                "{label} 的 cancel 必须产出 Unanswered: {wire} → {response:?}"
            );
        }
    }
}

/// 已知取值只映射到已知原因；无法识别的取值收敛成封闭的 `Unknown`，不把客户端
/// 自由文本原样转述给模型。
#[test]
fn test_parse_cancel_unanswered_cause_values() {
    let known = crate::broker::transport_broker::parse_elicitation_response(
        json!({
            "action": "cancel",
            "_meta": { "peri.elicitationUnanswered": "non_interactive_client" }
        }),
        sample_questions(),
    );
    assert!(
        matches!(
            known,
            InteractionResponse::Unanswered {
                cause: UnansweredCause::NonInteractiveClient
            }
        ),
        "wire 字面值 non_interactive_client 必须映射到已知原因: {known:?}"
    );

    let unknown = crate::broker::transport_broker::parse_elicitation_response(
        json!({
            "action": "cancel",
            "_meta": { "peri.elicitationUnanswered": "no_free_text_ever" }
        }),
        sample_questions(),
    );
    let InteractionResponse::Unanswered { cause } = unknown else {
        panic!("无法识别的取值仍必须产出 Unanswered: {unknown:?}");
    };
    assert_eq!(
        cause,
        UnansweredCause::Unknown,
        "无法识别的取值必须收敛为 Unknown: {cause:?}"
    );
    assert!(
        !cause.reason_text().contains("no_free_text_ever"),
        "不得转述客户端自由文本: {}",
        cause.reason_text()
    );
}

/// 缺声明、decline、accept、其它 `_meta` 键与 parse 失败都不得被误判成
/// 「无人可作答」：只有显式声明（`peri.elicitationUnanswered` 键存在）才算。
#[test]
fn test_parse_does_not_infer_unanswered_without_declaration() {
    let cases = [
        (
            "无 _meta 的 cancel（旧客户端/生命周期结算）",
            json!({ "action": "cancel" }),
        ),
        (
            "带无关 _meta 的 cancel",
            json!({ "action": "cancel", "_meta": { "peri.other": true } }),
        ),
        ("decline 不是无人可作答", json!({ "action": "decline" })),
        (
            "decline 带声明也不覆盖拒绝语义",
            json!({ "action": "decline", "_meta": { "peri.elicitationUnanswered": "non_interactive_client" } }),
        ),
        (
            "accept 不是无人可作答",
            json!({ "action": "accept", "content": { "choice": "生产" } }),
        ),
        ("未知 action", json!({ "action": "some_future_action" })),
        ("响应体畸形", json!({ "action": 7 })),
    ];
    for (label, wire) in cases {
        let response = crate::broker::transport_broker::parse_elicitation_response(
            wire.clone(),
            sample_questions(),
        );
        assert!(
            !matches!(response, InteractionResponse::Unanswered { .. }),
            "{label} 不得产出 Unanswered: {wire} → {response:?}"
        );
    }
}

/// [回归测试] 只有声明 `peri.elicitationUnanswered` 的 cancel 才是
/// `Unanswered`；无该声明的 cancel（含带无关 `_meta` 键）一律是明确拒绝
/// （`Rejected`），既不是「无人可作答」也不是空 `Answers`。
#[test]
fn test_parse_cancel_without_declaration_is_rejected() {
    for wire in [
        json!({ "action": "cancel", "_meta": { "peri.other": true } }),
        json!({ "action": "cancel", "_meta": {} }),
    ] {
        let response = crate::broker::transport_broker::parse_elicitation_response(
            wire.clone(),
            sample_questions(),
        );
        assert!(
            matches!(response, InteractionResponse::Rejected),
            "无声明 cancel 必须是 Rejected: {wire} → {response:?}"
        );
    }
}

#[test]
fn test_parse_permission_cancelled_maps_to_cancel_reject() {
    let response: RequestPermissionResponse = serde_json::from_value(json!({
        "outcome": { "outcome": "cancelled" }
    }))
    .unwrap();
    let decision = crate::broker::transport_broker::map_permission_response(response);
    let ApprovalDecision::Reject { reason, source } = decision else {
        panic!("cancelled permission 应映射为 Reject")
    };
    assert!(reason.contains("Cancelled by user"));
    assert_eq!(source, None);
}

#[test]
fn test_parse_unknown_action_returns_protocol_failure() {
    // 未知 action（Other 变体）是协议错误，不是用户提交空答案。
    let response = crate::broker::transport_broker::parse_elicitation_response(
        json!({ "action": "mystery" }),
        sample_questions(),
    );
    let InteractionResponse::Failed { failure } = response else {
        panic!("未知 action 必须显式失败: {response:?}");
    };
    let InteractionFailure::Transport { detail } = failure else {
        panic!("未知 action 属传输/协议失败: {failure:?}");
    };
    assert!(
        detail.contains("unsupported elicitation action"),
        "失败细节必须说明协议错误: {detail}"
    );
}

#[test]
fn test_parse_invalid_response_returns_protocol_failure() {
    // 响应解析失败是协议错误 → 显式失败（不 panic、不伪造空答案）。
    let response = crate::broker::transport_broker::parse_elicitation_response(
        json!({ "not": "a response" }),
        sample_questions(),
    );
    let InteractionResponse::Failed { failure } = response else {
        panic!("解析失败必须显式失败: {response:?}");
    };
    let InteractionFailure::Transport { detail } = failure else {
        panic!("解析失败属传输/协议失败: {failure:?}");
    };
    assert!(
        detail.contains("invalid elicitation response"),
        "失败细节必须说明协议错误: {detail}"
    );
}

// ─── AcpTransportBroker 行为：ApprovalMode / timeout（mock transport） ───

use std::future::Future;
use std::sync::Mutex;
use std::task::Poll;

use crate::transport::types::AcpError;
use crate::transport::RequestTransport;
use tokio::sync::{mpsc, oneshot, Semaphore};

/// Mock `RequestTransport`：记录调用；`Behavior::Respond` 回固定响应，
/// `Behavior::Pending` 永不完成（模拟客户端不响应）。
struct MockTransport {
    calls: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    behavior: Behavior,
}

enum Behavior {
    Respond(serde_json::Value),
    Pending,
}

#[async_trait::async_trait]
impl RequestTransport for MockTransport {
    async fn send_request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, AcpError> {
        self.calls
            .lock()
            .unwrap()
            .push((method.to_string(), params));
        match &self.behavior {
            Behavior::Respond(v) => Ok(v.clone()),
            Behavior::Pending => std::future::pending().await,
        }
    }
}

fn approval_context() -> InteractionContext {
    InteractionContext::Approval {
        items: vec![ApprovalItem {
            tool_call_id: "call_1".into(),
            tool_name: "Bash".into(),
            tool_input: json!({"command": "ls"}),
            mcp_origin: None,
        }],
    }
}

fn approval_context_with_items(count: usize) -> InteractionContext {
    InteractionContext::Approval {
        items: (0..count)
            .map(|index| ApprovalItem {
                tool_call_id: format!("call_{index}"),
                tool_name: "Bash".into(),
                tool_input: json!({"command": format!("echo {index}")}),
                mcp_origin: None,
            })
            .collect(),
    }
}

fn questions_context() -> InteractionContext {
    InteractionContext::Questions {
        requests: sample_questions(),
    }
}

/// 可因果控制 transport 入口和完成时机的 broker gate 测试夹具。
///
/// 每次请求进入后立刻记录并通知观察端，再消耗一个显式 release permit；
/// `forget` 保证完成的请求不会把 permit 自动归还给下一请求。
struct CausalTransport {
    calls: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    entered_tx: mpsc::UnboundedSender<String>,
    releases: Arc<Semaphore>,
}

#[async_trait::async_trait]
impl RequestTransport for CausalTransport {
    async fn send_request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, AcpError> {
        self.calls
            .lock()
            .unwrap()
            .push((method.to_string(), params));
        let _ = self.entered_tx.send(method.to_string());

        self.releases
            .clone()
            .acquire_owned()
            .await
            .expect("causal transport release semaphore must remain open")
            .forget();

        match method {
            "session/request_permission" => Ok(json!({
                "outcome": { "outcome": "selected", "optionId": "allow_once" }
            })),
            "elicitation/create" => Ok(json!({ "action": "accept", "content": {} })),
            other => panic!("unexpected causal transport method: {other}"),
        }
    }
}

struct CausalFixture {
    transport: Arc<CausalTransport>,
    entered_rx: mpsc::UnboundedReceiver<String>,
    calls: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    releases: Arc<Semaphore>,
}

fn causal_fixture() -> CausalFixture {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let releases = Arc::new(Semaphore::new(0));
    let (entered_tx, entered_rx) = mpsc::unbounded_channel();
    let transport = Arc::new(CausalTransport {
        calls: Arc::clone(&calls),
        entered_tx,
        releases: Arc::clone(&releases),
    });
    CausalFixture {
        transport,
        entered_rx,
        calls,
        releases,
    }
}

async fn next_entered(rx: &mut mpsc::UnboundedReceiver<String>) -> String {
    tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("transport entry must not hang")
        .expect("causal transport observation channel must remain open")
}

/// Poll a nested broker request once and mark only after that poll returned Pending.
async fn request_with_first_pending_marker(
    broker: Arc<dyn UserInteractionBroker>,
    context: InteractionContext,
    marker: oneshot::Sender<()>,
) -> InteractionResponse {
    let request = broker.request(owner(), context);
    tokio::pin!(request);
    let mut marker = Some(marker);
    std::future::poll_fn(|cx| match request.as_mut().poll(cx) {
        Poll::Pending => {
            if let Some(marker) = marker.take() {
                let _ = marker.send(());
            }
            Poll::Pending
        }
        Poll::Ready(response) => Poll::Ready(response),
    })
    .await
}

#[tokio::test]
async fn test_interaction_gate_serializes_whole_approval_before_questions() {
    let CausalFixture {
        transport,
        mut entered_rx,
        calls,
        releases,
        ..
    } = causal_fixture();
    let broker = Arc::new(AcpTransportBroker::new(
        transport,
        SessionId::new("s1"),
        interaction_gate(),
    ));

    let first = tokio::spawn({
        let broker = Arc::clone(&broker);
        async move {
            broker
                .request(owner(), approval_context_with_items(2))
                .await
        }
    });
    assert_eq!(
        next_entered(&mut entered_rx).await,
        "session/request_permission"
    );

    let (pending_tx, pending_rx) = oneshot::channel();
    let second = tokio::spawn(request_with_first_pending_marker(
        broker.clone(),
        questions_context(),
        pending_tx,
    ));
    pending_rx
        .await
        .expect("second request must report its first Pending poll");
    assert_eq!(
        calls.lock().unwrap().len(),
        1,
        "Questions must not enter transport while the whole Approval context owns the gate"
    );

    releases.add_permits(1);
    assert_eq!(
        next_entered(&mut entered_rx).await,
        "session/request_permission"
    );
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .map(|(method, _)| method.as_str())
            .collect::<Vec<_>>(),
        vec!["session/request_permission", "session/request_permission"]
    );

    releases.add_permits(1);
    let InteractionResponse::Decisions(decisions) = first.await.unwrap() else {
        panic!("first approval must return Decisions");
    };
    assert_eq!(decisions.len(), 2);

    assert_eq!(next_entered(&mut entered_rx).await, "elicitation/create");
    releases.add_permits(1);
    assert!(matches!(
        second.await.unwrap(),
        InteractionResponse::Answers(_)
    ));
}

#[tokio::test]
async fn test_interaction_gate_cancelled_waiter_does_not_block_third_request() {
    let CausalFixture {
        transport,
        mut entered_rx,
        calls,
        releases,
        ..
    } = causal_fixture();
    let broker = Arc::new(AcpTransportBroker::new(
        transport,
        SessionId::new("s1"),
        interaction_gate(),
    ));

    let first = tokio::spawn({
        let broker = Arc::clone(&broker);
        async move { broker.request(owner(), approval_context()).await }
    });
    assert_eq!(
        next_entered(&mut entered_rx).await,
        "session/request_permission"
    );

    let (pending_tx, pending_rx) = oneshot::channel();
    let second = tokio::spawn(request_with_first_pending_marker(
        broker.clone(),
        questions_context(),
        pending_tx,
    ));
    pending_rx
        .await
        .expect("second request must report its first Pending poll");
    assert_eq!(
        calls.lock().unwrap().len(),
        1,
        "queued waiter must not enter transport before the active interaction settles"
    );

    second.abort();
    let error = second.await.expect_err("aborted waiter must be cancelled");
    assert!(error.is_cancelled());

    releases.add_permits(1);
    assert!(matches!(
        first.await.unwrap(),
        InteractionResponse::Decisions(_)
    ));

    let third = tokio::spawn({
        let broker = Arc::clone(&broker);
        async move { broker.request(owner(), questions_context()).await }
    });
    assert_eq!(next_entered(&mut entered_rx).await, "elicitation/create");
    releases.add_permits(1);
    assert!(matches!(
        third.await.unwrap(),
        InteractionResponse::Answers(_)
    ));
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .map(|(method, _)| method.as_str())
            .collect::<Vec<_>>(),
        vec!["session/request_permission", "elicitation/create"]
    );
}

#[path = "transport_broker_session_gate_test.rs"]
mod session_gate;

#[tokio::test]
async fn test_auto_approve_bypasses_busy_transport_gate() {
    let CausalFixture {
        transport,
        mut entered_rx,
        calls,
        releases,
        ..
    } = causal_fixture();
    let broker = Arc::new(
        AcpTransportBroker::new(transport, SessionId::new("s1"), interaction_gate())
            .with_auto_approve(),
    );

    let questions = tokio::spawn({
        let broker = Arc::clone(&broker);
        async move { broker.request(owner(), questions_context()).await }
    });
    assert_eq!(next_entered(&mut entered_rx).await, "elicitation/create");

    let approval = broker.request(owner(), approval_context_with_items(2));
    tokio::pin!(approval);
    let first_poll = std::future::poll_fn(|cx| Poll::Ready(approval.as_mut().poll(cx))).await;
    let Poll::Ready(InteractionResponse::Decisions(decisions)) = first_poll else {
        panic!("AutoApprove must be Ready on its first poll while the transport gate is busy");
    };
    assert_eq!(decisions.len(), 2);
    assert!(decisions
        .iter()
        .all(|decision| matches!(decision, ApprovalDecision::Approve { source: None })));
    assert_eq!(
        calls.lock().unwrap().len(),
        1,
        "AutoApprove must not emit a transport request"
    );

    releases.add_permits(1);
    assert!(matches!(
        questions.await.unwrap(),
        InteractionResponse::Answers(_)
    ));
}

#[tokio::test]
async fn test_approval_mode_auto_approve_never_calls_transport() {
    // stdio 装配（with_auto_approve）：全部 Approve 且零 transport 调用。
    let calls: Arc<Mutex<Vec<(String, serde_json::Value)>>> = Arc::new(Mutex::new(vec![]));
    let transport = MockTransport {
        calls: Arc::clone(&calls),
        behavior: Behavior::Pending,
    };
    let broker = AcpTransportBroker::new(
        Arc::new(transport),
        SessionId::new("s1"),
        interaction_gate(),
    )
    .with_auto_approve();
    let resp = broker.request(owner(), approval_context()).await;
    let InteractionResponse::Decisions(decisions) = resp else {
        panic!("auto-approve 应返回 Decisions: {resp:?}");
    };
    assert_eq!(decisions.len(), 1);
    assert!(
        matches!(decisions[0], ApprovalDecision::Approve { source: None }),
        "auto-approve 应为 Approve: {:?}",
        decisions[0]
    );
    assert!(
        calls.lock().unwrap().is_empty(),
        "auto-approve 不应发起任何 transport 请求"
    );
}

#[tokio::test]
async fn test_approval_mode_forward_sends_request_permission() {
    // mpsc/TUI 默认装配（Forward）：经 request_permission 转发，accept → Approve。
    let calls: Arc<Mutex<Vec<(String, serde_json::Value)>>> = Arc::new(Mutex::new(vec![]));
    let transport = MockTransport {
        calls: Arc::clone(&calls),
        // ACP schema 真实 wire 格式（见 peri-tui hitl_response.rs 协议注释）。
        behavior: Behavior::Respond(json!({
            "outcome": { "outcome": "selected", "optionId": "allow_once" }
        })),
    };
    let broker = AcpTransportBroker::new(
        Arc::new(transport),
        SessionId::new("s1"),
        interaction_gate(),
    );
    let resp = broker.request(owner(), approval_context()).await;
    let InteractionResponse::Decisions(decisions) = resp else {
        panic!("forward 应返回 Decisions: {resp:?}");
    };
    assert!(matches!(decisions[0], ApprovalDecision::Approve { .. }));
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "session/request_permission");
}

#[tokio::test]
async fn test_questions_timeout_returns_timeout_failure() {
    // 客户端存活但不响应：超时是通道失败，不是 Rejected（用户拒绝）或空答案。
    let calls: Arc<Mutex<Vec<(String, serde_json::Value)>>> = Arc::new(Mutex::new(vec![]));
    let transport = MockTransport {
        calls: Arc::clone(&calls),
        behavior: Behavior::Pending,
    };
    let broker = AcpTransportBroker::new(
        Arc::new(transport),
        SessionId::new("s1"),
        interaction_gate(),
    )
    .with_timeout(Some(Duration::from_millis(50)));
    let resp = broker
        .request(
            owner(),
            InteractionContext::Questions {
                requests: sample_questions(),
            },
        )
        .await;
    assert!(
        matches!(
            resp,
            InteractionResponse::Failed {
                failure: InteractionFailure::Timeout
            }
        ),
        "超时必须显式失败: {resp:?}"
    );
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "elicitation/create");
}

/// 提问链路（不只纯函数）：客户端回裸 `cancel` 时 broker 必须按明确拒绝结算，
/// 不得产生空 `Answers`。
#[tokio::test]
async fn test_questions_bare_cancel_returns_rejected() {
    let calls: Arc<Mutex<Vec<(String, serde_json::Value)>>> = Arc::new(Mutex::new(vec![]));
    let transport = MockTransport {
        calls: Arc::clone(&calls),
        behavior: Behavior::Respond(json!({ "action": "cancel" })),
    };
    let broker = AcpTransportBroker::new(
        Arc::new(transport),
        SessionId::new("s1"),
        interaction_gate(),
    );
    let resp = broker.request(owner(), questions_context()).await;
    assert!(
        matches!(resp, InteractionResponse::Rejected),
        "裸 cancel 必须按明确拒绝结算: {resp:?}"
    );
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "elicitation/create");
}

/// `decline` 仍是明确拒绝（与裸 cancel 同结果，但由不同 wire 动作触发）。
#[tokio::test]
async fn test_questions_decline_returns_rejected() {
    let calls: Arc<Mutex<Vec<(String, serde_json::Value)>>> = Arc::new(Mutex::new(vec![]));
    let transport = MockTransport {
        calls: Arc::clone(&calls),
        behavior: Behavior::Respond(json!({ "action": "decline" })),
    };
    let broker = AcpTransportBroker::new(
        Arc::new(transport),
        SessionId::new("s1"),
        interaction_gate(),
    );
    let resp = broker.request(owner(), questions_context()).await;
    assert!(
        matches!(resp, InteractionResponse::Rejected),
        "decline 必须保持 Rejected: {resp:?}"
    );
}

/// 带 `_meta` 的 cancel（`-p` 非交互声明）仍走 `Unanswered`，不得升级为拒绝。
#[tokio::test]
async fn test_questions_cancel_with_unanswered_meta_returns_unanswered() {
    let calls: Arc<Mutex<Vec<(String, serde_json::Value)>>> = Arc::new(Mutex::new(vec![]));
    let transport = MockTransport {
        calls: Arc::clone(&calls),
        behavior: Behavior::Respond(json!({
            "action": "cancel",
            "_meta": { "peri.elicitationUnanswered": "non_interactive_client" }
        })),
    };
    let broker = AcpTransportBroker::new(
        Arc::new(transport),
        SessionId::new("s1"),
        interaction_gate(),
    );
    let resp = broker.request(owner(), questions_context()).await;
    assert!(
        matches!(
            resp,
            InteractionResponse::Unanswered {
                cause: UnansweredCause::NonInteractiveClient
            }
        ),
        "带声明的 cancel 必须保持 Unanswered: {resp:?}"
    );
}

#[tokio::test]
async fn test_questions_timeout_notifies_client_cancel_request() {
    // 回归：超时放弃 elicitation 后必须通知客户端撤销，否则 TUI 面板会永久
    // pending，后续输入无法作为普通 prompt 发出。
    use crate::transport::mpsc as mpsc_functions;
    use crate::transport::types::{IncomingMessage, RequestId};
    use crate::transport::{AcpRequestBridge, AcpTransport};

    let (client, server) = mpsc_functions::mpsc_transport_pair();
    let broker = AcpTransportBroker::new(
        Arc::new(AcpRequestBridge(Arc::new(server))),
        SessionId::new("s1"),
        interaction_gate(),
    )
    .with_timeout(Some(Duration::from_millis(50)));
    let resp = broker
        .request(
            owner(),
            InteractionContext::Questions {
                requests: sample_questions(),
            },
        )
        .await;
    assert!(matches!(
        resp,
        InteractionResponse::Failed {
            failure: InteractionFailure::Timeout
        }
    ));

    let IncomingMessage::Request { id, method, .. } = client.recv().await.unwrap() else {
        panic!("expected elicitation request");
    };
    assert_eq!(method, "elicitation/create");
    let message = tokio::time::timeout(Duration::from_secs(1), client.recv())
        .await
        .expect("超时后必须送达取消通知")
        .unwrap();
    let IncomingMessage::Notification { method, params } = message else {
        panic!("expected cancel notification");
    };
    assert_eq!(method, "$/cancel_request");
    assert_eq!(
        serde_json::from_value::<RequestId>(params["requestId"].clone()).unwrap(),
        id
    );
}

/// 固定错误码的 transport：返回指定 `AcpError`（模拟网关能力缺失/断连）。
struct ErrorTransport {
    code: i64,
    message: &'static str,
}

#[async_trait::async_trait]
impl RequestTransport for ErrorTransport {
    async fn send_request(
        &self,
        method: &str,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value, AcpError> {
        assert_eq!(method, "elicitation/create");
        Err(AcpError::new(self.code, self.message))
    }
}

async fn question_response_with_error(transport: ErrorTransport) -> InteractionResponse {
    let broker = AcpTransportBroker::new(
        Arc::new(transport),
        SessionId::new("s1"),
        interaction_gate(),
    );
    broker
        .request(
            owner(),
            InteractionContext::Questions {
                requests: sample_questions(),
            },
        )
        .await
}

#[tokio::test]
async fn test_questions_method_not_found_returns_method_unsupported_failure() {
    // 网关回 -32601（method not found）：能力缺失，显式失败，不是空答案。
    let resp = question_response_with_error(ErrorTransport {
        code: -32601,
        message: "Method not found",
    })
    .await;
    let InteractionResponse::Failed { failure } = resp else {
        panic!("-32601 必须显式失败: {resp:?}");
    };
    assert!(
        matches!(failure, InteractionFailure::MethodUnsupported),
        "应分类为方法不支持: {failure:?}"
    );
}

#[tokio::test]
async fn test_questions_transport_error_returns_transport_failure() {
    // transport 断连（send_request 报错）：显式传输失败，不是空答案。
    let resp = question_response_with_error(ErrorTransport {
        code: -32000,
        message: "transport closed",
    })
    .await;
    let InteractionResponse::Failed { failure } = resp else {
        panic!("transport 错误必须显式失败: {resp:?}");
    };
    let InteractionFailure::Transport { detail } = failure else {
        panic!("应分类为传输失败: {failure:?}");
    };
    assert!(
        detail.contains("-32000") && detail.contains("transport closed"),
        "失败细节必须保留协议码与原因: {detail}"
    );
}

// ─── PERI_ASK_USER_TIMEOUT_SECS 解析（批 4：统一 broker 恢复提问超时兜底）───

/// 纯逻辑形态：缺省 300 / 非法回落 300 / `0` → None / 合法值。
/// 语义与批 3 删除的 `host/stdio/context.rs::parse_ask_user_timeout` 完全一致。
#[test]
fn test_parse_ask_user_timeout_env_values() {
    assert_eq!(
        crate::broker::transport_broker::parse_ask_user_timeout(None),
        Some(Duration::from_secs(300)),
        "缺失 → 默认 300"
    );
    assert_eq!(
        crate::broker::transport_broker::parse_ask_user_timeout(Some("300")),
        Some(Duration::from_secs(300))
    );
    assert_eq!(
        crate::broker::transport_broker::parse_ask_user_timeout(Some("1")),
        Some(Duration::from_secs(1))
    );
    assert_eq!(
        crate::broker::transport_broker::parse_ask_user_timeout(Some("0")),
        None,
        "0 → 不超时"
    );
    assert_eq!(
        crate::broker::transport_broker::parse_ask_user_timeout(Some("abc")),
        Some(Duration::from_secs(300)),
        "非法值回落 300"
    );
    assert_eq!(
        crate::broker::transport_broker::parse_ask_user_timeout(Some("-5")),
        Some(Duration::from_secs(300)),
        "负数解析失败回落 300"
    );
    assert_eq!(
        crate::broker::transport_broker::parse_ask_user_timeout(Some("")),
        Some(Duration::from_secs(300))
    );
}

/// env 读取接线（serial 串行：`std::env::set_var` 为进程级全局，避免并行竞态）。
#[serial_test::serial]
#[test]
fn test_ask_user_timeout_reads_env() {
    std::env::set_var("PERI_ASK_USER_TIMEOUT_SECS", "0");
    assert_eq!(crate::broker::transport_broker::ask_user_timeout(), None);
    std::env::set_var("PERI_ASK_USER_TIMEOUT_SECS", "42");
    assert_eq!(
        crate::broker::transport_broker::ask_user_timeout(),
        Some(Duration::from_secs(42))
    );
    std::env::remove_var("PERI_ASK_USER_TIMEOUT_SECS");
    assert_eq!(
        crate::broker::transport_broker::ask_user_timeout(),
        Some(Duration::from_secs(300))
    );
}

// ─── 外部 MCP elicitation 投影（#589） ───

fn mcp_form_context() -> InteractionContext {
    InteractionContext::Elicitation {
        request: peri_acp_types::interaction::ElicitationRequest::Form {
            server: "expense".into(),
            tool: "submit_expense".into(),
            message: "Approve this expense?".into(),
            requested_schema: json!({
                "type": "object",
                "properties": {
                    "approve": { "type": "boolean", "title": "Approve" },
                    "note": { "type": "string", "title": "Note" }
                },
                "required": ["approve"]
            }),
        },
    }
}

#[tokio::test]
async fn mcp_form_projects_schema_and_origin_marker() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let broker = AcpTransportBroker::new(
        Arc::new(MockTransport {
            calls: Arc::clone(&calls),
            behavior: Behavior::Respond(json!({
                "action": "accept",
                "content": { "approve": false, "note": "not now" }
            })),
        }),
        SessionId::new("s1"),
        interaction_gate(),
    );

    let response = broker.request(owner(), mcp_form_context()).await;
    let InteractionResponse::Elicitation(
        peri_acp_types::interaction::ElicitationDecision::Accept { content },
    ) = response
    else {
        panic!("expected accept, got {response:?}");
    };
    assert_eq!(content, json!({ "approve": false, "note": "not now" }));

    let calls = calls.lock().unwrap();
    let (method, params) = &calls[0];
    assert_eq!(method, "elicitation/create");
    assert_eq!(params["mode"], "form");
    assert_eq!(params["message"], "Approve this expense?");
    assert_eq!(params["sessionId"], "s1");
    assert_eq!(
        params["requestedSchema"]["properties"]["approve"]["type"],
        "boolean"
    );
    assert_eq!(
        params["_meta"]["io.peri/mcpElicitation"],
        json!({ "server": "expense", "tool": "submit_expense" })
    );
}

#[tokio::test]
async fn mcp_url_projects_consent_card_and_preserves_id() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let broker = AcpTransportBroker::new(
        Arc::new(MockTransport {
            calls: Arc::clone(&calls),
            behavior: Behavior::Respond(json!({ "action": "accept" })),
        }),
        SessionId::new("s1"),
        interaction_gate(),
    );
    let response = broker
        .request(
            owner(),
            InteractionContext::Elicitation {
                request: peri_acp_types::interaction::ElicitationRequest::Url {
                    server: "oauth".into(),
                    tool: "connect".into(),
                    message: "Authorize in browser".into(),
                    url: "https://example.com/connect".into(),
                    elicitation_id: "e-1".into(),
                },
            },
        )
        .await;
    assert!(matches!(
        response,
        InteractionResponse::Elicitation(
            peri_acp_types::interaction::ElicitationDecision::Accept { .. }
        )
    ));
    let calls = calls.lock().unwrap();
    let params = &calls[0].1;
    assert_eq!(params["mode"], "url");
    assert_eq!(params["elicitationId"], "e-1");
    assert_eq!(params["url"], "https://example.com/connect");
    assert_eq!(
        params["_meta"]["io.peri/mcpElicitation"],
        json!({ "server": "oauth", "tool": "connect" })
    );
}

#[test]
fn mcp_elicitation_cancel_keeps_cancel_and_decline_stays_decline() {
    use crate::broker::transport_broker::parse_mcp_elicitation_response;
    use peri_acp_types::interaction::ElicitationDecision;

    let cancel = parse_mcp_elicitation_response(json!({ "action": "cancel" }));
    assert!(matches!(
        cancel,
        InteractionResponse::Elicitation(ElicitationDecision::Cancel)
    ));

    let decline = parse_mcp_elicitation_response(json!({ "action": "decline" }));
    assert!(matches!(
        decline,
        InteractionResponse::Elicitation(ElicitationDecision::Decline)
    ));
}

#[tokio::test]
async fn mcp_form_schema_that_acp_cannot_express_fails_closed() {
    let broker = AcpTransportBroker::new(
        Arc::new(MockTransport {
            calls: Arc::new(Mutex::new(Vec::new())),
            behavior: Behavior::Respond(json!({ "action": "decline" })),
        }),
        SessionId::new("s1"),
        interaction_gate(),
    );
    let response = broker
        .request(
            owner(),
            InteractionContext::Elicitation {
                request: peri_acp_types::interaction::ElicitationRequest::Form {
                    server: "srv".into(),
                    tool: "tool".into(),
                    message: "nested".into(),
                    requested_schema: json!({
                        "type": "object",
                        "properties": {
                            "nested": { "type": "object", "properties": {} }
                        }
                    }),
                },
            },
        )
        .await;
    assert!(matches!(
        response,
        InteractionResponse::Failed {
            failure: InteractionFailure::Transport { .. }
        }
    ));
}
