use super::*;
use peri_acp::event::AcpEvent;
use peri_acp::transport::{AcpTransport, mpsc::mpsc_transport_pair};
use peri_acp_types::{
    event::{ChildEventOrigin, ChildEventStream, ExecutorEvent},
    identity::AttemptId,
    session::{ExecutionBinding, TurnId},
};
use serde_json::json;

#[tokio::test]
#[serial_test::serial]
async fn child_origin_survives_wire_pump_and_notifier_queues() {
    let (transport, server) = mpsc_transport_pair();
    let (client, tx, rx) = AcpTuiClient::new_interactive(transport);
    client.lifecycle.force_stable("s1", false);
    client.spawn_pump(tx);
    let (bridge_tx, mut bridge_rx) = tokio::sync::mpsc::channel(4);
    let shutdown = tokio_util::sync::CancellationToken::new();
    let notifier = crate::kit::acp_notifier::spawn_kit_notifier(rx, bridge_tx, shutdown.clone());
    let execution = ExecutionBinding {
        turn_id: TurnId::new(),
        attempt_id: AttemptId::new(),
    };
    let event = ChildEventStream::new("child".into(), execution.clone()).wrap(
        ExecutorEvent::SubagentStarted {
            agent_name: "worker".into(),
            instance_id: "child".into(),
            is_background: false,
            parent_tool_call_id: None,
        },
    );
    let origin = event.child_origin().unwrap().clone();
    server.send_notification("peri/agent_event", json!({
        "sessionId": "s1", "_meta": {"peri": {"childRun": origin}},
        "event": AcpEvent::SubagentStarted { agent_name: "worker".into(), instance_id: "child".into(), is_background: false, parent_tool_call_id: None },
    })).await.unwrap();
    let delivered = bridge_rx.recv().await.unwrap();
    assert_eq!(delivered.child_origin, Some(origin));
    assert_eq!(delivered.active_session_id, "s1");
    assert!(matches!(
        delivered.event,
        crate::kit::acp_types::AcpEventData::SubagentStarted { .. }
    ));
    shutdown.cancel();
    notifier.await.unwrap();
    client.close();
}

#[tokio::test]
async fn malformed_child_origin_does_not_fall_back_to_unscoped_event() {
    let (transport, server) = mpsc_transport_pair();
    let (client, tx, mut rx) = AcpTuiClient::new_interactive(transport);
    client.lifecycle.force_stable("s1", false);
    client.spawn_pump(tx);
    server
        .send_notification(
            "session/update",
            json!({"sessionId":"s1", "_meta":{"peri":{"childRun":{}}}, "test":"invalid"}),
        )
        .await
        .unwrap();
    server
        .send_notification(
            "session/update",
            json!({"sessionId":"s1", "test":"barrier"}),
        )
        .await
        .unwrap();
    assert!(
        matches!(rx.recv().await, Some(AcpNotification::SessionUpdate { params, .. }) if params["test"] == "barrier")
    );
    client.close();
}

#[test]
fn notification_envelope_keeps_capture_epoch_across_local_reset() {
    let origin: ChildEventOrigin = ChildEventStream::new(
        "child".into(),
        ExecutionBinding {
            turn_id: TurnId::new(),
            attempt_id: AttemptId::new(),
        },
    )
    .wrap(ExecutorEvent::SubagentStarted {
        agent_name: "worker".into(),
        instance_id: "child".into(),
        is_background: false,
        parent_tool_call_id: None,
    })
    .child_origin()
    .unwrap()
    .clone();
    let queued = AcpNotification::SessionUpdate {
        session_id: "s1".into(),
        params: json!({}),
    }
    .with_child_origin(Some(origin.clone()), 41);
    assert!(
        matches!(queued, AcpNotification::ChildEvent { origin: actual, view_epoch: 41, .. } if actual == origin)
    );
}
