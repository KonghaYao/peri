use std::time::Duration;

use peri_acp::transport::{
    mpsc::{MpscServerTransport, mpsc_transport_pair},
    types::{IncomingMessage, RequestId},
};
use peri_acp_types::{messages::MessageContent, session::UserInputQueueReceipt};
use serde_json::json;

use super::*;
use crate::acp_client::interaction_lifecycle::TransitionKind;

async fn next_request(server: &MpscServerTransport, expected: &str) -> RequestId {
    let incoming = tokio::time::timeout(Duration::from_secs(2), server.recv())
        .await
        .unwrap()
        .unwrap();
    let IncomingMessage::Request { id, method, .. } = incoming else {
        panic!("expected {expected}, got {incoming:?}");
    };
    assert_eq!(method, expected);
    id
}

fn snapshot() -> serde_json::Value {
    json!({"sessionId":"s","generation":"g","revision":1,"items":[]})
}

fn enqueue() -> EnqueueUserInputRequest {
    EnqueueUserInputRequest {
        session_id: "s".into(),
        generation: "g".into(),
        command_id: "command".into(),
        input_id: "input".into(),
        content: MessageContent::text("synthetic input"),
        original_draft: "synthetic input".into(),
    }
}

/// [回归测试] 首次 snapshot 的 generation 绑定必须先于同时到达的 canonical Delivered。
#[tokio::test]
async fn test_initial_snapshot_reserves_generation_before_delivery() {
    let (transport, server) = mpsc_transport_pair();
    let (client, notification_tx, mut notifications) = AcpTuiClient::new(transport);
    client.force_stable_for_test("s", false);
    client.user_input_queue.store(true, Ordering::Release);
    client.spawn_pump(notification_tx);
    let binding_client = client.clone();
    let binding = tokio::spawn(async move { binding_client.user_input_snapshot("s").await });
    let request_id = next_request(&server, "session/input/snapshot").await;
    server.send_notification("peri/agent_event", json!({
        "sessionId":"s", "event_json":serde_json::to_string(&AcpEvent::UserInputDelivered {
            generation:"g".into(), input_id:"input".into(), content:MessageContent::text("synthetic input"),
        }).unwrap(),
    })).await.unwrap();
    assert!(notifications.try_recv().is_err());
    let mut response = snapshot();
    response["activeRequestId"] = json!("run");
    server
        .send_response(request_id, Ok(response))
        .await
        .unwrap();
    binding.await.unwrap().unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), notifications.recv())
            .await
            .unwrap(),
        Some(AcpNotification::AgentEvent {
            event: AcpEvent::UserInputRunStarted { .. },
            ..
        })
    ));
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), notifications.recv())
            .await
            .unwrap(),
        Some(AcpNotification::AgentEvent {
            event: AcpEvent::UserInputDelivered { .. },
            ..
        })
    ));
    client.close();
}

/// [回归测试] 慢 snapshot 曾跨 RPC 持有 gate，新的输入和取消无法准入。
#[tokio::test]
async fn test_slow_snapshot_allows_enqueue_before_snapshot_receipt() {
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.force_stable_for_test("s", false);
    assert!(client.lifecycle.bind_user_input_generation("s", 1, "g"));
    let refreshing_client = client.clone();
    let refreshing = tokio::spawn(async move { refreshing_client.user_input_snapshot("s").await });
    let snapshot_id = next_request(&server, "session/input/snapshot").await;
    let submitting_client = client.clone();
    let submitting =
        tokio::spawn(async move { submitting_client.enqueue_user_input(&enqueue()).await });
    let enqueue_id = next_request(&server, "session/input/enqueue").await;
    assert!(!refreshing.is_finished());
    let gate = tokio::time::timeout(
        Duration::from_secs(2),
        client.lifecycle.operation_gate().lock(),
    )
    .await
    .unwrap();
    drop(gate);
    server
        .send_response(enqueue_id, Ok(json!({"snapshot":snapshot(),"results":[]})))
        .await
        .unwrap();
    let receipt: UserInputQueueReceipt = submitting.await.unwrap().unwrap();
    assert_eq!(receipt.snapshot.generation, "g");
    server
        .send_response(snapshot_id, Ok(snapshot()))
        .await
        .unwrap();
    refreshing.await.unwrap().unwrap();
    client.close();
}

/// [回归测试] 缩短 gate 后，迟到的受理响应不能跨 load epoch 被重新应用。
#[tokio::test]
async fn test_late_enqueue_receipt_is_unknown_after_session_boundary() {
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.force_stable_for_test("s", false);
    let submitting_client = client.clone();
    let submitting =
        tokio::spawn(async move { submitting_client.enqueue_user_input(&enqueue()).await });
    let request_id = next_request(&server, "session/input/enqueue").await;
    let gate = tokio::time::timeout(
        Duration::from_secs(2),
        client.lifecycle.operation_gate().lock(),
    )
    .await
    .unwrap();
    let transition = client
        .lifecycle
        .begin_transition(TransitionKind::Load, Some("s".into()))
        .unwrap();
    client
        .lifecycle
        .commit_stable(transition.generation, "s".into());
    drop(gate);
    server
        .send_response(request_id, Ok(json!({"snapshot":snapshot(),"results":[]})))
        .await
        .unwrap();
    assert_eq!(submitting.await.unwrap().unwrap_err().code, -32603);
    client.close();
}

/// [回归测试] 旧 snapshot 不得覆盖更新 run，也不能在 Stop 后重开已退役 run。
#[tokio::test]
async fn test_late_snapshot_does_not_reopen_stopped_or_newer_run() {
    let (transport, server) = mpsc_transport_pair();
    let (client, _, mut notifications) = AcpTuiClient::new(transport);
    client.force_stable_for_test("s", false);
    assert!(client.lifecycle.bind_user_input_generation("s", 1, "g"));
    assert!(
        client
            .lifecycle
            .open_user_input_run("s", "g", "old")
            .is_some()
    );
    for stop in [false, true] {
        let refreshing_client = client.clone();
        let refreshing =
            tokio::spawn(async move { refreshing_client.user_input_snapshot("s").await });
        let request_id = next_request(&server, "session/input/snapshot").await;
        let gate = tokio::time::timeout(
            Duration::from_secs(2),
            client.lifecycle.operation_gate().lock(),
        )
        .await
        .unwrap();
        if stop {
            client.lifecycle.cancel_active_prompt();
        } else {
            assert!(
                client
                    .lifecycle
                    .open_user_input_run("s", "g", "new")
                    .is_some()
            );
        }
        drop(gate);
        let mut response = snapshot();
        response["activeRequestId"] = json!(if stop { "new" } else { "old" });
        server
            .send_response(request_id, Ok(response))
            .await
            .unwrap();
        refreshing.await.unwrap().unwrap();
        assert_eq!(
            client
                .lifecycle
                .active_user_input_run()
                .map(|identity| identity.2),
            if stop { None } else { Some("new".into()) }
        );
        assert!(notifications.try_recv().is_err());
    }
    client.close();
}

/// [回归测试] 尚未看到 run 的 Pause 也必须使已发出的 snapshot 恢复令牌失效。
#[tokio::test]
async fn test_snapshot_identity_invalidates_after_pause_without_local_run() {
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.force_stable_for_test("s", false);
    assert!(client.lifecycle.bind_user_input_generation("s", 1, "g"));
    let refreshing_client = client.clone();
    let refreshing = tokio::spawn(async move { refreshing_client.user_input_snapshot("s").await });
    let request_id = next_request(&server, "session/input/snapshot").await;
    let gate = client.lifecycle.operation_gate().lock().await;
    let before = client.lifecycle.user_input_snapshot_identity("s").unwrap();
    client.lifecycle.cancel_active_prompt();
    assert_ne!(
        client.lifecycle.user_input_snapshot_identity("s").unwrap(),
        before
    );
    drop(gate);
    let mut response = snapshot();
    response["activeRequestId"] = json!("unobserved-run");
    server
        .send_response(request_id, Ok(response))
        .await
        .unwrap();
    refreshing.await.unwrap().unwrap();
    assert!(client.lifecycle.active_user_input_run().is_none());
    client.close();
}
