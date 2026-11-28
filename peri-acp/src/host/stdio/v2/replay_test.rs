//! Replay ordering, deferred live updates and active-turn resume.

use serde_json::json;

use crate::transport::types::{AcpError, RequestId};
use crate::transport::AcpTransport;

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
    assert_eq!(forwarded.method, "session/resume");

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
    // The host's turn lock rejection reaches a v2 client as the stable Peri
    // busy code, not as the host's overloaded `-32010` (v1 surface only).
    let error = expect_error(&harness, resume, -32020).await;
    let data = error.data.expect("busy data");
    assert_eq!(data["reason"], "session_busy");
    assert_eq!(data["sessionId"], "A");

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
async fn a_failed_replay_reports_the_busy_session_with_a_stable_code() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    harness.open("A").await;
    begin_turn(&harness, "A").await;

    // The adapter accepts the replay and holds live updates, but the host's
    // per-session prompt lock refuses `session/load` while the turn runs. The
    // client must see one stable Peri code and reason, not the host's `-32010`.
    let replay = RequestId::Number(4700);
    harness
        .request(
            replay.clone(),
            "session/resume",
            json!({"sessionId": "A", "cwd": CWD, "replayFrom": replay_start()}),
        )
        .await;
    let forwarded = harness.expect_forwarded().await;
    assert_eq!(forwarded.method, "session/load");
    harness
        .respond(
            forwarded.id,
            Err(AcpError::new(
                -32010,
                "Session is executing; retry after the current turn",
            )),
        )
        .await;
    let error = expect_error(&harness, replay, -32020).await;
    let data = error.data.expect("busy data");
    assert_eq!(data["reason"], "session_busy");
    assert_eq!(data["sessionId"], "A");
    // A refused replay emits no `replay_complete`: the run never started.
    harness.expect_quiet().await;
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

#[tokio::test]
async fn a_failed_replay_publishes_the_state_transition_the_hold_swallowed() {
    for error_case in [false, true] {
        let mut harness = Harness::start();
        harness.negotiate().await;
        harness.open("A").await;
        begin_turn(&harness, "A").await;

        let replay = RequestId::Number(4800);
        harness
            .request(
                replay.clone(),
                "session/resume",
                json!({"sessionId": "A", "cwd": CWD, "replayFrom": replay_start()}),
            )
            .await;
        let forwarded = harness.expect_forwarded().await;
        assert_eq!(forwarded.method, "session/load");

        // The turn ends while the replay holds the slot: the state transition is
        // suppressed (stale snapshots are not parked) and must not be lost when
        // the host then refuses the replay.
        if error_case {
            harness.v2().observe_execution_failure("A").await;
        }
        let reason = if error_case { "error" } else { "cancelled" };
        harness.send_done("A", reason).await;
        harness.expect_quiet().await;

        harness
            .respond(
                forwarded.id,
                Err(AcpError::new(
                    -32010,
                    "Session is executing; retry after the current turn",
                )),
            )
            .await;
        let error = expect_error(&harness, replay, -32020).await;
        assert_eq!(error.data.expect("busy data")["reason"], "session_busy");

        // The client that received `session_busy` still learns the turn ended,
        // with the stop reason (and the execution error) it had.
        let state = expect_state(&harness, "A", "idle").await;
        assert_eq!(state["stopReason"], reason);
        if error_case {
            assert_eq!(state["error"]["code"], -32603);
        }
        harness.expect_quiet().await;
    }
}

#[tokio::test]
async fn a_superseded_late_failure_never_deletes_the_attached_slot() {
    let mut harness = Harness::start();
    harness.negotiate().await;

    // A's replay creates the slot and stays pending, so its late failure would
    // meet the "this attempt created the slot" rule.
    let a = RequestId::Number(4900);
    harness
        .request(
            a.clone(),
            "session/resume",
            json!({"sessionId": "A", "cwd": CWD, "replayFrom": replay_start()}),
        )
        .await;
    let a_forwarded = harness.expect_forwarded().await;

    // B takes over and completes, so the slot is genuinely attached.
    let b = RequestId::Number(4901);
    harness
        .request(
            b.clone(),
            "session/resume",
            json!({"sessionId": "A", "cwd": CWD, "replayFrom": replay_start()}),
        )
        .await;
    let b_forwarded = harness.expect_forwarded().await;
    harness
        .respond(b_forwarded.id, Ok(json!({"sessionId": "A"})))
        .await;
    assert_eq!(
        harness.expect_out().await.update()["sessionUpdate"],
        "replay_complete"
    );
    expect_state(&harness, "A", "idle").await;
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, b);
    assert!(result.is_ok());

    // C fails. A is older than the completed attach, so handing the slot back
    // to A would resurrect a superseded attempt.
    let c = RequestId::Number(4902);
    harness
        .request(
            c.clone(),
            "session/resume",
            json!({"sessionId": "A", "cwd": CWD}),
        )
        .await;
    let c_forwarded = harness.expect_forwarded().await;
    harness
        .respond(
            c_forwarded.id,
            Err(AcpError::new(
                -32010,
                "Session is executing; retry after the current turn",
            )),
        )
        .await;
    expect_error(&harness, c, -32020).await;

    // A's late failure must answer A but leave the live slot alone.
    harness
        .respond(
            a_forwarded.id,
            Err(AcpError::new(
                -32010,
                "Session is executing; retry after the current turn",
            )),
        )
        .await;
    expect_error(&harness, a, -32020).await;

    harness
        .send_update(
            "A",
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "t1"}),
        )
        .await;
    assert_eq!(
        harness.expect_out().await.update()["toolCallId"],
        "t1",
        "a superseded late failure must not drop the attached slot"
    );
    harness.expect_quiet().await;
}

#[tokio::test]
async fn a_failed_takeover_keeps_the_pending_owner_attaching() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    harness.open("A").await;

    // A is a cursorless resume: it owns the slot and parks metadata.
    let a = RequestId::Number(5000);
    harness
        .request(
            a.clone(),
            "session/resume",
            json!({"sessionId": "A", "cwd": CWD}),
        )
        .await;
    let a_forwarded = harness.expect_forwarded().await;
    assert_eq!(a_forwarded.method, "session/resume");

    let b = RequestId::Number(5001);
    harness
        .request(
            b.clone(),
            "session/resume",
            json!({"sessionId": "A", "cwd": CWD, "replayFrom": replay_start()}),
        )
        .await;
    let b_forwarded = harness.expect_forwarded().await;
    assert_eq!(b_forwarded.method, "session/load");

    harness
        .respond(
            b_forwarded.id,
            Err(AcpError::new(
                -32010,
                "Session is executing; retry after the current turn",
            )),
        )
        .await;
    expect_error(&harness, b, -32020).await;

    // A still owns the setup: its metadata must stay parked instead of being
    // published ahead of its own snapshot.
    harness
        .send_update(
            "A",
            json!({"sessionUpdate": "session_info_update", "title": "Y"}),
        )
        .await;
    harness.expect_quiet().await;

    harness
        .respond(a_forwarded.id, Ok(json!({"sessionId": "A"})))
        .await;
    expect_state(&harness, "A", "idle").await;
    let info = harness.expect_out().await.update();
    assert_eq!(info["sessionUpdate"], "session_info_update");
    assert_eq!(info["title"], "Y");
    harness.fetch_config("A", json!([])).await;
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, a);
    assert!(result.is_ok());
    harness.expect_quiet().await;
}

#[tokio::test]
async fn a_failed_cursorless_takeover_restores_the_owners_replay_state() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    harness.open("A").await;

    let a = RequestId::Number(5100);
    harness
        .request(
            a.clone(),
            "session/resume",
            json!({"sessionId": "A", "cwd": CWD, "replayFrom": replay_start()}),
        )
        .await;
    let a_forwarded = harness.expect_forwarded().await;

    // A starts streaming message m1; its first chunk gets one replacement
    // boundary.
    harness
        .send_update(
            "A",
            json!({"sessionUpdate": "agent_message_chunk", "messageId": "m1",
                   "content": {"type": "text", "text": "one"},
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

    // A cursorless takeover clears the active replay; when it fails, A's replay
    // state (including m1's identity) must come back.
    let b = RequestId::Number(5101);
    harness
        .request(
            b.clone(),
            "session/resume",
            json!({"sessionId": "A", "cwd": CWD}),
        )
        .await;
    let b_forwarded = harness.expect_forwarded().await;
    harness
        .respond(
            b_forwarded.id,
            Err(AcpError::new(
                -32010,
                "Session is executing; retry after the current turn",
            )),
        )
        .await;
    expect_error(&harness, b, -32020).await;

    // The already-started message keeps no second boundary; a new message gets
    // exactly one.
    harness
        .send_update(
            "A",
            json!({"sessionUpdate": "agent_message_chunk", "messageId": "m1",
                   "content": {"type": "text", "text": "two"},
                   "_meta": {"periReplay": true}}),
        )
        .await;
    assert_eq!(
        harness.expect_out().await.update()["sessionUpdate"],
        "agent_message_chunk"
    );
    harness
        .send_update(
            "A",
            json!({"sessionUpdate": "agent_message_chunk", "messageId": "m2",
                   "content": {"type": "text", "text": "three"},
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

    harness
        .respond(a_forwarded.id, Ok(json!({"sessionId": "A"})))
        .await;
    assert_eq!(
        harness.expect_out().await.update()["sessionUpdate"],
        "replay_complete"
    );
    expect_state(&harness, "A", "idle").await;
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, a);
    assert!(result.is_ok());
    harness.expect_quiet().await;
}

#[tokio::test]
async fn a_stale_success_never_hands_the_slot_back_to_an_older_attempt() {
    let mut harness = Harness::start();
    harness.negotiate().await;

    // Three resumes of a session this connection never attached. The first two
    // replay; the third takes the slot over.
    let old = RequestId::Number(4950);
    harness
        .request(
            old.clone(),
            "session/resume",
            json!({"sessionId": "A", "cwd": CWD, "replayFrom": replay_start()}),
        )
        .await;
    let old_forwarded = harness.expect_forwarded().await;
    let middle = RequestId::Number(4951);
    harness
        .request(
            middle.clone(),
            "session/resume",
            json!({"sessionId": "A", "cwd": CWD, "replayFrom": replay_start()}),
        )
        .await;
    let middle_forwarded = harness.expect_forwarded().await;
    let owner = RequestId::Number(4952);
    harness
        .request(
            owner.clone(),
            "session/resume",
            json!({"sessionId": "A", "cwd": CWD}),
        )
        .await;
    let owner_forwarded = harness.expect_forwarded().await;
    assert_eq!(owner_forwarded.method, "session/resume");

    // A stale success is still a completed attach. It must raise the watermark:
    // the handback below may only pick a pending attempt newer than the last
    // attach that actually completed.
    harness
        .respond(middle_forwarded.id, Ok(json!({"sessionId": "A"})))
        .await;
    expect_state(&harness, "A", "idle").await;
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, middle);
    assert!(result.is_ok());
    harness.expect_quiet().await;

    // The takeover fails. `old` is older than that attach, so it is not handed
    // back.
    harness
        .respond(
            owner_forwarded.id,
            Err(AcpError::new(
                -32010,
                "Session is executing; retry after the current turn",
            )),
        )
        .await;
    expect_error(&harness, owner, -32020).await;
    harness.expect_quiet().await;

    // `old` completing now must not resurrect itself as the owner: no
    // `replay_complete`, no flush, and the live session keeps serving.
    harness
        .respond(old_forwarded.id, Ok(json!({"sessionId": "A"})))
        .await;
    expect_state(&harness, "A", "idle").await;
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, old);
    assert!(result.is_ok());
    harness
        .send_update(
            "A",
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "t1"}),
        )
        .await;
    assert_eq!(harness.expect_out().await.update()["toolCallId"], "t1");
    harness.expect_quiet().await;
}

#[tokio::test]
async fn a_cancel_while_a_cursorless_resume_is_pending_still_reaches_the_turn() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    harness.open("A").await;
    begin_turn(&harness, "A").await;

    let resume = RequestId::Number(5200);
    harness
        .request(
            resume.clone(),
            "session/resume",
            json!({"sessionId": "A", "cwd": CWD}),
        )
        .await;
    let forwarded = harness.expect_forwarded().await;
    assert_eq!(forwarded.method, "session/resume");

    // The slot is `Attaching` with no replay, but the turn is live: the cancel
    // must reach the host instead of being dropped by the attach filter.
    harness
        .notification("session/cancel", json!({"sessionId": "A"}))
        .await;
    let cancel = harness.expect_forwarded().await;
    assert_eq!(cancel.method, super::super::CANCEL);
    harness.respond(cancel.id, Ok(json!([]))).await;

    harness.send_done("A", "cancelled").await;
    expect_state(&harness, "A", "idle").await;
    harness
        .respond(forwarded.id, Ok(json!({"sessionId": "A"})))
        .await;
    expect_state(&harness, "A", "idle").await;
    harness.fetch_config("A", json!([])).await;
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, resume);
    assert!(result.is_ok());

    // An idle session whose only setup is still attaching has nothing running:
    // the same cancel is dropped (no host request).
    let idle_resume = RequestId::Number(5201);
    harness
        .request(
            idle_resume.clone(),
            "session/resume",
            json!({"sessionId": "A", "cwd": CWD}),
        )
        .await;
    let idle_forwarded = harness.expect_forwarded().await;
    harness
        .notification("session/cancel", json!({"sessionId": "A"}))
        .await;
    harness.expect_quiet().await;
    harness
        .respond(idle_forwarded.id, Ok(json!({"sessionId": "A"})))
        .await;
    expect_state(&harness, "A", "idle").await;
    harness.fetch_config("A", json!([])).await;
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, idle_resume);
    assert!(result.is_ok());
}

#[test]
fn a_replayed_broadcast_keeps_only_the_newest_metadata_snapshot_per_kind() {
    let mut deferred = super::Deferred::default();
    deferred.begin();
    deferred.hold(json!({"sessionUpdate": "available_commands_update",
                         "availableCommands": [{"name": "stale"}]}));
    deferred.hold(json!({"sessionUpdate": "config_option_update", "configOptions": []}));
    deferred.hold(json!({"sessionUpdate": "available_commands_update",
                         "availableCommands": [{"name": "fresh"}]}));
    let flushed = deferred.finish();
    assert_eq!(
        flushed
            .iter()
            .map(|update| update["sessionUpdate"].clone())
            .collect::<Vec<_>>(),
        vec![
            json!("config_option_update"),
            json!("available_commands_update")
        ]
    );
    assert_eq!(flushed[1]["availableCommands"][0]["name"], "fresh");
}

#[test]
fn session_info_patches_are_all_kept_in_arrival_order() {
    // The pinned v2 `session_info_update` is a patch: omitted fields keep their
    // value. Parked during a replay it must therefore never be merged by kind,
    // or the title and the timestamp below would overwrite each other.
    let mut deferred = super::Deferred::default();
    deferred.begin();
    deferred.hold(json!({"sessionUpdate": "session_info_update", "title": "First"}));
    deferred.hold(json!({"sessionUpdate": "available_commands_update",
                         "availableCommands": [{"name": "stale"}]}));
    deferred.hold(json!({"sessionUpdate": "session_info_update",
                         "updatedAt": "2026-01-02T03:04:05Z"}));
    deferred.hold(json!({"sessionUpdate": "available_commands_update",
                         "availableCommands": [{"name": "fresh"}]}));
    let flushed = deferred.finish();
    assert_eq!(
        flushed
            .iter()
            .map(|update| update["sessionUpdate"].clone())
            .collect::<Vec<_>>(),
        vec![
            json!("session_info_update"),
            json!("session_info_update"),
            json!("available_commands_update"),
        ],
        "both patches survive, in arrival order; the stale whole table is dropped \
         and the fresh one keeps its own arrival position"
    );
    assert_eq!(flushed[0]["title"], "First");
    assert!(flushed[0].get("updatedAt").is_none());
    assert_eq!(flushed[1]["updatedAt"], "2026-01-02T03:04:05Z");
    assert!(flushed[1].get("title").is_none());
    assert_eq!(
        flushed[2]["availableCommands"][0]["name"], "fresh",
        "replace-whole tables still keep only the newest"
    );
}
