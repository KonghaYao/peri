use std::{collections::HashSet, sync::Arc, time::Duration};

use agent_client_protocol::schema::v1::{RequestPermissionOutcome, RequestPermissionResponse};
use agent_client_protocol_schema::v1::{CreateElicitationResponse, ElicitationAction};
use peri_acp::transport::{AcpTransport, mpsc::mpsc_transport_pair};
use serde_json::{Value, json};
use tokio::sync::mpsc::error::TryRecvError;

use super::*;

fn routing_state(current: Option<&str>, deleted: &[&str]) -> Arc<Mutex<SessionRoutingState>> {
    Arc::new(Mutex::new(SessionRoutingState {
        current_session_id: current.map(str::to_string),
        deleted_session_ids: deleted.iter().map(|id| (*id).to_string()).collect(),
    }))
}

fn assert_forward(plan: Option<ReverseRequestPlan>) {
    assert!(matches!(plan, Some(ReverseRequestPlan::Forward { .. })));
}

fn assert_settle(plan: Option<ReverseRequestPlan>) {
    assert!(matches!(plan, Some(ReverseRequestPlan::Settle { .. })));
}

#[test]
fn test_permission_reverse_session_id_matrix_fail_closed() {
    let state = routing_state(Some("s1"), &[]);
    let valid = [
        json!({"sessionId": "s1"}),
        json!({"session_id": "s1"}),
        json!({"sessionId": "s1", "session_id": "s1"}),
    ];
    for params in valid {
        assert_forward(plan_reverse_request(
            "session/request_permission",
            RequestId::Number(1),
            params,
            &state,
        ));
    }
    let invalid = [
        json!({}),
        json!({"sessionId": null}),
        json!({"sessionId": 1}),
        json!({"sessionId": ""}),
        json!({"sessionId": "s1", "session_id": null}),
        json!({"sessionId": null, "session_id": "s1"}),
        json!({"sessionId": "s1", "session_id": "s2"}),
    ];
    for params in invalid {
        assert_settle(plan_reverse_request(
            "session/request_permission",
            RequestId::Number(1),
            params,
            &state,
        ));
    }
}

#[test]
fn test_elicitation_reverse_session_id_requires_canonical_camel_case() {
    let state = routing_state(Some("s1"), &[]);
    for params in [
        json!({"sessionId": "s1"}),
        json!({"sessionId": "s1", "session_id": "ignored"}),
    ] {
        assert_forward(plan_reverse_request(
            "elicitation/create",
            RequestId::Number(1),
            params,
            &state,
        ));
    }
    for params in [
        json!({}),
        json!({"sessionId": null}),
        json!({"sessionId": 1}),
        json!({"sessionId": ""}),
        json!({"session_id": "s1"}),
    ] {
        assert_settle(plan_reverse_request(
            "elicitation/create",
            RequestId::Number(1),
            params,
            &state,
        ));
    }
}

#[test]
fn test_reverse_plan_requires_nonempty_exact_current_session() {
    for method in ["session/request_permission", "elicitation/create"] {
        assert_settle(plan_reverse_request(
            method,
            RequestId::Number(1),
            json!({"sessionId": "s1"}),
            &routing_state(None, &[]),
        ));
        assert_settle(plan_reverse_request(
            method,
            RequestId::Number(1),
            json!({"sessionId": "s2"}),
            &routing_state(Some("s1"), &[]),
        ));
        assert_settle(plan_reverse_request(
            method,
            RequestId::Number(1),
            json!({"sessionId": "s1"}),
            &routing_state(Some("s1"), &["s1"]),
        ));
        assert_forward(plan_reverse_request(
            method,
            RequestId::Number(1),
            json!({"sessionId": "s1"}),
            &routing_state(Some("s1"), &[]),
        ));
    }
}

#[test]
fn test_reverse_plan_preserves_number_and_string_request_ids() {
    let settle = plan_reverse_request(
        "session/request_permission",
        RequestId::Number(7),
        json!({"sessionId": "s1"}),
        &routing_state(None, &[]),
    );
    let forward = plan_reverse_request(
        "elicitation/create",
        RequestId::String("7".into()),
        json!({"sessionId": "s1"}),
        &routing_state(Some("s1"), &[]),
    );
    let Some(ReverseRequestPlan::Settle { id, .. }) = settle else {
        panic!("Number ID 应进入 settle plan")
    };
    assert_eq!(id, RequestId::Number(7));
    let Some(ReverseRequestPlan::Forward { id, .. }) = forward else {
        panic!("String ID 应进入 forward plan")
    };
    assert_eq!(id, RequestId::String("7".into()));
}

#[test]
fn test_unknown_reverse_method_is_not_claimed() {
    assert!(
        plan_reverse_request(
            "custom/request",
            RequestId::Number(1),
            json!({"sessionId": "s1"}),
            &routing_state(Some("s1"), &[]),
        )
        .is_none()
    );
}

async fn reverse_response(
    server: Arc<peri_acp::transport::mpsc::MpscServerTransport>,
    method: &'static str,
    params: Value,
) -> Value {
    tokio::time::timeout(Duration::from_secs(2), server.send_request(method, params))
        .await
        .expect("reverse request 不应挂起")
        .expect("reverse request 应以成功 response 结算")
}

fn assert_cancel_response(method: &str, response: Value) {
    if method == "session/request_permission" {
        let parsed: RequestPermissionResponse = serde_json::from_value(response).unwrap();
        assert!(matches!(
            parsed.outcome,
            RequestPermissionOutcome::Cancelled
        ));
    } else {
        let parsed: CreateElicitationResponse = serde_json::from_value(response).unwrap();
        assert!(matches!(parsed.action, ElicitationAction::Cancel));
    }
}

#[tokio::test]
async fn test_pump_cancels_no_active_reverse_requests() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
    client.spawn_pump(notification_tx);
    let server = Arc::new(server_transport);
    for method in ["session/request_permission", "elicitation/create"] {
        let response = reverse_response(server.clone(), method, json!({"sessionId": "s1"})).await;
        assert_cancel_response(method, response);
        assert!(matches!(
            notification_rx.try_recv(),
            Err(TryRecvError::Empty)
        ));
    }
}

#[tokio::test]
async fn test_pump_cancels_mismatched_and_deleted_reverse_requests() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
    {
        let mut state = client.session_routing.lock().unwrap();
        state.current_session_id = Some("s1".into());
        state.deleted_session_ids = HashSet::from(["deleted".into()]);
    }
    client.spawn_pump(notification_tx);
    let server = Arc::new(server_transport);
    let mismatch = reverse_response(
        server.clone(),
        "session/request_permission",
        json!({"sessionId": "s2"}),
    )
    .await;
    assert_cancel_response("session/request_permission", mismatch);
    let deleted = reverse_response(
        server,
        "elicitation/create",
        json!({"sessionId": "deleted"}),
    )
    .await;
    assert_cancel_response("elicitation/create", deleted);
    assert!(matches!(
        notification_rx.try_recv(),
        Err(TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn test_pump_cancels_missing_or_empty_reverse_session() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
    client.session_routing.lock().unwrap().current_session_id = Some("s1".into());
    client.spawn_pump(notification_tx);
    let server = Arc::new(server_transport);
    let missing = reverse_response(server.clone(), "session/request_permission", json!({})).await;
    assert_cancel_response("session/request_permission", missing);
    let empty = reverse_response(server, "elicitation/create", json!({"sessionId": ""})).await;
    assert_cancel_response("elicitation/create", empty);
    assert!(matches!(
        notification_rx.try_recv(),
        Err(TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn test_pump_cancels_snake_only_elicitation_without_ui_event() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
    client.session_routing.lock().unwrap().current_session_id = Some("s1".into());
    client.spawn_pump(notification_tx);
    let response = reverse_response(
        Arc::new(server_transport),
        "elicitation/create",
        json!({"session_id": "s1"}),
    )
    .await;
    assert_cancel_response("elicitation/create", response);
    assert!(matches!(
        notification_rx.try_recv(),
        Err(TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn test_pump_forwards_exact_permission_and_elicitation() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
    client.session_routing.lock().unwrap().current_session_id = Some("s1".into());
    client.spawn_pump(notification_tx);
    let server = Arc::new(server_transport);

    let permission_server = server.clone();
    let permission = tokio::spawn(async move {
        permission_server
            .send_request(
                "session/request_permission",
                json!({"sessionId": "s1", "marker": "permission"}),
            )
            .await
            .unwrap()
    });
    let AcpNotification::RequestPermission { id, params } =
        tokio::time::timeout(Duration::from_secs(2), notification_rx.recv())
            .await
            .expect("permission notification 不应挂起")
            .expect("permission notification channel 应保持打开")
    else {
        panic!("exact permission 应被转发")
    };
    assert!(matches!(id, RequestId::Number(_)));
    assert_eq!(params["marker"], "permission");
    client
        .send_response(id, Ok(json!({"done": "permission"})))
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), permission)
            .await
            .expect("permission server future 不应挂起")
            .unwrap(),
        json!({"done": "permission"})
    );

    let elicitation_server = server.clone();
    let elicitation = tokio::spawn(async move {
        elicitation_server
            .send_request(
                "elicitation/create",
                json!({"sessionId": "s1", "marker": "elicitation"}),
            )
            .await
            .unwrap()
    });
    let AcpNotification::Elicitation { id, params } =
        tokio::time::timeout(Duration::from_secs(2), notification_rx.recv())
            .await
            .expect("elicitation notification 不应挂起")
            .expect("elicitation notification channel 应保持打开")
    else {
        panic!("exact elicitation 应被转发")
    };
    assert!(matches!(id, RequestId::Number(_)));
    assert_eq!(params["marker"], "elicitation");
    client
        .send_response(id, Ok(json!({"done": "elicitation"})))
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), elicitation)
            .await
            .expect("elicitation server future 不应挂起")
            .unwrap(),
        json!({"done": "elicitation"})
    );
}

#[tokio::test]
async fn test_pump_cancels_exact_request_when_notification_receiver_is_dropped() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, notification_rx) = AcpTuiClient::new(client_transport);
    client.session_routing.lock().unwrap().current_session_id = Some("s1".into());
    drop(notification_rx);
    client.spawn_pump(notification_tx);
    let response = reverse_response(
        Arc::new(server_transport),
        "session/request_permission",
        json!({"sessionId": "s1"}),
    )
    .await;
    assert_cancel_response("session/request_permission", response);
}
