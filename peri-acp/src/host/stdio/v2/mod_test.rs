//! Adapter-level tests: the v2 state machine, routing and error surface driven
//! through an in-process fake transport.

use serde_json::{json, Value};

use super::harness::Harness;
use super::WITHDRAW;
use crate::transport::types::{AcpError, RequestId};
use crate::transport::AcpTransport;

fn code(error: &AcpError) -> i64 {
    error.code
}

/// Read the next outbound item and require it to be a `session/update` state.
async fn expect_state(harness: &Harness, sid: &str, state: &str) -> Value {
    let out = harness.expect_out().await;
    let (method, params) = match out {
        super::harness::Out::Notification { method, params } => (method, params),
        other => panic!("expected a state notification, got {other:?}"),
    };
    assert_eq!(method, "session/update");
    assert_eq!(params["sessionId"], sid, "state update for another session");
    let update = &params["update"];
    assert_eq!(update["sessionUpdate"], "state_update", "{update}");
    assert_eq!(update["state"], state, "{update}");
    update.clone()
}

async fn expect_error(harness: &Harness, id: RequestId, expected: i64) -> AcpError {
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, id);
    let error = result.expect_err("expected an error reply");
    assert_eq!(code(&error), expected, "{error}");
    error
}

async fn begin_turn(harness: &Harness, sid: &str) {
    harness.start_turn(sid).await;
    expect_state(harness, sid, "running").await;
}

fn permission(sid: &str) -> Value {
    json!({"sessionId": sid, "toolCall": {"title": "run a tool"}})
}

#[tokio::test]
async fn a_connection_may_hold_several_sessions() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    harness.open("A").await;
    harness.open("B").await;

    let a = harness.enqueue_prompt("A", "hello A").await;
    let b = harness.enqueue_prompt("B", "hello B").await;
    harness.deliver("A", &a).await;
    expect_state(&harness, "A", "running").await;
    let message = harness.expect_out().await.update();
    assert_eq!(message["sessionUpdate"], "user_message");
    assert_eq!(message["messageId"], a.input.as_str());
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, a.request);
    assert_eq!(result.expect("prompt ack")["messageId"], a.input.as_str());

    // B keeps its own prompt pending and its own turn.
    harness.deliver("B", &b).await;
    expect_state(&harness, "B", "running").await;
    let message = harness.expect_out().await.update();
    assert_eq!(message["sessionUpdate"], "user_message");
    assert_eq!(message["messageId"], b.input.as_str());
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, b.request);
    assert!(result.is_ok());
}

#[tokio::test]
async fn closing_one_session_never_silences_the_other() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    harness.open("A").await;
    harness.open("B").await;
    begin_turn(&harness, "A").await;

    let id = RequestId::Number(900);
    harness
        .request(id.clone(), "session/close", json!({"sessionId": "A"}))
        .await;
    let forwarded = harness.expect_forwarded().await;
    assert_eq!(forwarded.method, super::CLOSE);
    harness.respond(forwarded.id, Ok(json!(["queued"]))).await;
    // Foreground work is reported as cancelled before the close reply.
    expect_state(&harness, "A", "idle").await;
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, id);
    assert!(result.is_ok());

    // A's slot is gone: its updates and prompts are no longer routed.
    harness
        .send_update(
            "A",
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "t"}),
        )
        .await;
    harness.expect_quiet().await;
    harness
        .request(
            RequestId::Number(901),
            "session/prompt",
            json!({"sessionId": "A", "prompt": [{"type": "text", "text": "ghost"}]}),
        )
        .await;
    let error = expect_error(&harness, RequestId::Number(901), -32002).await;
    assert_eq!(error.data.expect("sessionId data")["sessionId"], "A");

    // B is untouched.
    harness
        .send_update(
            "B",
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "t"}),
        )
        .await;
    let out = harness.expect_out().await;
    let (_, params) = out.notification();
    assert_eq!(params["sessionId"], "B");

    // Closing a session this connection never attached is a success.
    harness
        .request(
            RequestId::Number(902),
            "session/close",
            json!({"sessionId": "elsewhere"}),
        )
        .await;
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, RequestId::Number(902));
    assert!(result.is_ok());
}

#[tokio::test]
async fn a_completion_from_an_ended_turn_cannot_publish_running() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    harness.open("A").await;
    begin_turn(&harness, "A").await;

    let v2 = harness.v2();
    let request = tokio::spawn(async move {
        v2.send_request("session/request_permission", permission("A"))
            .await
    });
    let peer = harness.fake().next_peer().await;
    assert_eq!(peer.method, "session/request_permission");
    let update = harness.expect_out().await.update();
    assert_eq!(update["state"], "requires_action");

    // The turn ends while the permission is still outstanding.
    harness.send_done("A", "cancelled").await;
    expect_state(&harness, "A", "idle").await;

    peer.respond(Ok(json!({"outcome": {"outcome": "cancelled"}})));
    assert!(request.await.expect("request task").is_ok());
    harness.expect_quiet().await;
}

#[tokio::test]
async fn concurrent_permissions_resume_running_only_after_the_last_one() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    harness.open("A").await;
    begin_turn(&harness, "A").await;

    let first = harness.v2();
    let second = harness.v2();
    let one = tokio::spawn(async move {
        first
            .send_request("session/request_permission", permission("A"))
            .await
    });
    let first_peer = harness.fake().next_peer().await;
    let two = tokio::spawn(async move {
        second
            .send_request("session/request_permission", permission("A"))
            .await
    });
    let second_peer = harness.fake().next_peer().await;

    // The aggregation publishes `requires_action` once for the turn.
    let update = harness.expect_out().await.update();
    assert_eq!(update["state"], "requires_action");

    let granted = json!({"outcome": {"outcome": "selected", "optionId": "allow_once"}});
    first_peer.respond(Ok(granted.clone()));
    assert!(one.await.expect("task").is_ok());
    // One of two outstanding requests is not progress.
    harness.expect_quiet().await;

    second_peer.respond(Ok(granted));
    assert!(two.await.expect("task").is_ok());
    let update = harness.expect_out().await.update();
    assert_eq!(update["state"], "running");
    harness.expect_quiet().await;
}

#[tokio::test]
async fn metadata_pushed_while_resuming_is_flushed_after_the_reply() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    harness.open("A").await;

    let id = RequestId::Number(500);
    harness
        .request(
            id.clone(),
            "session/resume",
            json!({"sessionId": "A", "cwd": "/workspace"}),
        )
        .await;
    let forwarded = harness.expect_forwarded().await;
    assert_eq!(forwarded.method, "session/resume");

    // The host may push metadata before the resume reply is written.
    harness
        .send_update(
            "A",
            json!({"sessionUpdate": "session_info_update", "title": "restored"}),
        )
        .await;
    harness.expect_quiet().await;
    // Live activity is not parked, only session metadata is.
    harness
        .send_update(
            "A",
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "t"}),
        )
        .await;
    let live = harness.expect_out().await.update();
    assert_eq!(live["sessionUpdate"], "tool_call_update");

    harness
        .respond(forwarded.id, Ok(json!({"sessionId": "A"})))
        .await;
    // Snapshot, then the reply itself, then the parked metadata.
    expect_state(&harness, "A", "idle").await;
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, id);
    assert_eq!(result.expect("resume")["sessionId"], "A");
    let held = harness.expect_out().await.update();
    assert_eq!(held["sessionUpdate"], "session_info_update");
    assert_eq!(held["title"], "restored");

    // Once attached the same update is no longer parked.
    harness
        .send_update(
            "A",
            json!({"sessionUpdate": "session_info_update", "title": "live"}),
        )
        .await;
    let live = harness.expect_out().await.update();
    assert_eq!(live["title"], "live");
}

#[tokio::test]
async fn a_failed_resume_leaves_no_slot_behind() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    harness.open("A").await;
    harness.open("B").await;
    harness
        .request(
            RequestId::Number(600),
            "session/resume",
            json!({"sessionId": "B", "cwd": "/workspace"}),
        )
        .await;
    let forwarded = harness.expect_forwarded().await;
    harness
        .respond(
            forwarded.id,
            Err(AcpError::new(-32010, "Session is closing")),
        )
        .await;
    expect_error(&harness, RequestId::Number(600), -32010).await;
    // B was already attached, so it keeps receiving updates.
    harness
        .send_update(
            "B",
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "t"}),
        )
        .await;
    let (_, params) = harness.expect_out().await.notification();
    assert_eq!(params["sessionId"], "B");
}

#[tokio::test]
async fn replayed_chunks_get_a_replacement_boundary() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    harness.open("A").await;
    harness
        .request(
            RequestId::Number(700),
            "session/resume",
            json!({"sessionId": "A", "cwd": "/workspace", "replayFrom": {"type": "start"}}),
        )
        .await;
    let forwarded = harness.expect_forwarded().await;
    assert_eq!(forwarded.method, "session/load");
    harness
        .send_update(
            "A",
            json!({"sessionUpdate": "agent_message_chunk", "messageId": "m1",
                   "content": {"type": "text", "text": "replayed"}}),
        )
        .await;
    let reset = harness.expect_out().await.update();
    assert_eq!(reset["sessionUpdate"], "agent_message");
    assert_eq!(reset["messageId"], "m1");
    let chunk = harness.expect_out().await.update();
    assert_eq!(chunk["sessionUpdate"], "agent_message_chunk");
    harness
        .respond(forwarded.id, Ok(json!({"sessionId": "A"})))
        .await;
    let marker = harness.expect_out().await.update();
    assert_eq!(marker["sessionUpdate"], "replay_complete");
    expect_state(&harness, "A", "idle").await;
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, RequestId::Number(700));
    assert!(result.is_ok());
}

#[tokio::test]
async fn unsupported_methods_and_object_cancels_use_the_documented_codes() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    for (offset, (method, expected)) in [
        ("auth/login", -32601),
        ("auth/logout", -32601),
        ("session/fork", -32601),
        ("providers/list", -32601),
        ("mcp/message", -32601),
        ("session/set_config_option", -32601),
        ("terminal/cancel", -32602),
        ("tool_call/cancel", -32602),
        ("not/a/method", -32601),
    ]
    .into_iter()
    .enumerate()
    {
        let id = RequestId::Number(1000 + offset as i64);
        harness.request(id.clone(), method, json!({})).await;
        expect_error(&harness, id, expected).await;
    }
    // `-32000` means Authentication required in v2: host passthroughs that use
    // it for backpressure are rewritten instead of leaking through.
    assert_eq!(
        super::errors::normalize(AcpError::new(-32000, "busy")).code,
        -32020
    );
    assert_eq!(
        super::errors::normalize(AcpError::new(-32000, "busy"))
            .data
            .expect("reason")["reason"],
        "server_error"
    );
    assert_eq!(
        super::errors::normalize(AcpError::new(-32603, "x")).code,
        -32603
    );
}

#[tokio::test]
async fn session_setup_parameters_are_validated() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    for (offset, params) in [
        json!({}),
        json!({"cwd": "relative"}),
        json!({"cwd": "/workspace", "additionalDirectories": ["/other"]}),
        json!({"cwd": "/workspace", "systemPrompt": {"text": "x"}}),
    ]
    .into_iter()
    .enumerate()
    {
        let id = RequestId::Number(2000 + offset as i64);
        harness.request(id.clone(), "session/new", params).await;
        expect_error(&harness, id, -32602).await;
    }
    let id = RequestId::Number(2010);
    harness
        .request(id.clone(), "session/list", json!({"cursor": "opaque"}))
        .await;
    expect_error(&harness, id, -32602).await;
    // Replay cursors other than `start` are not implemented.
    let id = RequestId::Number(2011);
    harness
        .request(
            id.clone(),
            "session/resume",
            json!({"sessionId": "A", "cwd": "/workspace", "replayFrom": {"type": "message"}}),
        )
        .await;
    expect_error(&harness, id, -32602).await;
}

#[tokio::test]
async fn elicitation_is_gated_on_the_client_form_capability() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    // Client declarations commit with a successful initialize: a failed repeat
    // must not leave the connection believing the client supports forms.
    let repeat = RequestId::Number(1);
    harness
        .request(
            repeat.clone(),
            "initialize",
            json!({"protocolVersion": 2, "info": {"name": "probe", "version": "1"},
                   "clientCapabilities": {"elicitation": {"form": {}}}}),
        )
        .await;
    let forwarded = harness.expect_forwarded().await;
    harness
        .respond(forwarded.id, Err(AcpError::new(-32603, "host failure")))
        .await;
    expect_error(&harness, repeat, -32603).await;

    let params = json!({"sessionId": "A", "mode": "form", "message": "pick one"});
    let error = harness
        .v2()
        .send_request("elicitation/create", params.clone())
        .await
        .expect_err("a client without elicitation.form must not receive the request");
    assert_eq!(code(&error), -32601);
    assert_eq!(
        harness.fake().peer_count().await,
        0,
        "no request may reach the client"
    );

    let mut declared = Harness::start();
    declared
        .negotiate_with(json!({"elicitation": {"form": {}}}))
        .await;
    let request = {
        let v2 = declared.v2();
        tokio::spawn(async move { v2.send_request("elicitation/create", params).await })
    };
    let peer = declared.fake().next_peer().await;
    assert_eq!(peer.method, "elicitation/create");
    assert_eq!(peer.params["mode"], "form");
    peer.respond(Ok(json!({"action": {"action": "accept", "content": {}}})));
    assert!(request.await.expect("task").is_ok());

    // Permission requests are baseline and never gated.
    let mut plain = Harness::start();
    plain.negotiate().await;
    plain.open("A").await;
    begin_turn(&plain, "A").await;
    let v2 = plain.v2();
    let request = tokio::spawn(async move {
        v2.send_request("session/request_permission", permission("A"))
            .await
    });
    let peer = plain.fake().next_peer().await;
    peer.respond(Ok(json!({"outcome": {"outcome": "cancelled"}})));
    assert!(request.await.expect("task").is_ok());
}

#[tokio::test]
async fn a_keyed_retry_of_an_inserted_prompt_reuses_the_persisted_identity() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    harness.open("A").await;

    // First submission: the derived id travels to the host as the input id.
    let (request, forwarded) = harness
        .send_prompt_with("A", "same input", Some("key"))
        .await;
    let input = forwarded.params["inputId"]
        .as_str()
        .expect("input id")
        .to_owned();
    assert_eq!(
        uuid::Uuid::parse_str(&input)
            .expect("input id is a uuid")
            .get_version_num(),
        5,
        "keyed submissions derive their identity"
    );
    harness
        .respond(
            forwarded.id,
            Ok(json!({"messageId": input, "state": "queued"})),
        )
        .await;
    harness
        .deliver(
            "A",
            &super::harness::Submission {
                request: request.clone(),
                input: input.clone(),
            },
        )
        .await;
    expect_state(&harness, "A", "running").await;
    let echoed = harness.expect_out().await.update();
    assert_eq!(echoed["sessionUpdate"], "user_message");
    assert_eq!(echoed["messageId"], input.as_str());
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, request);
    assert_eq!(result.expect("first prompt")["messageId"], input.as_str());
    harness.send_done("A", "end_turn").await;
    expect_state(&harness, "A", "idle").await;

    // Retry: the same key derives the same id, so the host can only report the
    // existing message. The retry is acknowledged immediately, without a second
    // `user_message` echo and without waiting for a delivery that cannot come.
    let (retry, forwarded) = harness
        .send_prompt_with("A", "same input", Some("key"))
        .await;
    assert_eq!(
        forwarded.params["inputId"],
        input.as_str(),
        "a keyed retry must address the same persistent identity"
    );
    harness
        .respond(
            forwarded.id,
            Ok(json!({"messageId": input, "state": "delivered"})),
        )
        .await;
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, retry);
    assert_eq!(result.expect("retry")["messageId"], input.as_str());
    harness.expect_quiet().await;
}

#[tokio::test]
async fn a_keyed_retry_joins_the_submission_still_in_flight() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    harness.open("A").await;

    let (first, forwarded) = harness
        .send_prompt_with("A", "one submission", Some("key"))
        .await;
    let input = forwarded.params["inputId"]
        .as_str()
        .expect("input id")
        .to_owned();
    // The retry is parked on the first submission: nothing reaches the host.
    let second = harness
        .request_prompt("A", "one submission", Some("key"))
        .await;
    harness.expect_quiet().await;

    harness
        .respond(
            forwarded.id,
            Ok(json!({"messageId": input, "state": "queued"})),
        )
        .await;
    harness
        .deliver(
            "A",
            &super::harness::Submission {
                request: first.clone(),
                input: input.clone(),
            },
        )
        .await;

    expect_state(&harness, "A", "running").await;
    let echoed = harness.expect_out().await.update();
    assert_eq!(echoed["sessionUpdate"], "user_message");
    assert_eq!(echoed["messageId"], input.as_str());
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, first);
    assert_eq!(result.expect("first")["messageId"], input.as_str());
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, second);
    assert_eq!(result.expect("join")["messageId"], input.as_str());
}

#[tokio::test]
async fn keyed_retries_in_different_sessions_are_independent() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    harness.open("A").await;
    harness.open("B").await;

    let (_, left) = harness
        .send_prompt_with("A", "same input", Some("key"))
        .await;
    let (_, right) = harness
        .send_prompt_with("B", "same input", Some("key"))
        .await;
    assert_ne!(
        left.params["inputId"], right.params["inputId"],
        "keys are scoped to one session"
    );
}

#[tokio::test]
async fn cancel_request_withdraws_a_parked_prompt_and_ignores_everything_else() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    harness.open("A").await;
    let submission = harness.enqueue_prompt("A", "queued").await;

    // An unknown request id has no effect at all.
    harness
        .notification("$/cancel_request", json!({"requestId": 4242}))
        .await;
    harness.expect_quiet().await;

    harness
        .notification("$/cancel_request", json!({"requestId": submission.request}))
        .await;
    let forwarded = harness.expect_forwarded().await;
    assert_eq!(forwarded.method, WITHDRAW);
    assert_eq!(forwarded.params["inputId"], submission.input.as_str());
    harness.respond(forwarded.id, Ok(json!({}))).await;
    expect_error(&harness, submission.request, -32800).await;

    // A cancel aimed at a setup request is ignored, and that request still gets
    // its own answer (nothing is left dangling).
    let setup = RequestId::Number(4300);
    harness
        .request(setup.clone(), "session/new", json!({"cwd": "/workspace"}))
        .await;
    let forwarded = harness.expect_forwarded().await;
    harness
        .notification("$/cancel_request", json!({"requestId": setup}))
        .await;
    assert_eq!(harness.fake().peer_count().await, 0);
    assert_eq!(harness.fake().out_count().await, 0);
    harness
        .respond(forwarded.id, Ok(json!({"sessionId": "C"})))
        .await;
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, setup);
    assert!(result.is_ok());
}

#[tokio::test]
async fn session_cancel_routes_by_session_and_withdraws_its_queued_prompts() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    harness.open("A").await;
    harness.open("B").await;
    let queued = harness.enqueue_prompt("A", "queued").await;

    // Cancelling an unknown session is a no-op.
    harness
        .notification("session/cancel", json!({"sessionId": "ghost"}))
        .await;
    harness.expect_quiet().await;

    harness
        .notification("session/cancel", json!({"sessionId": "A"}))
        .await;
    let forwarded = harness.expect_forwarded().await;
    assert_eq!(forwarded.method, super::CANCEL);
    assert_eq!(forwarded.params["sessionId"], "A");
    assert_eq!(forwarded.params["inputIds"], json!([queued.input]));
    harness
        .respond(forwarded.id, Ok(json!([queued.input])))
        .await;
    expect_error(&harness, queued.request, -32800).await;
}

#[tokio::test]
async fn delete_settles_parked_prompts_and_drops_the_slot() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    harness.open("A").await;
    let parked = harness.enqueue_prompt("A", "will be settled").await;

    let id = RequestId::Number(3000);
    harness
        .request(id.clone(), "session/delete", json!({"sessionId": "A"}))
        .await;
    let forwarded = harness.expect_forwarded().await;
    assert_eq!(forwarded.method, "session/delete");
    harness.respond(forwarded.id, Ok(json!({}))).await;

    expect_error(&harness, parked.request, -32800).await;
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, id);
    assert!(result.is_ok());

    harness
        .request(
            RequestId::Number(3001),
            "session/prompt",
            json!({"sessionId": "A", "prompt": [{"type": "text", "text": "gone"}]}),
        )
        .await;
    let error = expect_error(&harness, RequestId::Number(3001), -32002).await;
    assert_eq!(error.data.expect("sessionId data")["sessionId"], "A");

    // Delete of a session this connection never attached is -32002.
    harness
        .request(
            RequestId::Number(3002),
            "session/delete",
            json!({"sessionId": "A"}),
        )
        .await;
    expect_error(&harness, RequestId::Number(3002), -32002).await;
}
