use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use serde_json::json;
use tokio::sync::broadcast;

use super::{ItemLog, TurnOutcome};
use crate::domain::{turn_id_for_item, ItemKind, SessionState, StreamEvent, TurnEvent};
use crate::store::{MemorySwarmStore, SwarmSessionRow, SwarmStore};

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
        environment: crate::domain::EnvironmentRef::none(
            &crate::domain::LogicalId::parse("agent-1").expect("logical id"),
        ),
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
    json!({
        "sessionId": "agent-1",
        "update": {
            "sessionUpdate": "agent_message_chunk",
            "messageId": "msg-1",
            "content": { "type": "text", "text": text },
        },
    })
}

fn drain(receiver: &mut broadcast::Receiver<StreamEvent>) -> Vec<StreamEvent> {
    let mut events = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        events.push(event);
    }
    events
}

#[tokio::test]
async fn text_deltas_share_one_item_id_and_completion_carries_full_text() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;

    log.begin_turn("req-1", "turn_1").await.expect("turn");
    log.ingest("session/update", &chunk("hel")).await;
    log.ingest("session/update", &chunk("lo")).await;
    log.finish_turn("req-1", TurnOutcome::Completed("end_turn".into()))
        .await;

    let seen = drain(&mut events);
    let items: Vec<_> = seen
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Item(item) => Some(item.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(items.len(), 3, "two deltas plus one completion item");
    assert!(items.iter().all(|item| item.item_id == items[0].item_id));
    assert_eq!(items[0].payload["role"], "assistant");
    assert_eq!(items[0].payload["text"], "hel");
    assert_eq!(items[0].payload["delta"], true);
    assert_eq!(items[2].payload["role"], "assistant");
    assert_eq!(items[2].payload["text"], "hello");
    assert_eq!(items[2].payload["complete"], true);
    assert!(items[0].seq < items[1].seq && items[1].seq < items[2].seq);

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
    assert_eq!(row.last_item_seq, 3);
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
        log, mut events, ..
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
    let items: Vec<_> = seen
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Item(item) => Some(item),
            _ => None,
        })
        .collect();
    assert_eq!(items.len(), 2);
    assert!(items
        .iter()
        .all(|item| item.kind == crate::domain::ItemKind::Thought));
    assert_eq!(items[1].payload["text"], "why");
}
