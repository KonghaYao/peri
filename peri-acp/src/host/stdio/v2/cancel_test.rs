//! Adapter-level cancellation: `$/cancel_request`, `session/cancel` scoping and
//! the settlement `session/delete` performs on parked prompts.

use serde_json::json;

use super::harness::{expect_error, Harness, CWD};
use super::WITHDRAW;
use crate::transport::types::RequestId;

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
        .request(setup.clone(), "session/new", json!({"cwd": CWD}))
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
