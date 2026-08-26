//! Tests for mpsc

use super::*;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

fn assert_transport_closed(error: AcpError) {
    assert_eq!(error.code, -32603);
    assert_eq!(error.message, "Transport closed");
    assert!(error.data.is_none());
}

#[tokio::test]
async fn test_request_response() {
    let (client, server) = mpsc_transport_pair();

    // Server side: echo back the params
    let server_handle = tokio::spawn(async move {
        if let Some(IncomingMessage::Request {
            id,
            method: _,
            params,
        }) = server.recv().await
        {
            let _ = server.send_response(id, Ok(params)).await;
        }
    });

    // Client sends a request
    let result = client
        .send_request("test/echo", json!({"hello": "world"}))
        .await
        .unwrap();
    assert_eq!(result, json!({"hello": "world"}));

    server_handle.await.unwrap();
}

#[tokio::test]
async fn test_notification() {
    let (client, server) = mpsc_transport_pair();

    client
        .send_notification("test/notify", json!({"msg": "ping"}))
        .await
        .unwrap();

    // Server receives it
    if let Some(IncomingMessage::Notification { method, params }) = server.recv().await {
        assert_eq!(method, "test/notify");
        assert_eq!(params, json!({"msg": "ping"}));
    } else {
        panic!("expected notification");
    }
}

/// 统一 host 对 `session/prompt` fatal failure 返回 `Err(AcpError)` 后，
/// mpsc transport 将标准 JSON-RPC error 完整送回 `send_request` 的 Err
/// （code / message / data 逐字段一致，RequestId 经 router 配对）。
///
/// 模拟能力协商未开启的客户端：error response 不经任何私有事件/能力判定，
/// 仅凭响应本身即可感知 turn failure（spec/issues/2026-08-18-acp-error-handler.md
/// 必测矩阵「capability 未协商的 fatal」）。
#[tokio::test]
async fn test_error_response_roundtrip_preserves_code_message_data() {
    let (client, server) = mpsc_transport_pair();

    let server_handle = tokio::spawn(async move {
        if let Some(IncomingMessage::Request { id, method, .. }) = server.recv().await {
            assert_eq!(method, "session/prompt");
            let _ = server
                .send_response(
                    id,
                    Err(AcpError {
                        // 与 host::prompt::ACP_TURN_EXECUTION_FAILED_CODE 同值
                        // （host 为私有模块，此处以字面量锁定 wire 行为；命名常量
                        // 的映射契约由 host/prompt_test.rs 的 seam 测试固定）。
                        code: -32000,
                        message: "agent turn execution failed".to_string(),
                        data: None,
                    }),
                )
                .await;
        }
    });

    let err = client
        .send_request(
            "session/prompt",
            json!({"prompt": [{"type": "text", "text": "hi"}]}),
        )
        .await
        .expect_err("fatal 失败响应必须落到 send_request 的 Err");
    assert_eq!(
        err.code, -32000,
        "wire code 与命名常量 ACP_TURN_EXECUTION_FAILED_CODE(-32000) 一致"
    );
    assert_eq!(err.message, "agent turn execution failed");
    assert!(err.data.is_none(), "首版 data 必须为 None");

    server_handle.await.unwrap();
}

#[tokio::test]
async fn test_bidirectional_server_notification_to_client() {
    let (client, server) = mpsc_transport_pair();

    // Server sends a notification to client
    server
        .send_notification("test/hello", json!({"msg": "from_server"}))
        .await
        .unwrap();

    // Client receives it
    if let Some(IncomingMessage::Notification { method, params }) = client.recv().await {
        assert_eq!(method, "test/hello");
        assert_eq!(params, json!({"msg": "from_server"}));
    } else {
        panic!("expected notification from server");
    }
}

/// [回归测试] 任一 MPSC 方向终止后，两个方向的 pending 请求都必须结算。
#[tokio::test]
async fn test_peer_drop_settles_pending_requests_in_both_directions() {
    let (client, server) = mpsc_transport_pair();
    let client = Arc::new(client);
    let client_request = tokio::spawn({
        let client = Arc::clone(&client);
        async move { client.send_request("from/client", json!({})).await }
    });
    let second_client_request = tokio::spawn({
        let client = Arc::clone(&client);
        async move { client.send_request("from/client/two", json!({})).await }
    });
    let _ = server.recv().await.expect("server should receive request");
    let _ = server
        .recv()
        .await
        .expect("server should receive second request");
    drop(server);
    let client_error = tokio::time::timeout(Duration::from_secs(5), client_request)
        .await
        .expect("client request should settle")
        .unwrap()
        .unwrap_err();
    assert_transport_closed(client_error);
    let second_client_error = tokio::time::timeout(Duration::from_secs(5), second_client_request)
        .await
        .expect("second client request should settle")
        .unwrap()
        .unwrap_err();
    assert_transport_closed(second_client_error);
    assert_transport_closed(
        client
            .send_request("after/close", json!({}))
            .await
            .unwrap_err(),
    );
    assert_transport_closed(
        client
            .send_notification("after/close", json!({}))
            .await
            .unwrap_err(),
    );

    let (client, server) = mpsc_transport_pair();
    let server = Arc::new(server);
    let server_request = tokio::spawn({
        let server = Arc::clone(&server);
        async move { server.send_request("from/server", json!({})).await }
    });
    let _ = client.recv().await.expect("client should receive request");
    drop(client);
    let server_error = tokio::time::timeout(Duration::from_secs(5), server_request)
        .await
        .expect("server request should settle")
        .unwrap()
        .unwrap_err();
    assert_transport_closed(server_error);
}

#[tokio::test]
async fn test_response_before_peer_drop_wins() {
    let (client, server) = mpsc_transport_pair();
    let request = tokio::spawn(async move { client.send_request("race", json!({})).await });
    let IncomingMessage::Request { id, .. } = server.recv().await.unwrap() else {
        panic!("expected request");
    };
    server
        .send_response(id, Ok(json!("response won")))
        .await
        .unwrap();
    drop(server);
    assert_eq!(request.await.unwrap().unwrap(), json!("response won"));
}

/// [回归测试] caller abort 后 RAII handle 同步注销，迟到响应必须成为 unmatched 消息。
#[tokio::test]
async fn test_cancelled_request_unregisters_before_late_response() {
    let (client, server) = mpsc_transport_pair();
    let client = Arc::new(client);
    let task = tokio::spawn({
        let client = Arc::clone(&client);
        async move { client.send_request("cancel/me", json!({})).await }
    });
    let IncomingMessage::Request { id, .. } = server.recv().await.unwrap() else {
        panic!("expected request");
    };
    task.abort();
    task.await.expect_err("request task should be aborted");
    server
        .send_response(id.clone(), Ok(json!("late")))
        .await
        .unwrap();
    let late = tokio::time::timeout(Duration::from_secs(5), client.recv())
        .await
        .expect("late response should be forwarded")
        .expect("incoming queue should remain open");
    match late {
        IncomingMessage::Response { id: actual, result } => {
            assert_eq!(actual, id);
            assert_eq!(result.unwrap(), json!("late"));
        }
        other => panic!("expected unmatched response, got {other:?}"),
    }
}

/// 已由 pump 转发的消息必须在逻辑连接终止后按顺序排空，再返回 None。
#[tokio::test]
async fn test_peer_drop_preserves_forwarded_incoming_queue() {
    let (client, server) = mpsc_transport_pair();
    client
        .send_notification("queued/one", json!({"n": 1}))
        .await
        .unwrap();
    client
        .send_notification("queued/two", json!({"n": 2}))
        .await
        .unwrap();
    drop(client);

    let first = server.recv().await.expect("first queued notification");
    let second = server.recv().await.expect("second queued notification");
    assert!(
        server.recv().await.is_none(),
        "queue should close after draining"
    );
    match (first, second) {
        (
            IncomingMessage::Notification { method: first, .. },
            IncomingMessage::Notification { method: second, .. },
        ) => assert_eq!(
            (first.as_str(), second.as_str()),
            ("queued/one", "queued/two")
        ),
        other => panic!("expected two queued notifications, got {other:?}"),
    }
}
