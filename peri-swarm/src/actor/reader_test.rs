use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::sync::broadcast;

use super::{ItemLog, PermissionStart, StagedItems, TurnOutcome};
use crate::domain::{
    turn_id_for_item, ApprovalDecision, ItemKind, McpToolSpec, OfficialEvent, PromptCacheOptions,
    PromptCacheTtl, SessionState, StreamEvent, TurnEvent, TurnStatus,
};
use crate::store::{MemorySwarmStore, SwarmSessionRow, SwarmStore};

#[path = "reader_broadcast_test.rs"]
mod broadcast_tests;

fn session_row() -> SwarmSessionRow {
    SwarmSessionRow {
        logical_id: "agent-1".to_owned(),
        root_session_id: "0192f0c5-0000-7000-8000-000000000001".to_owned(),
        actor_key: "agent/agent-1".to_owned(),
        cwd: "/tmp".to_owned(),
        state: SessionState::Idle,
        model: None,
        instructions: None,
        mcp_tools: Vec::new(),
        environment: crate::domain::EnvironmentRef::none(),
        metadata: Default::default(),
        prompt_cache_options: None,
        created_at: "2026-01-01T00:00:00Z".to_owned(),
        last_activity_at: "2026-01-01T00:00:00Z".to_owned(),
        last_item_seq: 0,
    }
}

struct Fixture {
    log: Arc<ItemLog>,
    store: Arc<dyn SwarmStore>,
    events: broadcast::Receiver<StreamEvent>,
}

async fn fixture() -> Fixture {
    let store: Arc<dyn SwarmStore> = Arc::new(MemorySwarmStore::new());
    store
        .swarm_upsert_session(session_row())
        .await
        .expect("session row");
    let (channel, events) = broadcast::channel(64);
    let log = Arc::new(ItemLog::new(
        "agent-1",
        Arc::clone(&store),
        channel,
        Arc::new(AtomicU64::new(0)),
    ));
    Fixture { log, store, events }
}

fn chunk(text: &str) -> serde_json::Value {
    chunk_for("msg-1", text)
}

fn chunk_for(message_id: &str, text: &str) -> serde_json::Value {
    json!({
        "sessionId": "agent-1",
        "update": {
            "sessionUpdate": "agent_message_chunk",
            "messageId": message_id,
            "content": { "type": "text", "text": text },
        },
    })
}

fn thought_for(message_id: &str, text: &str) -> serde_json::Value {
    json!({
        "sessionId": "agent-1",
        "update": {
            "sessionUpdate": "agent_thought_chunk",
            "messageId": message_id,
            "content": { "type": "text", "text": text },
        },
    })
}

fn child_chunk_for(message_id: &str, text: &str) -> serde_json::Value {
    json!({
        "sessionId": "agent-1",
        "update": {
            "sessionUpdate": "agent_message_chunk",
            "messageId": message_id,
            "content": { "type": "text", "text": text },
        },
        "_meta": { "peri": { "sourceAgentId": "child-1" } },
    })
}

fn tool_call(call_id: &str, input: &str) -> serde_json::Value {
    json!({
        "sessionId": "agent-1",
        "update": {
            "sessionUpdate": "tool_call",
            "toolCallId": call_id,
            "title": "read",
            "status": "pending",
            "rawInput": { "q": input },
        },
    })
}

fn tool_result(call_id: &str, output: &str) -> serde_json::Value {
    json!({
        "sessionId": "agent-1",
        "update": {
            "sessionUpdate": "tool_call_update",
            "toolCallId": call_id,
            "status": "completed",
            "rawOutput": output,
        },
    })
}

fn deltas(events: &[StreamEvent]) -> Vec<crate::domain::ItemDelta> {
    events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Delta(delta) => Some(delta.clone()),
            _ => None,
        })
        .collect()
}

fn items(events: &[StreamEvent]) -> Vec<crate::domain::ItemView> {
    events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Item(item) => Some(item.clone()),
            _ => None,
        })
        .collect()
}

fn official_types(events: &[StreamEvent]) -> Vec<&'static str> {
    events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Official(official) => Some(official.event_type()),
            _ => None,
        })
        .collect()
}

fn drain(receiver: &mut broadcast::Receiver<StreamEvent>) -> Vec<StreamEvent> {
    let mut events = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        events.push(event);
    }
    events
}

/// ILOG-1: every chunk still reaches a live client, but only the turn terminal
/// writes a row, so one stream is one durable item carrying the full text.
#[tokio::test]
async fn chunks_broadcast_live_and_persist_once() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;

    log.begin_turn("req-1", "turn_1").await.expect("turn");
    log.ingest("session/update", &chunk("hel")).await;
    log.ingest("session/update", &chunk("lo")).await;

    let live = drain(&mut events);
    let live_deltas = deltas(&live);
    assert_eq!(
        live_deltas.len(),
        2,
        "every chunk is delivered live: {live:?}"
    );
    assert!(live_deltas
        .iter()
        .all(|delta| delta.item_id == live_deltas[0].item_id));
    assert_eq!(live_deltas[0].payload["role"], "assistant");
    assert_eq!(live_deltas[0].payload["text"], "hel");
    assert_eq!(live_deltas[1].payload["text"], "lo");
    assert!(live_deltas
        .iter()
        .all(|delta| delta.payload["delta"] == json!(true)));
    assert!(
        live.iter().all(|event| event.item_seq().is_none()),
        "a live chunk is not persisted, so it cannot advance the cursor: {live:?}"
    );
    assert_eq!(
        store.swarm_last_seq("agent-1").await.expect("seq"),
        0,
        "no chunk row is written"
    );
    assert!(store
        .swarm_items_after("agent-1", 0, 10)
        .await
        .expect("rows")
        .is_empty());

    log.finish_turn("req-1", TurnOutcome::Completed("end_turn".into()))
        .await;

    let seen = drain(&mut events);
    let written = items(&seen);
    assert_eq!(written.len(), 1, "one merged row per stream per turn");
    assert_eq!(written[0].payload["role"], "assistant");
    assert_eq!(written[0].payload["text"], "hello");
    assert_eq!(written[0].payload["complete"], true);
    assert_eq!(written[0].seq, 1);
    assert_eq!(
        written[0].item_id, live_deltas[0].item_id,
        "live deltas and the merged row share one item identity"
    );

    let rows = store
        .swarm_items_after("agent-1", 0, 10)
        .await
        .expect("rows");
    assert_eq!(rows.len(), 1, "exactly one stored row: {rows:?}");
    assert_eq!(rows[0].payload["text"], json!("hello"));
    assert_eq!(rows[0].payload["complete"], json!(true));
    assert!(rows[0].payload.get("delta").is_none());
    assert_eq!(store.swarm_last_seq("agent-1").await.expect("seq"), 1);

    // Ordering contract: the completion item is persisted before the turn
    // terminal is announced. The official terminal frames follow it.
    let terminal = seen
        .iter()
        .position(|event| matches!(event, StreamEvent::Turn(TurnEvent::Completed { .. })))
        .expect("turn completed");
    assert_eq!(terminal, seen.len() - 3);
    assert!(matches!(
        &seen[terminal + 1],
        StreamEvent::Official(event) if event.event_type() == "agent.session.turn.completed"
    ));
    assert!(matches!(
        &seen[terminal + 2],
        StreamEvent::Official(event) if event.event_type() == "agent.session.idle"
    ));

    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Idle);
    assert_eq!(row.last_item_seq, 1);
}

#[tokio::test]
async fn a_batched_burst_keeps_host_order_and_consecutive_seqs() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;

    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);
    log.ingest_batch(vec![
        ("session/update".to_owned(), tool_call("call-1", "read a")),
        ("session/update".to_owned(), chunk("he")),
        ("session/update".to_owned(), chunk("llo")),
        ("session/update".to_owned(), tool_result("call-1", "done")),
    ])
    .await;

    let seen = drain(&mut events);
    let written = items(&seen);
    assert_eq!(
        written.len(),
        2,
        "tool frames are one row each and chunks produce no row: {seen:?}"
    );
    assert_eq!(written[0].seq + 1, written[1].seq, "the run is contiguous");
    assert_eq!(written[0].kind, ItemKind::ToolCall);
    assert_eq!(written[1].kind, ItemKind::ToolResult);
    assert_eq!(
        store.swarm_last_seq("agent-1").await.expect("seq"),
        2,
        "only durable items consume seqs"
    );
    // The chunks between the tool rows are still delivered, in host order.
    let live_deltas = deltas(&seen);
    assert_eq!(live_deltas.len(), 2);
    let kinds: Vec<&str> = seen
        .iter()
        .filter(|event| !matches!(event, StreamEvent::Official(_)))
        .map(|event| match event {
            StreamEvent::Item(_) => "item",
            StreamEvent::Delta(_) => "delta",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["item", "delta", "delta", "item"], "{seen:?}");

    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Running);
    assert_eq!(row.last_item_seq, 2, "the cursor moves with the batch");

    // The turn's completion still flushes the whole streamed text under the
    // stable identity, as the next item.
    log.finish_turn("req-1", TurnOutcome::Completed("end_turn".into()))
        .await;
    let completion = items(&drain(&mut events))
        .into_iter()
        .find(|item| item.payload["complete"] == true)
        .expect("completion item");
    assert_eq!(completion.item_id, live_deltas[0].item_id);
    assert_eq!(completion.payload["text"], "hello");
    assert_eq!(completion.seq, 3);
}

#[tokio::test]
async fn user_input_is_one_frame_with_a_stable_identity_and_advances_the_tail() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;

    let input_id = "0192f0c5-0000-7000-8000-00000000abcd";
    let (seq, item_id) = log
        .record_user_input(Some(input_id), "hello")
        .await
        .expect("user item");
    assert_eq!(seq, 1);
    assert_eq!(item_id, format!("itim_input_{input_id}"));

    let seen = drain(&mut events);
    assert_eq!(seen.len(), 1, "the user message is a single frame");
    let StreamEvent::Item(item) = &seen[0] else {
        panic!("expected the user item, got {:?}", seen[0]);
    };
    assert_eq!(item.kind, ItemKind::Message);
    assert_eq!(item.seq, 1);
    assert_eq!(item.item_id, format!("itim_input_{input_id}"));
    assert_eq!(item.id, item.item_id);
    assert_eq!(item.source.agent_id, "root");
    assert_eq!(item.payload["role"], json!("user"));
    assert_eq!(item.payload["text"], json!("hello"));
    assert_eq!(item.payload["inputId"], json!(input_id));
    assert!(
        item.payload.get("complete").is_none(),
        "a user frame must not terminate an assistant replay: {:?}",
        item.payload
    );
    assert_eq!(
        log.tail().load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the shared tail tracks the appended seq"
    );

    // The host's `user_message_chunk` echo is dropped by the projection, so a
    // turn must not mint a second user row. The turn id is the one derived from
    // the accepted user item.
    let turn_id = turn_id_for_item(&item_id);
    log.begin_turn("req-user", &turn_id).await.expect("turn");
    log.ingest(
        "session/update",
        &json!({
            "update": {
                "sessionUpdate": "user_message_chunk",
                "content": { "type": "text", "text": "hello" },
            },
        }),
    )
    .await;
    log.finish_turn("req-user", TurnOutcome::Completed("end_turn".into()))
        .await;
    let rows = store
        .swarm_items_after("agent-1", 0, 10)
        .await
        .expect("items");
    assert_eq!(rows.len(), 1, "no duplicate user frame: {rows:?}");
}

#[tokio::test]
async fn terminal_for_another_turn_is_ignored() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;

    log.begin_turn("req-1", "turn_1").await.expect("turn");
    log.finish_turn("req-2", TurnOutcome::Completed("end_turn".into()))
        .await;

    let seen = drain(&mut events);
    assert!(
        !seen
            .iter()
            .any(|event| matches!(event, StreamEvent::Turn(TurnEvent::Completed { .. }))),
        "a stale terminal must not close the active turn"
    );
    assert_eq!(log.active_turn().as_deref(), Some("req-1"));
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Running);
}

#[tokio::test]
async fn agent_event_done_terminal_is_the_active_turn() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    log.ingest(
        "peri/agent_event_done",
        &json!({ "sessionId": "agent-1", "stopReason": "cancelled", "requestId": "req-1" }),
    )
    .await;

    let seen = drain(&mut events);
    assert!(seen.iter().any(|event| matches!(
        event,
        StreamEvent::Turn(TurnEvent::Completed { stop_reason, .. }) if stop_reason == "cancelled"
    )));
    assert!(log.active_turn().is_none());

    // Official vocabulary: a cancelled attempt is `turn.completed` with
    // `status:"cancelled"`, and `turns.retrieve` must report the same status
    // instead of inventing a successful completion.
    assert!(seen.iter().any(|event| matches!(
        event,
        StreamEvent::Official(crate::domain::OfficialEvent::TurnCompleted { turn, .. })
            if turn.status == "cancelled"
    )));
    let row = store
        .swarm_get_turn("agent-1", "turn_1")
        .await
        .expect("read turn")
        .expect("turn row");
    assert_eq!(row.status, crate::domain::TurnStatus::Cancelled);
    assert!(row.completed_at.is_some());

    // `turns.list`/`turns.retrieve` read the same row, so both report the
    // official status instead of `completed`.
    let page = crate::service::list_turns_from_store(
        store.as_ref(),
        "agent-1",
        &crate::domain::TurnsWindow {
            order: crate::domain::ItemOrder::Desc,
            after: None,
            limit: 10,
        },
    )
    .await
    .expect("turns.list");
    assert_eq!(page.data.len(), 1);
    assert_eq!(page.data[0].status, "cancelled");
    let retrieved = crate::service::get_turn_from_store(store.as_ref(), "agent-1", "turn_1")
        .await
        .expect("turns.retrieve");
    assert_eq!(retrieved.status, "cancelled");
}

#[tokio::test]
async fn idle_frame_carries_the_persisted_metadata_and_prompt_cache_options() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    let mut row = session_row();
    row.metadata = [("team".to_owned(), "alpha".to_owned())].into();
    row.prompt_cache_options = Some(PromptCacheOptions::new(PromptCacheTtl::TwelveHours));
    store.swarm_upsert_session(row).await.expect("session row");

    log.begin_turn("req-1", "turn_1").await.expect("turn");
    log.finish_turn("req-1", TurnOutcome::Completed("end_turn".into()))
        .await;

    let seen = drain(&mut events);
    let idle = seen
        .iter()
        .find_map(|event| match event {
            StreamEvent::Official(OfficialEvent::SessionIdle { session, .. }) => Some(session),
            _ => None,
        })
        .expect("agent.session.idle");
    assert_eq!(
        idle.metadata,
        [("team".to_owned(), "alpha".to_owned())].into()
    );
    assert_eq!(
        idle.prompt_cache_options,
        Some(PromptCacheOptions::new(PromptCacheTtl::TwelveHours))
    );
}

#[tokio::test]
async fn open_turn_is_visible_to_close_waiters() {
    let Fixture { log, .. } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    // After a steer hands the turn to a queued run, the prompt caller waits for
    // that run to close the turn. A waiter that reads the open turn as already
    // closed closes it with the attempt's `cancelled` terminal instead.
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(200),
            log.wait_turn_closed("req-1", std::time::Duration::from_secs(30)),
        )
        .await
        .is_err(),
        "an open turn must keep close waiters blocked"
    );
    log.finish_turn("req-1", TurnOutcome::Completed("end_turn".into()))
        .await;
    assert!(
        log.wait_turn_closed("req-1", std::time::Duration::from_secs(1))
            .await
    );
}

#[tokio::test]
async fn replay_frames_are_not_persisted() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    log.ingest(
        "session/update",
        &json!({
            "sessionId": "agent-1",
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "messageId": "msg-old",
                "content": { "type": "text", "text": "history" },
                "_meta": { "periReplay": true },
            },
        }),
    )
    .await;
    log.finish_turn("req-1", TurnOutcome::Completed("end_turn".into()))
        .await;

    let seen = drain(&mut events);
    assert!(
        !seen
            .iter()
            .any(|event| matches!(event, StreamEvent::Item(_))),
        "replayed history must not become Swarm items"
    );
    assert_eq!(store.swarm_last_seq("agent-1").await.expect("seq"), 0);
}

#[tokio::test]
async fn child_receipts_keep_their_own_source_and_identity() {
    let Fixture {
        log, mut events, ..
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    log.ingest(
        "peri/agent_event",
        &json!({
            "sessionId": "agent-1",
            "event_json": json!({
                "type": "subagent_stopped",
                "value": {
                    "agent_name": "explorer",
                    "result": "done",
                    "is_error": false,
                    "instance_id": "child-1",
                },
            })
            .to_string(),
        }),
    )
    .await;

    let seen = drain(&mut events);
    let item = seen
        .iter()
        .find_map(|event| match event {
            StreamEvent::Item(item) if item.kind == ItemKind::Receipt => Some(item),
            _ => None,
        })
        .expect("receipt item");
    assert_eq!(item.source.agent_id, "child:child-1");
}

#[tokio::test]
async fn thought_streams_flush_as_thoughts() {
    let Fixture {
        log, mut events, ..
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    log.ingest(
        "session/update",
        &json!({
            "sessionId": "agent-1",
            "update": {
                "sessionUpdate": "agent_thought_chunk",
                "messageId": "msg-t",
                "content": { "type": "text", "text": "why" },
            },
        }),
    )
    .await;
    log.finish_turn("req-1", TurnOutcome::Completed("end_turn".into()))
        .await;

    let seen = drain(&mut events);
    let written = items(&seen);
    assert_eq!(written.len(), 1, "one row for the thought stream");
    assert_eq!(written[0].kind, ItemKind::Thought);
    assert_eq!(written[0].payload["text"], "why");
    assert_eq!(written[0].payload["complete"], true);
    assert_eq!(deltas(&seen).len(), 1, "the chunk still streams live");
    assert_eq!(deltas(&seen)[0].kind, ItemKind::Thought);
}

#[tokio::test]
async fn live_failed_turn_carries_the_failure_text() {
    let Fixture {
        log, mut events, ..
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    log.finish_turn(
        "req-1",
        TurnOutcome::Failed {
            code: -32603,
            message: "model refused".to_owned(),
        },
    )
    .await;

    let seen = drain(&mut events);
    let failed_turn = seen
        .iter()
        .find_map(|event| match event {
            StreamEvent::Official(crate::domain::OfficialEvent::TurnFailed { turn, .. }) => {
                Some(turn)
            }
            _ => None,
        })
        .expect("turn.failed frame");
    assert_eq!(
        failed_turn.error,
        Some(json!({ "code": "internal_error", "message": "model refused" }))
    );
    let failed_session = seen
        .iter()
        .find_map(|event| match event {
            StreamEvent::Official(crate::domain::OfficialEvent::SessionFailed {
                session, ..
            }) => Some(session),
            _ => None,
        })
        .expect("session.failed frame");
    assert_eq!(failed_session.error.as_deref(), Some("model refused"));
}

#[tokio::test]
async fn steer_user_frame_keeps_the_open_turn_id() {
    let Fixture {
        log, mut events, ..
    } = fixture().await;
    log.begin_turn("req-1", "turn_input_x").await.expect("turn");
    drain(&mut events);

    // A user input accepted while the turn is open is a steer: the live frame
    // must name the open turn, which is exactly the turn id replay recovers
    // from the opener before the cursor.
    log.record_user_input(Some("steer-1"), "adjust")
        .await
        .expect("steer item");

    let seen = drain(&mut events);
    let added: Vec<_> = seen
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Official(crate::domain::OfficialEvent::TurnItemAdded {
                turn_id,
                item,
                ..
            }) => Some((turn_id.clone(), item.role.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        added,
        [(Some("turn_input_x".to_owned()), "user".to_owned())],
        "a steer keeps the live turn id: {seen:?}"
    );
    assert!(
        !seen.iter().any(|event| matches!(
            event,
            StreamEvent::Official(crate::domain::OfficialEvent::TurnCreated { .. })
        )),
        "a steer does not open a second turn"
    );
}

#[tokio::test]
async fn turn_start_announces_progress_before_the_user_item() {
    let Fixture {
        log, mut events, ..
    } = fixture().await;
    let (_, item_id) = log
        .record_user_input(Some("input-1"), "hello")
        .await
        .expect("user item");
    let turn_id = turn_id_for_item(&item_id);
    log.begin_turn("req-1", &turn_id).await.expect("turn");

    let seen = drain(&mut events);
    assert_eq!(
        official_types(&seen),
        [
            "agent.session.in_progress",
            "agent.session.turn.created",
            "agent.session.turn.in_progress",
            "agent.session.turn.item.added",
        ],
        "{seen:?}"
    );
    let ids: Vec<_> = seen
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Official(official) => Some(official.event_id()),
            _ => None,
        })
        .collect();
    assert_eq!(
        ids,
        [
            format!("evt_{turn_id}_session_in_progress"),
            format!("evt_{turn_id}_created"),
            format!("evt_{turn_id}_in_progress"),
            "evt_item_1_added".to_owned(),
        ]
    );
    // The custom frames keep their meaning: the accepted item, then the turn.
    assert!(matches!(&seen[0], StreamEvent::Item(_)));
    assert!(matches!(
        seen.last(),
        Some(StreamEvent::Turn(TurnEvent::Started { .. }))
    ));
}

#[tokio::test]
async fn assistant_completion_reports_item_parts_and_final_phase() {
    let Fixture {
        log, mut events, ..
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);
    log.ingest("session/update", &chunk("hel")).await;
    log.ingest("session/update", &chunk("lo")).await;
    log.finish_turn("req-1", TurnOutcome::Completed("end_turn".into()))
        .await;

    let seen = drain(&mut events);
    assert_eq!(
        official_types(&seen),
        [
            "agent.session.turn.item.added",
            "agent.session.turn.content_part.added",
            "agent.session.turn.output_text.delta",
            "agent.session.turn.output_text.delta",
            "agent.session.turn.output_text.done",
            "agent.session.turn.content_part.done",
            "agent.session.turn.item.done",
            "agent.session.turn.completed",
            "agent.session.idle",
        ],
        "{seen:?}"
    );
    let (completion_seq, completion_id) = seen
        .iter()
        .find_map(|event| match event {
            StreamEvent::Item(item) if item.payload["complete"] == json!(true) => {
                Some((item.seq, item.item_id.clone()))
            }
            _ => None,
        })
        .expect("completion item");
    let done = seen
        .iter()
        .find_map(|event| match event {
            StreamEvent::Official(OfficialEvent::TurnItemDone {
                output_index, item, ..
            }) => Some((*output_index, item)),
            _ => None,
        })
        .expect("item.done");
    assert_eq!(done.0, 0);
    assert_eq!(done.1.status, "completed");
    assert_eq!(done.1.phase.as_deref(), Some("final_answer"));
    assert_eq!(done.1.turn_id, "turn_1");
    assert_eq!(
        done.1.content,
        [crate::domain::OfficialContent::OutputText {
            text: "hello".to_owned(),
        }]
    );

    let ids: Vec<_> = seen
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Official(official)
                if official.event_type() != "agent.session.turn.completed"
                    && official.event_type() != "agent.session.idle" =>
            {
                Some(official.event_id())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        ids,
        [
            format!("evt_item_{completion_id}_added"),
            format!("evt_item_{completion_id}_part_added"),
            format!("evt_item_{completion_id}_delta_1"),
            format!("evt_item_{completion_id}_delta_2"),
            format!("evt_item_{completion_seq}_text_done"),
            format!("evt_item_{completion_seq}_part_done"),
            format!("evt_item_{completion_seq}_done"),
        ]
    );
}

#[tokio::test]
async fn live_chunks_emit_one_official_item_and_three_deltas() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);

    let chunks = ["one", "two", "three"];
    for text in chunks {
        log.ingest("session/update", &chunk(text)).await;
    }
    let live = drain(&mut events);
    let added: Vec<_> = live
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Official(OfficialEvent::TurnItemAdded { item, .. })
                if item.role == "assistant" =>
            {
                Some(item.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(added.len(), 1, "one item introduction: {live:?}");
    let item_id = added[0].id.clone();
    let deltas: Vec<_> = live
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Official(event @ OfficialEvent::TurnOutputTextDelta { .. }) => Some(event),
            _ => None,
        })
        .collect();
    assert_eq!(deltas.len(), 3, "one official delta per chunk: {live:?}");
    for (index, event) in deltas.iter().enumerate() {
        let OfficialEvent::TurnOutputTextDelta {
            event_id,
            session_id,
            turn_id,
            item_id: delta_item,
            content_index,
            output_index,
            delta,
            ..
        } = event
        else {
            unreachable!("filtered to deltas");
        };
        assert_eq!(delta_item, &item_id);
        assert_eq!(session_id, "agent-1");
        assert_eq!(turn_id.as_deref(), Some("turn_1"));
        assert_eq!(*content_index, 0);
        assert_eq!(*output_index, 0);
        assert_eq!(delta.as_str(), chunks[index]);
        assert_eq!(
            event_id,
            &format!("evt_item_{item_id}_delta_{}", index + 1),
            "live ids are deterministic per in-process chunk"
        );
    }
    let part_added: Vec<_> = live
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Official(event @ OfficialEvent::TurnContentPartAdded { .. }) => {
                Some(event)
            }
            _ => None,
        })
        .collect();
    assert_eq!(part_added.len(), 1, "the part is added once: {live:?}");
    let OfficialEvent::TurnContentPartAdded { output_index, .. } = part_added[0] else {
        unreachable!("filtered to parts");
    };
    assert_eq!(*output_index, 0);
    assert_eq!(
        official_types(&live),
        [
            "agent.session.turn.item.added",
            "agent.session.turn.content_part.added",
            "agent.session.turn.output_text.delta",
            "agent.session.turn.output_text.delta",
            "agent.session.turn.output_text.delta",
        ],
        "the first chunk introduces the item and part; later chunks are deltas only: {live:?}"
    );
    let position =
        |predicate: &dyn Fn(&StreamEvent) -> bool| live.iter().position(predicate).expect("frame");
    let added_at = position(&|event| {
        matches!(
            event,
            StreamEvent::Official(OfficialEvent::TurnItemAdded { item, .. })
                if item.role == "assistant"
        )
    });
    let part_at = position(&|event| {
        matches!(
            event,
            StreamEvent::Official(OfficialEvent::TurnContentPartAdded { .. })
        )
    });
    let first_delta_at = position(&|event| {
        matches!(
            event,
            StreamEvent::Official(OfficialEvent::TurnOutputTextDelta { .. })
        )
    });
    assert!(added_at < part_at && part_at < first_delta_at, "{live:?}");

    log.finish_turn("req-1", TurnOutcome::Completed("end_turn".into()))
        .await;
    let completion = drain(&mut events);
    assert_eq!(
        official_types(&completion),
        [
            "agent.session.turn.output_text.done",
            "agent.session.turn.content_part.done",
            "agent.session.turn.item.done",
            "agent.session.turn.completed",
            "agent.session.idle",
        ],
        "a live-chunked item is only closed at the terminal: {completion:?}"
    );
    assert!(
        !completion.iter().any(|event| matches!(
            event,
            StreamEvent::Official(OfficialEvent::TurnItemAdded { .. })
        )),
        "the terminal must not repeat item.added: {completion:?}"
    );
    let rows = store
        .swarm_items_after("agent-1", 0, 10)
        .await
        .expect("rows");
    assert_eq!(
        rows.len(),
        1,
        "three chunks still merge into one row: {rows:?}"
    );
    assert_eq!(rows[0].kind, ItemKind::Message);
    assert_eq!(rows[0].payload["text"], json!("onetwothree"));
    assert_eq!(rows[0].payload["complete"], json!(true));
    assert_eq!(rows[0].payload["outputIndex"], json!(0));
    assert!(
        rows.iter().all(|row| row.payload.get("delta").is_none()),
        "no chunk row is persisted: {rows:?}"
    );
    let done = completion
        .iter()
        .find_map(|event| match event {
            StreamEvent::Official(OfficialEvent::TurnItemDone {
                output_index, item, ..
            }) => Some((*output_index, item)),
            _ => None,
        })
        .expect("item.done");
    assert_eq!(done.0, 0, "done reuses the live output index");
    assert_eq!(done.1.id, item_id);
}

#[tokio::test]
async fn live_deltas_do_not_change_a_cancelled_terminal() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);
    log.ingest("session/update", &chunk("sto")).await;
    log.ingest("session/update", &chunk("p")).await;
    assert_eq!(
        official_types(&drain(&mut events))
            .iter()
            .filter(|event| **event == "agent.session.turn.output_text.delta")
            .count(),
        2,
        "both chunks streamed before the cancel"
    );

    log.finish_turn("req-1", TurnOutcome::Completed("cancelled".into()))
        .await;
    let seen = drain(&mut events);
    let completed = seen
        .iter()
        .find_map(|event| match event {
            StreamEvent::Official(OfficialEvent::TurnCompleted { turn, .. }) => Some(turn),
            _ => None,
        })
        .expect("turn.completed");
    assert_eq!(completed.status, "cancelled");
    assert_eq!(
        official_types(&seen),
        [
            "agent.session.turn.output_text.done",
            "agent.session.turn.content_part.done",
            "agent.session.turn.item.done",
            "agent.session.turn.completed",
            "agent.session.idle",
        ],
        "deltas neither repeat the item nor change the cancelled terminal: {seen:?}"
    );
    assert!(
        !seen
            .iter()
            .any(|event| matches!(event, StreamEvent::Turn(TurnEvent::Failed { .. }))),
        "cancelled output must not turn into a failure: {seen:?}"
    );
    let rows = store
        .swarm_items_after("agent-1", 0, 10)
        .await
        .expect("rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].payload["text"], json!("stop"));
}

#[tokio::test]
async fn interleaved_assistant_messages_are_ordered_by_arrival() {
    let Fixture {
        log, mut events, ..
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);
    // `msg-b` opens first and `msg-a` interleaves. Message ids must not decide
    // the output order; the order the streams opened does.
    log.ingest("session/update", &chunk_for("msg-b", "first"))
        .await;
    log.ingest("session/update", &chunk_for("msg-a", "second"))
        .await;
    log.ingest("session/update", &chunk_for("msg-b", "!")).await;
    log.finish_turn("req-1", TurnOutcome::Completed("end_turn".into()))
        .await;

    let seen = drain(&mut events);
    let done: Vec<_> = seen
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Official(OfficialEvent::TurnItemDone {
                output_index, item, ..
            }) => Some((*output_index, item.phase.clone(), item.content.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        done,
        [
            (
                0,
                Some("commentary".to_owned()),
                vec![crate::domain::OfficialContent::OutputText {
                    text: "first!".to_owned(),
                }],
            ),
            (
                1,
                Some("final_answer".to_owned()),
                vec![crate::domain::OfficialContent::OutputText {
                    text: "second".to_owned(),
                }],
            ),
        ],
        "{seen:?}"
    );
}

#[tokio::test]
async fn message_and_thought_sharing_a_message_id_stay_separate() {
    // Peri assigns reasoning and answer chunks of one model call the same
    // `messageId`. Each combination must keep them in separate streams and only
    // project the answer.
    for (name, thought_first, has_thought, has_text) in [
        ("thought then text", true, true, true),
        ("text then thought", false, true, true),
        ("only thought", true, true, false),
        ("only text", false, false, true),
    ] {
        let Fixture {
            log,
            store,
            mut events,
        } = fixture().await;
        log.begin_turn("req-1", "turn_1").await.expect("turn");
        drain(&mut events);

        let thought = thought_for("msg-shared", "reasoning");
        let text = chunk_for("msg-shared", "answer");
        match (has_thought, has_text, thought_first) {
            (true, true, true) => {
                log.ingest("session/update", &thought).await;
                log.ingest("session/update", &text).await;
            }
            (true, true, false) => {
                log.ingest("session/update", &text).await;
                log.ingest("session/update", &thought).await;
            }
            (true, false, _) => log.ingest("session/update", &thought).await,
            (false, true, _) => log.ingest("session/update", &text).await,
            (false, false, _) => {}
        }
        log.finish_turn("req-1", TurnOutcome::Completed("end_turn".into()))
            .await;

        let seen = drain(&mut events);
        let done: Vec<_> = seen
            .iter()
            .filter_map(|event| match event {
                StreamEvent::Official(OfficialEvent::TurnItemDone {
                    output_index, item, ..
                }) => Some((*output_index, item)),
                _ => None,
            })
            .collect();
        if has_text {
            assert_eq!(done.len(), 1, "{name}: {seen:?}");
            assert_eq!(done[0].0, 0, "{name}");
            assert_eq!(
                done[0].1.phase.as_deref(),
                Some("final_answer"),
                "{name}: {seen:?}"
            );
            assert_eq!(
                done[0].1.content,
                [crate::domain::OfficialContent::OutputText {
                    text: "answer".to_owned(),
                }],
                "{name}: the answer must not contain reasoning"
            );
        } else {
            assert!(
                done.is_empty(),
                "{name}: a thought-only turn has no official item: {seen:?}"
            );
            assert!(
                !seen.iter().any(|event| matches!(
                    event,
                    StreamEvent::Official(OfficialEvent::TurnItemAdded { .. })
                )),
                "{name}: a thought-only turn has no official item.added"
            );
        }

        let rows = store
            .swarm_items_after("agent-1", 0, 100)
            .await
            .expect("items");
        let completes: Vec<_> = rows
            .iter()
            .filter(|row| row.payload.get("complete") == Some(&json!(true)))
            .collect();
        let message_completes: Vec<_> = completes
            .iter()
            .filter(|row| row.kind == ItemKind::Message)
            .collect();
        let thought_completes: Vec<_> = completes
            .iter()
            .filter(|row| row.kind == ItemKind::Thought)
            .collect();

        assert_eq!(
            message_completes.len(),
            usize::from(has_text),
            "{name}: one message completion"
        );
        if let Some(row) = message_completes.first() {
            assert_eq!(row.payload["text"], json!("answer"), "{name}");
            assert_eq!(row.payload["phase"], json!("final_answer"), "{name}");
            assert_eq!(row.payload["outputIndex"], json!(0), "{name}");
        }
        assert_eq!(
            thought_completes.len(),
            usize::from(has_thought),
            "{name}: one thought completion"
        );
        if let Some(row) = thought_completes.first() {
            assert_eq!(row.payload["text"], json!("reasoning"), "{name}");
            assert!(
                row.payload.get("phase").is_none(),
                "{name}: thought carries no phase"
            );
            assert!(
                row.payload.get("outputIndex").is_none(),
                "{name}: thought carries no outputIndex"
            );
        }
        if let (Some(message), Some(thought)) =
            (message_completes.first(), thought_completes.first())
        {
            assert_ne!(
                message.item_id, thought.item_id,
                "{name}: answer and reasoning need distinct item ids"
            );
        }
    }
}

#[tokio::test]
async fn a_late_thought_stream_does_not_steal_the_final_answer() {
    let Fixture {
        log, mut events, ..
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);
    log.ingest("session/update", &chunk_for("msg-answer", "answer"))
        .await;
    // A later reasoning chunk has its own message id, so only the output-kind
    // filter (not the shared-id split) can keep it from becoming final.
    log.ingest(
        "session/update",
        &thought_for("msg-thought", "late reasoning"),
    )
    .await;
    log.finish_turn("req-1", TurnOutcome::Completed("end_turn".into()))
        .await;

    let seen = drain(&mut events);
    let done: Vec<_> = seen
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Official(OfficialEvent::TurnItemDone {
                output_index, item, ..
            }) => Some((*output_index, item)),
            _ => None,
        })
        .collect();
    assert_eq!(done.len(), 1, "{seen:?}");
    assert_eq!(done[0].0, 0);
    assert_eq!(done[0].1.phase.as_deref(), Some("final_answer"));
    assert_eq!(
        done[0].1.content,
        [crate::domain::OfficialContent::OutputText {
            text: "answer".to_owned(),
        }]
    );
}

#[tokio::test]
async fn child_messages_never_become_official_items_or_output_indices() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);
    // The child chunk arrives first: it must neither claim output 0 nor become
    // the final answer.
    log.ingest(
        "session/update",
        &child_chunk_for("msg-child", "child answer"),
    )
    .await;
    log.ingest("session/update", &chunk_for("msg-root-1", "first answer"))
        .await;
    log.ingest("session/update", &chunk_for("msg-root-2", "second answer"))
        .await;
    log.finish_turn("req-1", TurnOutcome::Completed("end_turn".into()))
        .await;

    let seen = drain(&mut events);
    let done: Vec<_> = seen
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Official(OfficialEvent::TurnItemDone {
                output_index, item, ..
            }) => Some((*output_index, item.phase.clone(), item.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(done.len(), 2, "{seen:?}");
    assert_eq!(done[0].0, 0);
    assert_eq!(done[0].1.as_deref(), Some("commentary"));
    assert_eq!(
        done[0].2.content,
        [crate::domain::OfficialContent::OutputText {
            text: "first answer".to_owned(),
        }]
    );
    assert_eq!(done[1].0, 1);
    assert_eq!(done[1].1.as_deref(), Some("final_answer"));
    assert_eq!(
        done[1].2.content,
        [crate::domain::OfficialContent::OutputText {
            text: "second answer".to_owned(),
        }]
    );

    let rows = store
        .swarm_items_after("agent-1", 0, 100)
        .await
        .expect("items");
    let child_complete = rows
        .iter()
        .find(|row| {
            row.kind == ItemKind::Message
                && row.payload.get("complete") == Some(&json!(true))
                && !row.source.is_root()
        })
        .expect("child completion row");
    assert_eq!(child_complete.source.agent_id, "child:child-1");
    assert_eq!(child_complete.payload["text"], json!("child answer"));
    assert!(
        child_complete.payload.get("phase").is_none(),
        "child rows carry no phase"
    );
    assert!(
        child_complete.payload.get("outputIndex").is_none(),
        "child rows carry no outputIndex"
    );
    assert!(
        done.iter()
            .all(|(_, _, item)| item.id != child_complete.item_id),
        "child item must not appear in official item.done"
    );
    assert!(
        !seen.iter().any(|event| matches!(
            event,
            StreamEvent::Official(OfficialEvent::TurnItemAdded { item, .. })
                if item.id == child_complete.item_id
        )),
        "child item must not appear in official item.added"
    );
}

/// ILOG-1: a turn with interleaved messages, reasoning, tool traffic and both
/// approval facts writes exactly one row per item, in the order the streams
/// opened. Only root assistant messages carry `phase`/`outputIndex`.
#[tokio::test]
async fn one_row_per_stream_per_turn() {
    let mut spec = McpToolSpec::new("server-a", "https://mcp.example.test/mcp");
    spec.require_approval = Some(json!("always"));
    let tools = vec![spec];
    let store: Arc<dyn SwarmStore> = Arc::new(MemorySwarmStore::new());
    store
        .swarm_upsert_session(SwarmSessionRow {
            mcp_tools: tools.clone(),
            ..session_row()
        })
        .await
        .expect("session row");
    let (channel, mut events) = broadcast::channel(64);
    let log = Arc::new(
        ItemLog::new(
            "agent-1",
            Arc::clone(&store),
            channel,
            Arc::new(AtomicU64::new(0)),
        )
        .with_approval_declarations(&tools),
    );

    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);
    log.ingest("session/update", &chunk_for("msg-b", "first"))
        .await;
    log.ingest("session/update", &chunk_for("msg-a", "second"))
        .await;
    log.ingest("session/update", &thought_for("msg-t", "why"))
        .await;
    log.ingest("session/update", &tool_call("call-1", "read a"))
        .await;
    log.ingest("session/update", &tool_result("call-1", "done"))
        .await;
    let start = log
        .begin_permission(
            &json!(7),
            &json!({
                "_meta": { "peri.mcp": { "server": "server-a", "tool": "search" } },
                "toolCall": {
                    "toolCallId": "call-1",
                    "title": "search",
                    "rawInput": { "q": "x" },
                },
            }),
        )
        .await
        .expect("permission recorded");
    assert_eq!(start, PermissionStart::Pending);
    let request_id = log.required_actions()[0]["request_id"]
        .as_str()
        .expect("request id")
        .to_owned();
    log.respond_approval(
        &request_id,
        ApprovalDecision::Approve,
        Some("input-1".into()),
    )
    .await;

    // No chunk is persisted; these four rows are the tool and approval items.
    let before_terminal = store
        .swarm_items_after("agent-1", 0, 10)
        .await
        .expect("rows");
    assert_eq!(
        before_terminal.len(),
        4,
        "tool_call, tool_result and both approval facts: {before_terminal:?}"
    );
    assert!(
        before_terminal
            .iter()
            .all(|row| row.payload.get("delta").is_none()),
        "no chunk is persisted"
    );

    log.finish_turn("req-1", TurnOutcome::Completed("end_turn".into()))
        .await;

    let rows = store
        .swarm_items_after("agent-1", 0, 10)
        .await
        .expect("rows");
    assert_eq!(
        rows.len(),
        7,
        "four item rows plus two merged assistant rows and one thought: {rows:?}"
    );
    let messages: Vec<_> = rows
        .iter()
        .filter(|row| row.kind == ItemKind::Message && row.payload["role"] == json!("assistant"))
        .collect();
    assert_eq!(messages.len(), 2, "one merged row per message stream");
    assert_eq!(messages[0].payload["text"], json!("first"));
    assert_eq!(messages[0].payload["phase"], json!("commentary"));
    assert_eq!(messages[0].payload["outputIndex"], json!(0));
    assert_eq!(messages[1].payload["text"], json!("second"));
    assert_eq!(messages[1].payload["phase"], json!("final_answer"));
    assert_eq!(messages[1].payload["outputIndex"], json!(1));
    let thought = rows
        .iter()
        .find(|row| row.kind == ItemKind::Thought)
        .expect("thought row");
    assert_eq!(thought.payload["text"], json!("why"));
    assert!(
        thought.payload.get("phase").is_none(),
        "a thought is not an official output"
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row.kind == ItemKind::Approval)
            .count(),
        2,
        "the pending fact and the settlement are each one row"
    );
    // Seq stays the ordering cursor: rows are contiguous and strictly increasing.
    let seqs: Vec<u64> = rows.iter().map(|row| row.seq).collect();
    assert!(
        seqs.windows(2).all(|pair| pair[1] == pair[0] + 1),
        "{seqs:?}"
    );
}

/// A chunk that arrives after the turn terminal must not extend the merged row
/// or open a stream for the next turn's flush.
#[tokio::test]
async fn a_late_chunk_after_the_terminal_is_dropped() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    log.ingest("session/update", &chunk("hi")).await;
    log.finish_turn("req-1", TurnOutcome::Completed("end_turn".into()))
        .await;
    let merged = items(&drain(&mut events));
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].payload["text"], json!("hi"));

    log.ingest("session/update", &chunk("late")).await;

    assert!(
        drain(&mut events).is_empty(),
        "a chunk after the terminal is neither stored nor broadcast"
    );
    let rows = store
        .swarm_items_after("agent-1", 0, 10)
        .await
        .expect("rows");
    assert_eq!(
        rows.len(),
        1,
        "the durable text stays the terminal's: {rows:?}"
    );
    assert_eq!(rows[0].payload["text"], json!("hi"));
    assert_eq!(store.swarm_last_seq("agent-1").await.expect("seq"), 1);
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(
        row.state,
        SessionState::Idle,
        "a late chunk must not reopen the session"
    );
}

/// Recovered root output has no gateway turn: its buffered text is still
/// written as one merged row, without official frames or phase metadata.
#[tokio::test]
async fn recovered_root_flushes_one_merged_row_without_official_frames() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.ingest("session/update", &chunk("hel")).await;
    log.ingest("session/update", &chunk("lo")).await;
    let buffered = drain(&mut events);
    assert_eq!(deltas(&buffered).len(), 2, "chunks stream live");
    assert!(
        official_types(&buffered).is_empty(),
        "recovered chunks have no turn, so no official added/delta frame: {buffered:?}"
    );

    log.finish_recovered_root().await;

    let seen = drain(&mut events);
    let written = items(&seen);
    assert_eq!(written.len(), 1, "one merged row for the recovered stream");
    assert_eq!(written[0].payload["text"], json!("hello"));
    assert_eq!(written[0].payload["complete"], json!(true));
    assert_eq!(
        written[0].payload["recovered"],
        json!(true),
        "recovered text is marked as belonging to no turn: {:?}",
        written[0].payload
    );
    assert!(
        written[0].payload.get("phase").is_none(),
        "recovered text belongs to no turn: {:?}",
        written[0].payload
    );
    assert!(written[0].payload.get("outputIndex").is_none());
    assert!(
        official_types(&seen).is_empty(),
        "no turn is open, so no official frame may name one: {seen:?}"
    );
    let rows = store
        .swarm_items_after("agent-1", 0, 10)
        .await
        .expect("rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].payload["text"], json!("hello"));
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Idle);
}

/// A merged payload over the store budget keeps its identity and cursor but is
/// stored as the truncation marker; live clients still saw every chunk.
#[tokio::test]
async fn an_oversized_stream_is_one_truncated_row() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    let huge = "x".repeat(crate::domain::item::MAX_PAYLOAD_BYTES + 1);
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    log.ingest("session/update", &chunk(&huge)).await;
    let live = deltas(&drain(&mut events));
    assert_eq!(live.len(), 1);
    assert_eq!(
        live[0].payload["text"].as_str().map(str::len),
        Some(huge.len()),
        "the live chunk is not truncated"
    );

    log.finish_turn("req-1", TurnOutcome::Completed("end_turn".into()))
        .await;

    let rows = store
        .swarm_items_after("agent-1", 0, 10)
        .await
        .expect("rows");
    assert_eq!(rows.len(), 1, "the stream is one row even when truncated");
    assert!(
        rows[0].truncated,
        "the merged row exceeds the payload budget"
    );
    assert_eq!(rows[0].payload["_truncated"], json!(true));
    assert!(
        rows[0].payload["originalBytes"]
            .as_u64()
            .expect("originalBytes")
            > u64::try_from(crate::domain::item::MAX_PAYLOAD_BYTES).expect("budget"),
        "the marker reports the oversized payload: {:?}",
        rows[0].payload
    );
    assert_eq!(rows[0].seq, 1);
    assert_eq!(store.swarm_last_seq("agent-1").await.expect("seq"), 1);
}

/// ILOG-7: a slow consumer loses events (including live deltas), is told once
/// with `gap`, and recovers by resuming from the last persisted item seq.
#[tokio::test]
async fn a_lagged_subscriber_can_resume_from_the_last_item() {
    let store: Arc<dyn SwarmStore> = Arc::new(MemorySwarmStore::new());
    store
        .swarm_upsert_session(session_row())
        .await
        .expect("session row");
    // Capacity 2 plus a receiver that does not read: the third broadcast below
    // is dropped from the channel.
    let (channel, mut events) = broadcast::channel(2);
    let log = Arc::new(ItemLog::new(
        "agent-1",
        Arc::clone(&store),
        channel,
        Arc::new(AtomicU64::new(0)),
    ));
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    // Consume the turn-start frames (a lagged `try_recv` still advances the
    // cursor) so the next three broadcasts are the ones the subscriber misses.
    while let Ok(_) | Err(broadcast::error::TryRecvError::Lagged(_)) = events.try_recv() {}

    log.ingest("session/update", &chunk("hel")).await; // delta + 3 official
    log.ingest("session/update", &tool_call("call-1", "read"))
        .await; // item seq 1
    log.ingest("session/update", &chunk("lo")).await; // delta + 1 official

    let skipped = loop {
        match events.recv().await {
            Ok(_) => continue,
            Err(broadcast::error::RecvError::Lagged(count)) => break count,
            Err(broadcast::error::RecvError::Closed) => panic!("channel closed before lag"),
        }
    };
    assert_eq!(
        skipped, 5,
        "the first chunk's custom item and official introduction, the persisted item, \
         and the next two events were dropped: non-persisted frames share the channel"
    );
    // The SSE writer ends here: one gap frame names the dropped upper bound.
    // Non-persisted official/delta frames inflate the count, so `toSeq` may
    // exceed the session cursor; the durable item stays readable.
    let gap = crate::event_stream::FrameWriter::new(0).gap(skipped);
    assert!(gap.starts_with("event: gap"), "{gap}");
    assert_eq!(
        gap, "event: gap\ndata: {\"fromSeq\":1,\"toSeq\":5}\n\n",
        "the gap starts after the last delivered item seq"
    );

    log.finish_turn("req-1", TurnOutcome::Completed("end_turn".into()))
        .await;
    // Reconnect from the last item the slow consumer saw: only the deltas were
    // lost; the merged row is still readable.
    let resumed = store
        .swarm_items_after("agent-1", 1, 10)
        .await
        .expect("rows");
    assert_eq!(resumed.len(), 1, "the complete row is after the last item");
    assert_eq!(resumed[0].payload["text"], json!("hello"));
    assert_eq!(resumed[0].payload["complete"], json!(true));
}

/// ILOG-6: a single turn emits the fixed official family in order; no chunk
/// adds or removes an official frame.
#[tokio::test]
async fn a_single_turn_keeps_the_official_sequence() {
    let Fixture {
        log, mut events, ..
    } = fixture().await;
    let (_, user_item_id) = log
        .record_user_input(Some("input-1"), "hello?")
        .await
        .expect("user item");
    // The real turn id is derived from the accepted user item; the deferred
    // `turn.created`/`item.added` frames wait for that turn to start.
    log.begin_turn("req-1", &turn_id_for_item(&user_item_id))
        .await
        .expect("turn");
    log.ingest("session/update", &chunk("hel")).await;
    log.ingest("session/update", &chunk("lo")).await;
    log.finish_turn("req-1", TurnOutcome::Completed("end_turn".into()))
        .await;

    let seen = drain(&mut events);
    assert_eq!(
        official_types(&seen),
        [
            "agent.session.in_progress",
            "agent.session.turn.created",
            "agent.session.turn.in_progress",
            "agent.session.turn.item.added",
            "agent.session.turn.item.added",
            "agent.session.turn.content_part.added",
            "agent.session.turn.output_text.delta",
            "agent.session.turn.output_text.delta",
            "agent.session.turn.output_text.done",
            "agent.session.turn.content_part.done",
            "agent.session.turn.item.done",
            "agent.session.turn.completed",
            "agent.session.idle",
        ],
        "{seen:?}"
    );
    let user_ids: Vec<_> = seen
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Official(OfficialEvent::TurnItemAdded { item, .. })
                if item.role == "user" =>
            {
                Some(item.id.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(user_ids, [user_item_id]);
}

#[tokio::test]
async fn failed_turn_reports_error_between_turn_and_session_failure() {
    let Fixture {
        log, mut events, ..
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);
    log.finish_turn(
        "req-1",
        TurnOutcome::Failed {
            code: -32603,
            message: "model refused".to_owned(),
        },
    )
    .await;

    let seen = drain(&mut events);
    assert_eq!(
        official_types(&seen),
        ["agent.session.turn.failed", "error", "agent.session.failed",],
        "{seen:?}"
    );
    let error = seen
        .iter()
        .find_map(|event| match event {
            StreamEvent::Official(OfficialEvent::Error {
                session_id, error, ..
            }) => Some((session_id, error)),
            _ => None,
        })
        .expect("error frame");
    assert_eq!(error.0, "agent-1");
    assert_eq!(error.1.code, "internal_error");
    assert_eq!(error.1.kind, "internal_error");
    assert_eq!(error.1.param, None);
    assert_eq!(error.1.message, "model refused");
}

#[tokio::test]
async fn a_failed_steer_handoff_closes_the_turn_with_the_prompt_terminal() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);

    // The steer owns the turn, so the cancelled prompt attempt's terminal does
    // not close it yet.
    let epoch = log.note_steer();
    assert_eq!(epoch, 1);
    log.ingest(
        "peri/agent_event_done",
        &json!({ "sessionId": "agent-1", "stopReason": "cancelled", "requestId": "req-1" }),
    )
    .await;
    assert_eq!(log.active_turn().as_deref(), Some("req-1"));
    assert!(
        !drain(&mut events)
            .iter()
            .any(|event| matches!(event, StreamEvent::Turn(TurnEvent::Completed { .. }))),
        "a steered attempt's terminal is not the turn's"
    );

    // The handoff failed: the kept terminal closes the turn instead of leaving
    // it open until the prompt's terminal timeout.
    log.abandon_steer(epoch).await;
    assert!(log.active_turn().is_none(), "the turn must not stay open");
    let seen = drain(&mut events);
    assert!(
        seen.iter().any(|event| matches!(
            event,
            StreamEvent::Turn(TurnEvent::Completed { stop_reason, .. })
                if stop_reason == "cancelled"
        )),
        "the reply's own stop reason closes the turn: {seen:?}"
    );
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Idle);
}

#[tokio::test(start_paused = true)]
async fn a_steer_admitted_while_settling_keeps_the_turn_open() {
    let Fixture {
        log, mut events, ..
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);

    let finishing = {
        let log = Arc::clone(&log);
        tokio::spawn(async move {
            log.finish_turn("req-1", TurnOutcome::Completed("end_turn".into()))
                .await;
        })
    };
    // Paused time advances only after ready tasks yield, so `finish_turn` is
    // already waiting inside `settle`.
    tokio::time::sleep(Duration::from_millis(1)).await;
    assert_eq!(log.note_steer(), 1);
    finishing.await.expect("finish_turn");

    assert_eq!(log.active_turn().as_deref(), Some("req-1"));
    assert_eq!(log.steer_generation(), 1);
    assert!(log.steer_pending());
    assert!(!drain(&mut events)
        .iter()
        .any(|event| matches!(event, StreamEvent::Turn(TurnEvent::Completed { .. }))));
}

#[tokio::test(start_paused = true)]
async fn a_terminal_kept_by_a_steer_guard_closes_the_turn_when_the_handoff_fails() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);

    let finishing = {
        let log = Arc::clone(&log);
        tokio::spawn(async move {
            log.finish_turn("req-1", TurnOutcome::Completed("end_turn".into()))
                .await;
        })
    };
    tokio::time::sleep(Duration::from_millis(1)).await;
    assert_eq!(log.note_steer(), 1);
    tokio::time::sleep(Duration::from_millis(20)).await;
    finishing.await.expect("finish_turn");

    assert_eq!(log.active_turn().as_deref(), Some("req-1"));
    assert_eq!(log.steer_generation(), 1);
    assert!(log.steer_pending());
    assert!(!drain(&mut events)
        .iter()
        .any(|event| matches!(event, StreamEvent::Turn(TurnEvent::Completed { .. }))));

    log.abandon_steer(1).await;
    assert!(
        log.active_turn().is_none(),
        "the turn must close after the handoff fails"
    );
    let seen = drain(&mut events);
    assert!(
        seen.iter().any(|event| matches!(
            event,
            StreamEvent::Turn(TurnEvent::Completed { stop_reason, .. })
                if stop_reason == "end_turn"
        )),
        "the completion must carry the kept stop reason: {seen:?}"
    );
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Idle);
}

#[tokio::test(start_paused = true)]
async fn a_failed_timeout_close_kept_by_a_steer_guard_closes_when_the_handoff_fails() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);

    let finishing = {
        let log = Arc::clone(&log);
        tokio::spawn(async move {
            log.finish_turn(
                "req-1",
                TurnOutcome::Failed {
                    code: -32603,
                    message: "agent turn reported no terminal".to_owned(),
                },
            )
            .await;
        })
    };
    tokio::time::sleep(Duration::from_millis(1)).await;
    assert_eq!(log.note_steer(), 1);
    tokio::time::sleep(Duration::from_millis(20)).await;
    finishing.await.expect("finish_turn");

    assert_eq!(log.active_turn().as_deref(), Some("req-1"));
    assert_eq!(log.steer_generation(), 1);
    assert!(log.steer_pending());
    assert!(!drain(&mut events)
        .iter()
        .any(|event| matches!(event, StreamEvent::Turn(TurnEvent::Failed { .. }))));

    log.abandon_steer(1).await;
    assert!(
        log.active_turn().is_none(),
        "the turn must close after the handoff fails"
    );
    let seen = drain(&mut events);
    assert!(
        seen.iter().any(|event| matches!(
            event,
            StreamEvent::Turn(TurnEvent::Failed { error, .. })
                if error.code == -32603 && error.message == "agent turn reported no terminal"
        )),
        "the failure must carry the kept code and message: {seen:?}"
    );
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    // Replaying Failed sets the session to Failed and emits session.failed as usual.
    assert_eq!(row.state, SessionState::Failed);
}

#[tokio::test(start_paused = true)]
async fn a_terminal_decided_on_an_older_epoch_is_not_used_to_close() {
    let Fixture {
        log, mut events, ..
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);
    assert_eq!(log.note_steer(), 1);

    log.finish_turn_observed(
        "req-1",
        TurnOutcome::Completed("end_turn".into()),
        Some(0),
        None,
    )
    .await;

    assert_eq!(log.active_turn().as_deref(), Some("req-1"));
    assert!(!drain(&mut events)
        .iter()
        .any(|event| matches!(event, StreamEvent::Turn(TurnEvent::Completed { .. }))));

    log.abandon_steer(1).await;
    assert!(
        log.active_turn().is_none(),
        "the turn must close after the handoff fails"
    );
    let seen = drain(&mut events);
    assert!(
        seen.iter().any(|event| matches!(
            event,
            StreamEvent::Turn(TurnEvent::Completed { stop_reason, .. })
                if stop_reason == "end_turn"
        )),
        "the completion must carry the stop reason kept for the older epoch: {seen:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_steer_rolled_back_while_settling_lets_the_timeout_close_the_turn() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);
    assert_eq!(log.note_steer(), 1);

    let finishing = {
        let log = Arc::clone(&log);
        tokio::spawn(async move {
            log.finish_turn(
                "req-1",
                TurnOutcome::Failed {
                    code: -32603,
                    message: "agent turn reported no terminal".to_owned(),
                },
            )
            .await;
        })
    };
    // Under the paused clock, `finish_turn` has its snapshot and is settling.
    tokio::time::sleep(Duration::from_millis(1)).await;
    log.abandon_steer(1).await;
    assert_eq!(log.steer_generation(), 1);
    assert!(!log.steer_pending());
    assert_eq!(log.active_turn().as_deref(), Some("req-1"));
    finishing.await.expect("finish_turn");

    assert!(
        log.active_turn().is_none(),
        "the timeout closer must still close the turn after a rollback"
    );
    let seen = drain(&mut events);
    assert!(
        seen.iter().any(|event| matches!(
            event,
            StreamEvent::Turn(TurnEvent::Failed { error, .. })
                if error.code == -32603 && error.message == "agent turn reported no terminal"
        )),
        "the failure must carry the original code and message: {seen:?}"
    );
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Failed);
}

#[tokio::test(start_paused = true)]
async fn a_stale_decision_epoch_after_rollback_still_closes_the_turn() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);
    assert_eq!(log.note_steer(), 1);
    log.abandon_steer(1).await;
    assert_eq!(log.steer_generation(), 1);
    assert!(!log.steer_pending());
    assert_eq!(log.active_turn().as_deref(), Some("req-1"));

    log.finish_turn_observed(
        "req-1",
        TurnOutcome::Completed("end_turn".into()),
        Some(1),
        None,
    )
    .await;

    assert!(
        log.active_turn().is_none(),
        "a prior decision must still close the turn after a rollback"
    );
    let seen = drain(&mut events);
    assert!(
        seen.iter().any(|event| matches!(
            event,
            StreamEvent::Turn(TurnEvent::Completed { stop_reason, .. })
                if stop_reason == "end_turn"
        )),
        "the completion must carry the previously decided stop reason: {seen:?}"
    );
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Idle);
}

/// A rollback must not recycle the generation a closer already observed: the
/// closer yields to the second steer and only that steer's run closes the turn.
#[tokio::test(start_paused = true)]
async fn a_rollback_does_not_reuse_the_generation_a_later_steer_advances() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);
    assert_eq!(log.note_steer(), 1);

    let finishing = {
        let log = Arc::clone(&log);
        tokio::spawn(async move {
            log.finish_turn_observed(
                "req-1",
                TurnOutcome::Completed("cancelled".into()),
                Some(1),
                None,
            )
            .await;
        })
    };
    // Under the paused clock the closer already has its generation snapshot and
    // is settling.
    tokio::time::sleep(Duration::from_millis(1)).await;
    log.abandon_steer(1).await;
    assert!(!log.steer_pending());
    // The rollback keeps generation 1, so the next steer must advance to 2
    // instead of handing out the value the closer observed.
    assert_eq!(log.note_steer(), 2);
    assert_eq!(log.steer_generation(), 2);
    assert!(log.steer_pending());
    // The second steer is adopted by its queued run. The claim removes 2 from
    // the unresolved set, so only the changed continuation identity can defer
    // the closer that is already settling.
    log.ingest(
        "peri/agent_event",
        &json!({
            "event_json": json!({
                "type": "user_input_run_started",
                "value": { "request_id": "req-2" },
            })
            .to_string(),
        }),
    )
    .await;
    finishing.await.expect("finish_turn");

    assert_eq!(
        log.active_turn().as_deref(),
        Some("req-1"),
        "the stale closer must not close a turn the second steer took over"
    );
    assert!(
        !drain(&mut events)
            .iter()
            .any(|event| matches!(event, StreamEvent::Turn(TurnEvent::Completed { .. }))),
        "the yielding closer must not publish a terminal"
    );

    log.ingest(
        "peri/agent_event_done",
        &json!({ "sessionId": "agent-1", "stopReason": "end_turn", "requestId": "req-2" }),
    )
    .await;

    assert!(log.active_turn().is_none());
    let terminals: Vec<_> = drain(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            StreamEvent::Turn(
                terminal @ (TurnEvent::Completed { .. } | TurnEvent::Failed { .. }),
            ) => Some(terminal),
            _ => None,
        })
        .collect();
    assert_eq!(
        terminals,
        [TurnEvent::completed("req-1", "turn_1", "end_turn")],
        "the continuation terminal closes the turn, not the retained one"
    );
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Idle);
}

/// A later failed steer must not release a turn an earlier successful steer
/// still owns: only the earlier queued run may terminate it.
#[tokio::test(start_paused = true)]
async fn a_failed_steer_does_not_release_an_earlier_steers_turn() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);

    // Steer A was handed off; the host cancelled the prompt attempt, whose
    // terminal is not the turn's.
    assert_eq!(log.note_steer(), 1);
    log.ingest(
        "peri/agent_event_done",
        &json!({ "sessionId": "agent-1", "stopReason": "cancelled", "requestId": "req-1" }),
    )
    .await;
    assert_eq!(log.active_turn().as_deref(), Some("req-1"));

    // Steer B fails before A's queued run started. Rolling B back must not
    // replay the retained terminal: steer A still owns the turn.
    assert_eq!(log.note_steer(), 2);
    log.abandon_steer(2).await;
    assert!(log.steer_pending(), "steer A still owns the turn");
    assert_eq!(log.active_turn().as_deref(), Some("req-1"));
    assert!(
        !drain(&mut events)
            .iter()
            .any(|event| matches!(event, StreamEvent::Turn(TurnEvent::Completed { .. }))),
        "the retained prompt terminal must not close A's turn"
    );

    // A's queued run is adopted and closes the turn with its own terminal.
    log.ingest(
        "peri/agent_event",
        &json!({
            "event_json": json!({
                "type": "user_input_run_started",
                "value": { "request_id": "req-2" },
            })
            .to_string(),
        }),
    )
    .await;
    log.ingest(
        "peri/agent_event_done",
        &json!({ "sessionId": "agent-1", "stopReason": "end_turn", "requestId": "req-2" }),
    )
    .await;

    assert!(log.active_turn().is_none());
    let terminals: Vec<_> = drain(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            StreamEvent::Turn(
                terminal @ (TurnEvent::Completed { .. } | TurnEvent::Failed { .. }),
            ) => Some(terminal),
            _ => None,
        })
        .collect();
    assert_eq!(
        terminals,
        [TurnEvent::completed("req-1", "turn_1", "end_turn")],
        "the earlier steer's run is the only terminal"
    );
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Idle);
}

/// Two failed steers rolled back out of order: the first rollback leaves the
/// turn owned by the other steer, and only the last one replays the terminal.
#[tokio::test]
async fn out_of_order_steer_rollbacks_release_the_turn_only_at_the_last_one() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);

    assert_eq!(log.note_steer(), 1);
    assert_eq!(log.note_steer(), 2);
    log.ingest(
        "peri/agent_event_done",
        &json!({ "sessionId": "agent-1", "stopReason": "cancelled", "requestId": "req-1" }),
    )
    .await;
    assert_eq!(log.active_turn().as_deref(), Some("req-1"));

    // The later steer rolls back first: steer 1 still owns the turn.
    log.abandon_steer(2).await;
    assert!(log.steer_pending(), "steer 1 is still outstanding");
    assert_eq!(log.active_turn().as_deref(), Some("req-1"));
    assert!(
        !drain(&mut events)
            .iter()
            .any(|event| matches!(event, StreamEvent::Turn(TurnEvent::Completed { .. }))),
        "one rollback must not replay the retained terminal"
    );

    // The last outstanding steer rolls back: now the retained terminal closes
    // the turn instead of leaving it open until the prompt timeout.
    log.abandon_steer(1).await;
    assert!(log.active_turn().is_none());
    assert!(!log.steer_pending());
    let seen = drain(&mut events);
    assert!(
        seen.iter().any(|event| matches!(
            event,
            StreamEvent::Turn(TurnEvent::Completed { stop_reason, .. })
                if stop_reason == "cancelled"
        )),
        "the reply's own stop reason closes the turn: {seen:?}"
    );
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Idle);
}

/// A rollback admitted during `settle` leaves no steer owning the turn, so the
/// closer must finish its job instead of deferring to the safety net.
#[tokio::test(start_paused = true)]
async fn a_rollback_during_settle_does_not_defer_the_close() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);

    let finishing = {
        let log = Arc::clone(&log);
        tokio::spawn(async move {
            log.finish_turn_observed(
                "req-1",
                TurnOutcome::Completed("end_turn".into()),
                Some(0),
                None,
            )
            .await;
        })
    };
    // Under the paused clock the closer already has its snapshot and is
    // settling.
    tokio::time::sleep(Duration::from_millis(1)).await;
    assert_eq!(log.note_steer(), 1);
    log.abandon_steer(1).await;
    finishing.await.expect("finish_turn");

    assert!(
        log.active_turn().is_none(),
        "a rolled-back steer must not defer the close"
    );
    let seen = drain(&mut events);
    assert!(
        seen.iter().any(|event| matches!(
            event,
            StreamEvent::Turn(TurnEvent::Completed { stop_reason, .. })
                if stop_reason == "end_turn"
        )),
        "the closer must publish its terminal: {seen:?}"
    );
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Idle);
}

/// The pre-settle guard only yields to a change that still owns the turn: a
/// lone rollback leaves a stale generation behind, and the closer must finish
/// its job instead of deferring to the safety net.
#[tokio::test(start_paused = true)]
async fn a_stale_generation_without_an_owner_does_not_defer_the_close() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);

    assert_eq!(log.note_steer(), 1);
    log.abandon_steer(1).await;
    assert!(!log.steer_pending(), "the rollback leaves no owner");
    assert_eq!(log.active_turn().as_deref(), Some("req-1"));

    // The expected value is stale, but no steer owns the turn any more.
    log.finish_turn_observed(
        "req-1",
        TurnOutcome::Completed("end_turn".into()),
        Some(0),
        None,
    )
    .await;

    assert!(
        log.active_turn().is_none(),
        "a stale generation without an owner must not defer the close"
    );
    let seen = drain(&mut events);
    let terminals = seen
        .iter()
        .filter(|event| {
            matches!(
                event,
                StreamEvent::Turn(TurnEvent::Completed { .. } | TurnEvent::Failed { .. })
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(terminals.len(), 1, "exactly one terminal: {seen:?}");
    assert!(
        matches!(
            terminals[0],
            StreamEvent::Turn(TurnEvent::Completed { stop_reason, .. }) if stop_reason == "end_turn"
        ),
        "the closer must publish its own terminal: {seen:?}"
    );
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Idle);
}

/// The generation keeps counting across turns, so a late rollback from a closed
/// turn cannot release the next turn's first steer.
#[tokio::test]
async fn a_late_rollback_does_not_release_the_next_turns_steer() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    assert_eq!(log.note_steer(), 1);
    log.finish_turn("req-1", TurnOutcome::Completed("end_turn".into()))
        .await;
    assert!(log.active_turn().is_none());
    drain(&mut events);

    log.begin_turn("req-2", "turn_2").await.expect("turn");
    let generation = log.note_steer();
    assert!(
        generation > 1,
        "the generation must not be reused across turns: {generation}"
    );
    assert!(log.steer_pending());

    log.abandon_steer(1).await;
    assert!(
        log.steer_pending(),
        "a stale rollback must not release the new turn's steer"
    );
    assert_eq!(log.active_turn().as_deref(), Some("req-2"));
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Running);
}

/// The queued-run start that adopts `request_id` as the open turn's
/// continuation.
fn queued_run_started(request_id: &str) -> serde_json::Value {
    json!({
        "event_json": json!({
            "type": "user_input_run_started",
            "value": { "request_id": request_id },
        })
        .to_string(),
    })
}

/// `peri/agent_event_done` for a run (or the prompt attempt) of the open turn.
fn run_terminal(request_id: &str, stop_reason: &str) -> serde_json::Value {
    json!({ "sessionId": "agent-1", "stopReason": stop_reason, "requestId": request_id })
}

/// Finding A: a continuation terminal already inside `settle` must still close
/// the turn when a steer is noted and rolled back meanwhile.
#[tokio::test(start_paused = true)]
async fn a_rolled_back_steer_does_not_strand_the_continuation_terminal() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);
    assert_eq!(log.note_steer(), 1);
    log.ingest("peri/agent_event", &queued_run_started("req-2"))
        .await;

    // The continuation terminal reaches `settle`; a second steer is admitted
    // and rolled back while it waits.
    let finishing = {
        let log = Arc::clone(&log);
        tokio::spawn(async move {
            log.ingest(
                "peri/agent_event_done",
                &json!({ "sessionId": "agent-1", "stopReason": "end_turn", "requestId": "req-2" }),
            )
            .await;
        })
    };
    tokio::time::sleep(Duration::from_millis(1)).await;
    assert_eq!(log.note_steer(), 2);
    log.abandon_steer(2).await;
    finishing.await.expect("continuation terminal");

    assert_eq!(
        log.active_turn(),
        None,
        "the continuation terminal must close the turn instead of waiting for the safety net"
    );
    let terminals: Vec<_> = drain(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            StreamEvent::Turn(
                terminal @ (TurnEvent::Completed { .. } | TurnEvent::Failed { .. }),
            ) => Some(terminal),
            _ => None,
        })
        .collect();
    assert_eq!(
        terminals,
        [TurnEvent::completed("req-1", "turn_1", "end_turn")]
    );
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Idle);
}

/// Finding B: rolling back a newer steer must not hand the older run the
/// ownership a still-outstanding steer holds.
#[tokio::test]
async fn a_failed_newer_steer_does_not_hand_the_turn_to_the_older_run() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);

    // Steer A's prompt attempt is cancelled, then its queued run continues the
    // turn.
    assert_eq!(log.note_steer(), 1);
    log.ingest(
        "peri/agent_event_done",
        &json!({ "sessionId": "agent-1", "stopReason": "cancelled", "requestId": "req-1" }),
    )
    .await;
    log.ingest("peri/agent_event", &queued_run_started("req-2"))
        .await;

    // B fails while C is still waiting to start.
    assert_eq!(log.note_steer(), 2);
    assert_eq!(log.note_steer(), 3);
    log.abandon_steer(2).await;

    // A's terminal must not close the turn C still owns.
    log.ingest(
        "peri/agent_event_done",
        &json!({ "sessionId": "agent-1", "stopReason": "end_turn", "requestId": "req-2" }),
    )
    .await;
    assert_eq!(
        log.active_turn().as_deref(),
        Some("req-1"),
        "steer C still owns the turn"
    );
    assert!(
        !drain(&mut events).iter().any(|event| matches!(
            event,
            StreamEvent::Turn(TurnEvent::Completed { .. } | TurnEvent::Failed { .. })
        )),
        "the older run's terminal must not close C's turn"
    );

    // C's queued run takes over and closes the turn.
    log.ingest("peri/agent_event", &queued_run_started("req-3"))
        .await;
    log.ingest(
        "peri/agent_event_done",
        &json!({ "sessionId": "agent-1", "stopReason": "end_turn", "requestId": "req-3" }),
    )
    .await;

    assert_eq!(log.active_turn(), None);
    let terminals: Vec<_> = drain(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            StreamEvent::Turn(
                terminal @ (TurnEvent::Completed { .. } | TurnEvent::Failed { .. }),
            ) => Some(terminal),
            _ => None,
        })
        .collect();
    assert_eq!(
        terminals,
        [TurnEvent::completed("req-1", "turn_1", "end_turn")],
        "only C's terminal closes the turn"
    );
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Idle);
}

/// The other half of Finding B: with no newer steer outstanding, the adopted
/// run still closes the turn after a rollback.
#[tokio::test]
async fn a_rolled_back_steer_still_lets_the_adopted_run_close_the_turn() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);

    assert_eq!(log.note_steer(), 1);
    log.ingest(
        "peri/agent_event_done",
        &json!({ "sessionId": "agent-1", "stopReason": "cancelled", "requestId": "req-1" }),
    )
    .await;
    log.ingest("peri/agent_event", &queued_run_started("req-2"))
        .await;

    assert_eq!(log.note_steer(), 2);
    log.abandon_steer(2).await;
    assert_eq!(
        log.active_turn().as_deref(),
        Some("req-1"),
        "the adopted run owns the turn"
    );

    log.ingest(
        "peri/agent_event_done",
        &json!({ "sessionId": "agent-1", "stopReason": "end_turn", "requestId": "req-2" }),
    )
    .await;

    assert_eq!(log.active_turn(), None);
    let terminals: Vec<_> = drain(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            StreamEvent::Turn(
                terminal @ (TurnEvent::Completed { .. } | TurnEvent::Failed { .. }),
            ) => Some(terminal),
            _ => None,
        })
        .collect();
    assert_eq!(
        terminals,
        [TurnEvent::completed("req-1", "turn_1", "end_turn")]
    );
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Idle);
}

/// A rollback must not replay the prompt attempt's `cancelled` terminal while a
/// live run owns the turn.
#[tokio::test]
async fn a_rolled_back_steer_does_not_replay_the_prompts_terminal_for_a_live_run() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);

    // The steered prompt attempt's terminal is retained; a queued run owns the
    // turn from then on.
    assert_eq!(log.note_steer(), 1);
    log.ingest(
        "peri/agent_event_done",
        &json!({ "sessionId": "agent-1", "stopReason": "cancelled", "requestId": "req-1" }),
    )
    .await;
    log.ingest("peri/agent_event", &queued_run_started("req-2"))
        .await;

    // A later steer is rolled back, then the first one too: the set is empty,
    // but the retained prompt terminal still must not close the turn.
    assert_eq!(log.note_steer(), 2);
    log.abandon_steer(2).await;
    log.abandon_steer(1).await;
    assert_eq!(
        log.active_turn().as_deref(),
        Some("req-1"),
        "the live run still owns the turn"
    );
    assert!(
        !drain(&mut events)
            .iter()
            .any(|event| matches!(event, StreamEvent::Turn(TurnEvent::Completed { .. }))),
        "the prompt's cancelled terminal must not be replayed"
    );

    log.ingest(
        "peri/agent_event_done",
        &json!({ "sessionId": "agent-1", "stopReason": "end_turn", "requestId": "req-2" }),
    )
    .await;

    assert_eq!(log.active_turn(), None);
    let terminals: Vec<_> = drain(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            StreamEvent::Turn(
                terminal @ (TurnEvent::Completed { .. } | TurnEvent::Failed { .. }),
            ) => Some(terminal),
            _ => None,
        })
        .collect();
    assert_eq!(
        terminals,
        [TurnEvent::completed("req-1", "turn_1", "end_turn")],
        "the run's own terminal is the only one"
    );
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Idle);
}

#[tokio::test(start_paused = true)]
async fn a_deferred_lost_terminal_close_is_retried() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);

    let retry_window = Duration::from_secs(5);
    let closing = {
        let log = Arc::clone(&log);
        tokio::spawn(async move {
            log.close_lost_turn(
                "req-1",
                TurnOutcome::Failed {
                    code: -32603,
                    message: "agent turn reported no terminal".to_owned(),
                },
                retry_window,
            )
            .await;
        })
    };
    tokio::time::sleep(Duration::from_millis(1)).await;
    assert_eq!(log.note_steer(), 1);
    tokio::time::sleep(retry_window + Duration::from_secs(1)).await;
    closing.await.expect("close_lost_turn");

    assert!(
        log.active_turn().is_none(),
        "the retry must close a turn whose continuation never reported"
    );
    let seen = drain(&mut events);
    assert!(
        seen.iter().any(|event| matches!(
            event,
            StreamEvent::Turn(TurnEvent::Failed { error, .. })
                if error.code == -32603 && error.message == "agent turn reported no terminal"
        )),
        "the failure must keep the original code and message: {seen:?}"
    );
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Failed);
}

#[tokio::test(start_paused = true)]
async fn a_retry_does_not_close_after_the_continuation_terminal() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);

    let closing = {
        let log = Arc::clone(&log);
        tokio::spawn(async move {
            log.close_lost_turn(
                "req-1",
                TurnOutcome::Failed {
                    code: -32603,
                    message: "agent turn reported no terminal".to_owned(),
                },
                Duration::from_secs(5),
            )
            .await;
        })
    };
    tokio::time::sleep(Duration::from_millis(1)).await;
    assert_eq!(log.note_steer(), 1);
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(log.active_turn().as_deref(), Some("req-1"));
    assert!(!closing.is_finished());

    log.finish_turn("req-1", TurnOutcome::Completed("end_turn".into()))
        .await;
    closing.await.expect("close_lost_turn");

    assert!(log.active_turn().is_none());
    let terminals: Vec<_> = drain(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            StreamEvent::Turn(
                terminal @ (TurnEvent::Completed { .. } | TurnEvent::Failed { .. }),
            ) => Some(terminal),
            _ => None,
        })
        .collect();
    assert_eq!(
        terminals,
        [TurnEvent::completed("req-1", "turn_1", "end_turn")]
    );
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Idle);
}

/// The safety net must give a steered continuation its grace: a terminal that
/// arrives first closes the turn and no forced terminal is published.
#[tokio::test(start_paused = true)]
async fn a_grace_close_waits_for_the_continuation_terminal() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);
    assert_eq!(log.note_steer(), 1);
    log.ingest(
        "peri/agent_event",
        &json!({
            "event_json": json!({
                "type": "user_input_run_started",
                "value": { "request_id": "req-2" },
            })
            .to_string(),
        }),
    )
    .await;

    let grace = Duration::from_secs(5);
    let closing = {
        let log = Arc::clone(&log);
        tokio::spawn(async move {
            log.close_lost_turn_after_grace(
                "req-1",
                TurnOutcome::Failed {
                    code: -32603,
                    message: "agent turn reported no terminal".to_owned(),
                },
                grace,
            )
            .await;
        })
    };
    tokio::time::sleep(Duration::from_millis(1)).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(log.active_turn().as_deref(), Some("req-1"));
    assert!(
        !closing.is_finished(),
        "the safety net must wait out the grace period"
    );

    log.ingest(
        "peri/agent_event_done",
        &json!({ "sessionId": "agent-1", "stopReason": "end_turn", "requestId": "req-2" }),
    )
    .await;
    closing.await.expect("close_lost_turn_after_grace");

    assert!(log.active_turn().is_none());
    let terminals: Vec<_> = drain(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            StreamEvent::Turn(
                terminal @ (TurnEvent::Completed { .. } | TurnEvent::Failed { .. }),
            ) => Some(terminal),
            _ => None,
        })
        .collect();
    assert_eq!(
        terminals,
        [TurnEvent::completed("req-1", "turn_1", "end_turn")],
        "the continuation terminal must be the only one"
    );
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Idle);
}

/// Without any continuation terminal the safety net force-closes the turn once
/// the grace expires.
#[tokio::test(start_paused = true)]
async fn a_grace_close_forces_the_turn_after_the_grace_without_terminal() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);
    assert_eq!(log.note_steer(), 1);
    log.ingest(
        "peri/agent_event",
        &json!({
            "event_json": json!({
                "type": "user_input_run_started",
                "value": { "request_id": "req-2" },
            })
            .to_string(),
        }),
    )
    .await;

    let grace = Duration::from_secs(5);
    let closing = {
        let log = Arc::clone(&log);
        tokio::spawn(async move {
            log.close_lost_turn_after_grace(
                "req-1",
                TurnOutcome::Failed {
                    code: -32603,
                    message: "agent turn reported no terminal".to_owned(),
                },
                grace,
            )
            .await;
        })
    };
    tokio::time::sleep(Duration::from_millis(1)).await;
    tokio::time::sleep(grace + Duration::from_secs(1)).await;
    closing.await.expect("close_lost_turn_after_grace");

    assert!(
        log.active_turn().is_none(),
        "the safety net must close a turn whose continuation never reported"
    );
    let terminals: Vec<_> = drain(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            StreamEvent::Turn(
                terminal @ (TurnEvent::Completed { .. } | TurnEvent::Failed { .. }),
            ) => Some(terminal),
            _ => None,
        })
        .collect();
    assert_eq!(terminals.len(), 1, "exactly one terminal: {terminals:?}");
    assert!(
        matches!(
            &terminals[0],
            TurnEvent::Failed { error, .. }
                if error.code == -32603 && error.message == "agent turn reported no terminal"
        ),
        "the forced close keeps the original failure: {terminals:?}"
    );
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Failed);
}

/// Blocking finding 1: a `user_input_run_started` that reaches the reader only
/// after a later steer was noted must claim the oldest unresolved steer, not
/// the current counter. Inheriting the counter let the older run's `cancelled`
/// terminal close the newer steer's turn while the host still ran it.
#[tokio::test]
async fn a_late_run_started_claims_the_oldest_steer_not_the_current_counter() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);

    // Steer A was dispatched; its run start is still inside the transport's
    // coalescing window when steer C is noted.
    assert_eq!(log.note_steer(), 1);
    assert_eq!(log.note_steer(), 2);

    // A's run start arrives late and claims steer A (1), not the counter (2).
    log.ingest("peri/agent_event", &queued_run_started("req-2"))
        .await;
    // The host cancels A's run for C: that terminal must not close the turn C
    // still owns.
    log.ingest("peri/agent_event_done", &run_terminal("req-2", "cancelled"))
        .await;
    assert_eq!(
        log.active_turn().as_deref(),
        Some("req-1"),
        "steer C still owns the turn"
    );
    assert!(
        !drain(&mut events).iter().any(|event| matches!(
            event,
            StreamEvent::Turn(TurnEvent::Completed { .. } | TurnEvent::Failed { .. })
        )),
        "the superseded run's terminal must not close C's turn"
    );

    // C's run starts, claims C, and its terminal closes the turn.
    log.ingest("peri/agent_event", &queued_run_started("req-3"))
        .await;
    log.ingest("peri/agent_event_done", &run_terminal("req-3", "end_turn"))
        .await;

    assert_eq!(log.active_turn(), None);
    let terminals: Vec<_> = drain(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            StreamEvent::Turn(
                terminal @ (TurnEvent::Completed { .. } | TurnEvent::Failed { .. }),
            ) => Some(terminal),
            _ => None,
        })
        .collect();
    assert_eq!(
        terminals,
        [TurnEvent::completed("req-1", "turn_1", "end_turn")],
        "only C's terminal closes the turn"
    );
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Idle);
}

/// Blocking finding 2a: a continuation terminal that reached `settle` before a
/// steer was noted is retained, and the failed handoff replays it instead of
/// leaving the turn to the 300s grace.
#[tokio::test(start_paused = true)]
async fn a_retained_continuation_terminal_closes_the_turn_after_the_rollback() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);
    assert_eq!(log.note_steer(), 1);
    log.ingest("peri/agent_event", &queued_run_started("req-2"))
        .await;

    // The continuation terminal passes the filter and waits in `settle`; a
    // second steer is admitted while it waits, so the closer retains it.
    let finishing = {
        let log = Arc::clone(&log);
        tokio::spawn(async move {
            log.ingest("peri/agent_event_done", &run_terminal("req-2", "end_turn"))
                .await;
        })
    };
    tokio::time::sleep(Duration::from_millis(1)).await;
    assert_eq!(log.note_steer(), 2);
    finishing.await.expect("continuation terminal");

    assert_eq!(
        log.active_turn().as_deref(),
        Some("req-1"),
        "the retained terminal must not close the turn steer 2 owns"
    );
    assert!(!drain(&mut events).iter().any(|event| matches!(
        event,
        StreamEvent::Turn(TurnEvent::Completed { .. } | TurnEvent::Failed { .. })
    )));

    // The handoff failed: the retained continuation terminal closes the turn
    // instead of waiting for the safety net.
    log.abandon_steer(2).await;

    assert_eq!(log.active_turn(), None);
    let terminals: Vec<_> = drain(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            StreamEvent::Turn(
                terminal @ (TurnEvent::Completed { .. } | TurnEvent::Failed { .. }),
            ) => Some(terminal),
            _ => None,
        })
        .collect();
    assert_eq!(
        terminals,
        [TurnEvent::completed("req-1", "turn_1", "end_turn")]
    );
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Idle);
}

/// Blocking finding 2b: a continuation terminal that arrives after a newer
/// steer was noted is kept (not dropped), so the failed handoff can still close
/// the turn with it.
#[tokio::test]
async fn a_superseded_continuation_terminal_is_replayed_after_the_rollback() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);

    assert_eq!(log.note_steer(), 1);
    log.ingest("peri/agent_event", &queued_run_started("req-2"))
        .await;
    // Steer 2 is noted before the adopted run reports: the run is superseded.
    assert_eq!(log.note_steer(), 2);
    log.ingest("peri/agent_event_done", &run_terminal("req-2", "end_turn"))
        .await;

    assert_eq!(
        log.active_turn().as_deref(),
        Some("req-1"),
        "the superseded terminal must not close the turn steer 2 owns"
    );
    assert!(!drain(&mut events).iter().any(|event| matches!(
        event,
        StreamEvent::Turn(TurnEvent::Completed { .. } | TurnEvent::Failed { .. })
    )));

    // The steer 2 handoff failed: the kept continuation terminal closes the
    // turn now.
    log.abandon_steer(2).await;

    assert_eq!(log.active_turn(), None);
    let terminals: Vec<_> = drain(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            StreamEvent::Turn(
                terminal @ (TurnEvent::Completed { .. } | TurnEvent::Failed { .. }),
            ) => Some(terminal),
            _ => None,
        })
        .collect();
    assert_eq!(
        terminals,
        [TurnEvent::completed("req-1", "turn_1", "end_turn")]
    );
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Idle);
}

/// Each queued run claims the oldest unclaimed steer, so an older run's
/// terminal is stranded instead of closing the turn a later steer owns.
#[tokio::test]
async fn each_queued_run_claims_the_oldest_unclaimed_steer() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);

    assert_eq!(log.note_steer(), 1);
    assert_eq!(log.note_steer(), 2);
    log.ingest("peri/agent_event", &queued_run_started("req-2"))
        .await;
    log.ingest("peri/agent_event", &queued_run_started("req-3"))
        .await;

    // `req-2` owns steer 1, which steer 2 has superseded: its terminal is
    // stranded while `req-3` owns the turn.
    log.ingest("peri/agent_event_done", &run_terminal("req-2", "end_turn"))
        .await;
    assert_eq!(
        log.active_turn().as_deref(),
        Some("req-1"),
        "the older run's terminal must not close steer 2's turn"
    );
    assert!(!drain(&mut events).iter().any(|event| matches!(
        event,
        StreamEvent::Turn(TurnEvent::Completed { .. } | TurnEvent::Failed { .. })
    )));

    log.ingest("peri/agent_event_done", &run_terminal("req-3", "end_turn"))
        .await;

    assert_eq!(log.active_turn(), None);
    let terminals: Vec<_> = drain(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            StreamEvent::Turn(
                terminal @ (TurnEvent::Completed { .. } | TurnEvent::Failed { .. }),
            ) => Some(terminal),
            _ => None,
        })
        .collect();
    assert_eq!(
        terminals,
        [TurnEvent::completed("req-1", "turn_1", "end_turn")],
        "only the newest run's terminal closes the turn"
    );
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Idle);
}

/// A continuation's terminal replaces the prompt attempt's retained terminal,
/// so the turn closes with the outcome of the run that owned it instead of the
/// `cancelled` attempt a steer interrupted.
#[tokio::test]
async fn a_continuation_terminal_replaces_the_retained_prompt_terminal() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);

    // The steered prompt attempt's `cancelled` terminal is retained first.
    assert_eq!(log.note_steer(), 1);
    log.ingest("peri/agent_event_done", &run_terminal("req-1", "cancelled"))
        .await;
    assert_eq!(log.active_turn().as_deref(), Some("req-1"));

    // The queued run owns the turn from now on; a newer steer supersedes it.
    log.ingest("peri/agent_event", &queued_run_started("req-2"))
        .await;
    assert_eq!(log.note_steer(), 2);
    log.ingest("peri/agent_event_done", &run_terminal("req-2", "end_turn"))
        .await;
    assert_eq!(
        log.active_turn().as_deref(),
        Some("req-1"),
        "the newer steer still owns the turn"
    );

    // The newer handoff failed: the run's own terminal closes the turn.
    log.abandon_steer(2).await;

    assert_eq!(log.active_turn(), None);
    let terminals: Vec<_> = drain(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            StreamEvent::Turn(
                terminal @ (TurnEvent::Completed { .. } | TurnEvent::Failed { .. }),
            ) => Some(terminal),
            _ => None,
        })
        .collect();
    assert_eq!(
        terminals,
        [TurnEvent::completed("req-1", "turn_1", "end_turn")],
        "the cancelled prompt terminal must not close a live run's turn"
    );
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Idle);
}

#[tokio::test]
async fn a_failed_group_does_not_discard_the_rest_of_the_run() {
    let memory = Arc::new(MemorySwarmStore::new());
    memory
        .swarm_upsert_session(session_row())
        .await
        .expect("session row");
    let store: Arc<dyn SwarmStore> = memory.clone();
    let (channel, mut events) = broadcast::channel(64);
    let log = Arc::new(ItemLog::new(
        "agent-1",
        Arc::clone(&store),
        channel,
        Arc::new(AtomicU64::new(0)),
    ));
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);

    // The oversized tool row sits alone in its durable group; the store rejects
    // that group, and the small item behind it must still be persisted and
    // broadcast.
    memory.fail_next_appends(1);
    let oversized = "x".repeat(9_000);
    log.ingest_batch(vec![
        (
            "session/update".to_owned(),
            tool_call("call-big", &oversized),
        ),
        ("session/update".to_owned(), tool_call("call-small", "tail")),
    ])
    .await;

    assert_eq!(
        store.swarm_last_seq("agent-1").await.expect("seq"),
        1,
        "the surviving group keeps the run's sequence"
    );
    let stored = store
        .swarm_items_after("agent-1", 0, 10)
        .await
        .expect("rows");
    assert_eq!(
        stored.len(),
        1,
        "only the surviving row is stored: {stored:?}"
    );
    assert_eq!(stored[0].payload["toolCallId"], json!("call-small"));
    assert_eq!(stored[0].payload["input"], json!({ "q": "tail" }));

    let broadcast = items(&drain(&mut events));
    assert_eq!(
        broadcast.len(),
        1,
        "the failed group is not broadcast; the surviving row is"
    );
    assert_eq!(broadcast[0].payload["toolCallId"], json!("call-small"));
}

/// Round 9 finding 1: the newer continuation's terminal replaces the retained
/// terminal of the continuation it superseded, so a failed third handoff
/// replays the outcome of the run that owns the turn instead of leaving it to
/// the 300s safety net.
#[tokio::test]
async fn a_newer_continuation_terminal_replaces_the_superseded_one() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);

    // A continues the turn (generation 1); its terminal arrives after B was
    // noted, so it is retained.
    assert_eq!(log.note_steer(), 1);
    log.ingest("peri/agent_event", &queued_run_started("run-a"))
        .await;
    assert_eq!(log.note_steer(), 2);
    log.ingest("peri/agent_event_done", &run_terminal("run-a", "cancelled"))
        .await;
    // A second terminal of the same run keeps the first outcome.
    log.ingest("peri/agent_event_done", &run_terminal("run-a", "end_turn"))
        .await;
    {
        let state = log.state.lock();
        let retained = state
            .suppressed_terminal
            .as_ref()
            .expect("A's terminal is retained");
        assert_eq!(retained.source.as_deref(), Some("run-a"));
        assert_eq!(
            retained.outcome,
            TurnOutcome::Completed("cancelled".to_owned()),
            "the same run's second terminal must not overwrite the first"
        );
    }

    // B claims generation 2 and C supersedes it: B's terminal replaces A's
    // retained one because B is the current continuation.
    log.ingest("peri/agent_event", &queued_run_started("run-b"))
        .await;
    assert_eq!(log.note_steer(), 3);
    log.ingest("peri/agent_event_done", &run_terminal("run-b", "end_turn"))
        .await;
    {
        let state = log.state.lock();
        let retained = state
            .suppressed_terminal
            .as_ref()
            .expect("B's terminal is retained");
        assert_eq!(retained.source.as_deref(), Some("run-b"));
        assert_eq!(
            retained.outcome,
            TurnOutcome::Completed("end_turn".to_owned())
        );
    }
    assert_eq!(
        log.active_turn().as_deref(),
        Some("req-1"),
        "steer C still owns the turn"
    );
    assert!(!drain(&mut events).iter().any(|event| matches!(
        event,
        StreamEvent::Turn(TurnEvent::Completed { .. } | TurnEvent::Failed { .. })
    )));

    // C's handoff fails: B's terminal closes the turn instead of waiting for
    // the safety net.
    log.abandon_steer(3).await;

    assert_eq!(log.active_turn(), None);
    let terminals: Vec<_> = drain(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            StreamEvent::Turn(
                terminal @ (TurnEvent::Completed { .. } | TurnEvent::Failed { .. }),
            ) => Some(terminal),
            _ => None,
        })
        .collect();
    assert_eq!(
        terminals,
        [TurnEvent::completed("req-1", "turn_1", "end_turn")]
    );
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Idle);
}

/// Round 9 finding 3: a closer that observed a continuation's terminal must not
/// close the turn once another run continued it, even though the unresolved
/// maximum fell back to the caller's snapshot when that run claimed its
/// generation.
#[tokio::test]
async fn a_terminal_from_a_run_the_turn_no_longer_continues_does_not_close() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);
    assert_eq!(log.note_steer(), 1);
    log.ingest("peri/agent_event", &queued_run_started("req-9"))
        .await;

    // A stale closer: it decided on `req-3`'s terminal with no unresolved
    // steer, but `req-9` owns the turn now.
    log.finish_turn_observed(
        "req-1",
        TurnOutcome::Completed("end_turn".to_owned()),
        Some(0),
        Some("req-3"),
    )
    .await;

    assert_eq!(
        log.active_turn().as_deref(),
        Some("req-1"),
        "a superseded run's terminal must not close the adopted run's turn"
    );
    assert!(
        !drain(&mut events).iter().any(|event| matches!(
            event,
            StreamEvent::Turn(TurnEvent::Completed { .. } | TurnEvent::Failed { .. })
        )),
        "the stale terminal must not be published"
    );
    assert!(
        log.state.lock().suppressed_terminal.is_some(),
        "the stale terminal is kept, not dropped"
    );

    // The adopted run's own terminal closes the turn: the closer yielded only
    // once.
    log.ingest("peri/agent_event_done", &run_terminal("req-9", "end_turn"))
        .await;

    assert_eq!(log.active_turn(), None);
    let terminals: Vec<_> = drain(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            StreamEvent::Turn(
                terminal @ (TurnEvent::Completed { .. } | TurnEvent::Failed { .. }),
            ) => Some(terminal),
            _ => None,
        })
        .collect();
    assert_eq!(
        terminals,
        [TurnEvent::completed("req-1", "turn_1", "end_turn")]
    );
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Idle);
}

/// Round 9 finding 3: the prompt attempt's terminal must not close a turn a
/// continuation owns, even when the caller saw no unresolved steer.
#[tokio::test]
async fn a_prompt_terminal_does_not_close_a_turn_a_continuation_owns() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);
    assert_eq!(log.note_steer(), 1);
    log.ingest("peri/agent_event", &queued_run_started("req-9"))
        .await;

    log.finish_turn_observed(
        "req-1",
        TurnOutcome::Completed("cancelled".to_owned()),
        Some(0),
        None,
    )
    .await;

    assert_eq!(
        log.active_turn().as_deref(),
        Some("req-1"),
        "the prompt attempt's terminal must not close the adopted run's turn"
    );
    assert!(
        !drain(&mut events).iter().any(|event| matches!(
            event,
            StreamEvent::Turn(TurnEvent::Completed { .. } | TurnEvent::Failed { .. })
        )),
        "the prompt terminal must not be published"
    );
    assert!(
        log.state.lock().suppressed_terminal.is_some(),
        "the prompt terminal is kept as the handoff fallback"
    );

    // The adopted run's own terminal closes the turn: the closer yielded only
    // once.
    log.ingest("peri/agent_event_done", &run_terminal("req-9", "end_turn"))
        .await;

    assert_eq!(log.active_turn(), None);
    let terminals: Vec<_> = drain(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            StreamEvent::Turn(
                terminal @ (TurnEvent::Completed { .. } | TurnEvent::Failed { .. }),
            ) => Some(terminal),
            _ => None,
        })
        .collect();
    assert_eq!(
        terminals,
        [TurnEvent::completed("req-1", "turn_1", "end_turn")]
    );
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Idle);
}

/// Round 9 finding 3 regression on the production dispatch path: a prompt
/// terminal ingested through `ingest` waits in `settle` while a steer is noted,
/// rolled back, and a second steer is adopted by its queued run. The closer
/// must keep the prompt terminal instead of closing the turn the adopted run
/// now owns.
#[tokio::test(start_paused = true)]
async fn a_prompt_terminal_in_settle_yields_to_a_run_adopted_meanwhile() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);

    // The prompt attempt's real terminal enters the production dispatch path
    // with no steer pending, so the closer decides on `expected = Some(0)` with
    // `source = None` and starts `settle`.
    let finishing = {
        let log = Arc::clone(&log);
        tokio::spawn(async move {
            log.ingest("peri/agent_event_done", &run_terminal("req-1", "cancelled"))
                .await;
        })
    };
    // Under the paused clock the closer already holds its decision snapshot
    // and is waiting in `settle`, whose window is larger than 1ms.
    tokio::time::sleep(Duration::from_millis(1)).await;

    // A steer is admitted and rolled back, then a second steer is handed off
    // and claimed by its queued run: `max_unresolved()` falls back to 0 while a
    // new continuation owns the turn.
    assert_eq!(log.note_steer(), 1);
    log.abandon_steer(1).await;
    assert_eq!(log.note_steer(), 2);
    log.ingest("peri/agent_event", &queued_run_started("req-2"))
        .await;
    finishing.await.expect("finish_turn");

    assert_eq!(
        log.active_turn().as_deref(),
        Some("req-1"),
        "the prompt terminal must not close the turn the adopted run owns"
    );
    assert!(
        !drain(&mut events).iter().any(|event| matches!(
            event,
            StreamEvent::Turn(TurnEvent::Completed { .. } | TurnEvent::Failed { .. })
        )),
        "the prompt terminal must not be published"
    );
    {
        let state = log.state.lock();
        let retained = state
            .suppressed_terminal
            .as_ref()
            .expect("the prompt terminal is kept as the handoff fallback");
        assert_eq!(retained.request_id, "req-1");
        assert_eq!(retained.source, None);
        assert_eq!(
            retained.outcome,
            TurnOutcome::Completed("cancelled".to_owned())
        );
    }

    // The adopted run's own terminal is the turn's: it closes the turn and its
    // `end_turn` — not the retained prompt `cancelled` — is published.
    log.ingest("peri/agent_event_done", &run_terminal("req-2", "end_turn"))
        .await;

    assert_eq!(log.active_turn(), None);
    let terminals: Vec<_> = drain(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            StreamEvent::Turn(
                terminal @ (TurnEvent::Completed { .. } | TurnEvent::Failed { .. }),
            ) => Some(terminal),
            _ => None,
        })
        .collect();
    assert_eq!(
        terminals,
        [TurnEvent::completed("req-1", "turn_1", "end_turn")],
        "only the adopted run's terminal closes the turn"
    );
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Idle);
}

/// F1: a completion flush whose first store group fails keeps the group that
/// did commit. The committed row is broadcast before the failure is reported,
/// and the turn ends failed instead of "completed without that text".
#[tokio::test]
async fn a_partially_failed_completion_flush_broadcasts_the_committed_row() {
    let memory = Arc::new(MemorySwarmStore::new());
    memory
        .swarm_upsert_session(session_row())
        .await
        .expect("session row");
    let store: Arc<dyn SwarmStore> = memory.clone();
    let (channel, mut events) = broadcast::channel(64);
    let log = Arc::new(ItemLog::new(
        "agent-1",
        Arc::clone(&store),
        channel,
        Arc::new(AtomicU64::new(0)),
    ));
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    // The oversized stream owns the first store group (the batch splitter
    // isolates a >8 KiB payload); the small stream follows in its own group.
    let big = "x".repeat(9_000);
    log.ingest("session/update", &chunk_for("msg-big", &big))
        .await;
    log.ingest("session/update", &chunk_for("msg-small", "tail"))
        .await;
    drain(&mut events);

    memory.fail_next_appends(1);
    log.finish_turn("req-1", TurnOutcome::Completed("end_turn".into()))
        .await;

    let seen = drain(&mut events);
    let written = items(&seen);
    assert_eq!(
        written.len(),
        1,
        "the committed group is broadcast; the failed one is not: {seen:?}"
    );
    assert_eq!(written[0].payload["messageId"], json!("msg-small"));
    assert_eq!(written[0].payload["text"], json!("tail"));
    assert_eq!(written[0].payload["complete"], json!(true));
    let committed_position = seen
        .iter()
        .position(|event| matches!(event, StreamEvent::Item(_)))
        .expect("the committed row is broadcast");
    let terminal_position = seen
        .iter()
        .position(|event| matches!(event, StreamEvent::Turn(TurnEvent::Failed { .. })))
        .expect("the failed terminal follows");
    assert!(
        committed_position < terminal_position,
        "the committed group must be broadcast before the turn failure: {seen:?}"
    );
    assert!(
        matches!(
            seen.iter()
                .find(|event| matches!(event, StreamEvent::Turn(_))),
            Some(StreamEvent::Turn(TurnEvent::Failed { .. }))
        ),
        "a flush error fails the turn instead of completing it: {seen:?}"
    );
    assert!(
        !official_types(&seen).contains(&"agent.session.turn.completed"),
        "no successful terminal may follow a failed flush: {seen:?}"
    );

    let rows = store
        .swarm_items_after("agent-1", 0, 10)
        .await
        .expect("rows");
    assert_eq!(
        rows.len(),
        1,
        "only the committed group is durable: {rows:?}"
    );
    assert_eq!(rows[0].payload["messageId"], json!("msg-small"));
    assert_eq!(store.swarm_last_seq("agent-1").await.expect("seq"), 1);
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Failed);
}

/// F1: a flush that persists nothing must not end as a normal
/// `turn.completed` ("successful but no text"). The visible failure is the
/// turn terminal and the session state.
#[tokio::test]
async fn a_completion_flush_that_persists_nothing_fails_the_turn() {
    let memory = Arc::new(MemorySwarmStore::new());
    memory
        .swarm_upsert_session(session_row())
        .await
        .expect("session row");
    let store: Arc<dyn SwarmStore> = memory.clone();
    let (channel, mut events) = broadcast::channel(64);
    let log = Arc::new(ItemLog::new(
        "agent-1",
        Arc::clone(&store),
        channel,
        Arc::new(AtomicU64::new(0)),
    ));
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    log.ingest("session/update", &chunk("hello")).await;
    drain(&mut events);

    memory.fail_next_appends(1);
    log.finish_turn("req-1", TurnOutcome::Completed("end_turn".into()))
        .await;

    let seen = drain(&mut events);
    assert!(
        items(&seen).is_empty(),
        "the rejected merged row is not broadcast: {seen:?}"
    );
    assert!(
        seen.iter()
            .any(|event| matches!(event, StreamEvent::Turn(TurnEvent::Failed { .. }))),
        "the turn terminal reports the failure: {seen:?}"
    );
    assert!(
        !seen
            .iter()
            .any(|event| matches!(event, StreamEvent::Turn(TurnEvent::Completed { .. }))),
        "a turn whose whole output was lost is not completed: {seen:?}"
    );
    let types = official_types(&seen);
    assert!(
        !types.contains(&"agent.session.turn.completed"),
        "no official completion either: {types:?}"
    );
    assert!(
        types.contains(&"agent.session.turn.failed"),
        "the official terminal is a failure: {types:?}"
    );
    assert_eq!(store.swarm_last_seq("agent-1").await.expect("seq"), 0);
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Failed);
}

/// A cancelled turn keeps its cancelled terminal even when its completion row
/// cannot be persisted: the user's cancel is the outcome, the lost text is
/// logged, and the session still returns to idle. Before the fix the flush
/// failure rewrote `Completed("cancelled")` into `Failed`, so the cancel
/// vanished from the live frames, `turns.retrieve` and the session state.
#[tokio::test]
async fn a_cancelled_turn_stays_cancelled_when_its_completion_flush_fails() {
    let memory = Arc::new(MemorySwarmStore::new());
    memory
        .swarm_upsert_session(session_row())
        .await
        .expect("session row");
    let store: Arc<dyn SwarmStore> = memory.clone();
    let (channel, mut events) = broadcast::channel(64);
    let log = Arc::new(ItemLog::new(
        "agent-1",
        Arc::clone(&store),
        channel,
        Arc::new(AtomicU64::new(0)),
    ));
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    log.ingest("session/update", &chunk("hello")).await;
    drain(&mut events);

    memory.fail_next_appends(1);
    log.finish_turn("req-1", TurnOutcome::Completed("cancelled".into()))
        .await;

    let seen = drain(&mut events);
    assert!(
        items(&seen).is_empty(),
        "the lost completion row is not broadcast: {seen:?}"
    );
    let terminals: Vec<_> = seen
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Turn(
                terminal @ (TurnEvent::Completed { .. } | TurnEvent::Failed { .. }),
            ) => Some(terminal.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        terminals,
        [TurnEvent::completed("req-1", "turn_1", "cancelled")],
        "the cancel terminal survives a lost completion row: {seen:?}"
    );
    let types = official_types(&seen);
    assert!(
        types.contains(&"agent.session.turn.completed"),
        "the official terminal is still a completion: {types:?}"
    );
    assert!(
        !types.contains(&"agent.session.turn.failed"),
        "a cancel must not be rewritten as a failure: {types:?}"
    );
    assert!(
        !types.contains(&"agent.session.failed"),
        "the session is not failed by a lost completion row: {types:?}"
    );
    let official = seen
        .iter()
        .find_map(|event| match event {
            StreamEvent::Official(OfficialEvent::TurnCompleted { turn, .. }) => Some(turn),
            _ => None,
        })
        .expect("turn.completed");
    assert_eq!(official.status, "cancelled");

    let turn = store
        .swarm_get_turn("agent-1", "turn_1")
        .await
        .expect("turn row")
        .expect("row");
    assert_eq!(
        turn.status,
        TurnStatus::Cancelled,
        "turns.retrieve keeps reporting the cancel"
    );
    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Idle);
}

/// F3: a delta staged before its stream's terminal is not broadcast after the
/// merged row; the merged row itself still goes out.
#[tokio::test]
async fn a_staged_delta_is_dropped_when_its_stream_closed_before_the_flush() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);

    // Stage the chunk without flushing it, then let the terminal close the
    // stream: this is the interleaving the broadcast-time check must catch.
    let mut staged = StagedItems::default();
    for event in crate::actor::projection::project("session/update", &chunk("hi")) {
        if let crate::actor::projection::Projected::Item(item) = event {
            log.stage_item(item, &mut staged);
        }
    }
    log.finish_turn("req-1", TurnOutcome::Completed("end_turn".into()))
        .await;
    let terminal = drain(&mut events);
    assert_eq!(items(&terminal).len(), 1, "the merged row is written");
    assert_eq!(items(&terminal)[0].payload["text"], json!("hi"));

    log.flush_staged(&mut staged).await;
    let after = drain(&mut events);
    assert!(
        deltas(&after).is_empty(),
        "a delta must not follow its stream's complete row: {after:?}"
    );
    let rows = store
        .swarm_items_after("agent-1", 0, 10)
        .await
        .expect("rows");
    assert_eq!(rows.len(), 1, "the dropped delta wrote nothing: {rows:?}");
}

/// One `usage_update` as `peri-acp` emits it when `token_stats` is negotiated.
fn usage_update(
    input: u64,
    output: u64,
    cache_read: u64,
    cache_creation: u64,
    used: u64,
    size: u64,
) -> serde_json::Value {
    json!({
        "sessionId": "agent-1",
        "update": {
            "sessionUpdate": "usage_update",
            "used": used,
            "size": size,
            "_meta": {
                "inputTokens": input,
                "outputTokens": output,
                "cacheReadTokens": cache_read,
                "cacheCreationTokens": cache_creation,
                "requestId": "req-usage",
                "model": "claude-test",
            },
        },
    })
}

/// The turn accumulator is persisted by every observation and by the terminal;
/// no write in between may erase it.
#[tokio::test]
async fn usage_updates_accumulate_into_the_open_turn_and_survive_the_terminal() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    let opening = store
        .swarm_get_turn("agent-1", "turn_1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(opening.usage, None, "opening a turn observes no usage");

    log.ingest(
        "session/update",
        &usage_update(1_000, 10, 600, 100, 950, 200_000),
    )
    .await;
    let observed = store
        .swarm_get_turn("agent-1", "turn_1")
        .await
        .expect("read")
        .expect("row");
    let first = observed
        .usage
        .clone()
        .expect("the observation persisted itself");
    assert_eq!(first["input_tokens"], json!(300), "cache leaves input");
    assert_eq!(first["output_tokens"], json!(10));
    assert_eq!(first["cache_read_tokens"], json!(600));
    assert_eq!(first["cache_creation_tokens"], json!(100));
    assert_eq!(first["total_tokens"], json!(1_010));
    assert_eq!(first["context_used"], json!(950));
    assert_eq!(first["context_size"], json!(200_000));
    assert_eq!(first["model"], json!("claude-test"));
    assert_eq!(first["request_id"], json!("req-usage"));
    assert_eq!(first["calls"], json!(1));

    log.ingest(
        "session/update",
        &usage_update(200, 5, 0, 0, 1_200, 200_000),
    )
    .await;
    let observed = store
        .swarm_get_turn("agent-1", "turn_1")
        .await
        .expect("read")
        .expect("row");
    let second = observed.usage.expect("second observation");
    assert_eq!(second["input_tokens"], json!(500), "counters add up");
    assert_eq!(second["output_tokens"], json!(15));
    assert_eq!(second["total_tokens"], json!(1_215));
    assert_eq!(
        second["context_used"],
        json!(1_200),
        "context keeps the latest"
    );
    assert_eq!(second["calls"], json!(2));

    log.finish_turn("req-1", TurnOutcome::Completed("end_turn".into()))
        .await;
    let closed = store
        .swarm_get_turn("agent-1", "turn_1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(closed.status, TurnStatus::Completed);
    assert_eq!(
        closed.usage,
        Some(second.clone()),
        "the terminal write keeps the accumulated usage"
    );
    let frame_usage = drain(&mut events)
        .into_iter()
        .find_map(|event| match event {
            StreamEvent::Official(OfficialEvent::TurnCompleted { usage, .. }) => Some(usage),
            _ => None,
        })
        .expect("turn.completed");
    assert_eq!(
        frame_usage,
        Some(second),
        "the live frame and turns.retrieve agree"
    );
}

/// A usage frame that arrives after the terminal belongs to no turn: it must
/// neither extend the closed turn nor leak into the next one.
#[tokio::test]
async fn usage_after_the_terminal_is_not_attributed_to_the_next_turn() {
    let Fixture { log, store, .. } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    log.ingest("session/update", &usage_update(100, 1, 0, 0, 10, 1_000))
        .await;
    log.finish_turn("req-1", TurnOutcome::Completed("end_turn".into()))
        .await;
    let closed = store
        .swarm_get_turn("agent-1", "turn_1")
        .await
        .expect("read")
        .expect("row");
    let closed_usage = closed.usage.expect("usage");

    log.ingest(
        "session/update",
        &usage_update(9_999, 999, 0, 0, 999, 1_000),
    )
    .await;
    let still_closed = store
        .swarm_get_turn("agent-1", "turn_1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(
        still_closed.usage,
        Some(closed_usage),
        "a late usage frame does not change the closed row"
    );

    log.begin_turn("req-2", "turn_2")
        .await
        .expect("second turn");
    let next = store
        .swarm_get_turn("agent-1", "turn_2")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(
        next.usage, None,
        "the next turn starts with a clear accumulator"
    );
}
