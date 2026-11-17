//! Pump settlement regression scenarios.

use super::AcpNotification;
use super::AcpTuiClient;
use super::Arc;
use super::IncomingMessage;
use super::InteractionUiOutcome;
use super::TryRecvError;
use super::json;

#[tokio::test]
async fn test_pump_cancels_no_active_reverse_requests() {
    let (client_transport, server_transport) = peri_acp::transport::mpsc::mpsc_transport_pair();
    let (client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
    client.spawn_pump(notification_tx);
    let server = Arc::new(server_transport);
    for method in ["session/request_permission", "elicitation/create"] {
        super::assert_cancel_response(
            method,
            super::reverse_response(server.clone(), method, json!({"sessionId": "s1"})).await,
        );
        assert!(matches!(
            notification_rx.try_recv(),
            Err(TryRecvError::Empty)
        ));
    }
}

#[tokio::test]
async fn test_pump_cancels_snake_only_elicitation_without_ui_event() {
    let (client_transport, server_transport) = peri_acp::transport::mpsc::mpsc_transport_pair();
    let (client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
    client.lifecycle.force_stable("s1", true);
    client.spawn_pump(notification_tx);
    let response = super::reverse_response(
        Arc::new(server_transport),
        "elicitation/create",
        json!({"session_id": "s1"}),
    )
    .await;
    super::assert_cancel_response("elicitation/create", response);
    assert!(matches!(
        notification_rx.try_recv(),
        Err(TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn test_pump_forwards_registered_owner_and_response_claims_once() {
    let (client_transport, server_transport) = peri_acp::transport::mpsc::mpsc_transport_pair();
    let (client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
    client.lifecycle.force_stable("s1", true);
    client.spawn_pump(notification_tx);
    let server = Arc::new(server_transport);
    let task_server = server.clone();
    let request = tokio::spawn(async move {
        task_server
            .send_request(
                "session/request_permission",
                json!({"sessionId": "s1", "marker": "permission"}),
            )
            .await
            .expect("pump forwards registered owner and response claims once: fixture task and request must complete successfully")
    });
    let AcpNotification::RequestPermission { owner, params, .. } =
        notification_rx.recv().await.expect("pump forwards registered owner and response claims once: fixture task and request must complete successfully")
    else {
        panic!()
    };
    assert_eq!(params["marker"], "permission");
    assert!(
        client
            .respond_interaction(&owner, json!({"done": true}), "done".into())
            .await
            .expect("pump forwards registered owner and response claims once: the fixture operation must finish without an inner failure")
    );
    assert!(
        !client
            .respond_interaction(&owner, json!({"done": false}), "late".into())
            .await
            .expect("pump forwards registered owner and response claims once: the fixture operation must finish without an inner failure")
    );
    assert_eq!(request.await.expect("pump forwards registered owner and response claims once: the fixture operation must finish without an inner failure"), json!({"done": true}));
}

#[tokio::test]
async fn test_pump_cancels_exact_request_when_notification_receiver_is_dropped() {
    let (client_transport, server_transport) = peri_acp::transport::mpsc::mpsc_transport_pair();
    let (client, notification_tx, notification_rx) = AcpTuiClient::new(client_transport);
    client.lifecycle.force_stable("s1", true);
    drop(notification_rx);
    client.spawn_pump(notification_tx);
    let response = super::reverse_response(
        Arc::new(server_transport),
        "session/request_permission",
        json!({"sessionId": "s1"}),
    )
    .await;
    super::assert_cancel_response("session/request_permission", response);
}

#[tokio::test]
async fn test_cancel_drains_permission_before_turn_terminal() {
    super::cancel_drains_kind_before_notification("session/request_permission").await;
}

#[tokio::test]
async fn test_cancel_drains_elicitation_before_turn_terminal() {
    super::cancel_drains_kind_before_notification("elicitation/create").await;
}

#[tokio::test]
#[serial_test::serial]
async fn test_new_immediate_post_response_notification_buffered_until_commit() {
    let (client_transport, server_transport) = peri_acp::transport::mpsc::mpsc_transport_pair();
    let (client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
    client.spawn_pump(notification_tx);
    let (commit_hook_tx, mut commit_hook_rx) = tokio::sync::mpsc::unbounded_channel();
    client.install_transition_commit_hook(commit_hook_tx);
    let server = Arc::new(server_transport);
    let server_task = server.clone();
    let serve = tokio::spawn(async move {
        let IncomingMessage::Request { id, method, .. } = server_task.recv().await.expect("new immediate post response notification buffered until commit: fixture task and request must complete successfully") else {
            panic!()
        };
        assert_eq!(method, "session/new");
        server_task
            .send_notification("session/update", json!({"sessionId":"old","update":{}}))
            .await
            .expect("new immediate post response notification buffered until commit: fixture task and request must complete successfully");
        server_task
            .send_response(id, Ok(json!({"sessionId":"new"})))
            .await
            .expect("new immediate post response notification buffered until commit: fixture task and request must complete successfully");
    });
    let new_client = client.clone();
    let new_session = tokio::spawn(async move { new_client.new_session("/tmp", None).await });
    let release_commit = commit_hook_rx.recv().await.expect("new immediate post response notification buffered until commit: fixture task and request must complete successfully");
    serve.await.expect("new immediate post response notification buffered until commit: fixture task and request must complete successfully");

    // The lifecycle response has arrived, but commit is causally paused. FIFO on
    // the real MPSC transport makes the reverse request below an acknowledgement
    // that the preceding ordinary notification was routed while Transitioning.
    server
        .send_notification(
            "session/update",
            json!({"sessionId":"new","update":{"marker":"new"}}),
        )
        .await
        .expect("new immediate post response notification buffered until commit: fixture task and request must complete successfully");
    let reverse_server = server.clone();
    let reverse = tokio::spawn(async move {
        reverse_server
            .send_request("session/request_permission", json!({"sessionId":"new"}))
            .await
            .expect("new immediate post response notification buffered until commit: fixture task and request must complete successfully")
    });
    super::assert_cancel_response("session/request_permission", reverse.await.expect("new immediate post response notification buffered until commit: fixture task and request must complete successfully"));
    assert!(matches!(
        notification_rx.try_recv(),
        Err(TryRecvError::Empty)
    ));

    release_commit.send(()).expect("new immediate post response notification buffered until commit: fixture channel must deliver its expected message");
    assert_eq!(new_session.await.expect("new immediate post response notification buffered until commit: the fixture operation must finish without an inner failure").expect("new immediate post response notification buffered until commit: the fixture operation must finish without an inner failure"), "new");
    let AcpNotification::SessionUpdate { session_id, params } =
        notification_rx.recv().await.expect("new immediate post response notification buffered until commit: fixture task and request must complete successfully")
    else {
        panic!()
    };
    assert_eq!(session_id, "new");
    assert_eq!(params["update"]["marker"], "new");
    assert!(matches!(
        notification_rx.try_recv(),
        Err(TryRecvError::Empty)
    ));
}

/// 服务端通过 `$/cancel_request` 撤销挂起 elicitation（提问超时）：
/// 客户端按 wire id 认领，结算为 `Expired(ServerCancelled)`，供 bridge 关闭
/// 问答面板并恢复普通输入。
#[tokio::test]
async fn test_server_cancel_request_expires_pending_elicitation() {
    use crate::acp_client::InteractionExpiryReason;
    use tokio_util::sync::CancellationToken;

    let (client_transport, server_transport) = peri_acp::transport::mpsc::mpsc_transport_pair();
    let (client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
    client.lifecycle.force_stable("s1", true);
    client.spawn_pump(notification_tx);
    let server = Arc::new(server_transport);

    let cancellation = CancellationToken::new();
    let request_server = server.clone();
    let request_cancellation = cancellation.clone();
    let request = tokio::spawn(async move {
        request_server
            .send_cancellable_request(
                "elicitation/create",
                json!({"sessionId":"s1"}),
                request_cancellation,
            )
            .await
    });

    let AcpNotification::Elicitation { owner, .. } = notification_rx.recv().await.expect("server cancel request expires pending elicitation: fixture task and request must complete successfully") else {
        panic!("expected elicitation request");
    };
    cancellation.cancel();

    let error = request
        .await
        .expect("server cancel request expires pending elicitation: fixture task and request must complete successfully")
        .expect_err("cancelled request must fail");
    assert_eq!(error.code, -32800);

    let notification = notification_rx.recv().await.expect("server cancel request expires pending elicitation: fixture task and request must complete successfully");
    let AcpNotification::InteractionTerminal {
        owner: ended,
        outcome,
    } = notification
    else {
        panic!("expected interaction terminal");
    };
    assert_eq!(ended, owner);
    assert_eq!(
        outcome,
        InteractionUiOutcome::Expired {
            reason: InteractionExpiryReason::ServerCancelled,
        }
    );
}

use peri_acp::transport::AcpTransport;
