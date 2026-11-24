//! tools/call 中继单测（fixtures/fakes 复用父模块 `tests`）。

use super::*;
use crate::kit::apps::runtime::relay::{
    MAX_APP_REQUEST_BYTES, dispatch_app_request, outcome_payload, relay_tools_call,
};

// ── 阶段 2：tools/call 中继 ─────────────────────────────────────────────────

#[tokio::test]
async fn relay_posts_raw_completed_outcome_and_uses_read_next_seq() {
    let rpc = Arc::new(FakeRpc::with_session("session-1"));
    let raw = json!({"content": [{"type": "text", "text": "12:00"}], "isError": false});
    *rpc.tool_default.lock().unwrap() = Some(Ok(tool_response(completed_tool(raw.clone()), 8)));
    let api = Arc::new(FakeViewApi::default());
    let context = relay_context(
        "att-a",
        3,
        "v1",
        "view-1",
        Arc::clone(&rpc) as Arc<dyn AppsRpc>,
        Arc::clone(&api) as Arc<dyn ViewSessionApi>,
        Duration::from_secs(1),
    );
    // read 阶段已消耗 seq 1..=7，tools/call 必须从 8 开始。
    let mut ledger_next = 8;
    let payload = relay_tools_call(
        &context,
        &tools_call(1, "get-time", json!({})),
        &mut ledger_next,
    )
    .await;
    assert_eq!(
        payload,
        ViewAppResult::Result(raw),
        "原始 outcome 必须原样回填"
    );
    let calls = rpc.tool_calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].attachment_id, "att-a");
    assert_eq!(calls[0].epoch, 3);
    assert_eq!(calls[0].seq, 8);
    assert_eq!(calls[0].name, "get-time");
    assert_eq!(calls[0].arguments, json!({}));
    assert_eq!(ledger_next, 8, "terminal 响应的 nextSeq 是账本下一序号");
}

#[tokio::test]
async fn relay_advances_through_response_next_seq() {
    let rpc = Arc::new(FakeRpc::with_session("session-1"));
    {
        let mut queue = rpc.tool_queue.lock().unwrap();
        queue.push_back(Ok(tool_response(
            completed_tool(json!({"content": [{"type": "text", "text": "first"}]})),
            10,
        )));
        queue.push_back(Ok(tool_response(
            completed_tool(json!({"content": [{"type": "text", "text": "second"}]})),
            11,
        )));
    }
    let api = Arc::new(FakeViewApi::default());
    let context = relay_context(
        "att-a",
        1,
        "v1",
        "view-1",
        Arc::clone(&rpc) as Arc<dyn AppsRpc>,
        Arc::clone(&api) as Arc<dyn ViewSessionApi>,
        Duration::from_secs(1),
    );
    let mut ledger_next = 2;
    relay_tools_call(
        &context,
        &tools_call(1, "get-time", json!({})),
        &mut ledger_next,
    )
    .await;
    relay_tools_call(
        &context,
        &tools_call(2, "get-time", json!({})),
        &mut ledger_next,
    )
    .await;
    let seqs: Vec<u64> = rpc
        .tool_calls
        .lock()
        .unwrap()
        .iter()
        .map(|call| call.seq)
        .collect();
    assert_eq!(seqs, vec![2, 10], "第二条必须使用第一条响应的 nextSeq");
}

#[tokio::test]
async fn two_views_keep_attachment_and_seq_independent() {
    let rpc = Arc::new(FakeRpc::with_session("session-1"));
    {
        let mut queue = rpc.tool_queue.lock().unwrap();
        queue.push_back(Ok(tool_response(completed_tool(json!({"content": []})), 6)));
        queue.push_back(Ok(tool_response(
            completed_tool(json!({"content": []})),
            21,
        )));
    }
    let api = Arc::new(FakeViewApi::default());
    let context_a = relay_context(
        "att-a",
        2,
        "va",
        "view-a",
        Arc::clone(&rpc) as Arc<dyn AppsRpc>,
        Arc::clone(&api) as Arc<dyn ViewSessionApi>,
        Duration::from_secs(1),
    );
    let context_b = relay_context(
        "att-b",
        9,
        "vb",
        "view-b",
        Arc::clone(&rpc) as Arc<dyn AppsRpc>,
        Arc::clone(&api) as Arc<dyn ViewSessionApi>,
        Duration::from_secs(1),
    );
    let mut next_a = 5;
    let mut next_b = 20;
    let request_a = tools_call(1, "get-time", json!({}));
    let request_b = tools_call(1, "get-time", json!({}));
    let (payload_a, payload_b) = tokio::join!(
        relay_tools_call(&context_a, &request_a, &mut next_a),
        relay_tools_call(&context_b, &request_b, &mut next_b),
    );
    assert!(matches!(payload_a, ViewAppResult::Result(_)));
    assert!(matches!(payload_b, ViewAppResult::Result(_)));
    let mut calls = rpc
        .tool_calls
        .lock()
        .unwrap()
        .iter()
        .map(|call| (call.attachment_id.clone(), call.epoch, call.seq))
        .collect::<Vec<_>>();
    calls.sort();
    assert_eq!(
        calls,
        vec![("att-a".to_string(), 2, 5), ("att-b".to_string(), 9, 20)],
        "两视图并发时 attachment/epoch/seq 不得串台"
    );
}

#[tokio::test]
async fn relay_retries_pending_with_the_same_seq() {
    let rpc = Arc::new(FakeRpc::with_session("session-1"));
    {
        let mut queue = rpc.tool_queue.lock().unwrap();
        queue.push_back(Ok(AppsRequestResponse {
            verdict: RequestVerdict::Accepted,
            admitted: true,
            outcome: Some(AppsRequestOutcome::Pending),
            next_seq: RequestSeq::new(2),
        }));
        queue.push_back(Ok(AppsRequestResponse {
            verdict: RequestVerdict::JoinedPending,
            admitted: false,
            outcome: Some(AppsRequestOutcome::ToolsCall(completed_tool(json!({
                "content": [{"type": "text", "text": "ok"}]
            })))),
            next_seq: RequestSeq::new(2),
        }));
    }
    let api = Arc::new(FakeViewApi::default());
    let context = relay_context(
        "att-a",
        1,
        "v1",
        "view-1",
        Arc::clone(&rpc) as Arc<dyn AppsRpc>,
        Arc::clone(&api) as Arc<dyn ViewSessionApi>,
        Duration::from_secs(1),
    );
    let mut ledger_next = 1;
    let payload = relay_tools_call(
        &context,
        &tools_call(1, "get-time", json!({})),
        &mut ledger_next,
    )
    .await;
    assert!(matches!(payload, ViewAppResult::Result(_)));
    let seqs: Vec<u64> = rpc
        .tool_calls
        .lock()
        .unwrap()
        .iter()
        .map(|call| call.seq)
        .collect();
    assert_eq!(seqs, vec![1, 1], "Pending 必须复用同一 seq");
}

#[tokio::test]
async fn relay_rejection_and_timeout_return_errors() {
    let rpc = Arc::new(FakeRpc::with_session("session-1"));
    *rpc.tool_default.lock().unwrap() = Some(Ok(AppsRequestResponse {
        verdict: RequestVerdict::Conflict,
        admitted: false,
        outcome: None,
        next_seq: RequestSeq::new(1),
    }));
    let api = Arc::new(FakeViewApi::default());
    let context = relay_context(
        "att-a",
        1,
        "v1",
        "view-1",
        Arc::clone(&rpc) as Arc<dyn AppsRpc>,
        Arc::clone(&api) as Arc<dyn ViewSessionApi>,
        Duration::from_secs(1),
    );
    let mut ledger_next = 1;
    let payload = relay_tools_call(
        &context,
        &tools_call(1, "get-time", json!({})),
        &mut ledger_next,
    )
    .await;
    assert_eq!(
        payload,
        ViewAppResult::Error {
            code: -32603,
            message: "request_conflict".to_string()
        }
    );

    // 超时：永远 Pending，预算 30ms；必须回填错误而不是挂死。
    let rpc = Arc::new(FakeRpc::with_session("session-1"));
    *rpc.tool_default.lock().unwrap() = Some(Ok(AppsRequestResponse {
        verdict: RequestVerdict::Accepted,
        admitted: true,
        outcome: Some(AppsRequestOutcome::Pending),
        next_seq: RequestSeq::new(2),
    }));
    let api = Arc::new(FakeViewApi::default());
    let context = relay_context(
        "att-a",
        1,
        "v1",
        "view-1",
        Arc::clone(&rpc) as Arc<dyn AppsRpc>,
        Arc::clone(&api) as Arc<dyn ViewSessionApi>,
        Duration::from_millis(30),
    );
    let started = Instant::now();
    let mut ledger_next = 1;
    let payload = relay_tools_call(
        &context,
        &tools_call(1, "get-time", json!({})),
        &mut ledger_next,
    )
    .await;
    assert_eq!(
        payload,
        ViewAppResult::Error {
            code: -32603,
            message: "apps_request_timeout".to_string()
        }
    );
    assert!(started.elapsed() < Duration::from_secs(1), "不得挂死");
}

#[tokio::test]
async fn unsupported_method_is_rejected_without_calling_acp() {
    let rpc = Arc::new(FakeRpc::with_session("session-1"));
    let api = Arc::new(FakeViewApi::default());
    let context = relay_context(
        "att-a",
        1,
        "v1",
        "view-1",
        Arc::clone(&rpc) as Arc<dyn AppsRpc>,
        Arc::clone(&api) as Arc<dyn ViewSessionApi>,
        Duration::from_secs(1),
    );
    let mut ledger_next = 1;
    let payload = relay_tools_call(
        &context,
        &app_request(1, "resources/read", json!({"uri": RESOURCE_URI})),
        &mut ledger_next,
    )
    .await;
    assert_eq!(
        payload,
        ViewAppResult::Error {
            code: -32601,
            message: "unsupported_method".to_string()
        }
    );
    assert!(rpc.tool_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn queue_overflow_returns_error_to_the_app() {
    let api = Arc::new(FakeViewApi::default());
    let (tx, _rx) = mpsc::channel(1);
    tx.try_send(tools_call(999, "filler", json!({}))).unwrap();
    let api_dyn: Arc<dyn ViewSessionApi> = Arc::clone(&api) as Arc<dyn ViewSessionApi>;
    dispatch_app_request(
        &tx,
        &api_dyn,
        "view-1",
        "v1",
        tools_call(7, "get-time", json!({})),
    )
    .await;
    let results = api.app_results.lock().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, "view-1");
    assert_eq!(results[0].1, 7);
    assert_eq!(
        results[0].2,
        ViewAppResult::Error {
            code: -32603,
            message: "app_request_overflow".to_string()
        }
    );
}

#[serial_test::serial]
#[tokio::test]
async fn run_view_relays_app_request_to_raw_result() {
    seed_view("v1");
    let rpc = Arc::new(FakeRpc::with_session("session-1"));
    rpc.set_attach_ok(Some(HTML));
    *rpc.read_default.lock().unwrap() = Some(Ok(read_response(vec![raw_html(HTML)], 1)));
    let raw = json!({"content": [{"type": "text", "text": "12:01"}]});
    *rpc.tool_default.lock().unwrap() = Some(Ok(tool_response(completed_tool(raw.clone()), 3)));
    let api = Arc::new(FakeViewApi {
        hold: true,
        ..FakeViewApi::default()
    });
    api.scripts.lock().unwrap().push_back(VecDeque::from(vec![
        ready_status(),
        ViewEvent::AppRequest(tools_call(41, "get-time", json!({}))),
    ]));

    let cancel = CancellationToken::new();
    let task = tokio::spawn(run_view(view_context(
        "v1",
        Arc::clone(&rpc) as Arc<dyn AppsRpc>,
        Arc::clone(&api) as Arc<dyn ViewSessionApi>,
        cancel.clone(),
    )));
    wait_for_async(|| !api.app_results.lock().unwrap().is_empty()).await;
    let results = api.app_results.lock().unwrap().clone();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, "view-1");
    assert_eq!(
        results[0].1, 41,
        "app-results 的 seq 是网关 app-request 序号"
    );
    assert_eq!(results[0].2, ViewAppResult::Result(raw));
    {
        let calls = rpc.tool_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].seq, 2, "read 用掉 seq 1，tools/call 从 2 开始");
        assert_eq!(calls[0].name, "get-time");
    }
    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
}

#[serial_test::serial]
#[tokio::test]
async fn run_view_posts_error_when_tool_call_is_rejected() {
    seed_view("v1");
    let rpc = Arc::new(FakeRpc::with_session("session-1"));
    rpc.set_attach_ok(Some(HTML));
    *rpc.read_default.lock().unwrap() = Some(Ok(read_response(vec![raw_html(HTML)], 1)));
    *rpc.tool_default.lock().unwrap() = Some(Ok(AppsRequestResponse {
        verdict: RequestVerdict::Conflict,
        admitted: false,
        outcome: None,
        next_seq: RequestSeq::new(2),
    }));
    let api = Arc::new(FakeViewApi {
        hold: true,
        ..FakeViewApi::default()
    });
    api.scripts.lock().unwrap().push_back(VecDeque::from(vec![
        ready_status(),
        ViewEvent::AppRequest(tools_call(42, "get-time", json!({}))),
    ]));

    let cancel = CancellationToken::new();
    let task = tokio::spawn(run_view(view_context(
        "v1",
        Arc::clone(&rpc) as Arc<dyn AppsRpc>,
        Arc::clone(&api) as Arc<dyn ViewSessionApi>,
        cancel.clone(),
    )));
    wait_for_async(|| !api.app_results.lock().unwrap().is_empty()).await;
    let results = api.app_results.lock().unwrap().clone();
    assert_eq!(results[0].0, "view-1");
    assert_eq!(results[0].1, 42);
    assert_eq!(
        results[0].2,
        ViewAppResult::Error {
            code: -32603,
            message: "request_conflict".to_string()
        }
    );
    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
}

#[serial_test::serial]
#[tokio::test]
async fn request_view_click_reaches_the_view_session() {
    APPS_STATE
        .state()
        .set(crate::kit::apps::state::AppsState::default());
    let rpc = Arc::new(FakeRpc::with_session("session-1"));
    *rpc.list.lock().unwrap() = Some(list_response(vec![descriptor("app-1", Some("call-1"))]));
    rpc.set_attach_ok(Some(HTML));
    *rpc.read_default.lock().unwrap() = Some(Ok(read_response(vec![raw_html(HTML)], 1)));
    let api = Arc::new(FakeViewApi {
        hold: true,
        ..FakeViewApi::default()
    });
    let handle = spawn_with_parts(
        super::super::config::AppsConfig {
            view_url: "http://127.0.0.1:1".to_string(),
            token: super::super::config::ViewToken::new("token").unwrap(),
            discovery_interval: Duration::from_millis(10),
        },
        Arc::clone(&rpc) as Arc<dyn AppsRpc>,
        Arc::clone(&api) as Arc<dyn ViewSessionApi>,
        CancellationToken::new(),
    );
    wait_for_async(|| !APPS_STATE.state().read().descriptors.is_empty()).await;
    assert!(request_open_app("app-1"));
    wait_for_async(|| api.created.lock().unwrap().len() == 1).await;
    let view_id = APPS_STATE.state().read().views[0].view_id.clone();
    assert!(request_view_click(
        &view_id,
        ViewportPoint { x: 12.5, y: 34.0 }
    ));
    wait_for_async(|| !api.clicks.lock().unwrap().is_empty()).await;
    let clicks = api.clicks.lock().unwrap().clone();
    assert_eq!(clicks[0].0, "view-1");
    assert_eq!(clicks[0].1, ViewportPoint { x: 12.5, y: 34.0 });
    drop(clicks);
    handle.shutdown().await;
}

#[serial_test::serial]
#[tokio::test]
async fn click_is_zero_network_when_disabled() {
    let rpc = Arc::new(FakeRpc::with_session("session-1"));
    let handle = spawn_with_availability(
        AppsAvailability::Disabled,
        Arc::clone(&rpc) as Arc<dyn AppsRpc>,
        CancellationToken::new(),
    );
    assert!(handle.is_none());
    assert!(!request_view_click("v1", ViewportPoint { x: 1.0, y: 2.0 }));
}

#[test]
fn completed_error_result_is_forwarded_as_result_not_folded() {
    let raw = json!({"content": [{"type": "text", "text": "boom"}], "isError": true});
    assert_eq!(
        outcome_payload(completed_tool(raw.clone())),
        ViewAppResult::Result(raw),
        "isError:true 仍是工具结果事实，必须原样回填"
    );
}

#[test]
fn upstream_error_messages_are_sanitized() {
    let error = peri_acp_types::tool_outcome::JsonRpcError {
        code: -32000,
        message: "<html>secret</html>\nnext".to_string(),
        data: None,
    };
    assert_eq!(
        outcome_payload(SessionInvocationOutcome::RemoteProtocolError(error)),
        ViewAppResult::Error {
            code: -32000,
            message: "upstream_protocol_error".to_string()
        }
    );
    let error = peri_acp_types::tool_outcome::JsonRpcError {
        code: -32001,
        message: "plain detail".to_string(),
        data: None,
    };
    assert_eq!(
        outcome_payload(SessionInvocationOutcome::RemoteProtocolError(error)),
        ViewAppResult::Error {
            code: -32001,
            message: "plain detail".to_string()
        }
    );
}

#[test]
fn tools_call_request_wire_shape_matches_facade() {
    let request = AppsRequestRequest {
        session_id: "session-1".to_string(),
        attachment_id: AttachmentId::new("att-a"),
        epoch: ConnectionEpoch::new(3),
        seq: RequestSeq::new(8),
        operation: AppsOperation::ToolsCall {
            name: "get-time".to_string(),
            arguments: serde_json::Map::from_iter([(
                "tz".to_string(),
                serde_json::Value::String("UTC".to_string()),
            )]),
        },
    };
    assert_eq!(
        serde_json::to_value(request).expect("serialize"),
        json!({
            "sessionId": "session-1",
            "attachmentId": "att-a",
            "epoch": 3,
            "seq": 8,
            "operation": {"method": "tools/call", "name": "get-time", "arguments": {"tz": "UTC"}}
        })
    );
}

#[tokio::test]
async fn relay_follows_sequence_gap_expected_value() {
    let rpc = Arc::new(FakeRpc::with_session("session-1"));
    {
        let mut queue = rpc.tool_queue.lock().unwrap();
        queue.push_back(Ok(AppsRequestResponse {
            verdict: RequestVerdict::SequenceGap {
                expected: RequestSeq::new(5),
            },
            admitted: false,
            outcome: None,
            next_seq: RequestSeq::new(5),
        }));
        queue.push_back(Ok(tool_response(completed_tool(json!({"content": []})), 6)));
    }
    let api = Arc::new(FakeViewApi::default());
    let context = relay_context(
        "att-a",
        1,
        "v1",
        "view-1",
        Arc::clone(&rpc) as Arc<dyn AppsRpc>,
        Arc::clone(&api) as Arc<dyn ViewSessionApi>,
        Duration::from_secs(1),
    );
    let mut ledger_next = 2;
    let payload = relay_tools_call(
        &context,
        &tools_call(1, "get-time", json!({})),
        &mut ledger_next,
    )
    .await;
    assert!(matches!(payload, ViewAppResult::Result(_)));
    let seqs: Vec<u64> = rpc
        .tool_calls
        .lock()
        .unwrap()
        .iter()
        .map(|call| call.seq)
        .collect();
    assert_eq!(seqs, vec![2, 5], "SequenceGap 后必须改用 ledger 期望序号");
}

#[tokio::test]
async fn oversized_app_request_is_rejected_without_acp_call() {
    let rpc = Arc::new(FakeRpc::with_session("session-1"));
    let api = Arc::new(FakeViewApi::default());
    let context = relay_context(
        "att-a",
        1,
        "v1",
        "view-1",
        Arc::clone(&rpc) as Arc<dyn AppsRpc>,
        Arc::clone(&api) as Arc<dyn ViewSessionApi>,
        Duration::from_secs(1),
    );
    let mut ledger_next = 1;
    let payload = relay_tools_call(
        &context,
        &tools_call(
            1,
            "get-time",
            json!({"blob": "x".repeat(MAX_APP_REQUEST_BYTES + 1)}),
        ),
        &mut ledger_next,
    )
    .await;
    assert_eq!(
        payload,
        ViewAppResult::Error {
            code: -32603,
            message: "app_request_too_large".to_string()
        }
    );
    assert!(rpc.tool_calls.lock().unwrap().is_empty());
}
