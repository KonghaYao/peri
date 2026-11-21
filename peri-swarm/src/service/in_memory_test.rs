use std::sync::Arc;

use super::*;
use crate::domain::{EventCursor, ItemOrder};
use crate::store::MemorySwarmStore;

fn create_command(logical_id: &str, cwd: &str) -> CreateSessionCommand {
    CreateSessionCommand {
        logical_id: LogicalId::parse(logical_id).expect("logical id"),
        cwd: std::path::PathBuf::from(cwd),
        model: None,
        instructions: None,
        mcp_tools: Vec::new(),
        initial_input: None,
        environment: EnvironmentKind::None,
        environment_id: None,
        root_session_id: None,
    }
}

fn submission(input_id: &str, text: &str) -> InputSubmission {
    InputSubmission {
        input_id: input_id.to_owned(),
        kind: InputKind::User,
        mode: InputMode::Turn,
        text: Some(text.to_owned()),
        attachments: Vec::new(),
        plugin_selections: Vec::new(),
        original_draft: None,
    }
}

async fn submit(service: &InMemorySwarmService, logical_id: &str, input_id: &str, text: &str) {
    let id = LogicalId::parse(logical_id).expect("logical id");
    service
        .submit_input(&id, submission(input_id, text))
        .await
        .expect("submit");
}

#[tokio::test]
async fn create_honors_the_reserved_root_session_id() {
    let service = InMemorySwarmService::in_memory(ServiceConfig::default());
    let reserved = "0192f0c5-0000-7000-8000-000000000042".to_owned();
    let mut command = create_command("agent-01", "/tmp/agent-01");
    command.root_session_id = Some(reserved.clone());
    let created = service.create_session(command).await.expect("create");
    assert_eq!(created.session.root_session_id, reserved);
    // The identity is retained by the repeat create through the store.
    let repeated = service
        .create_session(create_command("agent-01", "/tmp/agent-01"))
        .await
        .expect("repeat create");
    assert!(!repeated.created);
    assert_eq!(repeated.session.root_session_id, reserved);
}

#[tokio::test]
async fn create_then_get_returns_the_same_identity() {
    let service = InMemorySwarmService::in_memory(ServiceConfig::default());
    let created = service
        .create_session(create_command("agent-01", "/tmp/agent-01"))
        .await
        .expect("create");
    assert!(created.created);
    assert_eq!(created.session.logical_id, "agent-01");
    assert_eq!(created.session.id, "agent-01");
    assert_eq!(created.session.object, "agent.session");
    assert_eq!(created.session.state, SessionState::Idle);
    assert_eq!(created.session.last_item_seq, 0);
    assert!(created.session.sandbox.is_none());
    assert_eq!(created.session.environment.kind, EnvironmentKind::None);

    let fetched = service
        .get_session(&LogicalId::parse("agent-01").expect("id"))
        .await
        .expect("get");
    assert_eq!(fetched.root_session_id, created.session.root_session_id);
    assert_eq!(fetched.cwd, "/tmp/agent-01");
}

#[tokio::test]
async fn create_reuses_identity_and_rejects_changes() {
    let service = InMemorySwarmService::in_memory(ServiceConfig::default());
    service
        .create_session(create_command("agent-01", "/tmp/agent-01"))
        .await
        .expect("create");

    let reused = service
        .create_session(create_command("agent-01", "/tmp/agent-01"))
        .await
        .expect("reuse");
    assert!(!reused.created);

    let conflict = service
        .create_session(create_command("agent-01", "/tmp/other"))
        .await
        .expect_err("cwd conflict");
    assert_eq!(conflict.code(), ErrorCode::IdentityConflict);

    let mut with_instructions = create_command("agent-01", "/tmp/agent-01");
    with_instructions.instructions = Some("do the thing".to_owned());
    let conflict = service
        .create_session(with_instructions)
        .await
        .expect_err("instructions conflict");
    assert_eq!(conflict.code(), ErrorCode::IdentityConflict);
}

#[tokio::test]
async fn model_must_match_the_deployment_config() {
    let service = InMemorySwarmService::in_memory(ServiceConfig {
        model: Some("deployment-model".to_owned()),
        ..ServiceConfig::default()
    });
    let mut command = create_command("agent-01", "/tmp/agent-01");
    command.model = Some("other-model".to_owned());
    let error = service.create_session(command).await.expect_err("model");
    assert_eq!(error.code(), ErrorCode::ModelNotSupported);
}

#[tokio::test]
async fn input_idempotency_and_conflict() {
    let service = InMemorySwarmService::in_memory(ServiceConfig::default());
    service
        .create_session(create_command("agent-01", "/tmp/agent-01"))
        .await
        .expect("create");
    let id = LogicalId::parse("agent-01").expect("id");

    let first = service
        .submit_input(&id, submission("input-1", "hello"))
        .await
        .expect("first");
    assert_eq!(first.disposition, Disposition::Inserted);
    // One user message at acceptance plus the fake assistant reply.
    assert_eq!(first.last_item_seq, 2);

    let replay = service
        .submit_input(&id, submission("input-1", "hello"))
        .await
        .expect("replay");
    assert_eq!(replay.disposition, Disposition::Existing);
    assert_eq!(replay.last_item_seq, 2);

    let conflict = service
        .submit_input(&id, submission("input-1", "different"))
        .await
        .expect_err("conflict");
    assert_eq!(conflict.code(), ErrorCode::InputConflict);

    let page = service
        .list_items(
            &id,
            ItemsWindow {
                order: ItemOrder::Asc,
                after: None,
                after_id: None,
                limit: 10,
            },
        )
        .await
        .expect("items");
    assert_eq!(
        page.items.iter().map(|item| item.seq).collect::<Vec<_>>(),
        [1, 2],
        "a replayed inputId must not append a second user item"
    );
    assert_eq!(page.items[0].payload["role"], serde_json::json!("user"));
    assert_eq!(page.items[0].payload["text"], serde_json::json!("hello"));
    assert_eq!(page.items[0].item_id, "itim_input_input-1");
    assert_eq!(
        page.items[1].payload["text"],
        serde_json::json!("fake-echo: hello")
    );
}

#[tokio::test]
async fn empty_input_is_rejected() {
    let service = InMemorySwarmService::in_memory(ServiceConfig::default());
    service
        .create_session(create_command("agent-01", "/tmp/agent-01"))
        .await
        .expect("create");
    let id = LogicalId::parse("agent-01").expect("id");
    let mut empty = submission("input-1", "");
    empty.text = Some("  ".to_owned());
    let error = service.submit_input(&id, empty).await.expect_err("empty");
    assert_eq!(error.code(), ErrorCode::EmptyInput);
}

#[tokio::test]
async fn turn_in_progress_and_steer() {
    let service = InMemorySwarmService::in_memory(ServiceConfig {
        turn_policy: TurnPolicy::Manual,
        ..ServiceConfig::default()
    });
    service
        .create_session(create_command("agent-01", "/tmp/agent-01"))
        .await
        .expect("create");
    let id = LogicalId::parse("agent-01").expect("id");
    service
        .submit_input(&id, submission("input-1", "long task"))
        .await
        .expect("first");
    let session = service.get_session(&id).await.expect("get");
    assert_eq!(session.state, SessionState::Running);

    let error = service
        .submit_input(&id, submission("input-2", "second"))
        .await
        .expect_err("busy");
    assert_eq!(error.code(), ErrorCode::TurnInProgress);
    assert_eq!(error.details()["suggest"], serde_json::json!("steer"));

    let mut steer = submission("input-3", "adjust");
    steer.mode = InputMode::Steer;
    let accepted = service.submit_input(&id, steer).await.expect("steer");
    assert_eq!(accepted.disposition, Disposition::Inserted);
    // user(1) + fake reply(2) + steered user input(3).
    assert_eq!(accepted.last_item_seq, 3);
    let page = service
        .list_items(
            &id,
            ItemsWindow {
                order: ItemOrder::Asc,
                after: None,
                after_id: None,
                limit: 10,
            },
        )
        .await
        .expect("items");
    assert_eq!(page.items[2].payload["role"], serde_json::json!("user"));
    assert_eq!(page.items[2].payload["text"], serde_json::json!("adjust"));
}

#[tokio::test]
async fn control_input_is_recorded_without_a_turn() {
    let service = InMemorySwarmService::in_memory(ServiceConfig::default());
    service
        .create_session(create_command("agent-01", "/tmp/agent-01"))
        .await
        .expect("create");
    let id = LogicalId::parse("agent-01").expect("id");
    let mut control = submission("input-1", "cancel");
    control.kind = InputKind::Control;
    let outcome = service.submit_input(&id, control).await.expect("control");
    assert_eq!(outcome.disposition, Disposition::Inserted);
    assert_eq!(outcome.last_item_seq, 0);
    assert_eq!(
        service.get_session(&id).await.expect("get").state,
        SessionState::Idle
    );
}

#[tokio::test]
async fn pagination_is_stable_in_both_orders() {
    let service = InMemorySwarmService::in_memory(ServiceConfig::default());
    service
        .create_session(create_command("agent-01", "/tmp/agent-01"))
        .await
        .expect("create");
    for index in 1..=5 {
        submit(
            &service,
            "agent-01",
            &format!("input-{index}"),
            &format!("message {index}"),
        )
        .await;
    }
    let id = LogicalId::parse("agent-01").expect("id");

    let first = service
        .list_items(
            &id,
            ItemsWindow {
                order: ItemOrder::Asc,
                after: None,
                after_id: None,
                limit: 2,
            },
        )
        .await
        .expect("page 1");
    assert_eq!(
        first.items.iter().map(|item| item.seq).collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(first.next_cursor, Some(2));
    assert!(first.has_more);
    // Five turns, each appending user + fake assistant.
    assert_eq!(first.last_item_seq, 10);

    let second = service
        .list_items(
            &id,
            ItemsWindow {
                order: ItemOrder::Asc,
                after: Some(2),
                after_id: None,
                limit: 2,
            },
        )
        .await
        .expect("page 2");
    assert_eq!(
        second.items.iter().map(|item| item.seq).collect::<Vec<_>>(),
        [3, 4]
    );
    assert_eq!(second.next_cursor, Some(4));

    let third = service
        .list_items(
            &id,
            ItemsWindow {
                order: ItemOrder::Asc,
                after: Some(8),
                after_id: None,
                limit: 2,
            },
        )
        .await
        .expect("page 3");
    assert_eq!(
        third.items.iter().map(|item| item.seq).collect::<Vec<_>>(),
        [9, 10]
    );
    assert_eq!(third.next_cursor, None);
    assert!(!third.has_more);

    let newest = service
        .list_items(
            &id,
            ItemsWindow {
                order: ItemOrder::Desc,
                after: None,
                after_id: None,
                limit: 2,
            },
        )
        .await
        .expect("desc 1");
    assert_eq!(
        newest.items.iter().map(|item| item.seq).collect::<Vec<_>>(),
        [10, 9]
    );
    assert_eq!(newest.next_cursor, Some(9));

    let older = service
        .list_items(
            &id,
            ItemsWindow {
                order: ItemOrder::Desc,
                after: Some(9),
                after_id: None,
                limit: 2,
            },
        )
        .await
        .expect("desc 2");
    assert_eq!(
        older.items.iter().map(|item| item.seq).collect::<Vec<_>>(),
        [8, 7]
    );

    let oldest = service
        .list_items(
            &id,
            ItemsWindow {
                order: ItemOrder::Desc,
                after: Some(3),
                after_id: None,
                limit: 2,
            },
        )
        .await
        .expect("desc 3");
    assert_eq!(
        oldest.items.iter().map(|item| item.seq).collect::<Vec<_>>(),
        [2, 1]
    );
    assert_eq!(oldest.next_cursor, None);
}

#[tokio::test]
async fn subscribe_replays_from_the_cursor() {
    let service = InMemorySwarmService::in_memory(ServiceConfig::default());
    service
        .create_session(create_command("agent-01", "/tmp/agent-01"))
        .await
        .expect("create");
    for index in 1..=3 {
        submit(
            &service,
            "agent-01",
            &format!("input-{index}"),
            &format!("message {index}"),
        )
        .await;
    }
    let id = LogicalId::parse("agent-01").expect("id");
    let subscription = service
        .subscribe(&id, EventCursor::after(1))
        .await
        .expect("subscribe");
    let seqs: Vec<u64> = subscription
        .backlog()
        .iter()
        .filter_map(StreamEvent::item_seq)
        .collect();
    assert_eq!(seqs, [2, 3, 4, 5, 6]);
    assert_eq!(
        subscription
            .tail()
            .load(std::sync::atomic::Ordering::Relaxed),
        6,
        "the subscription tail is the persisted MAX(seq)"
    );
}

#[tokio::test]
async fn stream_is_opened_before_the_first_turn_executes() {
    let service = InMemorySwarmService::in_memory(ServiceConfig::default());
    let id = LogicalId::parse("agent-01").expect("id");
    let subscription = service.open_stream(&id).await;
    let mut command = create_command("agent-01", "/tmp/agent-01");
    command.initial_input = Some("hello".to_owned());
    service.create_session(command).await.expect("create");

    let (backlog, mut live) = subscription.into_parts();
    assert!(backlog.is_empty());
    // The input is logged before the turn starts, so a stream opened first sees
    // the user frame even though the create call runs the turn synchronously.
    // The official `turn.created` and the user's `turn.item.added` follow the
    // persisted frame, as the official replay does.
    let user = live.recv().await.expect("user item");
    assert_eq!(user.item_seq(), Some(1));
    match user {
        StreamEvent::Item(item) => {
            assert_eq!(item.kind, ItemKind::Message);
            assert_eq!(item.payload["role"], serde_json::json!("user"));
            assert_eq!(item.payload["text"], serde_json::json!("hello"));
        }
        other => panic!("expected the user item, got {other:?}"),
    }
    let created = live.recv().await.expect("turn created");
    assert!(
        matches!(
            created,
            StreamEvent::Official(OfficialEvent::TurnCreated { .. })
        ),
        "{created:?}"
    );
    let added = live.recv().await.expect("user item added");
    assert!(
        matches!(
            added,
            StreamEvent::Official(OfficialEvent::TurnItemAdded { .. })
        ),
        "{added:?}"
    );
    let started = live.recv().await.expect("started");
    assert!(matches!(
        started,
        StreamEvent::Turn(TurnEvent::Started { .. })
    ));
    let item = live.recv().await.expect("assistant item");
    assert_eq!(item.item_seq(), Some(2));
    let added = live.recv().await.expect("assistant item added");
    assert!(
        matches!(
            added,
            StreamEvent::Official(OfficialEvent::TurnItemAdded { .. })
        ),
        "{added:?}"
    );
    let completed = live.recv().await.expect("completed");
    assert!(matches!(
        completed,
        StreamEvent::Turn(TurnEvent::Completed { .. })
    ));
    let official = live.recv().await.expect("official turn completed");
    assert!(
        matches!(
            official,
            StreamEvent::Official(OfficialEvent::TurnCompleted { .. })
        ),
        "{official:?}"
    );
    let idle = live.recv().await.expect("session idle");
    assert!(
        matches!(
            idle,
            StreamEvent::Official(OfficialEvent::SessionIdle { .. })
        ),
        "{idle:?}"
    );

    assert_eq!(
        service.calls(),
        vec![
            ServiceCall::OpenStream("agent-01".to_owned()),
            ServiceCall::CreateSession("agent-01".to_owned()),
        ]
    );
}

#[tokio::test]
async fn sequence_resumes_from_the_store_after_restart() {
    let store: Arc<dyn SwarmStore> = Arc::new(MemorySwarmStore::new());
    let first = InMemorySwarmService::new(store.clone(), ServiceConfig::default());
    first
        .create_session(create_command("agent-01", "/tmp/agent-01"))
        .await
        .expect("create");
    submit(&first, "agent-01", "input-1", "before restart").await;
    drop(first);

    let second = InMemorySwarmService::new(store, ServiceConfig::default());
    submit(&second, "agent-01", "input-2", "after restart").await;
    let id = LogicalId::parse("agent-01").expect("id");
    let page = second
        .list_items(
            &id,
            ItemsWindow {
                order: ItemOrder::Asc,
                after: None,
                after_id: None,
                limit: 10,
            },
        )
        .await
        .expect("items");
    assert_eq!(
        page.items.iter().map(|item| item.seq).collect::<Vec<_>>(),
        [1, 2, 3, 4]
    );
    assert_eq!(page.last_item_seq, 4);
    assert_eq!(page.items[0].payload["role"], serde_json::json!("user"));
    assert_eq!(page.items[2].payload["role"], serde_json::json!("user"));
    assert_eq!(
        page.items[2].payload["text"],
        serde_json::json!("after restart")
    );
}

#[tokio::test]
async fn oversized_payload_is_marked_truncated() {
    let service = InMemorySwarmService::in_memory(ServiceConfig::default());
    service
        .create_session(create_command("agent-01", "/tmp/agent-01"))
        .await
        .expect("create");
    let text = "x".repeat(1024 * 1024 + 64);
    submit(&service, "agent-01", "input-1", &text).await;
    let id = LogicalId::parse("agent-01").expect("id");
    let page = service
        .list_items(
            &id,
            ItemsWindow {
                order: ItemOrder::Asc,
                after: None,
                after_id: None,
                limit: 10,
            },
        )
        .await
        .expect("items");
    let item = &page.items[0];
    assert!(item.truncated);
    assert_eq!(item.payload["_truncated"], serde_json::json!(true));
    assert!(item.payload["originalBytes"].as_u64().expect("bytes") > 1024 * 1024);
}

#[tokio::test]
async fn unknown_session_is_404_for_every_read() {
    let service = InMemorySwarmService::in_memory(ServiceConfig::default());
    let id = LogicalId::parse("missing").expect("id");
    assert_eq!(
        service.get_session(&id).await.expect_err("get").code(),
        ErrorCode::UnknownAgent
    );
    assert_eq!(
        service
            .submit_input(&id, submission("input-1", "hello"))
            .await
            .expect_err("submit")
            .code(),
        ErrorCode::UnknownAgent
    );
    assert_eq!(
        service
            .list_items(
                &id,
                ItemsWindow {
                    order: ItemOrder::Asc,
                    after: None,
                    after_id: None,
                    limit: 10,
                },
            )
            .await
            .expect_err("items")
            .code(),
        ErrorCode::UnknownAgent
    );
    assert_eq!(
        service
            .subscribe(&id, EventCursor::START)
            .await
            .expect_err("subscribe")
            .code(),
        ErrorCode::UnknownAgent
    );
}

/// Official `(type, event_id)` pairs of a replay window, in delivery order.
fn official_ids(events: &[StreamEvent]) -> Vec<(String, String)> {
    events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Official(official) => Some((
                official.event_type().to_owned(),
                official.event_id().to_owned(),
            )),
            _ => None,
        })
        .collect()
}

async fn seed_session(service: &InMemorySwarmService, logical_id: &str) {
    service
        .create_session(create_command(logical_id, "/tmp"))
        .await
        .expect("create");
}

async fn append_message(
    service: &InMemorySwarmService,
    logical_id: &str,
    item_id: &str,
    payload: serde_json::Value,
) -> u64 {
    let id = LogicalId::parse(logical_id).expect("logical id");
    service
        .append_item_with_id(
            &id,
            ItemKind::Message,
            ItemSource::root(),
            payload,
            Some(item_id.to_owned()),
        )
        .await
        .expect("append")
}

async fn set_state(service: &InMemorySwarmService, logical_id: &str, state: SessionState) {
    let id = LogicalId::parse(logical_id).expect("logical id");
    service
        .set_session_state(&id, state, None)
        .await
        .expect("state");
}

#[tokio::test]
async fn replay_keeps_every_assistant_message_of_one_turn() {
    let service = InMemorySwarmService::in_memory(ServiceConfig::default());
    seed_session(&service, "agent-01").await;
    append_message(
        &service,
        "agent-01",
        "itim_input_x",
        json!({"role": "user", "text": "hi"}),
    )
    .await;
    // A turn can persist several final assistant messages (e.g. across tool
    // calls); the first one must not close the turn.
    append_message(
        &service,
        "agent-01",
        "itim_msg_1",
        json!({"role": "assistant", "messageId": "msg_1", "text": "first", "complete": true}),
    )
    .await;
    append_message(
        &service,
        "agent-01",
        "itim_msg_2",
        json!({"role": "assistant", "messageId": "msg_2", "text": "second", "complete": true}),
    )
    .await;
    set_state(&service, "agent-01", SessionState::Idle).await;

    let backlog = service
        .replay_after(
            &LogicalId::parse("agent-01").expect("id"),
            EventCursor::START,
        )
        .await
        .expect("replay");
    let ids = official_ids(&backlog);
    assert_eq!(
        ids.iter()
            .map(|(kind, _)| kind.as_str())
            .collect::<Vec<_>>(),
        [
            "agent.session.turn.created",
            "agent.session.turn.item.added",
            "agent.session.turn.item.added",
            "agent.session.turn.item.added",
            "agent.session.turn.completed",
            "agent.session.idle",
        ],
        "{ids:?}"
    );
    let turn_id = "turn_input_x";
    assert_eq!(ids[0].1, "evt_turn_input_x_created");
    assert_eq!(ids[1].1, "evt_item_1_added");
    assert_eq!(ids[2].1, "evt_item_2_added");
    assert_eq!(ids[3].1, "evt_item_3_added");
    assert_eq!(ids[4].1, "evt_turn_input_x_completed");
    assert_eq!(ids[5].1, "evt_turn_input_x_idle");
    let added: Vec<_> = backlog
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Official(OfficialEvent::TurnItemAdded { item, .. }) => Some(item),
            _ => None,
        })
        .collect();
    assert_eq!(added.len(), 3);
    assert!(added.iter().all(|item| item.turn_id == turn_id));
}

#[tokio::test]
async fn replay_marks_a_failed_last_turn_failed_after_partial_output() {
    let service = InMemorySwarmService::in_memory(ServiceConfig::default());
    seed_session(&service, "agent-01").await;
    append_message(
        &service,
        "agent-01",
        "itim_input_x",
        json!({"role": "user", "text": "hi"}),
    )
    .await;
    // `finish_turn` flushes streamed text before the failure terminal, so a
    // failed turn can still have a persisted final assistant frame.
    append_message(
        &service,
        "agent-01",
        "itim_msg_1",
        json!({"role": "assistant", "messageId": "msg_1", "text": "partial", "complete": true}),
    )
    .await;
    set_state(&service, "agent-01", SessionState::Failed).await;

    let backlog = service
        .replay_after(
            &LogicalId::parse("agent-01").expect("id"),
            EventCursor::START,
        )
        .await
        .expect("replay");
    let ids = official_ids(&backlog);
    assert_eq!(
        ids.iter()
            .map(|(kind, _)| kind.as_str())
            .collect::<Vec<_>>(),
        [
            "agent.session.turn.created",
            "agent.session.turn.item.added",
            "agent.session.turn.item.added",
            "agent.session.turn.failed",
            "agent.session.failed",
        ],
        "a persisted assistant frame must not turn a failed turn into a completion: {ids:?}"
    );
    assert_eq!(ids[3].1, "evt_turn_input_x_failed");
    assert_eq!(ids[4].1, "evt_turn_input_x_session_failed");
}

#[tokio::test]
async fn replay_from_inside_a_turn_seeds_the_turn_identity() {
    let service = InMemorySwarmService::in_memory(ServiceConfig::default());
    seed_session(&service, "agent-01").await;
    let user_seq = append_message(
        &service,
        "agent-01",
        "itim_input_x",
        json!({"role": "user", "text": "hi"}),
    )
    .await;
    append_message(
        &service,
        "agent-01",
        "itim_msg_1",
        json!({"role": "assistant", "messageId": "msg_1", "text": "par", "complete": false}),
    )
    .await;
    append_message(
        &service,
        "agent-01",
        "itim_msg_1",
        json!({"role": "assistant", "messageId": "msg_1", "text": "partial", "complete": true}),
    )
    .await;
    set_state(&service, "agent-01", SessionState::Idle).await;

    // The reconnect cursor is the opening item: replay starts after it, so the
    // turn identity comes from the persisted history rather than a new opener.
    let backlog = service
        .replay_after(
            &LogicalId::parse("agent-01").expect("id"),
            EventCursor::after(user_seq),
        )
        .await
        .expect("replay");
    let ids = official_ids(&backlog);
    assert_eq!(
        ids.iter()
            .map(|(kind, _)| kind.as_str())
            .collect::<Vec<_>>(),
        [
            "agent.session.turn.item.added",
            "agent.session.turn.completed",
            "agent.session.idle",
        ],
        "{ids:?}"
    );
    assert_eq!(ids[0].1, "evt_item_3_added");
    assert_eq!(ids[1].1, "evt_turn_input_x_completed");
    match &backlog[1] {
        StreamEvent::Item(item) => assert_eq!(item.seq, 3),
        other => panic!("expected the final assistant item, got {other:?}"),
    }
}

#[tokio::test]
async fn replay_leaves_a_live_turn_open() {
    let service = InMemorySwarmService::in_memory(ServiceConfig {
        turn_policy: TurnPolicy::Manual,
        ..ServiceConfig::default()
    });
    seed_session(&service, "agent-01").await;
    submit(&service, "agent-01", "input-1", "hello").await;

    let backlog = service
        .replay_after(
            &LogicalId::parse("agent-01").expect("id"),
            EventCursor::START,
        )
        .await
        .expect("replay");
    let ids = official_ids(&backlog);
    assert_eq!(
        ids.iter()
            .map(|(kind, _)| kind.as_str())
            .collect::<Vec<_>>(),
        [
            "agent.session.turn.created",
            "agent.session.turn.item.added",
            "agent.session.turn.item.added",
        ],
        "a still-running turn must not gain a fabricated terminal: {ids:?}"
    );
}

#[tokio::test]
async fn no_cursor_subscription_has_no_backlog() {
    let service = InMemorySwarmService::in_memory(ServiceConfig::default());
    seed_session(&service, "agent-01").await;
    submit(&service, "agent-01", "input-1", "hello").await;

    let backlog = service
        .replay_after(
            &LogicalId::parse("agent-01").expect("id"),
            EventCursor::Tail,
        )
        .await
        .expect("replay");
    assert!(
        backlog.is_empty(),
        "a subscription without a cursor must start at the tail: {backlog:?}"
    );
}

#[tokio::test]
async fn replay_from_a_steer_keeps_the_opener_turn_id() {
    let service = InMemorySwarmService::in_memory(ServiceConfig::default());
    seed_session(&service, "agent-01").await;
    append_message(
        &service,
        "agent-01",
        "itim_input_x",
        json!({"role": "user", "text": "hi"}),
    )
    .await;
    // A delta before the steer proves the turn has no final assistant frame
    // yet, so the live stream would attach the steer to the same turn.
    append_message(
        &service,
        "agent-01",
        "itim_msg_1",
        json!({"role": "assistant", "messageId": "msg_1", "text": "par", "delta": true}),
    )
    .await;
    let steer_seq = append_message(
        &service,
        "agent-01",
        "itim_input_y",
        json!({"role": "user", "text": "adjust"}),
    )
    .await;
    append_message(
        &service,
        "agent-01",
        "itim_msg_1",
        json!({"role": "assistant", "messageId": "msg_1", "text": "partial", "complete": true}),
    )
    .await;
    set_state(&service, "agent-01", SessionState::Idle).await;

    // The reconnect cursor is just before the steer: the window starts with a
    // user item that must not mint `turn_input_y`.
    let backlog = service
        .replay_after(
            &LogicalId::parse("agent-01").expect("id"),
            EventCursor::after(steer_seq - 1),
        )
        .await
        .expect("replay");
    let ids = official_ids(&backlog);
    assert_eq!(
        ids.iter()
            .map(|(kind, _)| kind.as_str())
            .collect::<Vec<_>>(),
        [
            "agent.session.turn.item.added",
            "agent.session.turn.item.added",
            "agent.session.turn.completed",
            "agent.session.idle",
        ],
        "{ids:?}"
    );
    assert_eq!(ids[0].1, "evt_item_3_added");
    assert_eq!(ids[1].1, "evt_item_4_added");
    assert_eq!(
        ids[2].1, "evt_turn_input_x_completed",
        "the steer belongs to the opener's turn: {ids:?}"
    );
    let turn_ids: Vec<_> = backlog
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Official(OfficialEvent::TurnItemAdded { turn_id, .. }) => turn_id.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(
        turn_ids,
        ["turn_input_x", "turn_input_x"],
        "the steer must keep the live turn id"
    );
}

#[tokio::test]
async fn replay_from_a_tool_only_turn_emits_its_terminal() {
    let service = InMemorySwarmService::in_memory(ServiceConfig::default());
    seed_session(&service, "agent-01").await;
    append_message(
        &service,
        "agent-01",
        "itim_input_x",
        json!({"role": "user", "text": "run a tool"}),
    )
    .await;
    let id = LogicalId::parse("agent-01").expect("id");
    for (kind, payload) in [
        (
            ItemKind::ToolCall,
            json!({"toolCallId": "call-1", "name": "read"}),
        ),
        (
            ItemKind::ToolResult,
            json!({"toolCallId": "call-1", "text": "ok"}),
        ),
    ] {
        service
            .append_item_with_id(
                &id,
                kind,
                ItemSource::root(),
                payload,
                Some("itim_tool_1".to_owned()),
            )
            .await
            .expect("append");
    }
    set_state(&service, "agent-01", SessionState::Idle).await;

    // A turn whose only rows are tool frames has no assistant final frame. The
    // lookback still finds its opener, so the window carries a terminal.
    let backlog = service
        .replay_after(&id, EventCursor::after(1))
        .await
        .expect("replay");
    let ids = official_ids(&backlog);
    assert_eq!(
        ids.iter()
            .map(|(kind, _)| kind.as_str())
            .collect::<Vec<_>>(),
        ["agent.session.turn.completed", "agent.session.idle"],
        "{ids:?}"
    );
    assert_eq!(ids[0].1, "evt_turn_input_x_completed");
    assert_eq!(ids[1].1, "evt_turn_input_x_idle");
    let items: Vec<_> = backlog
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Item(item) => Some(item.seq),
            _ => None,
        })
        .collect();
    assert_eq!(items, [2, 3], "tool rows keep their custom item frames");
}

#[tokio::test]
async fn empty_window_at_the_tail_republishes_the_last_terminal() {
    let service = InMemorySwarmService::in_memory(ServiceConfig::default());
    seed_session(&service, "agent-01").await;
    append_message(
        &service,
        "agent-01",
        "itim_input_x",
        json!({"role": "user", "text": "hi"}),
    )
    .await;
    append_message(
        &service,
        "agent-01",
        "itim_msg_1",
        json!({"role": "assistant", "messageId": "msg_1", "text": "done", "complete": true}),
    )
    .await;
    set_state(&service, "agent-01", SessionState::Idle).await;

    // `afterSeq` equal to the tail has no rows to replay, but the client may
    // have missed the (unpersisted) terminal. The idle session row proves the
    // last turn closed, so its terminal is republished with the same event ids.
    let backlog = service
        .replay_after(
            &LogicalId::parse("agent-01").expect("id"),
            EventCursor::after(2),
        )
        .await
        .expect("replay");
    assert_eq!(
        official_ids(&backlog)
            .iter()
            .map(|(kind, _)| kind.as_str())
            .collect::<Vec<_>>(),
        ["agent.session.turn.completed", "agent.session.idle"]
    );
    assert!(backlog
        .iter()
        .all(|event| !matches!(event, StreamEvent::Item(_))));
}

#[tokio::test]
async fn replay_after_output_starts_a_new_turn_for_the_next_user_item() {
    let service = InMemorySwarmService::in_memory(ServiceConfig::default());
    seed_session(&service, "agent-01").await;
    append_message(
        &service,
        "agent-01",
        "itim_input_x",
        json!({"role": "user", "text": "first"}),
    )
    .await;
    append_message(
        &service,
        "agent-01",
        "itim_msg_1",
        json!({"role": "assistant", "messageId": "msg_1", "text": "done", "complete": true}),
    )
    .await;
    append_message(
        &service,
        "agent-01",
        "itim_input_y",
        json!({"role": "user", "text": "second"}),
    )
    .await;
    set_state(&service, "agent-01", SessionState::Idle).await;

    // The previous turn already produced its final frame under the replay
    // boundary rule, so the next user item opens a new turn and the window
    // closes it.
    let backlog = service
        .replay_after(
            &LogicalId::parse("agent-01").expect("id"),
            EventCursor::after(2),
        )
        .await
        .expect("replay");
    let ids = official_ids(&backlog);
    assert_eq!(
        ids.iter()
            .map(|(kind, _)| kind.as_str())
            .collect::<Vec<_>>(),
        [
            "agent.session.turn.created",
            "agent.session.turn.item.added",
            "agent.session.turn.completed",
            "agent.session.idle",
        ],
        "{ids:?}"
    );
    assert_eq!(ids[0].1, "evt_turn_input_y_created");
    assert_eq!(ids[2].1, "evt_turn_input_y_completed");
}

#[tokio::test]
async fn replay_keeps_the_failure_text_null_because_it_is_not_persisted() {
    let service = InMemorySwarmService::in_memory(ServiceConfig::default());
    seed_session(&service, "agent-01").await;
    append_message(
        &service,
        "agent-01",
        "itim_input_x",
        json!({"role": "user", "text": "hi"}),
    )
    .await;
    set_state(&service, "agent-01", SessionState::Failed).await;

    let backlog = service
        .replay_after(
            &LogicalId::parse("agent-01").expect("id"),
            EventCursor::START,
        )
        .await
        .expect("replay");
    let failed_turn = backlog
        .iter()
        .find_map(|event| match event {
            StreamEvent::Official(OfficialEvent::TurnFailed { turn, .. }) => Some(turn),
            _ => None,
        })
        .expect("turn.failed");
    // The persisted log carries no failure text, so replay keeps it null
    // instead of inventing a reason (the live frame carries the real message).
    assert_eq!(failed_turn.error, None);
}
