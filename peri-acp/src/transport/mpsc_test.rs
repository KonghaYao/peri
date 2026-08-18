//! Tests for mpsc

use super::*;
use serde_json::json;

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
