use std::{sync::Arc, time::Duration};

use agent_client_protocol::schema::v1::{RequestPermissionOutcome, RequestPermissionResponse};
use agent_client_protocol_schema::v1::{CreateElicitationResponse, ElicitationAction};
use peri_acp::transport::{AcpTransport, mpsc::mpsc_transport_pair};
use serde_json::{Value, json};
use serial_test::serial;
use tokio::sync::mpsc::error::TryRecvError;

use super::*;

fn lifecycle(current: Option<&str>, accepting: bool) -> InteractionLifecycle {
    let lifecycle = InteractionLifecycle::new();
    if let Some(current) = current {
        lifecycle.force_stable(current, accepting);
    }
    lifecycle
}

fn assert_forward(plan: Option<RegisterDecision>) {
    assert!(matches!(plan, Some(RegisterDecision::Forward(_))));
}

fn assert_settle(plan: Option<RegisterDecision>) {
    assert!(matches!(plan, Some(RegisterDecision::Settle { .. })));
}

#[tokio::test]
#[serial]
async fn test_bridge_publish_and_transition_are_one_operation_gate_order() {
    use crate::kit::acp_bridge::spawn_acp_bridge_observed_with_client;
    use crate::kit::acp_types::{AcpEventData, AcpEventWithEpoch, PendingInteraction};
    use peri_acp_types::event_data::HitlPending;
    use tokio_util::sync::CancellationToken;

    crate::kit::atoms::init_atoms();
    use crate::kit::atoms;
    let old_active = crate::kit::atoms::ACTIVE_SESSION_ID.state().read().clone();
    let old_pending = crate::kit::atoms::HITL_PENDING.state().read().clone();
    let old_popup = *crate::kit::atoms::POPUP_KIND.state().read();
    let old_view = atoms::VIEW_MODELS.state().read().clone();
    let old_acp_state = atoms::ACP_STATE.state().read().clone();
    let old_reset = atoms::BRIDGE_RESET_COUNTER.get();
    let old_input = atoms::INPUT_BUFFER.state().read().clone();
    let old_fold_overrides = atoms::FOLD_OVERRIDES.state().read().clone();
    let old_ask = atoms::ASK_USER_PENDING.state().read().clone();
    let old_open_panels = atoms::OPEN_PANELS.state().read().clone();
    let old_active_panel = *atoms::ACTIVE_PANEL.state().read();
    let old_confirm = atoms::CONFIRM_PAYLOAD.state().read().clone();
    let old_todos = atoms::TODO_ITEMS.state().read().clone();
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, mut notification_rx) =
        AcpTuiClient::new_interactive(client_transport);
    client.lifecycle.force_stable("s1", true);
    client.spawn_pump(notification_tx);
    *crate::kit::atoms::ACTIVE_SESSION_ID.state().write() = "s1".into();
    let server = Arc::new(server_transport);
    let (bridge_tx, bridge_rx) = tokio::sync::mpsc::unbounded_channel();
    let (observed_tx, mut observed_rx) = tokio::sync::mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let bridge = spawn_acp_bridge_observed_with_client(
        bridge_rx,
        shutdown.clone(),
        client.clone(),
        observed_tx,
    );

    let request_server = server.clone();
    let publish_first_response = tokio::spawn(async move {
        request_server
            .send_request("session/request_permission", json!({"sessionId":"s1"}))
            .await
            .unwrap()
    });
    let AcpNotification::RequestPermission {
        owner: publish_first,
        request_id_json,
        ..
    } = notification_rx.recv().await.unwrap()
    else {
        panic!()
    };
    bridge_tx
        .send(AcpEventWithEpoch {
            event: AcpEventData::HitlPending(PendingInteraction {
                owner: publish_first.clone(),
                request_id_json,
                payload: HitlPending {
                    tool_name: "Bash".into(),
                    tool_input: Value::Null,
                    batch: None,
                },
            }),
            active_session_id: "s1".into(),
        })
        .unwrap();
    assert_eq!(observed_rx.recv().await, Some(true));
    assert_eq!(
        crate::kit::atoms::HITL_PENDING
            .state()
            .read()
            .as_ref()
            .map(|pending| &pending.owner),
        Some(&publish_first)
    );

    let operation = client.lifecycle.operation_gate().lock().await;
    let start = client
        .lifecycle
        .begin_transition(TransitionKind::Load, Some("s2".into()))
        .unwrap();
    let transition = client.lifecycle.arm_transition(start.generation);
    crate::kit::session_boundary::project_session_boundary(Some("s2"));
    client.settle_claims_owned(start.claims).await;
    client.lifecycle.fail_transition(start.generation);
    transition.disarm();
    drop(operation);
    assert_cancel_response(
        "session/request_permission",
        publish_first_response.await.unwrap(),
    );
    assert!(matches!(
        notification_rx.recv().await.unwrap(),
        AcpNotification::InteractionTerminal { owner, .. } if owner == publish_first
    ));
    assert!(crate::kit::atoms::HITL_PENDING.state().read().is_none());

    // Reverse the order: the transition claims and wire-settles the owner while
    // holding the operation gate; the production bridge then observes false and
    // must not republish any UI surface.
    client.lifecycle.force_stable("s1", true);
    *crate::kit::atoms::ACTIVE_SESSION_ID.state().write() = "s1".into();
    let request_server = server.clone();
    let transition_first_response = tokio::spawn(async move {
        request_server
            .send_request("session/request_permission", json!({"sessionId":"s1"}))
            .await
            .unwrap()
    });
    let AcpNotification::RequestPermission {
        owner: transition_first,
        request_id_json,
        ..
    } = notification_rx.recv().await.unwrap()
    else {
        panic!()
    };
    let operation = client.lifecycle.operation_gate().lock().await;
    let start = client
        .lifecycle
        .begin_transition(TransitionKind::New, None)
        .unwrap();
    let transition = client.lifecycle.arm_transition(start.generation);
    crate::kit::session_boundary::project_session_boundary(None);
    client.settle_claims_owned(start.claims).await;
    assert_cancel_response(
        "session/request_permission",
        transition_first_response.await.unwrap(),
    );
    bridge_tx
        .send(AcpEventWithEpoch {
            event: AcpEventData::HitlPending(PendingInteraction {
                owner: transition_first,
                request_id_json,
                payload: HitlPending {
                    tool_name: "Bash".into(),
                    tool_input: Value::Null,
                    batch: None,
                },
            }),
            active_session_id: "s1".into(),
        })
        .unwrap();
    client.lifecycle.fail_transition(start.generation);
    transition.disarm();
    drop(operation);
    assert_eq!(observed_rx.recv().await, Some(false));
    assert!(crate::kit::atoms::HITL_PENDING.state().read().is_none());

    shutdown.cancel();
    drop(bridge_tx);
    bridge.await.unwrap();
    *crate::kit::atoms::ACTIVE_SESSION_ID.state().write() = old_active;
    *crate::kit::atoms::HITL_PENDING.state().write() = old_pending;
    *crate::kit::atoms::POPUP_KIND.state().write() = old_popup;
    *atoms::VIEW_MODELS.state().write() = old_view;
    *atoms::ACP_STATE.state().write() = old_acp_state;
    atoms::BRIDGE_RESET_COUNTER.set(old_reset);
    *atoms::INPUT_BUFFER.state().write() = old_input;
    *atoms::FOLD_OVERRIDES.state().write() = old_fold_overrides;
    *atoms::ASK_USER_PENDING.state().write() = old_ask;
    *atoms::OPEN_PANELS.state().write() = old_open_panels;
    *atoms::ACTIVE_PANEL.state().write() = old_active_panel;
    *atoms::CONFIRM_PAYLOAD.state().write() = old_confirm;
    *atoms::TODO_ITEMS.state().write() = old_todos;
}

#[test]
fn test_permission_reverse_session_id_matrix_fail_closed() {
    let lifecycle = lifecycle(Some("s1"), true);
    for params in [
        json!({"sessionId": "s1"}),
        json!({"session_id": "s1"}),
        json!({"sessionId": "s1", "session_id": "s1"}),
    ] {
        assert_forward(plan_reverse_request(
            "session/request_permission",
            RequestId::Number(1),
            params,
            &lifecycle,
        ));
    }
    for params in [
        json!({}),
        json!({"sessionId": null}),
        json!({"sessionId": ""}),
        json!({"sessionId": "s1", "session_id": "s2"}),
    ] {
        assert_settle(plan_reverse_request(
            "session/request_permission",
            RequestId::Number(2),
            params,
            &lifecycle,
        ));
    }
}

#[test]
fn test_elicitation_reverse_session_id_requires_canonical_camel_case() {
    let lifecycle = lifecycle(Some("s1"), true);
    assert_forward(plan_reverse_request(
        "elicitation/create",
        RequestId::Number(1),
        json!({"sessionId": "s1"}),
        &lifecycle,
    ));
    for params in [
        json!({}),
        json!({"sessionId": ""}),
        json!({"session_id": "s1"}),
    ] {
        assert_settle(plan_reverse_request(
            "elicitation/create",
            RequestId::Number(2),
            params,
            &lifecycle,
        ));
    }
}

#[test]
fn test_reverse_plan_requires_open_prompt_and_exact_current_session() {
    for method in ["session/request_permission", "elicitation/create"] {
        assert_settle(plan_reverse_request(
            method,
            RequestId::Number(1),
            json!({"sessionId": "s1"}),
            &lifecycle(None, false),
        ));
        assert_settle(plan_reverse_request(
            method,
            RequestId::Number(1),
            json!({"sessionId": "s2"}),
            &lifecycle(Some("s1"), true),
        ));
        assert_settle(plan_reverse_request(
            method,
            RequestId::Number(1),
            json!({"sessionId": "s1"}),
            &lifecycle(Some("s1"), false),
        ));
        assert_forward(plan_reverse_request(
            method,
            RequestId::Number(1),
            json!({"sessionId": "s1"}),
            &lifecycle(Some("s1"), true),
        ));
    }
}

#[test]
fn test_reverse_plan_preserves_number_and_string_request_ids() {
    let settle = plan_reverse_request(
        "session/request_permission",
        RequestId::Number(7),
        json!({"sessionId": "s1"}),
        &lifecycle(None, false),
    );
    let lifecycle = lifecycle(Some("s1"), true);
    let forward = plan_reverse_request(
        "elicitation/create",
        RequestId::String("7".into()),
        json!({"sessionId": "s1"}),
        &lifecycle,
    );
    let Some(RegisterDecision::Settle { id, .. }) = settle else {
        panic!()
    };
    assert_eq!(id, RequestId::Number(7));
    let Some(RegisterDecision::Forward(forward)) = forward else {
        panic!()
    };
    let claimed = lifecycle
        .claim(&forward.owner, ClaimCause::UserResponse)
        .unwrap();
    assert_eq!(claimed.request_id, RequestId::String("7".into()));
}

#[test]
fn test_unknown_reverse_method_is_not_claimed() {
    assert!(
        plan_reverse_request(
            "custom/request",
            RequestId::Number(1),
            json!({"sessionId": "s1"}),
            &lifecycle(Some("s1"), true)
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
        .expect("reverse request 应结算")
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

async fn receive_lifecycle_request(
    server: &peri_acp::transport::mpsc::MpscServerTransport,
    expected_method: &str,
) -> (RequestId, Value) {
    let IncomingMessage::Request { id, method, params } = server.recv().await.unwrap() else {
        panic!("expected lifecycle request")
    };
    assert_eq!(method, expected_method);
    (id, params)
}

#[tokio::test]
#[serial]
async fn test_initial_ensure_and_submit_ensure_share_one_lifecycle_request_both_orders() {
    crate::kit::atoms::init_atoms();
    let old_active = crate::kit::atoms::ACTIVE_SESSION_ID.state().read().clone();
    let old_view = crate::kit::atoms::VIEW_MODELS.state().read().clone();
    let old_reset = crate::kit::atoms::BRIDGE_RESET_COUNTER.get();
    let old_fold_overrides = crate::kit::atoms::FOLD_OVERRIDES.state().read().clone();
    for startup_first in [true, false] {
        let (client_transport, server_transport) = mpsc_transport_pair();
        let (client, _notification_tx, _notification_rx) =
            AcpTuiClient::new_interactive(client_transport);

        let startup_client = client.clone();
        let submit_client = client.clone();
        let (first_client, second_client) = if startup_first {
            (startup_client, submit_client)
        } else {
            (submit_client, startup_client)
        };
        let first_ensure =
            tokio::spawn(async move { first_client.ensure_session("/tmp", None).await });
        let (new_id, params) = receive_lifecycle_request(&server_transport, "session/new").await;
        assert_eq!(params["cwd"], "/tmp");

        let second_ensure =
            tokio::spawn(async move { second_client.ensure_session("/tmp", None).await });
        server_transport
            .send_response(new_id, Ok(json!({"sessionId": "shared"})))
            .await
            .unwrap();
        assert_eq!(first_ensure.await.unwrap().unwrap(), "shared");
        assert_eq!(second_ensure.await.unwrap().unwrap(), "shared");
        assert_eq!(client.current_session_id().as_deref(), Some("shared"));
        assert_eq!(
            crate::kit::atoms::ACTIVE_SESSION_ID.state().read().as_str(),
            "shared"
        );

        // Treat the selected first caller as either startup or submit.  Once its
        // response is committed, the other caller must reuse the same identity,
        // not emit close/new and replace a prompt opened on that identity.
        let prompt_client = client.clone();
        let prompt = tokio::spawn(async move {
            prompt_client
                .prompt(
                    &peri_acp_types::messages::MessageContent::text("first"),
                    Some("p1".into()),
                )
                .await
        });
        let IncomingMessage::Request { id, method, params } =
            server_transport.recv().await.unwrap()
        else {
            panic!("expected prompt request")
        };
        assert_eq!(method, "session/prompt");
        assert_eq!(params["sessionId"], "shared");
        server_transport
            .send_response(id, Ok(json!({})))
            .await
            .unwrap();
        prompt.await.unwrap().unwrap();
    }
    *crate::kit::atoms::ACTIVE_SESSION_ID.state().write() = old_active;
    *crate::kit::atoms::VIEW_MODELS.state().write() = old_view;
    crate::kit::atoms::BRIDGE_RESET_COUNTER.set(old_reset);
    *crate::kit::atoms::FOLD_OVERRIDES.state().write() = old_fold_overrides;
}

#[tokio::test]
#[serial]
async fn test_startup_restore_reservation_wins_submit_ensure_both_orders() {
    crate::kit::atoms::init_atoms();
    let old_active = crate::kit::atoms::ACTIVE_SESSION_ID.state().read().clone();
    let old_view = crate::kit::atoms::VIEW_MODELS.state().read().clone();
    let old_reset = crate::kit::atoms::BRIDGE_RESET_COUNTER.get();
    let old_fold_overrides = crate::kit::atoms::FOLD_OVERRIDES.state().read().clone();
    for submit_first in [true, false] {
        let (client_transport, server_transport) = mpsc_transport_pair();
        let (client, _notification_tx, _notification_rx) =
            AcpTuiClient::new_interactive(client_transport);
        client.reserve_startup_restore().await;

        let submit_client = client.clone();
        let load_client = client.clone();
        let (first_started_tx, first_started_rx) = tokio::sync::oneshot::channel();
        let (submit, load) = if submit_first {
            let submit = tokio::spawn(async move {
                let _ = first_started_tx.send(());
                submit_client.ensure_session("/tmp", None).await
            });
            first_started_rx.await.unwrap();
            let load = tokio::spawn(async move {
                load_client
                    .load_startup_session("restored", "/tmp", None)
                    .await
            });
            (submit, load)
        } else {
            let load = tokio::spawn(async move {
                let _ = first_started_tx.send(());
                load_client
                    .load_startup_session("restored", "/tmp", None)
                    .await
            });
            first_started_rx.await.unwrap();
            let submit =
                tokio::spawn(async move { submit_client.ensure_session("/tmp", None).await });
            (submit, load)
        };
        let (load_id, params) = receive_lifecycle_request(&server_transport, "session/load").await;
        assert_eq!(params["sessionId"], "restored");
        server_transport
            .send_response(load_id, Ok(json!({})))
            .await
            .unwrap();
        assert_eq!(load.await.unwrap().unwrap(), "restored");
        assert_eq!(submit.await.unwrap().unwrap(), "restored");
        assert_eq!(client.current_session_id().as_deref(), Some("restored"));
        assert_eq!(
            crate::kit::atoms::ACTIVE_SESSION_ID.state().read().as_str(),
            "restored"
        );

        let prompt_client = client.clone();
        let prompt = tokio::spawn(async move {
            prompt_client
                .prompt(
                    &peri_acp_types::messages::MessageContent::text("first"),
                    Some("p1".into()),
                )
                .await
        });
        let (prompt_id, params) =
            receive_lifecycle_request(&server_transport, "session/prompt").await;
        assert_eq!(params["sessionId"], "restored");
        server_transport
            .send_response(prompt_id, Ok(json!({})))
            .await
            .unwrap();
        prompt.await.unwrap().unwrap();
    }
    *crate::kit::atoms::ACTIVE_SESSION_ID.state().write() = old_active;
    *crate::kit::atoms::VIEW_MODELS.state().write() = old_view;
    crate::kit::atoms::BRIDGE_RESET_COUNTER.set(old_reset);
    *crate::kit::atoms::FOLD_OVERRIDES.state().write() = old_fold_overrides;
}

#[tokio::test]
async fn test_pump_cancels_no_active_reverse_requests() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
    client.spawn_pump(notification_tx);
    let server = Arc::new(server_transport);
    for method in ["session/request_permission", "elicitation/create"] {
        assert_cancel_response(
            method,
            reverse_response(server.clone(), method, json!({"sessionId": "s1"})).await,
        );
        assert!(matches!(
            notification_rx.try_recv(),
            Err(TryRecvError::Empty)
        ));
    }
}

#[tokio::test]
async fn test_pump_cancels_snake_only_elicitation_without_ui_event() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
    client.lifecycle.force_stable("s1", true);
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
async fn test_pump_forwards_registered_owner_and_response_claims_once() {
    let (client_transport, server_transport) = mpsc_transport_pair();
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
            .unwrap()
    });
    let AcpNotification::RequestPermission { owner, params, .. } =
        notification_rx.recv().await.unwrap()
    else {
        panic!()
    };
    assert_eq!(params["marker"], "permission");
    assert!(
        client
            .respond_interaction(&owner, json!({"done": true}), "done".into())
            .await
            .unwrap()
    );
    assert!(
        !client
            .respond_interaction(&owner, json!({"done": false}), "late".into())
            .await
            .unwrap()
    );
    assert_eq!(request.await.unwrap(), json!({"done": true}));
}

#[tokio::test]
async fn test_pump_cancels_exact_request_when_notification_receiver_is_dropped() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, notification_rx) = AcpTuiClient::new(client_transport);
    client.lifecycle.force_stable("s1", true);
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

async fn cancel_drains_kind_before_notification(method: &'static str) {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
    client.lifecycle.force_stable("s1", true);
    client.spawn_pump(notification_tx);
    let server = Arc::new(server_transport);
    let request_server = server.clone();
    let request = tokio::spawn(async move {
        request_server
            .send_request(method, json!({"sessionId":"s1"}))
            .await
            .unwrap()
    });
    let notification = notification_rx.recv().await.unwrap();
    assert!(matches!(
        notification,
        AcpNotification::RequestPermission { .. } | AcpNotification::Elicitation { .. }
    ));
    let cancel_client = client.clone();
    let cancel = tokio::spawn(async move { cancel_client.cancel().await.unwrap() });
    let response = request.await.unwrap();
    assert_cancel_response(method, response);
    let message = server.recv().await.unwrap();
    let IncomingMessage::Notification {
        method: notification_method,
        ..
    } = message
    else {
        panic!()
    };
    assert_eq!(notification_method, "session/cancel");
    cancel.await.unwrap();
}

#[tokio::test]
async fn test_cancel_drains_permission_before_turn_terminal() {
    cancel_drains_kind_before_notification("session/request_permission").await;
}

#[tokio::test]
async fn test_cancel_drains_elicitation_before_turn_terminal() {
    cancel_drains_kind_before_notification("elicitation/create").await;
}

#[tokio::test]
async fn test_new_immediate_post_response_notification_buffered_until_commit() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
    client.spawn_pump(notification_tx);
    let (commit_hook_tx, mut commit_hook_rx) = tokio::sync::mpsc::unbounded_channel();
    client.install_transition_commit_hook(commit_hook_tx);
    let server = Arc::new(server_transport);
    let server_task = server.clone();
    let serve = tokio::spawn(async move {
        let IncomingMessage::Request { id, method, .. } = server_task.recv().await.unwrap() else {
            panic!()
        };
        assert_eq!(method, "session/new");
        server_task
            .send_notification("session/update", json!({"sessionId":"old","update":{}}))
            .await
            .unwrap();
        server_task
            .send_response(id, Ok(json!({"sessionId":"new"})))
            .await
            .unwrap();
    });
    let new_client = client.clone();
    let new_session = tokio::spawn(async move { new_client.new_session("/tmp", None).await });
    let release_commit = commit_hook_rx.recv().await.unwrap();
    serve.await.unwrap();

    // The lifecycle response has arrived, but commit is causally paused. FIFO on
    // the real MPSC transport makes the reverse request below an acknowledgement
    // that the preceding ordinary notification was routed while Transitioning.
    server
        .send_notification(
            "session/update",
            json!({"sessionId":"new","update":{"marker":"new"}}),
        )
        .await
        .unwrap();
    let reverse_server = server.clone();
    let reverse = tokio::spawn(async move {
        reverse_server
            .send_request("session/request_permission", json!({"sessionId":"new"}))
            .await
            .unwrap()
    });
    assert_cancel_response("session/request_permission", reverse.await.unwrap());
    assert!(matches!(
        notification_rx.try_recv(),
        Err(TryRecvError::Empty)
    ));

    release_commit.send(()).unwrap();
    assert_eq!(new_session.await.unwrap().unwrap(), "new");
    let AcpNotification::SessionUpdate { session_id, params } =
        notification_rx.recv().await.unwrap()
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

#[tokio::test]
async fn test_aborted_prompt_lease_settles_registered_reverse() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
    client.lifecycle.force_stable("s1", false);
    client.spawn_pump(notification_tx);
    let prompt_client = client.clone();
    let prompt = tokio::spawn(async move {
        prompt_client
            .prompt(
                &peri_acp_types::messages::MessageContent::text("hi"),
                Some("p1".into()),
            )
            .await
    });
    let server = Arc::new(server_transport);
    let IncomingMessage::Request { method, .. } = server.recv().await.unwrap() else {
        panic!()
    };
    assert_eq!(method, "session/prompt");
    let reverse_server = server.clone();
    let reverse = tokio::spawn(async move {
        reverse_server
            .send_request("session/request_permission", json!({"sessionId":"s1"}))
            .await
            .unwrap()
    });
    assert!(matches!(
        notification_rx.recv().await.unwrap(),
        AcpNotification::RequestPermission { .. }
    ));
    prompt.abort();
    let response = reverse.await.unwrap();
    assert_cancel_response("session/request_permission", response);
}

#[tokio::test]
async fn test_unidentified_or_stale_turn_terminal_preserves_prompt_b() {
    let lifecycle = lifecycle(Some("s1"), false);
    let lease_a = lifecycle.open_prompt(Some("A".into())).unwrap();
    drop(lease_a);
    let _lease_b = lifecycle.open_prompt(Some("B".into())).unwrap();
    let b = match lifecycle.register_reverse(
        ReverseInteractionKind::Permission,
        RequestId::Number(9),
        Some("s1"),
        json!({}),
    ) {
        RegisterDecision::Forward(request) => request,
        _ => panic!(),
    };
    assert!(
        lifecycle
            .close_prompt_by_wire_identity("s1", None)
            .is_empty()
    );
    assert!(
        lifecycle
            .close_prompt_by_wire_identity("s1", Some("A"))
            .is_empty()
    );
    assert!(lifecycle.is_pending_owner(&b.owner));
}

#[tokio::test]
async fn test_aborted_transition_guard_queues_unexecuted_claims_once() {
    let lifecycle = lifecycle(Some("s1"), true);
    for id in [1, 2] {
        assert!(matches!(
            lifecycle.register_reverse(
                ReverseInteractionKind::Permission,
                RequestId::Number(id),
                Some("s1"),
                json!({}),
            ),
            RegisterDecision::Forward(_)
        ));
    }
    let (settlement_tx, mut settlement_rx) = tokio::sync::mpsc::unbounded_channel();
    lifecycle.install_drop_settlement_sender(settlement_tx);
    let start = lifecycle
        .begin_transition(TransitionKind::New, None)
        .unwrap();
    let mut batch = lifecycle.arm_claimed_batch(start.claims);
    let in_flight = batch.next_claim().unwrap();
    drop(in_flight);
    drop(batch);
    assert_eq!(
        settlement_rx.try_recv().unwrap().request_id,
        RequestId::Number(1)
    );
    assert_eq!(
        settlement_rx.try_recv().unwrap().request_id,
        RequestId::Number(2)
    );
    assert!(matches!(settlement_rx.try_recv(), Err(TryRecvError::Empty)));
}

#[tokio::test]
async fn test_aborted_real_transition_future_fails_closed_and_can_recover() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, _notification_tx, _notification_rx) = AcpTuiClient::new(client_transport);
    let first_client = client.clone();
    let first = tokio::spawn(async move { first_client.new_session("/tmp", None).await });
    let (_abandoned_id, _) = receive_lifecycle_request(&server_transport, "session/new").await;

    // The future is armed and suspended on its real MPSC lifecycle response.
    // Dropping it must synchronously run TransitionLease::drop -> NoSession.
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    assert_eq!(client.current_session_id(), None);
    assert!(!client.has_session());

    let retry_client = client.clone();
    let retry = tokio::spawn(async move { retry_client.ensure_session("/tmp", None).await });
    let (retry_id, _) = receive_lifecycle_request(&server_transport, "session/new").await;
    server_transport
        .send_response(retry_id, Ok(json!({"sessionId":"recovered"})))
        .await
        .unwrap();
    assert_eq!(retry.await.unwrap().unwrap(), "recovered");
}
