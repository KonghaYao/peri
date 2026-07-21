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
