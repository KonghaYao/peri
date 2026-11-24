use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use serde_json::json;
use tokio::sync::broadcast;

use super::{ItemLog, TurnOutcome};
use crate::domain::{
    turn_id_for_item, ItemKind, OfficialEvent, PromptCacheOptions, PromptCacheTtl, SessionState,
    StreamEvent, TurnEvent,
};
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
async fn a_batched_burst_keeps_host_order_and_consecutive_seqs() {
    let Fixture {
        log,
        store,
        mut events,
    } = fixture().await;

    log.begin_turn("req-1", "turn_1").await.expect("turn");
    log.ingest_batch(vec![
        ("session/update".to_owned(), chunk("he")),
        ("session/update".to_owned(), chunk("llo")),
    ])
    .await;

    let items: Vec<_> = drain(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            StreamEvent::Item(item) => Some(item),
            _ => None,
        })
        .collect();
    assert_eq!(items.len(), 2, "one delta item per chunk");
    assert_eq!(items[0].seq + 1, items[1].seq, "the run is contiguous");
    assert_eq!(items[0].item_id, items[1].item_id, "one message identity");
    assert_eq!(items[0].payload["text"], "he");
    assert_eq!(items[1].payload["text"], "llo");
    assert_eq!(store.swarm_last_seq("agent-1").await.expect("seq"), 2);

    let row = store
        .swarm_get_session("agent-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SessionState::Running);
    assert_eq!(row.last_item_seq, 2, "the cursor moves with the batch");

    // The turn's completion still flushes the whole streamed text under the
    // stable identity, after the batched deltas.
    log.finish_turn("req-1", TurnOutcome::Completed("end_turn".into()))
        .await;
    let completion = drain(&mut events)
        .into_iter()
        .find_map(|event| match event {
            StreamEvent::Item(item) if item.payload["complete"] == true => Some(item),
            _ => None,
        })
        .expect("completion item");
    assert_eq!(completion.item_id, items[0].item_id);
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
            "agent.session.turn.output_text.done",
            "agent.session.turn.content_part.done",
            "agent.session.turn.item.done",
            "agent.session.turn.completed",
            "agent.session.idle",
        ],
        "{seen:?}"
    );
    let completion_seq = seen
        .iter()
        .find_map(|event| match event {
            StreamEvent::Item(item) if item.payload["complete"] == json!(true) => Some(item.seq),
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
            format!("evt_item_{completion_seq}_added"),
            format!("evt_item_{completion_seq}_part_added"),
            format!("evt_item_{completion_seq}_text_done"),
            format!("evt_item_{completion_seq}_part_done"),
            format!("evt_item_{completion_seq}_done"),
        ]
    );
}

#[tokio::test]
async fn interleaved_assistant_messages_share_the_turn_by_first_chunk_seq() {
    let Fixture {
        log, mut events, ..
    } = fixture().await;
    log.begin_turn("req-1", "turn_1").await.expect("turn");
    drain(&mut events);
    // `msg-b` opens first and `msg-a` interleaves. Message ids must not decide
    // the output order; the first persisted chunk does.
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
async fn thought_first_chunk_later_does_not_steal_final_answer() {
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
