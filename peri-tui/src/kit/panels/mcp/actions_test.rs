use super::*;
use peri_acp::transport::{AcpTransport, mpsc::mpsc_transport_pair, types::IncomingMessage};

#[test]
fn test_same_server_restart_and_stop_share_one_inflight_gate() {
    let mut state = ControlState::default();
    let scope = OperationScope::new("session-a", 1);
    let ticket = state
        .begin("plugin:docs", ControlAction::Restart, scope.clone())
        .unwrap();
    assert!(
        state
            .begin("plugin:docs", ControlAction::Stop, scope.clone())
            .is_none()
    );
    assert!(state.begin("other", ControlAction::Stop, scope).is_some());
    assert!(state.complete(
        "plugin:docs",
        &ticket,
        OperationOutcome::Succeeded,
        Some("connected".into())
    ));
}

#[test]
fn test_session_reset_cancels_old_ticket_and_rejects_late_completion() {
    let mut state = ControlState::default();
    let ticket = state
        .begin(
            "docs",
            ControlAction::Stop,
            OperationScope::new("session-a", 1),
        )
        .unwrap();
    state.reset_scope(OperationScope::new("session-a", 2));
    assert!(ticket.is_cancelled());
    assert!(!state.complete(
        "docs",
        &ticket,
        OperationOutcome::Succeeded,
        Some("stopped".into())
    ));
    assert!(state.message("docs").is_none());
}

#[test]
fn test_cancel_wait_keeps_unknown_result_and_rejects_late_success() {
    let mut state = ControlState::default();
    let ticket = state
        .begin(
            "docs",
            ControlAction::Restart,
            OperationScope::new("session-a", 1),
        )
        .unwrap();
    assert!(state.cancel_wait("docs"));
    assert_eq!(
        state.servers["docs"].outcome,
        Some(OperationOutcome::OutcomeUnknown {
            reason: OutcomeUnknownReason::WaitCancelled
        })
    );
    assert!(!state.complete(
        "docs",
        &ticket,
        OperationOutcome::Succeeded,
        Some("connected".into())
    ));
    assert!(!state.servers["docs"].gate.is_pending());
}

#[tokio::test]
async fn test_control_wire_keeps_session_server_and_action_identity() {
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.force_stable_for_test("session-a", false);
    let identity = client.stable_session_identity().unwrap();
    let server_task = tokio::spawn(async move {
        for (action, status) in [
            ("restart", "connected"),
            ("stop", "stopped"),
            ("restart", "needsAuthorization"),
        ] {
            let Some(IncomingMessage::Request { id, method, params }) = server.recv().await else {
                panic!("应收到控制请求");
            };
            assert_eq!(method, "peri/mcp/control");
            assert_eq!(
                params,
                json!({"sessionId":"session-a", "serverName":"plugin:docs", "action":action})
            );
            server.send_response(id, Ok(json!({"sessionId":"session-a", "serverName":"plugin:docs", "action":action, "status":status}))).await.unwrap();
        }
    });
    for (action, status) in [
        (ControlAction::Restart, "connected"),
        (ControlAction::Stop, "stopped"),
        (ControlAction::Restart, "needsAuthorization"),
    ] {
        assert_eq!(
            request(&client, &identity, "plugin:docs", action)
                .await
                .unwrap(),
            status
        );
    }
    server_task.await.unwrap();
}

#[tokio::test]
async fn test_control_rejects_mismatched_server_success() {
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.force_stable_for_test("session-a", false);
    let identity = client.stable_session_identity().unwrap();
    let server_task = tokio::spawn(async move {
        let Some(IncomingMessage::Request { id, .. }) = server.recv().await else {
            panic!("应收到控制请求");
        };
        server.send_response(id, Ok(json!({"sessionId":"session-a", "serverName":"different", "action":"stop", "status":"stopped"}))).await.unwrap();
    });
    let error = request(&client, &identity, "docs", ControlAction::Stop)
        .await
        .unwrap_err();
    assert_eq!(error.data.unwrap()["kind"], "invalid_response");
    server_task.await.unwrap();
}

#[test]
fn test_transport_loss_is_unknown_but_server_rejection_is_failed() {
    assert_eq!(
        classify_error(AcpError::new(-32603, "Transport closed")),
        OperationOutcome::OutcomeUnknown {
            reason: OutcomeUnknownReason::ObserverGone
        }
    );
    assert_eq!(
        classify_error(
            AcpError::new(-32603, "close incomplete")
                .with_data(json!({"kind":"shutdown_incomplete"}))
        ),
        OperationOutcome::Failed {
            reason: "close incomplete".into()
        }
    );
}

#[test]
fn test_detail_controls_follow_host_capabilities_only() {
    let mut server = crate::kit::atoms::McpServerSummary {
        name: "plugin:docs".into(),
        ..Default::default()
    };
    assert_eq!(
        super::super::detail::detail_actions(&server),
        vec![super::super::detail::DetailAction::Back]
    );
    server.control.restart = true;
    server.control.stop = true;
    assert_eq!(
        super::super::detail::detail_actions(&server),
        vec![
            super::super::detail::DetailAction::Restart,
            super::super::detail::DetailAction::Stop,
            super::super::detail::DetailAction::Back
        ]
    );
}
