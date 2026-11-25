use std::sync::Arc;

use peri_acp::transport::AcpTransport;
use serde_json::json;

use super::AcpTuiClient;
use super::AskUserResponseAction;
use super::CancellationToken;
use super::mpsc;

#[tokio::test]
async fn test_shutdown_exits_loop() {
    let (client_transport, _) = peri_acp::transport::mpsc::mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(client_transport);
    let (_tx, rx) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let handle = super::spawn_ask_user_consumer(client, rx, shutdown.clone());
    shutdown.cancel();
    handle
        .await
        .expect("shutdown exits loop: fixture task and request must complete successfully");
}

/// 走一次真实 consumer 提交：返回 wire response 与终态 outcome（display result
/// 由 outcome 携带）。
async fn submit_once(
    answers: serde_json::Value,
    first_question_id: Option<&str>,
) -> (serde_json::Value, crate::acp_client::InteractionUiOutcome) {
    let (client_transport, server_transport) = peri_acp::transport::mpsc::mpsc_transport_pair();
    let (client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
    client.force_stable_for_test("s1", true);
    client.spawn_pump(notification_tx);
    let (tx, rx) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let handle = super::spawn_ask_user_consumer(client, rx, shutdown.clone());
    let server = Arc::new(server_transport);
    let request_server = server.clone();
    let request = tokio::spawn(async move {
        request_server
            .send_request(
                "elicitation/create",
                json!({"sessionId":"s1","requestedSchema":{"type":"object","properties":{}}}),
            )
            .await
            .expect("submit fixture: elicitation request must complete")
    });
    let crate::acp_client::AcpNotification::Elicitation {
        owner,
        request_id_json,
        ..
    } = notification_rx
        .recv()
        .await
        .expect("submit fixture: elicitation notification must arrive")
    else {
        panic!("submit fixture: expected elicitation notification")
    };
    tx.send(AskUserResponseAction::Submit {
        owner,
        request_id_str: request_id_json,
        answers,
        first_question_id: first_question_id.map(str::to_string),
        open_url: None,
    })
    .expect("submit fixture: response channel must accept the action");
    let response = request
        .await
        .expect("submit fixture: elicitation request must complete");
    let notification = notification_rx
        .recv()
        .await
        .expect("submit fixture: terminal notification must arrive");
    let crate::acp_client::AcpNotification::InteractionTerminal { outcome, .. } = notification
    else {
        panic!("submit fixture: expected interaction terminal notification")
    };
    shutdown.cancel();
    handle
        .await
        .expect("submit fixture: consumer task must exit");
    (response, outcome)
}

fn resolved_result(outcome: crate::acp_client::InteractionUiOutcome) -> String {
    let crate::acp_client::InteractionUiOutcome::Resolved { result } = outcome else {
        panic!("submit fixture: expected resolved outcome")
    };
    result
}

#[tokio::test]
async fn test_ask_user_submit_sends_full_answers_once() {
    crate::i18n::init(Some("en"));
    let (response, outcome) = submit_once(json!({"q1":"Fast","q2":"Complete"}), Some("q1")).await;
    assert_eq!(response["action"], "accept");
    assert_eq!(response["content"], json!({"q1":"Fast","q2":"Complete"}));
    assert_eq!(
        resolved_result(outcome),
        "Fast",
        "display result 取首问答案，实际 wire content 仍为完整 map"
    );
}

/// 首问留空、第 2 问有答案：display result 不得借用第 2 问的答案（inline
/// block 只展示首问，借用会把答案挂到首问下）；wire content 仍包含全部键。
#[tokio::test]
async fn test_ask_user_submit_display_result_ignores_later_answers() {
    crate::i18n::init(Some("en"));
    let (response, outcome) = submit_once(
        json!({"ask_user_question_0":"", "ask_user_question_1":"B"}),
        Some("ask_user_question_0"),
    )
    .await;
    assert_eq!(response["content"]["ask_user_question_0"], "");
    assert_eq!(
        response["content"]["ask_user_question_1"], "B",
        "wire 仍携带全部问题的答案"
    );
    assert_eq!(
        resolved_result(outcome),
        "Answered",
        "首问未答时回退 i18n 文案，不得显示第 2 问答案"
    );
}

/// 首问自己的答案非空 → display result = 该答案。
#[tokio::test]
async fn test_ask_user_submit_display_result_uses_first_question_answer() {
    crate::i18n::init(Some("en"));
    let (response, outcome) = submit_once(
        json!({"ask_user_question_0":"A", "ask_user_question_1":"B"}),
        Some("ask_user_question_0"),
    )
    .await;
    assert_eq!(
        response["content"],
        json!({"ask_user_question_0":"A", "ask_user_question_1":"B"})
    );
    assert_eq!(resolved_result(outcome), "A");
}

/// 多选首问：display result 拼接 label（`,` 分隔），供 completed block 的
/// `已选：` 行展示。
#[tokio::test]
async fn test_ask_user_submit_display_result_joins_multi_select_first_question() {
    crate::i18n::init(Some("en"));
    let (response, outcome) = submit_once(json!({"q0":["A","B"], "q1":["C"]}), Some("q0")).await;
    assert_eq!(response["content"], json!({"q0":["A","B"], "q1":["C"]}));
    assert_eq!(resolved_result(outcome), "A, B");
}

/// 首问 id 缺失（旧调用/异常数据）→ 回退 i18n 文案，不回退到其它问题的答案。
#[tokio::test]
async fn test_ask_user_submit_display_result_missing_first_id_falls_back() {
    crate::i18n::init(Some("en"));
    let (response, outcome) = submit_once(json!({"q0":"A", "q1":"B"}), None).await;
    assert_eq!(response["content"], json!({"q0":"A", "q1":"B"}));
    assert_eq!(resolved_result(outcome), "Answered");
}

#[test]
fn test_display_answer_shapes() {
    assert_eq!(super::display_answer(&json!("Fast")), "Fast");
    assert_eq!(super::display_answer(&json!(["A", "B"])), "A, B");
    assert_eq!(super::display_answer(&json!([])), "");
    assert_eq!(super::display_answer(&json!([""])), "");
    assert_eq!(super::display_answer(&json!(1)), "");
    assert_eq!(super::display_answer(&serde_json::Value::Null), "");
}

#[test]
fn test_ask_user_response_action_variants_carry_owner() {
    let owner = crate::acp_client::InteractionOwner::default();
    let submit = AskUserResponseAction::Submit {
        owner: owner.clone(),
        request_id_str: "\"abc\"".into(),
        answers: json!({"q1":"yes"}),
        first_question_id: Some("q1".into()),
        open_url: None,
    };
    let AskUserResponseAction::Submit {
        owner: actual,
        answers,
        first_question_id,
        ..
    } = submit
    else {
        panic!()
    };
    assert_eq!(actual, owner);
    assert_eq!(answers["q1"], "yes");
    assert_eq!(first_question_id.as_deref(), Some("q1"));
}
