//! Replay ordering, deferred live updates and active-turn resume.

use serde_json::json;

use crate::transport::types::{AcpError, RequestId};

use super::super::harness::{begin_turn, expect_error, expect_state, Harness, CWD};
use super::MAX_DEFERRED_UPDATES;

fn replay_start() -> serde_json::Value {
    json!({"type": "start"})
}

#[tokio::test]
async fn live_updates_are_held_during_replay_and_flushed_after_the_reply() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    harness.open("A").await;
    // A turn is already running: replay must not be refused, and the snapshot
    // must report the live foreground state.
    begin_turn(&harness, "A").await;

    let id = RequestId::Number(4000);
    harness
        .request(
            id.clone(),
            "session/resume",
            json!({"sessionId": "A", "cwd": CWD, "replayFrom": replay_start()}),
        )
        .await;
    let forwarded = harness.expect_forwarded().await;
    assert_eq!(forwarded.method, "session/load");

    // Replay-marked chunks stream, and each message gets one replacement
    // boundary before its first chunk.
    harness
        .send_update(
            "A",
            json!({"sessionUpdate": "agent_message_chunk", "messageId": "m1",
                   "content": {"type": "text", "text": "replayed"},
                   "_meta": {"periReplay": true}}),
        )
        .await;
    assert_eq!(
        harness.expect_out().await.update()["sessionUpdate"],
        "agent_message"
    );
    assert_eq!(
        harness.expect_out().await.update()["sessionUpdate"],
        "agent_message_chunk"
    );

    // Live activity of the running turn is parked: a live chunk (which must not
    // reset its message) and a tool update.
    harness
        .send_update(
            "A",
            json!({"sessionUpdate": "agent_message_chunk", "messageId": "m2",
                   "content": {"type": "text", "text": "live"}}),
        )
        .await;
    harness
        .send_update(
            "A",
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "in_progress"}),
        )
        .await;
    harness.expect_quiet().await;

    harness
        .respond(forwarded.id, Ok(json!({"sessionId": "A"})))
        .await;
    assert_eq!(
        harness.expect_out().await.update()["sessionUpdate"],
        "replay_complete"
    );
    expect_state(&harness, "A", "running").await;
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, id);
    assert!(result.is_ok());

    // Deferred live updates follow the reply in arrival order. The live chunk
    // keeps its identity and gets no replay boundary.
    let live = harness.expect_out().await.update();
    assert_eq!(live["sessionUpdate"], "agent_message_chunk");
    assert_eq!(live["messageId"], "m2");
    let tool = harness.expect_out().await.update();
    assert_eq!(tool["sessionUpdate"], "tool_call_update");
    assert_eq!(tool["toolCallId"], "t1");
    harness.expect_quiet().await;
}

#[tokio::test]
async fn replay_complete_is_sent_exactly_once_even_for_an_empty_replay() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    harness.open("A").await;

    let id = RequestId::Number(4100);
    harness
        .request(
            id.clone(),
            "session/resume",
            json!({"sessionId": "A", "cwd": CWD, "replayFrom": replay_start()}),
        )
        .await;
    let forwarded = harness.expect_forwarded().await;
    harness
        .respond(forwarded.id, Ok(json!({"sessionId": "A"})))
        .await;
    assert_eq!(
        harness.expect_out().await.update()["sessionUpdate"],
        "replay_complete"
    );
    expect_state(&harness, "A", "idle").await;
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, id);
    assert!(result.is_ok());
    harness.expect_quiet().await;
}

#[tokio::test]
async fn a_cursorless_resume_of_an_attached_session_only_snapshots_state() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    harness.open("A").await;
    begin_turn(&harness, "A").await;

    let id = RequestId::Number(4200);
    harness
        .request(
            id.clone(),
            "session/resume",
            json!({"sessionId": "A", "cwd": CWD}),
        )
        .await;
    let forwarded = harness.expect_forwarded().await;
    assert_eq!(forwarded.method, "session/resume");
    harness
        .respond(forwarded.id, Ok(json!({"sessionId": "A"})))
        .await;
    // No `replay_complete`: the request asked for no replay.
    expect_state(&harness, "A", "running").await;
    harness.fetch_config("A", json!([])).await;
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, id);
    assert!(result.is_ok());
    harness.expect_quiet().await;
}

#[tokio::test]
async fn the_replay_buffer_is_bounded_and_drops_past_its_limit() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    harness.open("A").await;

    let id = RequestId::Number(4300);
    harness
        .request(
            id.clone(),
            "session/resume",
            json!({"sessionId": "A", "cwd": CWD, "replayFrom": replay_start()}),
        )
        .await;
    let forwarded = harness.expect_forwarded().await;
    for index in 0..=MAX_DEFERRED_UPDATES {
        harness
            .send_update(
                "A",
                json!({"sessionUpdate": "tool_call_update", "toolCallId": format!("t{index}")}),
            )
            .await;
    }
    harness.expect_quiet().await;

    harness
        .respond(forwarded.id, Ok(json!({"sessionId": "A"})))
        .await;
    assert_eq!(
        harness.expect_out().await.update()["sessionUpdate"],
        "replay_complete"
    );
    expect_state(&harness, "A", "idle").await;
    assert!(harness.expect_out().await.response().1.is_ok());
    for index in 0..MAX_DEFERRED_UPDATES {
        let update = harness.expect_out().await.update();
        assert_eq!(update["toolCallId"], format!("t{index}"));
    }
    // The update past the bound was dropped, and the stream keeps working.
    harness
        .send_update(
            "A",
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "after"}),
        )
        .await;
    assert_eq!(harness.expect_out().await.update()["toolCallId"], "after");
}

#[tokio::test]
async fn a_cancel_during_replay_still_stops_the_turn_and_converges_to_idle() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    harness.open("A").await;
    begin_turn(&harness, "A").await;

    let id = RequestId::Number(4400);
    harness
        .request(
            id.clone(),
            "session/resume",
            json!({"sessionId": "A", "cwd": CWD, "replayFrom": replay_start()}),
        )
        .await;
    let forwarded = harness.expect_forwarded().await;
    assert_eq!(forwarded.method, "session/load");

    harness
        .notification("session/cancel", json!({"sessionId": "A"}))
        .await;
    let cancel = harness.expect_forwarded().await;
    assert_eq!(cancel.method, super::super::CANCEL);
    harness.respond(cancel.id, Ok(json!([]))).await;

    // The turn ends while the replay still holds the slot: the idle snapshot is
    // represented by the resume state snapshot, not by a parked update.
    harness.send_done("A", "cancelled").await;
    harness.expect_quiet().await;

    harness
        .respond(forwarded.id, Ok(json!({"sessionId": "A"})))
        .await;
    assert_eq!(
        harness.expect_out().await.update()["sessionUpdate"],
        "replay_complete"
    );
    expect_state(&harness, "A", "idle").await;
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, id);
    assert!(result.is_ok());
    harness.expect_quiet().await;
}

#[tokio::test]
async fn a_pending_resume_never_drops_the_turn_end() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    harness.open("A").await;
    begin_turn(&harness, "A").await;

    let resume = RequestId::Number(4600);
    harness
        .request(
            resume.clone(),
            "session/resume",
            json!({"sessionId": "A", "cwd": CWD}),
        )
        .await;
    let forwarded = harness.expect_forwarded().await;

    // The turn ends while the resume is still pending. Ending the foreground
    // state must not depend on the attach phase: otherwise a later host
    // rejection leaves the session looking `running` forever.
    harness.send_done("A", "end_turn").await;
    expect_state(&harness, "A", "idle").await;

    harness
        .respond(
            forwarded.id,
            Err(AcpError::new(
                -32010,
                "Session is executing; retry after the current turn",
            )),
        )
        .await;
    expect_error(&harness, resume, -32010).await;

    // The slot is attached again; a replay request reaches the host instead of
    // being refused from a stale foreground state.
    let replay = RequestId::Number(4601);
    harness
        .request(
            replay,
            "session/resume",
            json!({"sessionId": "A", "cwd": CWD, "replayFrom": replay_start()}),
        )
        .await;
    let forwarded = harness.expect_forwarded().await;
    assert_eq!(forwarded.method, "session/load");
}

#[tokio::test]
async fn a_stale_success_never_flushes_the_takeovers_deferred_updates() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    harness.open("A").await;

    let stale = RequestId::Number(4500);
    harness
        .request(
            stale.clone(),
            "session/resume",
            json!({"sessionId": "A", "cwd": CWD, "replayFrom": replay_start()}),
        )
        .await;
    let stale_forwarded = harness.expect_forwarded().await;
    let newer = RequestId::Number(4501);
    harness
        .request(
            newer.clone(),
            "session/resume",
            json!({"sessionId": "A", "cwd": CWD, "replayFrom": replay_start()}),
        )
        .await;
    let newer_forwarded = harness.expect_forwarded().await;

    harness
        .send_update(
            "A",
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "t1"}),
        )
        .await;
    harness.expect_quiet().await;

    // The stale attempt answers its client but must not end the takeover's
    // replay or release its parked updates.
    harness
        .respond(stale_forwarded.id, Ok(json!({"sessionId": "A"})))
        .await;
    expect_state(&harness, "A", "idle").await;
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, stale);
    assert!(result.is_ok());
    harness.expect_quiet().await;

    harness
        .respond(newer_forwarded.id, Ok(json!({"sessionId": "A"})))
        .await;
    assert_eq!(
        harness.expect_out().await.update()["sessionUpdate"],
        "replay_complete"
    );
    expect_state(&harness, "A", "idle").await;
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, newer);
    assert!(result.is_ok());
    assert_eq!(
        harness.expect_out().await.update()["toolCallId"],
        "t1",
        "the takeover owns the deferred updates"
    );
}
