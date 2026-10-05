use super::*;
use crate::acp_client::interaction_lifecycle::{RegisterDecision, ReverseInteractionKind};
use peri_acp::transport::{
    mpsc::{MpscServerTransport, mpsc_transport_pair},
    types::{IncomingMessage, RequestId},
};
use peri_acp_types::{
    identity::AttemptId,
    session::TurnId,
    session_resources::{ControlAttempt, ControlRejection, ControlStatus, control::decide_control},
};
use std::time::Duration;

fn running_state() -> Value {
    serde_json::to_value(ControlState {
        lifecycle: 3,
        revision: 9,
        control_generation: 4,
        status: ControlStatus::Active,
        attempt: Some(ControlAttempt {
            turn_id: TurnId::new(),
            attempt_id: AttemptId::new(),
        }),
    })
    .unwrap()
}

#[test]
fn running_state_matches_uuid_v7_wire_contract() {
    let state: ControlState = serde_json::from_value(running_state()).unwrap();
    let attempt = state.attempt.as_ref().unwrap();
    assert_eq!(attempt.turn_id.as_uuid().get_version_num(), 7);
    assert_eq!(
        uuid::Uuid::parse_str(attempt.attempt_id.as_str())
            .unwrap()
            .get_version_num(),
        7
    );
    let decoded: ControlState =
        serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
    assert_eq!(decoded, state);
}

async fn request(server: &MpscServerTransport, expected: &str) -> (RequestId, Value) {
    let incoming = tokio::time::timeout(Duration::from_secs(5), server.recv())
        .await
        .unwrap()
        .unwrap();
    let IncomingMessage::Request { id, method, params } = incoming else {
        panic!("expected RPC {expected}, got {incoming:?}");
    };
    assert_eq!(method, expected);
    (id, params)
}

async fn answer_state(server: &MpscServerTransport, state: Value) {
    let (id, params) = request(server, "session/control/state").await;
    assert_eq!(params, json!({"sessionId":"s"}));
    server
        .send_response(id, Ok(json!({"state":state,"settlement":null})))
        .await
        .unwrap();
}

fn receipt(params: &Value, state: Value) -> ControlReceipt {
    let command: ControlCommand = serde_json::from_value(params.clone()).unwrap();
    let state: ControlState = serde_json::from_value(state).unwrap();
    decide_control(&command, &state)
}

fn client_with_run() -> (AcpTuiClient, MpscServerTransport) {
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.force_stable_for_test("s", false);
    let (_, generation) = client.lifecycle.stable_identity().unwrap();
    assert!(
        client
            .lifecycle
            .bind_user_input_generation("s", generation, "queue")
    );
    assert!(
        client
            .lifecycle
            .open_user_input_run("s", "queue", "local-run")
            .is_some()
    );
    (client, server)
}

#[tokio::test]
async fn stop_waits_for_receipt_and_drains_reverse_requests_only_when_accepted() {
    let (client, server) = client_with_run();
    let mut owners = Vec::new();
    for (kind, id) in [
        (ReverseInteractionKind::Permission, 90),
        (ReverseInteractionKind::Elicitation, 91),
    ] {
        let RegisterDecision::Forward(registered) =
            client
                .lifecycle
                .register_reverse(kind, RequestId::Number(id), Some("s"), json!({}))
        else {
            panic!("expected pending reverse owner");
        };
        owners.push(registered.owner);
    }
    let stopping_client = client.clone();
    let stopping = tokio::spawn(async move { stopping_client.cancel().await });
    let state = running_state();
    answer_state(&server, state.clone()).await;
    let (id, params) = request(&server, "session/control").await;
    assert_eq!(params["sessionId"], "s");
    assert_eq!(params["expectedLifecycle"], 3);
    assert_eq!(params["expectedRevision"], 9);
    assert_eq!(params["expectedControlGeneration"], 4);
    assert_eq!(
        params["action"],
        json!({"kind":"stop","target":state["attempt"]})
    );
    uuid::Uuid::parse_str(params["commandId"].as_str().unwrap()).unwrap();
    assert!(!stopping.is_finished());
    assert!(
        owners
            .iter()
            .all(|owner| client.lifecycle.is_pending_owner(owner))
    );
    let expected = receipt(&params, state);
    assert_eq!(expected.decision, ControlDecision::Accepted);
    server
        .send_response(id, Ok(serde_json::to_value(&expected).unwrap()))
        .await
        .unwrap();
    assert_eq!(stopping.await.unwrap().unwrap(), expected);
    assert!(client.lifecycle.active_user_input_run().is_none());
    assert!(
        owners
            .iter()
            .all(|owner| !client.lifecycle.is_pending_owner(owner))
    );
    for expected_id in [90, 91] {
        let IncomingMessage::Response { id, result } = server.recv().await.unwrap() else {
            panic!("expected reverse cancellation response");
        };
        assert_eq!(id, RequestId::Number(expected_id));
        assert!(result.is_ok());
    }
    client.close();
}

#[tokio::test]
async fn stale_stop_receipts_preserve_the_new_run_and_reverse_owner() {
    for reason in [
        ControlRejection::StaleLifecycle,
        ControlRejection::StaleRevision,
        ControlRejection::StaleControlGeneration,
        ControlRejection::StaleAttempt,
        ControlRejection::InvalidTransition,
    ] {
        let (client, server) = client_with_run();
        let RegisterDecision::Forward(registered) = client.lifecycle.register_reverse(
            ReverseInteractionKind::Permission,
            RequestId::Number(99),
            Some("s"),
            json!({}),
        ) else {
            panic!("expected owner");
        };
        let stopping_client = client.clone();
        let stopping = tokio::spawn(async move { stopping_client.cancel().await });
        let state = running_state();
        answer_state(&server, state.clone()).await;
        let (id, params) = request(&server, "session/control").await;
        let mut current = state;
        match reason {
            ControlRejection::StaleLifecycle => current["lifecycle"] = json!(4),
            ControlRejection::StaleRevision => current["revision"] = json!(10),
            ControlRejection::StaleControlGeneration => current["controlGeneration"] = json!(5),
            ControlRejection::StaleAttempt => {
                current["attempt"] = running_state()["attempt"].clone()
            }
            ControlRejection::InvalidTransition => current["status"] = json!("closed"),
            ControlRejection::VersionExhausted => unreachable!(),
        }
        let rejected = receipt(&params, current);
        assert_eq!(rejected.decision, ControlDecision::Rejected { reason });
        server
            .send_response(id, Ok(serde_json::to_value(&rejected).unwrap()))
            .await
            .unwrap();
        assert_eq!(stopping.await.unwrap().unwrap(), rejected);
        assert!(client.lifecycle.active_user_input_run().is_some());
        assert!(client.lifecycle.is_pending_owner(&registered.owner));
        assert!(
            tokio::time::timeout(Duration::from_millis(20), server.recv())
                .await
                .is_err()
        );
        client.close();
    }
}

#[tokio::test]
async fn no_attempt_uses_typed_pause_even_for_queued_paused_or_closed_state() {
    for status in ["active", "paused", "closing", "closed"] {
        let (client, server) = client_with_run();
        let stopping_client = client.clone();
        let stopping = tokio::spawn(async move { stopping_client.cancel().await });
        let mut state = running_state();
        state["status"] = json!(status);
        state["attempt"] = Value::Null;
        answer_state(&server, state.clone()).await;
        let (id, params) = request(&server, "session/control").await;
        assert_eq!(params["action"], json!({"kind":"pause"}));
        let expected = receipt(&params, state);
        server
            .send_response(id, Ok(serde_json::to_value(&expected).unwrap()))
            .await
            .unwrap();
        assert_eq!(stopping.await.unwrap().unwrap(), expected);
        assert_eq!(
            client.lifecycle.active_user_input_run().is_none(),
            expected.decision == ControlDecision::Accepted
        );
        client.close();
    }
}

#[tokio::test]
async fn lost_response_resolves_the_original_command_without_retargeting() {
    for accepted in [true, false] {
        let (client, server) = client_with_run();
        let stopping_client = client.clone();
        let stopping = tokio::spawn(async move { stopping_client.cancel().await });
        let state = running_state();
        answer_state(&server, state.clone()).await;
        let (id, original) = request(&server, "session/control").await;
        server
            .send_response(id, Err(AcpError::new(-32603, "receipt response lost")))
            .await
            .unwrap();
        let (id, resolving) = request(&server, "session/control/resolve").await;
        assert_eq!(resolving, original);
        let mut current = state;
        if !accepted {
            current["attempt"] = running_state()["attempt"].clone();
        }
        let expected = receipt(&original, current);
        assert_eq!(expected.decision == ControlDecision::Accepted, accepted);
        server
            .send_response(id, Ok(json!({"status":"applied","receipt":expected})))
            .await
            .unwrap();
        assert_eq!(stopping.await.unwrap().unwrap(), expected);
        assert_eq!(client.lifecycle.active_user_input_run().is_none(), accepted);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), server.recv())
                .await
                .is_err()
        );
        client.close();
    }
}

#[tokio::test]
async fn not_applied_retries_with_identical_command_and_returns_stale_receipt() {
    let (client, server) = client_with_run();
    let stopping_client = client.clone();
    let stopping = tokio::spawn(async move { stopping_client.cancel().await });
    let state = running_state();
    answer_state(&server, state.clone()).await;
    let (id, original) = request(&server, "session/control").await;
    server
        .send_response(id, Err(AcpError::new(-32603, "dispatch failed")))
        .await
        .unwrap();
    let (id, resolving) = request(&server, "session/control/resolve").await;
    assert_eq!(resolving, original);
    server
        .send_response(id, Ok(json!({"status":"notApplied"})))
        .await
        .unwrap();
    let (id, retrying) = request(&server, "session/control").await;
    assert_eq!(retrying, original);
    let mut current = state;
    current["revision"] = json!(10);
    let expected = receipt(&original, current);
    assert_eq!(
        expected.decision,
        ControlDecision::Rejected {
            reason: ControlRejection::StaleRevision
        }
    );
    server
        .send_response(id, Ok(serde_json::to_value(&expected).unwrap()))
        .await
        .unwrap();
    assert_eq!(stopping.await.unwrap().unwrap(), expected);
    assert!(client.lifecycle.active_user_input_run().is_some());
    client.close();
}

#[tokio::test]
async fn unknown_or_failed_resolution_preserves_command_and_local_run() {
    for resolution in [
        Ok(json!({"status":"unknown"})),
        Err(AcpError::new(-32603, "resolution unavailable")),
    ] {
        let (client, server) = client_with_run();
        let stopping_client = client.clone();
        let stopping = tokio::spawn(async move { stopping_client.cancel().await });
        answer_state(&server, running_state()).await;
        let (id, original) = request(&server, "session/control").await;
        server
            .send_response(id, Err(AcpError::new(-32603, "outcome uncertain")))
            .await
            .unwrap();
        let (id, resolving) = request(&server, "session/control/resolve").await;
        assert_eq!(resolving, original);
        server.send_response(id, resolution).await.unwrap();
        let error = stopping.await.unwrap().unwrap_err();
        assert_eq!(error.data.unwrap()["command"], original);
        assert!(client.lifecycle.active_user_input_run().is_some());
        assert!(
            tokio::time::timeout(Duration::from_millis(20), server.recv())
                .await
                .is_err()
        );
        client.close();
    }
}

#[tokio::test]
async fn state_failure_or_malformed_state_never_sends_stop() {
    let mut invalid_state = running_state();
    invalid_state["attempt"]["turnId"] = json!("old");
    for response in [
        Err(AcpError::new(-32603, "state unavailable")),
        Ok(json!({"state":{"attempt":null}})),
        Ok(json!({"state":invalid_state})),
    ] {
        let (client, server) = client_with_run();
        let stopping_client = client.clone();
        let stopping = tokio::spawn(async move { stopping_client.cancel().await });
        let (id, _) = request(&server, "session/control/state").await;
        server.send_response(id, response).await.unwrap();
        assert!(stopping.await.unwrap().is_err());
        assert!(client.lifecycle.active_user_input_run().is_some());
        assert!(
            tokio::time::timeout(Duration::from_millis(20), server.recv())
                .await
                .is_err()
        );
        client.close();
    }
}

#[tokio::test]
async fn mismatched_receipt_never_retires_local_run() {
    let (client, server) = client_with_run();
    let stopping_client = client.clone();
    let stopping = tokio::spawn(async move { stopping_client.cancel().await });
    let state = running_state();
    answer_state(&server, state.clone()).await;
    let (id, params) = request(&server, "session/control").await;
    let mut wrong = receipt(&params, state);
    wrong.command_id = "another-command".into();
    server
        .send_response(id, Ok(serde_json::to_value(wrong).unwrap()))
        .await
        .unwrap();
    let error = stopping.await.unwrap().unwrap_err();
    assert_eq!(error.data.unwrap()["command"], params);
    assert!(client.lifecycle.active_user_input_run().is_some());
    client.close();
}

#[tokio::test]
async fn timed_out_response_resolves_with_the_same_command_id() {
    let (client, server) = client_with_run();
    let stopping_client = client.clone();
    let stopping = tokio::spawn(async move { stopping_client.cancel().await });
    let state = running_state();
    answer_state(&server, state.clone()).await;
    let (_, original) = request(&server, "session/control").await;
    let (id, resolving) = request(&server, "session/control/resolve").await;
    assert_eq!(resolving, original);
    assert!(client.lifecycle.active_user_input_run().is_some());
    let expected = receipt(&original, state);
    assert_eq!(expected.decision, ControlDecision::Accepted);
    server
        .send_response(id, Ok(json!({"status":"applied","receipt":expected})))
        .await
        .unwrap();
    assert_eq!(stopping.await.unwrap().unwrap(), expected);
    assert!(client.lifecycle.active_user_input_run().is_none());
    client.close();
}
