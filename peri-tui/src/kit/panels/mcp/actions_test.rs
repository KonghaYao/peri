use super::AcpError;
use super::AcpTuiClient;
use super::ControlAction;
use super::ControlState;
use super::Duration;
use super::OperationOutcome;
use super::OperationScope;
use super::OutcomeUnknownReason;
use super::json;

use peri_acp::transport::AcpTransport;
use peri_acp::transport::types::IncomingMessage;

#[test]
fn test_same_server_restart_and_stop_share_one_inflight_gate() {
    let mut state = ControlState::default();
    let scope = OperationScope::new("session-a", 1);
    let ticket = state
        .begin("plugin:docs", ControlAction::Restart, scope.clone())
        .expect("same server restart and stop share one inflight gate: the begin fixture operation uses valid inputs");
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
        .expect("session reset cancels old ticket and rejects late completion: fixture parameters must construct a valid value");
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
        .expect("cancel wait keeps unknown result and rejects late success: fixture parameters must construct a valid value");
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
    let (transport, server) = peri_acp::transport::mpsc::mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.force_stable_for_test("session-a", false);
    let identity = client.stable_session_identity().expect("control wire keeps session server and action identity: the stable_session_identity fixture operation uses valid inputs");
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
            server.send_response(id, Ok(json!({"sessionId":"session-a", "serverName":"plugin:docs", "action":action, "status":status}))).await.expect("control wire keeps session server and action identity: fixture task and request must complete successfully");
        }
    });
    for (action, status) in [
        (ControlAction::Restart, "connected"),
        (ControlAction::Stop, "stopped"),
        (ControlAction::Restart, "needsAuthorization"),
    ] {
        assert_eq!(
            super::request(&client, &identity, "plugin:docs", action)
                .await
                .expect("control wire keeps session server and action identity: the fixture operation must finish without an inner failure"),
            status
        );
    }
    server_task.await.expect("control wire keeps session server and action identity: fixture task and request must complete successfully");
}

#[tokio::test(start_paused = true)]
async fn test_control_request_accepts_response_after_twelve_seconds() {
    for (action, status) in [
        (ControlAction::Restart, "connected"),
        (ControlAction::Stop, "stopped"),
    ] {
        let (transport, server) = peri_acp::transport::mpsc::mpsc_transport_pair();
        let (client, _, _) = AcpTuiClient::new(transport);
        client.force_stable_for_test("session-a", false);
        let identity = client.stable_session_identity().expect("control request accepts response after twelve seconds: the stable_session_identity fixture operation uses valid inputs");
        let task =
            tokio::spawn(async move { super::request(&client, &identity, "docs", action).await });
        let Some(IncomingMessage::Request { id, method, params }) = server.recv().await else {
            panic!("control request was not sent")
        };
        assert_eq!(method, "peri/mcp/control");
        assert_eq!(params["action"], action.wire());
        let sent_at = tokio::time::Instant::now();

        tokio::time::advance(Duration::from_secs(11)).await;
        tokio::task::yield_now().await;
        assert!(
            !task.is_finished(),
            "control RPC was cut off at ten seconds"
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        server
            .send_response(
                id,
                Ok(json!({
                    "sessionId":"session-a", "serverName":"docs",
                    "action":action.wire(), "status":status,
                })),
            )
            .await
            .expect("control request accepts response after twelve seconds: fixture task and request must complete successfully");
        assert_eq!(task.await.expect("control request accepts response after twelve seconds: the fixture operation must finish without an inner failure").expect("control request accepts response after twelve seconds: the fixture operation must finish without an inner failure"), status);
        assert_eq!(sent_at.elapsed(), Duration::from_secs(12));
    }
}

#[tokio::test(start_paused = true)]
async fn test_control_request_times_out_at_sixty_seconds_with_unknown_outcome() {
    for action in [ControlAction::Restart, ControlAction::Stop] {
        let (transport, server) = peri_acp::transport::mpsc::mpsc_transport_pair();
        let (client, _, _) = AcpTuiClient::new(transport);
        client.force_stable_for_test("session-a", false);
        let identity = client.stable_session_identity().expect("control request times out at sixty seconds with unknown outcome: the stable_session_identity fixture operation uses valid inputs");
        let task =
            tokio::spawn(async move { super::request(&client, &identity, "docs", action).await });
        let Some(IncomingMessage::Request { .. }) = server.recv().await else {
            panic!("control request was not sent")
        };

        tokio::time::advance(Duration::from_secs(59)).await;
        tokio::task::yield_now().await;
        assert!(
            !task.is_finished(),
            "control RPC expired before sixty seconds"
        );
        tokio::time::advance(Duration::from_secs(1) + Duration::from_millis(1)).await;
        let error = tokio::time::timeout(Duration::from_millis(1), task)
            .await
            .expect("control RPC must expire at its sixty-second deadline")
            .expect("control request times out at sixty seconds with unknown outcome: fixture task and request must complete successfully")
            .expect_err("control request times out at sixty seconds with unknown outcome: the invalid fixture must be rejected");
        assert_eq!(
            super::classify_error(error),
            OperationOutcome::OutcomeUnknown {
                reason: OutcomeUnknownReason::TimedOut
            }
        );
    }
}

#[tokio::test]
async fn test_control_rejects_mismatched_server_success() {
    let (transport, server) = peri_acp::transport::mpsc::mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.force_stable_for_test("session-a", false);
    let identity = client.stable_session_identity().expect("control rejects mismatched server success: the stable_session_identity fixture operation uses valid inputs");
    let server_task = tokio::spawn(async move {
        let Some(IncomingMessage::Request { id, .. }) = server.recv().await else {
            panic!("应收到控制请求");
        };
        server.send_response(id, Ok(json!({"sessionId":"session-a", "serverName":"different", "action":"stop", "status":"stopped"}))).await.expect("control rejects mismatched server success: fixture task and request must complete successfully");
    });
    let error = super::request(&client, &identity, "docs", ControlAction::Stop)
        .await
        .expect_err(
            "control rejects mismatched server success: the invalid fixture must be rejected",
        );
    assert_eq!(error.data.expect("control rejects mismatched server success: the fixture establishes error.data before this assertion")["kind"], "invalid_response");
    server_task.await.expect("control rejects mismatched server success: fixture task and request must complete successfully");
}

#[test]
fn test_transport_loss_is_unknown_but_server_rejection_is_failed() {
    assert_eq!(
        super::classify_error(AcpError::new(-32603, "Transport closed")),
        OperationOutcome::OutcomeUnknown {
            reason: OutcomeUnknownReason::ObserverGone
        }
    );
    assert_eq!(
        super::classify_error(
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
