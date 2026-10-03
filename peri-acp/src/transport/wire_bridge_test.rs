use serde_json::json;

use super::*;
use crate::transport::mpsc::mpsc_transport_pair;

#[tokio::test]
async fn preserves_wire_ids_and_forwards_both_directions() {
    let (client, server) = mpsc_transport_pair();
    let bridge = WireBridge::new(client);

    bridge
        .send(json!({"jsonrpc":"2.0","id":"external-1","method":"arbitrary/request","params":{"x":1}}))
        .await
        .unwrap();
    let request = server.recv().await.unwrap();
    let IncomingMessage::Request { id, method, params } = request else {
        panic!("expected request");
    };
    assert_eq!(method, "arbitrary/request");
    assert_eq!(params, json!({"x":1}));
    server
        .send_response(id, Ok(json!({"ok":true})))
        .await
        .unwrap();
    assert_eq!(
        bridge.recv().await.unwrap(),
        json!({"jsonrpc":"2.0","id":"external-1","result":{"ok":true}})
    );

    server
        .send_notification("arbitrary/event", json!({"n":2}))
        .await
        .unwrap();
    assert_eq!(
        bridge.recv().await.unwrap(),
        json!({"jsonrpc":"2.0","method":"arbitrary/event","params":{"n":2}})
    );

    let reverse = tokio::spawn(async move {
        server
            .send_request("arbitrary/reverse", json!({"q":3}))
            .await
    });
    let frame = bridge.recv().await.unwrap();
    assert_eq!(frame["method"], "arbitrary/reverse");
    bridge
        .send(json!({"jsonrpc":"2.0","id":frame["id"],"result":{"answer":4}}))
        .await
        .unwrap();
    assert_eq!(reverse.await.unwrap().unwrap(), json!({"answer":4}));
    bridge.close().await;
}

#[tokio::test]
async fn rejects_invalid_frames_without_dispatching() {
    let (client, _server) = mpsc_transport_pair();
    let bridge = WireBridge::new(client);
    assert_eq!(
        bridge
            .send(json!({"jsonrpc":"2.0","id":null,"method":"x"}))
            .await
            .unwrap_err()
            .code,
        -32600
    );
    assert_eq!(
        bridge
            .send(json!({"jsonrpc":"2.0","id":1,"result":1,"error":{}}))
            .await
            .unwrap_err()
            .code,
        -32600
    );
    bridge.close().await;
}

#[tokio::test]
async fn notification_can_overtake_a_pending_request() {
    let (client, server) = mpsc_transport_pair();
    let bridge = WireBridge::new(client);
    bridge
        .send(json!({"jsonrpc":"2.0","id":7,"method":"slow/request"}))
        .await
        .unwrap();
    let IncomingMessage::Request { id, .. } = server.recv().await.unwrap() else {
        panic!("expected pending request");
    };
    bridge
        .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"s"}}))
        .await
        .unwrap();
    let IncomingMessage::Notification { method, params } = server.recv().await.unwrap() else {
        panic!("expected cancellation notification");
    };
    assert_eq!(method, "session/cancel");
    assert_eq!(params, json!({"sessionId":"s"}));
    server
        .send_response(id, Err(AcpError::new(-32800, "request cancelled")))
        .await
        .unwrap();
    assert_eq!(
        bridge.recv().await.unwrap(),
        json!({
            "jsonrpc":"2.0", "id":7,
            "error":{"code":-32800,"message":"request cancelled"}
        })
    );
    bridge.close().await;
}
