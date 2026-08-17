//! Tests for `host/stdio/context.rs` 的 `StdioQuestionBroker`（stdio 提问转发）。
//!
//! 双端 builder 驱动（对齐 `session/create_test.rs` / `commands_test.rs`）：
//! agent 端 handler 处理触发请求后 `tokio::spawn` broker 任务——broker 的
//! `request` 会 await client 响应，必须在 dispatch loop 之外运行（与生产路径
//! `prompt_exec::run` 在后台任务中 await broker 一致；ACP `block_task` 明确
//! 警告 loop 内 await 会死锁）；client 端 `on_receive_request` 捕获
//! `elicitation/create` 并按场景回 accept/cancel/decline 或不响应。
//!
//! 覆盖验收（spec/issues/2026-08-17-stdio-ask-user-question-forward.md）：
//! 1（schema 三形态 + option description 注入）、2（accept → Answers）、
//! 3（decline → Rejected；cancel → 空 Answers）、4（transport 关闭 → 空
//! Answers 而非挂死）、5（Approval 自动 approve 回归）、7（超时 → Rejected；
//! None 不超时）、8（env 超时解析纯逻辑）。

use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::{
    schema::v1::SessionId, Agent, Channel, Client, ConnectionTo, UntypedMessage,
};
use peri_acp_types::interaction::{
    ApprovalDecision, ApprovalItem, InteractionContext, InteractionResponse, QuestionItem,
    QuestionOption, UserInteractionBroker,
};
use serde_json::{json, Value};

use super::{ask_user_timeout, parse_ask_user_timeout, StdioQuestionBroker};

const TEST_SESSION_ID: &str = "test-session";

/// client 端对 `elicitation/create` 的响应：`None` 表示不响应。
type RespondFn = fn(Value) -> Option<Value>;

/// 三形态问题集：单选（choice）/ 多选（multi）/ 自由文本（text）。
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

fn accept_response(_params: Value) -> Option<Value> {
    Some(json!({
        "action": "accept",
        "content": {
            "choice": "生产",
            "multi": ["a", "b"],
            "text": "自由补充",
        }
    }))
}

fn decline_response(_params: Value) -> Option<Value> {
    Some(json!({ "action": "decline" }))
}

fn cancel_response(_params: Value) -> Option<Value> {
    Some(json!({ "action": "cancel" }))
}

fn no_response(_params: Value) -> Option<Value> {
    None
}

/// 双端驱动：agent 端（server）收触发请求 → spawn broker 任务；client 端捕获
/// `elicitation/create` 存 params、按 `respond` 回包（`client_delay` 为回包前
/// 延迟，模拟慢 client）。`close_after_capture` = true 时 main 在请求到达后
/// 立即关闭连接（transport 关闭场景），broker 结果随后在驱动内等待产生。
async fn drive_broker(
    context: InteractionContext,
    timeout: Option<Duration>,
    respond: RespondFn,
    client_delay: Duration,
    close_after_capture: bool,
) -> (Option<Value>, InteractionResponse) {
    let (channel_a, channel_b) = Channel::duplex();
    let session_id = SessionId::new(TEST_SESSION_ID);
    let captured: Arc<std::sync::Mutex<Option<Value>>> = Arc::new(std::sync::Mutex::new(None));
    let result: Arc<std::sync::Mutex<Option<InteractionResponse>>> =
        Arc::new(std::sync::Mutex::new(None));

    // agent 端（server）：响应触发请求后 spawn broker 任务（await 在 loop 外）。
    let result_agent = Arc::clone(&result);
    let session_id_agent = session_id.clone();
    let server = Agent
        .builder()
        .on_receive_request(
            {
                async move |_req: UntypedMessage, responder, cx: ConnectionTo<Client>| {
                    let _ = responder.respond(json!({ "ok": true }));
                    let cx_task = cx.clone();
                    let session_id_task = session_id_agent.clone();
                    let context_task = context.clone();
                    let result_task = Arc::clone(&result_agent);
                    tokio::spawn(async move {
                        let broker = StdioQuestionBroker::new(cx_task, session_id_task, timeout);
                        let response = broker.request(context_task).await;
                        *result_task.lock().unwrap() = Some(response);
                    });
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_to(channel_b);
    let _server_task = tokio::spawn(server);

    // client 端：捕获 elicitation/create → 存 params → 按场景回包。
    let captured_client = Arc::clone(&captured);
    let outcome = Client
        .builder()
        .on_receive_request(
            {
                async move |req: UntypedMessage, responder, _cx: ConnectionTo<Agent>| {
                    if req.method() == "elicitation/create" {
                        *captured_client.lock().unwrap() = Some(req.params().clone());
                        if !client_delay.is_zero() {
                            tokio::time::sleep(client_delay).await;
                        }
                        if let Some(value) = respond(req.params().clone()) {
                            let _ = responder.respond(value);
                        }
                    }
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(channel_a, {
            let captured_main = Arc::clone(&captured);
            let result_main = Arc::clone(&result);
            async move |cx: ConnectionTo<Agent>| -> Result<(), agent_client_protocol::Error> {
                let _: Value = cx
                    .send_request(UntypedMessage::new("test/trigger", json!({})).unwrap())
                    .block_task()
                    .await?;
                // 轮询：broker 完成（结果已写入容器），或断连场景要求关闭连接。
                let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
                loop {
                    if result_main.lock().unwrap().is_some() {
                        return Ok(());
                    }
                    if captured_main.lock().unwrap().is_some() && close_after_capture {
                        return Ok(());
                    }
                    if tokio::time::Instant::now() >= deadline {
                        panic!("测试驱动超时：broker 未在 5s 内完成");
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        })
        .await;

    assert!(outcome.is_ok(), "双端 builder 应成功: {outcome:?}");

    // 断连场景：结果在连接关闭后产生，统一在驱动内等待（5s 防挂死）。
    let response = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(resp) = result.lock().unwrap().clone() {
                return resp;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("broker 结果超时");

    let captured_params = captured.lock().unwrap().clone();
    (captured_params, response)
}

// ─── 验收 1：schema 三形态 + title/description + option description 注入 ───

#[tokio::test]
async fn test_questions_schema_three_forms_and_option_descriptions() {
    let (params, response) = drive_broker(
        InteractionContext::Questions {
            requests: sample_questions(),
        },
        None,
        cancel_response,
        Duration::ZERO,
        false,
    )
    .await;
    let params = params.expect("client 应捕获 elicitation/create");
    assert_eq!(params["mode"], "form");
    assert_eq!(params["sessionId"], TEST_SESSION_ID);
    assert_eq!(
        params["message"],
        "Please provide the requested information"
    );

    let props = &params["requestedSchema"]["properties"];

    // 单选：type=string + oneOf（const/title），title/description 透传
    let choice = &props["choice"];
    assert_eq!(choice["type"], "string");
    assert_eq!(choice["title"], "部署环境");
    assert_eq!(choice["description"], "选择部署环境？");
    assert_eq!(choice["oneOf"][0]["const"], "生产");
    assert_eq!(choice["oneOf"][0]["title"], "生产");
    assert_eq!(
        choice["oneOf"][0]["description"], "生产环境",
        "option description 应注入 oneOf"
    );
    assert_eq!(choice["oneOf"][1]["const"], "测试");
    assert!(
        choice["oneOf"][1].get("description").is_none(),
        "无 description 的 option 不应注入"
    );

    // 多选：type=array + items.anyOf，option description 注入 items 层
    let multi = &props["multi"];
    assert_eq!(multi["type"], "array");
    assert_eq!(multi["title"], "功能");
    assert_eq!(multi["description"], "选择功能？");
    assert_eq!(multi["items"]["anyOf"][0]["const"], "a");
    assert_eq!(
        multi["items"]["anyOf"][0]["description"], "desc-a",
        "多选 option description 应注入 items.anyOf"
    );
    assert!(multi["items"]["anyOf"][1].get("description").is_none());

    // 自由文本：无 options → 无 oneOf/anyOf
    let text = &props["text"];
    assert_eq!(text["type"], "string");
    assert_eq!(text["title"], "补充");
    assert_eq!(text["description"], "补充说明？");
    assert!(text.get("oneOf").is_none(), "自由文本不应有 oneOf");

    assert!(
        matches!(response, InteractionResponse::Answers(_)),
        "cancel 兜底应为空 Answers: {response:?}"
    );
}

// ─── 验收 2：accept（content 含 q_id → label）→ Answers ───

#[tokio::test]
async fn test_accept_returns_answers() {
    let (_, response) = drive_broker(
        InteractionContext::Questions {
            requests: sample_questions(),
        },
        None,
        accept_response,
        Duration::ZERO,
        false,
    )
    .await;

    let InteractionResponse::Answers(answers) = response else {
        panic!("accept 应返回 Answers，实际: {response:?}");
    };
    assert_eq!(answers.len(), 3, "answers 应与问题一一对应");

    let choice = &answers[0];
    assert_eq!(choice.id, "choice");
    assert_eq!(choice.text.as_deref(), Some("生产"), "单选 accept → text");
    assert!(choice.selected.is_empty());

    let multi = &answers[1];
    assert_eq!(multi.id, "multi");
    assert_eq!(multi.selected, vec!["a", "b"], "多选 accept → selected");
    assert!(multi.text.is_none());

    let text = &answers[2];
    assert_eq!(text.id, "text");
    assert_eq!(text.text.as_deref(), Some("自由补充"));
}

// ─── 验收 3：decline → Rejected；cancel → 空 Answers ───

#[tokio::test]
async fn test_decline_returns_rejected() {
    let (_, response) = drive_broker(
        InteractionContext::Questions {
            requests: sample_questions(),
        },
        None,
        decline_response,
        Duration::ZERO,
        false,
    )
    .await;
    assert!(
        matches!(response, InteractionResponse::Rejected),
        "decline 应返回 Rejected: {response:?}"
    );
}

#[tokio::test]
async fn test_cancel_returns_empty_answers() {
    let (_, response) = drive_broker(
        InteractionContext::Questions {
            requests: sample_questions(),
        },
        None,
        cancel_response,
        Duration::ZERO,
        false,
    )
    .await;

    let InteractionResponse::Answers(answers) = response else {
        panic!("cancel 应返回 Answers，实际: {response:?}");
    };
    assert_eq!(answers.len(), 3);
    let ids: Vec<&str> = answers.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(ids, vec!["choice", "multi", "text"], "id 顺序应保留");
    for a in &answers {
        assert!(a.selected.is_empty());
        assert_eq!(
            a.text.as_deref(),
            Some(""),
            "cancel → 空 Answers（空串 text）"
        );
    }
}

// ─── 验收 4：transport 关闭（client 端连接关闭）→ 空 Answers 而非挂死 ───

#[tokio::test]
async fn test_transport_closed_returns_empty_answers() {
    // client 捕获到请求但不响应，main 立即返回 → 连接关闭 → agent 端挂起
    // 请求失败（pending requests failed first）→ 空 Answers。
    let (params, response) = drive_broker(
        InteractionContext::Questions {
            requests: sample_questions(),
        },
        None,
        no_response,
        Duration::ZERO,
        true,
    )
    .await;

    assert!(params.is_some(), "请求应已到达 client");
    let InteractionResponse::Answers(answers) = response else {
        panic!("transport 关闭应返回空 Answers，实际: {response:?}");
    };
    assert_eq!(answers.len(), 3);
    for a in &answers {
        assert_eq!(a.text.as_deref(), Some(""), "transport 关闭 → 空 Answers");
        assert!(a.selected.is_empty());
    }
}

// ─── 验收 5：Approval 分支自动 approve 回归 ───

#[tokio::test]
async fn test_approval_branch_auto_approve() {
    let (params, response) = drive_broker(
        InteractionContext::Approval {
            items: vec![
                ApprovalItem {
                    tool_call_id: "call_1".into(),
                    tool_name: "Bash".into(),
                    tool_input: json!({ "cmd": "ls" }),
                },
                ApprovalItem {
                    tool_call_id: "call_2".into(),
                    tool_name: "Read".into(),
                    tool_input: json!({ "path": "a.txt" }),
                },
            ],
        },
        None,
        no_response,
        Duration::ZERO,
        false,
    )
    .await;

    assert!(params.is_none(), "Approval 分支不应发出 elicitation/create");
    let InteractionResponse::Decisions(decisions) = response else {
        panic!("Approval 应返回 Decisions: {response:?}");
    };
    assert_eq!(decisions.len(), 2);
    for d in &decisions {
        assert!(
            matches!(d, ApprovalDecision::Approve { source: None }),
            "Approval 应自动 approve: {d:?}"
        );
    }
}

// ─── 验收 7：超时兜底（client 不响应）→ Rejected；None 不受影响 ───

#[tokio::test]
async fn test_timeout_returns_rejected() {
    // broker 构造 timeout=50ms + client 不响应 → Rejected（与 decline 语义一致）。
    let (params, response) = drive_broker(
        InteractionContext::Questions {
            requests: sample_questions(),
        },
        Some(Duration::from_millis(50)),
        no_response,
        Duration::ZERO,
        false,
    )
    .await;

    assert!(params.is_some(), "请求应已到达 client（未响应）");
    assert!(
        matches!(response, InteractionResponse::Rejected),
        "超时应返回 Rejected: {response:?}"
    );
}

#[tokio::test]
async fn test_no_timeout_waits_for_slow_client() {
    // timeout=None：不做超时兜底，慢响应（100ms 延迟后 accept）正常返回 Answers。
    let (_, response) = drive_broker(
        InteractionContext::Questions {
            requests: sample_questions(),
        },
        None,
        accept_response,
        Duration::from_millis(100),
        false,
    )
    .await;

    let InteractionResponse::Answers(answers) = response else {
        panic!("慢响应应返回 Answers: {response:?}");
    };
    assert_eq!(answers[0].text.as_deref(), Some("生产"));
}

// ─── 验收 8：PERI_ASK_USER_TIMEOUT_SECS 解析（纯逻辑 + env 接线） ───

#[test]
fn test_parse_ask_user_timeout_env_values() {
    // 缺失/非法回落 300；0 → None（不超时）。
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
