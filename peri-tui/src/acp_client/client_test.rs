use super::*;
use peri_acp::transport::{AcpTransport, mpsc::mpsc_transport_pair, types::IncomingMessage};
use serde_json::json;

/// Issue 2026-08-05 返工链路测试：pump 解析 `peri/agent_event_done` 的
/// requestId → `AgentDone.request_id`（服务器回带 → TUI stale 配对）。
#[tokio::test]
async fn test_pump_parses_agent_event_done_request_id() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
    client.lifecycle.force_stable("s1", false);
    client.spawn_pump(notification_tx);

    server_transport
        .send_notification(
            "peri/agent_event_done",
            json!({
                "sessionId": "s1",
                "stopReason": "cancelled",
                "requestId": "rid-1",
            }),
        )
        .await
        .unwrap();

    match notification_rx.recv().await.unwrap() {
        AcpNotification::AgentDone {
            session_id,
            stop_reason,
            request_id,
        } => {
            assert_eq!(session_id, "s1");
            assert_eq!(stop_reason, "cancelled");
            assert_eq!(request_id.as_deref(), Some("rid-1"));
        }
        other => panic!("expected AgentDone, got {other:?}"),
    }
}

/// 兼容性：requestId 缺失时 AgentDone.request_id 应为 None（continuation /
/// Immediate 命令 / stdio 等路径）。
#[tokio::test]
async fn test_pump_agent_event_done_without_request_id() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
    client.lifecycle.force_stable("s1", false);
    client.spawn_pump(notification_tx);

    server_transport
        .send_notification(
            "peri/agent_event_done",
            json!({ "sessionId": "s1", "stopReason": "end_turn" }),
        )
        .await
        .unwrap();

    match notification_rx.recv().await.unwrap() {
        AcpNotification::AgentDone {
            session_id,
            stop_reason,
            request_id,
        } => {
            assert_eq!(session_id, "s1");
            assert_eq!(stop_reason, "end_turn");
            assert_eq!(request_id, None);
        }
        other => panic!("expected AgentDone, got {other:?}"),
    }
}

// ── M3 回归：已删除会话的延迟通知必须被过滤 ──────────────────────────────

/// 复现"幽灵播放"场景：current_session_id=None（删除当前会话后）时，
/// 黑名单中的会话事件必须被 drop——None 放行语义只服务于首次连接初始化。
#[tokio::test]
async fn test_pump_drops_events_from_deleted_session_when_current_none() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
    client.spawn_pump(notification_tx);

    // 删除会话（模拟：current 置 None + 黑名单插入）
    client.lifecycle.mark_deleted("deleted-sess");

    // 已删除会话的残流通知（agent_event / unstable_event / agent_done）
    server_transport
        .send_notification(
            "peri/agent_event",
            json!({ "sessionId": "deleted-sess", "event_json": serde_json::to_string(&AcpEvent::StateSnapshot { messages_json: "[]".to_string() }).unwrap() }),
        )
        .await
        .unwrap();
    server_transport
        .send_notification(
            "peri/agent_event_done",
            json!({ "sessionId": "deleted-sess", "stopReason": "cancelled" }),
        )
        .await
        .unwrap();

    // 无事件应到达 UI
    match tokio::time::timeout(
        std::time::Duration::from_millis(200),
        notification_rx.recv(),
    )
    .await
    {
        Err(_) => {} // 超时 = 全部被过滤 ✓
        Ok(Some(other)) => panic!("已删除会话的事件不应回写 UI: {other:?}"),
        Ok(None) => panic!("pump 意外退出"),
    }
}

/// 黑名单不误伤：current=None 时未删除会话的初始化通知仍正常放行。
#[tokio::test]
async fn test_pump_still_forwards_init_notifications_when_current_none() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
    client.lifecycle.force_stable("s-init", false);
    client.spawn_pump(notification_tx);

    server_transport
        .send_notification(
            "session/update",
            json!({ "sessionId": "s-init", "commands": [] }),
        )
        .await
        .unwrap();

    match notification_rx.recv().await.unwrap() {
        AcpNotification::SessionUpdate { session_id, .. } => {
            assert_eq!(session_id, "s-init");
        }
        other => panic!("初始化通知应放行, got {other:?}"),
    }
}

/// delete_session 完整链路：服务端响应后，current 清空 + 黑名单记录，
/// 该会话后续事件被过滤。
#[tokio::test]
async fn test_delete_session_clears_current_and_blacklists() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
    client.spawn_pump(notification_tx);

    // 先构造"当前会话"状态（等价于 new_session 成功后）
    client.lifecycle.force_stable("sess-1", false);

    // server 端响应 session/delete（标准空对象）
    let server_transport = std::sync::Arc::new(server_transport);
    let server_tx_for_task = server_transport.clone();
    let server = tokio::spawn(async move {
        let msg = server_tx_for_task.recv().await.unwrap();
        let IncomingMessage::Request { id, method, params } = msg else {
            panic!("expected request, got {msg:?}");
        };
        assert_eq!(method, "session/delete");
        assert_eq!(
            params.get("sessionId").and_then(|v| v.as_str()),
            Some("sess-1")
        );
        server_tx_for_task
            .send_response(id, Ok(serde_json::json!({})))
            .await
            .unwrap();
    });

    client
        .delete_session("sess-1")
        .await
        .expect("delete 应成功");
    server.await.unwrap();

    assert!(
        client.current_session_id().is_none(),
        "删除当前会话后 current_session_id 应清空"
    );
    assert!(
        !client.lifecycle.is_current_session("sess-1"),
        "删除的会话应记入黑名单"
    );

    // 已删除会话的残流被过滤
    server_transport
        .send_notification(
            "peri/agent_event_done",
            json!({ "sessionId": "sess-1", "stopReason": "cancelled" }),
        )
        .await
        .unwrap();
    match tokio::time::timeout(
        std::time::Duration::from_millis(200),
        notification_rx.recv(),
    )
    .await
    {
        Err(_) => {}
        Ok(Some(other)) => panic!("已删除会话的事件不应回写 UI: {other:?}"),
        Ok(None) => panic!("pump 意外退出"),
    }
}
