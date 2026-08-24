//! Tests for `broker/transport_broker.rs` 提取的 elicitation 纯函数
//! （`build_elicitation_params` / `parse_elicitation_response`）。
//!
//! TUI/notify（mpsc）与 stdio 两路共用同一 `AcpTransportBroker` 及这两处
//! 逻辑（行为零变化复用面），纯函数单测直接锁住 schema 构造
//! （单选 oneOf / 多选 array+items.anyOf / 自由文本 + option description 注入）
//! 与响应解析语义（accept/decline/cancel/未知 action/解析失败兜底）。

use peri_acp_types::interaction::{InteractionResponse, QuestionItem, QuestionOption};
use serde_json::json;

use super::*;

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
    let params = build_elicitation_params(&sample_questions(), SessionId::new("s1"));

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
    let params = build_elicitation_params(&sample_questions(), SessionId::new("s1"));
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
    let params = build_elicitation_params(&[], SessionId::new("s1"));
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
    let response = parse_elicitation_response(
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
    let response = parse_elicitation_response(
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
    // content 缺失/缺 q_id → text=None（不兜底空串，区别于 cancel）。
    let response = parse_elicitation_response(json!({ "action": "accept" }), sample_questions());
    let InteractionResponse::Answers(answers) = response else {
        panic!("accept 应返回 Answers: {response:?}");
    };
    for a in &answers {
        assert!(a.text.is_none(), "缺 content → text=None: {a:?}");
        assert!(a.selected.is_empty());
    }
}

// ─── parse_elicitation_response：decline / cancel / 兜底 ───

#[test]
fn test_parse_decline_rejected() {
    let response = parse_elicitation_response(json!({ "action": "decline" }), sample_questions());
    assert!(
        matches!(response, InteractionResponse::Rejected),
        "decline 应返回 Rejected: {response:?}"
    );
}

#[test]
fn test_parse_cancel_empty_answers() {
    let response = parse_elicitation_response(json!({ "action": "cancel" }), sample_questions());
    let InteractionResponse::Answers(answers) = response else {
        panic!("cancel 应返回 Answers: {response:?}");
    };
    assert_eq!(answers.len(), 3);
    for a in &answers {
        assert!(a.selected.is_empty());
        assert_eq!(a.text.as_deref(), Some(""), "cancel → 空 Answers");
    }
}

#[test]
fn test_parse_unknown_action_empty_answers() {
    // 未知 action（Other 变体）→ 空 Answers 兜底。
    let response = parse_elicitation_response(json!({ "action": "mystery" }), sample_questions());
    let InteractionResponse::Answers(answers) = response else {
        panic!("未知 action 应返回空 Answers: {response:?}");
    };
    assert_eq!(answers.len(), 3);
    assert_eq!(answers[0].text.as_deref(), Some(""));
}

#[test]
fn test_parse_invalid_response_empty_answers() {
    // 响应解析失败 → 空 Answers 兜底（不 panic）。
    let response = parse_elicitation_response(json!({ "not": "a response" }), sample_questions());
    let InteractionResponse::Answers(answers) = response else {
        panic!("解析失败应返回空 Answers: {response:?}");
    };
    assert_eq!(answers.len(), 3);
    assert_eq!(answers[0].text.as_deref(), Some(""));
}

// ─── AcpTransportBroker 行为：ApprovalMode / timeout（mock transport） ───

use std::{
    future::{poll_fn, Future},
    sync::Mutex,
    task::Poll,
};

use crate::transport::types::AcpError;
use crate::transport::RequestTransport;
use peri_agent::interaction::MultiplexBroker;
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
    entered_signal: Arc<Semaphore>,
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
        self.entered_signal.add_permits(1);

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
    entered_signal: Arc<Semaphore>,
    releases: Arc<Semaphore>,
}

fn causal_fixture() -> CausalFixture {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let entered_signal = Arc::new(Semaphore::new(0));
    let releases = Arc::new(Semaphore::new(0));
    let (entered_tx, entered_rx) = mpsc::unbounded_channel();
    let transport = Arc::new(CausalTransport {
        calls: Arc::clone(&calls),
        entered_tx,
        entered_signal: Arc::clone(&entered_signal),
        releases: Arc::clone(&releases),
    });
    CausalFixture {
        transport,
        entered_rx,
        calls,
        entered_signal,
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
    let request = broker.request(context);
    tokio::pin!(request);
    let mut marker = Some(marker);
    poll_fn(|cx| match request.as_mut().poll(cx) {
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
    let broker = Arc::new(AcpTransportBroker::new(transport, SessionId::new("s1")));

    let first = tokio::spawn({
        let broker = Arc::clone(&broker);
        async move { broker.request(approval_context_with_items(2)).await }
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
    let broker = Arc::new(AcpTransportBroker::new(transport, SessionId::new("s1")));

    let first = tokio::spawn({
        let broker = Arc::clone(&broker);
        async move { broker.request(approval_context()).await }
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
        async move { broker.request(questions_context()).await }
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

#[tokio::test]
async fn test_interaction_gate_is_per_broker_instance() {
    let CausalFixture {
        transport,
        mut entered_rx,
        calls,
        releases,
        ..
    } = causal_fixture();
    let broker_a = Arc::new(AcpTransportBroker::new(
        transport.clone(),
        SessionId::new("session-a"),
    ));
    let broker_b = Arc::new(AcpTransportBroker::new(
        transport,
        SessionId::new("session-b"),
    ));

    let first = tokio::spawn(async move { broker_a.request(approval_context()).await });
    assert_eq!(
        next_entered(&mut entered_rx).await,
        "session/request_permission"
    );

    let (pending_tx, pending_rx) = oneshot::channel();
    let second = tokio::spawn(request_with_first_pending_marker(
        broker_b,
        questions_context(),
        pending_tx,
    ));
    pending_rx
        .await
        .expect("second broker must report its first Pending poll");
    assert_eq!(
        calls.lock().unwrap().len(),
        2,
        "separate broker instances must not share a global interaction gate"
    );
    assert_eq!(next_entered(&mut entered_rx).await, "elicitation/create");

    releases.add_permits(2);
    assert!(matches!(
        first.await.unwrap(),
        InteractionResponse::Decisions(_)
    ));
    assert!(matches!(
        second.await.unwrap(),
        InteractionResponse::Answers(_)
    ));
}

struct EnteredWinnerBroker {
    entered_signal: Arc<Semaphore>,
}

#[async_trait::async_trait]
impl UserInteractionBroker for EnteredWinnerBroker {
    async fn request(&self, context: InteractionContext) -> InteractionResponse {
        self.entered_signal
            .clone()
            .acquire_owned()
            .await
            .expect("causal entry semaphore must remain open")
            .forget();
        let InteractionContext::Approval { items } = context else {
            return InteractionResponse::Rejected;
        };
        InteractionResponse::Decisions(
            items
                .into_iter()
                .map(|_| ApprovalDecision::Approve { source: None })
                .collect(),
        )
    }
}

#[tokio::test]
async fn test_multiplex_winner_drop_releases_transport_broker_gate() {
    let CausalFixture {
        transport,
        mut entered_rx,
        entered_signal,
        releases,
        ..
    } = causal_fixture();
    let raw = Arc::new(AcpTransportBroker::new(transport, SessionId::new("s1")));
    let winner = Arc::new(EnteredWinnerBroker { entered_signal });
    let multiplex =
        MultiplexBroker::new(vec![("raw".into(), raw.clone()), ("winner".into(), winner)]);

    let response = multiplex.request(approval_context()).await;
    let InteractionResponse::Decisions(decisions) = response else {
        panic!("multiplex winner must return Decisions");
    };
    assert!(matches!(
        decisions.as_slice(),
        [ApprovalDecision::Approve { source: Some(source) }] if source == "winner"
    ));
    assert_eq!(
        next_entered(&mut entered_rx).await,
        "session/request_permission"
    );

    let next = tokio::spawn({
        let raw = Arc::clone(&raw);
        async move { raw.request(approval_context()).await }
    });
    assert_eq!(
        next_entered(&mut entered_rx).await,
        "session/request_permission"
    );
    releases.add_permits(1);
    assert!(matches!(
        next.await.unwrap(),
        InteractionResponse::Decisions(_)
    ));
}

#[tokio::test]
async fn test_auto_approve_bypasses_busy_transport_gate() {
    let CausalFixture {
        transport,
        mut entered_rx,
        calls,
        releases,
        ..
    } = causal_fixture();
    let broker =
        Arc::new(AcpTransportBroker::new(transport, SessionId::new("s1")).with_auto_approve());

    let questions = tokio::spawn({
        let broker = Arc::clone(&broker);
        async move { broker.request(questions_context()).await }
    });
    assert_eq!(next_entered(&mut entered_rx).await, "elicitation/create");

    let approval = broker.request(approval_context_with_items(2));
    tokio::pin!(approval);
    let first_poll = poll_fn(|cx| Poll::Ready(approval.as_mut().poll(cx))).await;
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
    let broker =
        AcpTransportBroker::new(Arc::new(transport), SessionId::new("s1")).with_auto_approve();
    let resp = broker.request(approval_context()).await;
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
    let broker = AcpTransportBroker::new(Arc::new(transport), SessionId::new("s1"));
    let resp = broker.request(approval_context()).await;
    let InteractionResponse::Decisions(decisions) = resp else {
        panic!("forward 应返回 Decisions: {resp:?}");
    };
    assert!(matches!(decisions[0], ApprovalDecision::Approve { .. }));
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "session/request_permission");
}

#[tokio::test]
async fn test_questions_timeout_returns_rejected() {
    // 客户端存活但不响应：超时 → Rejected（LLM 侧 ToolRejected）。
    let calls: Arc<Mutex<Vec<(String, serde_json::Value)>>> = Arc::new(Mutex::new(vec![]));
    let transport = MockTransport {
        calls: Arc::clone(&calls),
        behavior: Behavior::Pending,
    };
    let broker = AcpTransportBroker::new(Arc::new(transport), SessionId::new("s1"))
        .with_timeout(Some(Duration::from_millis(50)));
    let resp = broker
        .request(InteractionContext::Questions {
            requests: sample_questions(),
        })
        .await;
    assert!(
        matches!(resp, InteractionResponse::Rejected),
        "超时应返回 Rejected: {resp:?}"
    );
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "elicitation/create");
}

#[tokio::test]
async fn test_questions_transport_error_returns_empty_answers() {
    // transport 断连（send_request 报错）：空 Answers 而非挂死。
    struct ErrorTransport;
    #[async_trait::async_trait]
    impl RequestTransport for ErrorTransport {
        async fn send_request(
            &self,
            _method: &str,
            _params: serde_json::Value,
        ) -> Result<serde_json::Value, AcpError> {
            Err(AcpError::new(-32000, "transport closed"))
        }
    }
    let broker = AcpTransportBroker::new(Arc::new(ErrorTransport), SessionId::new("s1"));
    let resp = broker
        .request(InteractionContext::Questions {
            requests: sample_questions(),
        })
        .await;
    let InteractionResponse::Answers(answers) = resp else {
        panic!("transport 错误应返回空 Answers: {resp:?}");
    };
    assert_eq!(answers.len(), 3);
    assert_eq!(answers[0].text.as_deref(), Some(""));
}

// ─── PERI_ASK_USER_TIMEOUT_SECS 解析（批 4：统一 broker 恢复提问超时兜底）───

/// 纯逻辑形态：缺省 300 / 非法回落 300 / `0` → None / 合法值。
/// 语义与批 3 删除的 `host/stdio/context.rs::parse_ask_user_timeout` 完全一致。
#[test]
fn test_parse_ask_user_timeout_env_values() {
    assert_eq!(
        parse_ask_user_timeout(None),
        Some(Duration::from_secs(300)),
        "缺失 → 默认 300"
    );
    assert_eq!(
        parse_ask_user_timeout(Some("300")),
        Some(Duration::from_secs(300))
    );
    assert_eq!(
        parse_ask_user_timeout(Some("1")),
        Some(Duration::from_secs(1))
    );
    assert_eq!(parse_ask_user_timeout(Some("0")), None, "0 → 不超时");
    assert_eq!(
        parse_ask_user_timeout(Some("abc")),
        Some(Duration::from_secs(300)),
        "非法值回落 300"
    );
    assert_eq!(
        parse_ask_user_timeout(Some("-5")),
        Some(Duration::from_secs(300)),
        "负数解析失败回落 300"
    );
    assert_eq!(
        parse_ask_user_timeout(Some("")),
        Some(Duration::from_secs(300))
    );
}

/// env 读取接线（serial 串行：`std::env::set_var` 为进程级全局，避免并行竞态）。
#[serial_test::serial]
#[test]
fn test_ask_user_timeout_reads_env() {
    std::env::set_var("PERI_ASK_USER_TIMEOUT_SECS", "0");
    assert_eq!(ask_user_timeout(), None);
    std::env::set_var("PERI_ASK_USER_TIMEOUT_SECS", "42");
    assert_eq!(ask_user_timeout(), Some(Duration::from_secs(42)));
    std::env::remove_var("PERI_ASK_USER_TIMEOUT_SECS");
    assert_eq!(ask_user_timeout(), Some(Duration::from_secs(300)));
}
