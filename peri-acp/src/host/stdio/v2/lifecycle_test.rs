//! `session/delete`: durable deletion through the host, with the local slot
//! teardown only on a successful reply.

use serde_json::json;

use super::super::harness::{expect_error, Harness, CWD};
use crate::transport::types::{AcpError, RequestId};

#[tokio::test]
async fn a_delete_without_a_slot_still_reaches_the_host() {
    let mut harness = Harness::start();
    harness.negotiate().await;

    // A pinned `session/list` → `session/delete` → `session/list` client deletes
    // sessions this connection never attached. The durable delete must reach the
    // host; answering locally would leave the session in storage.
    let id = RequestId::Number(3100);
    harness
        .request(id.clone(), "session/delete", json!({"sessionId": "gone"}))
        .await;
    let forwarded = harness.expect_forwarded().await;
    assert_eq!(forwarded.method, "session/delete");
    assert_eq!(forwarded.params["sessionId"], "gone");
    harness.respond(forwarded.id, Ok(json!({}))).await;
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, id);
    assert_eq!(
        result.expect("slotless delete replies the pinned empty payload"),
        json!({})
    );
    harness.expect_quiet().await;
}

#[tokio::test]
async fn a_delete_the_host_refuses_is_passed_through_and_keeps_the_slot() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    harness.open("A").await;

    let id = RequestId::Number(3110);
    harness
        .request(id.clone(), "session/delete", json!({"sessionId": "A"}))
        .await;
    let forwarded = harness.expect_forwarded().await;
    assert_eq!(forwarded.method, "session/delete");
    harness
        .respond(
            forwarded.id,
            Err(AcpError::new(-32002, "Session not found or not active")
                .with_data(json!({"sessionId": "A"}))),
        )
        .await;
    let error = expect_error(&harness, id, -32002).await;
    assert_eq!(
        error.data.expect("the host error keeps its data")["sessionId"],
        "A"
    );

    // The slot was not torn down: the session is still addressable.
    harness.enqueue_prompt("A", "still here").await;
}

#[tokio::test]
async fn a_successful_delete_with_a_slot_tears_it_down() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    harness.open("A").await;

    let id = RequestId::Number(3120);
    harness
        .request(id.clone(), "session/delete", json!({"sessionId": "A"}))
        .await;
    let forwarded = harness.expect_forwarded().await;
    assert_eq!(forwarded.method, "session/delete");
    harness.respond(forwarded.id, Ok(json!({}))).await;
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, id);
    assert_eq!(result.expect("delete reply"), json!({}));

    // The deleted session is no longer addressable on this connection.
    let prompt = RequestId::Number(3121);
    harness
        .request(
            prompt.clone(),
            "session/prompt",
            json!({"sessionId": "A", "prompt": [{"type": "text", "text": "gone"}]}),
        )
        .await;
    let error = expect_error(&harness, prompt, -32002).await;
    assert_eq!(error.data.expect("sessionId data")["sessionId"], "A");
    harness.expect_quiet().await;
}

#[tokio::test]
async fn a_delete_still_requires_a_session_id() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    let id = RequestId::Number(3130);
    harness
        .request(id.clone(), "session/delete", json!({"cwd": CWD}))
        .await;
    expect_error(&harness, id, -32602).await;
    harness.expect_quiet().await;
}
