//! Session boundaries: version-gated admission, slot identity across
//! close/attach, and metadata parked while a session attaches.

use serde_json::json;

use super::harness::{
    begin_turn, config_options, expect_error, expect_state, permission, Harness, CWD,
};
use crate::transport::types::{AcpError, RequestId};
use crate::transport::AcpTransport;

#[tokio::test]
async fn v1_connections_keep_the_hosts_one_session_gate() {
    // The preview wrapper reports multi-session support before the version is
    // known, so the shared host skips its one-session gate for every connection
    // through it. A v1 client must keep the historical rule at the adapter.
    // (v2 connections are covered by `a_connection_may_hold_several_sessions`.)
    let mut harness = Harness::start();
    harness.negotiate_v1().await;
    harness.open("A").await;

    let second = RequestId::Number(900);
    harness
        .request(second.clone(), "session/new", json!({"cwd": CWD}))
        .await;
    let error = expect_error(&harness, second, -32010).await;
    assert_eq!(
        error.data.expect("switch reason")["reason"],
        "session_switch_requires_close"
    );

    // A different session may not be restored either.
    let other = RequestId::Number(901);
    harness
        .request(
            other.clone(),
            "session/resume",
            json!({"sessionId": "B", "cwd": CWD}),
        )
        .await;
    let error = expect_error(&harness, other, -32010).await;
    assert_eq!(
        error.data.expect("switch reason")["reason"],
        "session_switch_requires_close"
    );

    // Re-attaching the current session is the host's `Reuse` case and still
    // reaches the host.
    let reuse = RequestId::Number(902);
    harness
        .request(
            reuse.clone(),
            "session/resume",
            json!({"sessionId": "A", "cwd": CWD}),
        )
        .await;
    let forwarded = harness.expect_forwarded().await;
    assert_eq!(forwarded.method, "session/resume");
    harness
        .respond(forwarded.id, Ok(json!({"sessionId": "A"})))
        .await;
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, reuse);
    assert!(result.is_ok());

    // A successful close releases the binding.
    let close = RequestId::Number(903);
    harness
        .request(close.clone(), "session/close", json!({"sessionId": "A"}))
        .await;
    let forwarded = harness.expect_forwarded().await;
    assert_eq!(forwarded.method, "session/close");
    harness.respond(forwarded.id, Ok(json!({}))).await;
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, close);
    assert!(result.is_ok());

    let fresh = RequestId::Number(904);
    harness
        .request(fresh.clone(), "session/new", json!({"cwd": CWD}))
        .await;
    let forwarded = harness.expect_forwarded().await;
    assert_eq!(forwarded.method, "session/new");
    harness
        .respond(forwarded.id, Ok(json!({"sessionId": "C"})))
        .await;
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, fresh);
    assert!(result.is_ok());
}

#[tokio::test]
async fn a_v1_setup_reservation_clears_on_failure() {
    let mut harness = Harness::start();
    harness.negotiate_v1().await;

    let first = RequestId::Number(910);
    harness
        .request(first.clone(), "session/new", json!({"cwd": CWD}))
        .await;
    let forwarded = harness.expect_forwarded().await;

    // Only one setup may be in flight on a v1 connection.
    let blocked = RequestId::Number(911);
    harness
        .request(blocked.clone(), "session/new", json!({"cwd": CWD}))
        .await;
    let error = expect_error(&harness, blocked, -32010).await;
    assert_eq!(
        error.data.expect("setup reason")["reason"],
        "session_setup_in_progress"
    );

    // A failed setup binds nothing, so the next one is admitted.
    harness
        .respond(forwarded.id, Err(AcpError::new(-32603, "host failure")))
        .await;
    expect_error(&harness, first, -32603).await;
    let retry = RequestId::Number(912);
    harness
        .request(retry.clone(), "session/new", json!({"cwd": CWD}))
        .await;
    let forwarded = harness.expect_forwarded().await;
    assert_eq!(forwarded.method, "session/new");
    harness
        .respond(forwarded.id, Ok(json!({"sessionId": "A"})))
        .await;
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, retry);
    assert!(result.is_ok());
}

#[tokio::test]
async fn a_stale_failed_resume_keeps_the_newer_attach() {
    let mut harness = Harness::start();
    harness.negotiate().await;

    // The first resume creates the slot; the second takes it over.
    let stale = RequestId::Number(610);
    harness
        .request(
            stale.clone(),
            "session/resume",
            json!({"sessionId": "A", "cwd": CWD}),
        )
        .await;
    let stale_forwarded = harness.expect_forwarded().await;
    let newer = RequestId::Number(611);
    harness
        .request(
            newer.clone(),
            "session/resume",
            json!({"sessionId": "A", "cwd": CWD}),
        )
        .await;
    let newer_forwarded = harness.expect_forwarded().await;

    harness
        .respond(newer_forwarded.id, Ok(json!({"sessionId": "A"})))
        .await;
    expect_state(&harness, "A", "idle").await;
    harness.fetch_config("A", config_options("default")).await;
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, newer);
    assert!(result.is_ok());

    // The earlier attempt fails afterwards. Only the attempt that still owns
    // the slot may unwind it, so the attach and its updates must survive.
    harness
        .respond(
            stale_forwarded.id,
            Err(AcpError::new(-32010, "Session is closing")),
        )
        .await;
    expect_error(&harness, stale, -32010).await;
    harness
        .send_update(
            "A",
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "t"}),
        )
        .await;
    let live = harness.expect_out().await.update();
    assert_eq!(live["sessionUpdate"], "tool_call_update");
}

#[tokio::test]
async fn a_completion_from_a_replaced_slot_cannot_publish_running() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    harness.open("A").await;
    begin_turn(&harness, "A").await;

    let v2 = harness.v2();
    let old = tokio::spawn(async move {
        v2.send_request("session/request_permission", permission("A"))
            .await
    });
    let old_peer = harness.fake().next_peer().await;
    assert_eq!(
        harness.expect_out().await.update()["state"],
        "requires_action"
    );

    // Close while the permission is outstanding: the slot is dropped.
    let close = RequestId::Number(700);
    harness
        .request(close.clone(), "session/close", json!({"sessionId": "A"}))
        .await;
    let forwarded = harness.expect_forwarded().await;
    assert_eq!(forwarded.method, super::CLOSE);
    harness.respond(forwarded.id, Ok(json!({}))).await;
    expect_state(&harness, "A", "idle").await;
    let (answered, _) = harness.expect_out().await.response();
    assert_eq!(answered, close);

    // A fresh attach and turn restart the epoch at 1 in a new slot.
    harness
        .request(
            RequestId::Number(701),
            "session/resume",
            json!({"sessionId": "A", "cwd": CWD}),
        )
        .await;
    let forwarded = harness.expect_forwarded().await;
    harness
        .respond(forwarded.id, Ok(json!({"sessionId": "A"})))
        .await;
    expect_state(&harness, "A", "idle").await;
    harness.fetch_config("A", config_options("default")).await;
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, RequestId::Number(701));
    assert!(result.is_ok());
    begin_turn(&harness, "A").await;

    let v2 = harness.v2();
    let current = tokio::spawn(async move {
        v2.send_request("session/request_permission", permission("A"))
            .await
    });
    let current_peer = harness.fake().next_peer().await;
    assert_eq!(
        harness.expect_out().await.update()["state"],
        "requires_action"
    );

    // The old completion names the same session and epoch but not the slot.
    old_peer.respond(Ok(json!({"outcome": {"outcome": "cancelled"}})));
    assert!(old.await.expect("old request").is_ok());
    harness.expect_quiet().await;

    current_peer.respond(Ok(json!({"outcome": {"outcome": "cancelled"}})));
    assert!(current.await.expect("current request").is_ok());
    assert_eq!(harness.expect_out().await.update()["state"], "running");
}

#[tokio::test]
async fn metadata_held_while_resuming_is_projected_before_the_reply() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    harness.open("A").await;

    let id = RequestId::Number(510);
    harness
        .request(
            id.clone(),
            "session/resume",
            json!({"sessionId": "A", "cwd": CWD}),
        )
        .await;
    let forwarded = harness.expect_forwarded().await;
    assert_eq!(forwarded.method, "session/resume");
    for update in [
        json!({"sessionUpdate": "available_commands_update",
               "availableCommands": [{"name": "verify", "description": "run the probe",
                                      "input": {"hint": "what to verify"}}]}),
        json!({"sessionUpdate": "config_option_update",
               "configOptions": [{"id": "mode", "name": "Session Mode", "type": "select",
                                  "currentValue": "default", "options": []}]}),
        json!({"sessionUpdate": "current_mode_update", "currentModeId": "default",
               "_meta": {"private": true}}),
    ] {
        harness.send_update("A", update).await;
    }
    harness.expect_quiet().await;

    harness
        .respond(forwarded.id, Ok(json!({"sessionId": "A"})))
        .await;
    // The state snapshot, then the metadata parked while attaching: both reach
    // the client *before* the resume reply, in the order the host produced them,
    // in their v2 shape.
    expect_state(&harness, "A", "idle").await;
    let commands = harness.expect_out().await.update();
    assert_eq!(commands["sessionUpdate"], "available_commands_update");
    assert_eq!(commands["availableCommands"][0]["type"], "command");
    assert_eq!(commands["availableCommands"][0]["name"], "verify");
    assert_eq!(commands["availableCommands"][0]["input"]["type"], "text");
    assert_eq!(
        commands["availableCommands"][0]["input"]["hint"],
        "what to verify"
    );
    let options = harness.expect_out().await.update();
    assert_eq!(options["sessionUpdate"], "config_option_update");
    assert_eq!(options["configOptions"][0]["configId"], "mode");
    assert!(options["configOptions"][0].get("id").is_none());
    // `current_mode_update` has no v2 equivalent and is dropped; the reply waits
    // for the option snapshot v1 resume does not return.
    harness.fetch_config("A", config_options("default")).await;
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, id);
    assert!(result.is_ok());
    harness.expect_quiet().await;
}
