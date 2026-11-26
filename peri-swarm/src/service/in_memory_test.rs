use std::sync::Arc;

use super::*;
use crate::domain::{EventCursor, ItemOrder};
use crate::service::UpdateField;
use crate::store::MemorySwarmStore;

fn create_command(logical_id: &str, cwd: &str) -> CreateSessionCommand {
    CreateSessionCommand {
        logical_id: LogicalId::parse(logical_id).expect("logical id"),
        cwd: std::path::PathBuf::from(cwd),
        model: None,
        instructions: None,
        mcp_tools: Vec::new(),
        metadata: Default::default(),
        prompt_cache_options: Default::default(),
        initial_input: None,
        environment: EnvironmentKind::None,
        environment_id: None,
        root_session_id: None,
        defer_initial_turn: false,
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
async fn create_on_a_running_session_is_refused_before_recording_the_input() {
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
    assert_eq!(
        service.get_session(&id).await.expect("get").state,
        SessionState::Running
    );
    let before = service
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

    // Both create modes follow the same admission rule: a create that replays
    // its initial input on a busy session is refused before the user item is
    // recorded, so no opener is left behind for a turn that cannot start.
    for defer in [false, true] {
        let mut command = create_command("agent-01", "/tmp/agent-01");
        command.initial_input = Some("replayed".to_owned());
        command.defer_initial_turn = defer;
        let error = service
            .create_session(command)
            .await
            .expect_err("running create");
        assert_eq!(error.code(), ErrorCode::TurnInProgress);
    }
    let after = service
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
        after.items.iter().map(|item| item.seq).collect::<Vec<_>>(),
        before.items.iter().map(|item| item.seq).collect::<Vec<_>>(),
        "a refused create must not append a stray user item"
    );
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
    let created = live.recv().await.expect("session in progress");
    assert!(
        matches!(
            created,
            StreamEvent::Official(OfficialEvent::SessionInProgress { .. })
        ),
        "{created:?}"
    );
    let created = live.recv().await.expect("turn created");
    assert!(
        matches!(
            created,
            StreamEvent::Official(OfficialEvent::TurnCreated { .. })
        ),
        "{created:?}"
    );
    let in_progress = live.recv().await.expect("turn in progress");
    assert!(
        matches!(
            in_progress,
            StreamEvent::Official(OfficialEvent::TurnInProgress { .. })
        ),
        "{in_progress:?}"
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
    let part = live.recv().await.expect("content part added");
    assert!(
        matches!(
            part,
            StreamEvent::Official(OfficialEvent::TurnContentPartAdded { .. })
        ),
        "{part:?}"
    );
    let text = live.recv().await.expect("output text done");
    assert!(
        matches!(
            text,
            StreamEvent::Official(OfficialEvent::TurnOutputTextDone { .. })
        ),
        "{text:?}"
    );
    let part = live.recv().await.expect("content part done");
    assert!(
        matches!(
            part,
            StreamEvent::Official(OfficialEvent::TurnContentPartDone { .. })
        ),
        "{part:?}"
    );
    let done = live.recv().await.expect("item done");
    assert!(
        matches!(
            done,
            StreamEvent::Official(OfficialEvent::TurnItemDone { .. })
        ),
        "{done:?}"
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
async fn deferred_create_returns_the_first_turn_without_running_it() {
    let service = InMemorySwarmService::in_memory(ServiceConfig::default());
    let id = LogicalId::parse("agent-01").expect("id");
    let subscription = service.open_stream(&id).await;
    let mut command = create_command("agent-01", "/tmp/agent-01");
    command.initial_input = Some("hello".to_owned());
    command.defer_initial_turn = true;

    let created = service.create_session(command).await.expect("create");
    assert!(created.created);
    let pending = created.pending_initial.clone().expect("deferred turn");
    assert_eq!(pending.text, "hello");
    assert_eq!(pending.logical_id, id);
    // Acceptance happened; execution did not. Recording the user item marks
    // the session running, exactly as an accepted `POST /events` does.
    assert_eq!(
        created.session.last_item_seq, 1,
        "defer records only the user item"
    );
    assert_eq!(created.session.state, SessionState::Running);
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
    assert_eq!(page.items.len(), 1, "no assistant item before the turn");
    assert_eq!(page.items[0].payload["role"], serde_json::json!("user"));

    // The pre-create subscription saw the user item and its official frames,
    // but neither the gateway `turn` frames nor a terminal.
    let (backlog, mut live) = subscription.into_parts();
    assert!(backlog.is_empty());
    let user = live.recv().await.expect("user item");
    assert_eq!(user.item_seq(), Some(1));
    let progress = live.recv().await.expect("session in progress");
    assert!(matches!(
        progress,
        StreamEvent::Official(OfficialEvent::SessionInProgress { .. })
    ));
    let created_event = live.recv().await.expect("turn created");
    assert!(matches!(
        created_event,
        StreamEvent::Official(OfficialEvent::TurnCreated { .. })
    ));
    let in_progress = live.recv().await.expect("turn in progress");
    assert!(matches!(
        in_progress,
        StreamEvent::Official(OfficialEvent::TurnInProgress { .. })
    ));
    let added = live.recv().await.expect("user item added");
    assert!(matches!(
        added,
        StreamEvent::Official(OfficialEvent::TurnItemAdded { .. })
    ));
    assert!(
        matches!(
            live.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ),
        "the deferred turn has not started"
    );
}

#[tokio::test]
async fn pending_initial_does_not_cross_the_actor_boundary() {
    let service = InMemorySwarmService::in_memory(ServiceConfig::default());
    let mut command = create_command("agent-01", "/tmp/agent-01");
    command.initial_input = Some("hello".to_owned());
    command.defer_initial_turn = true;
    let created = service.create_session(command).await.expect("create");
    assert!(
        created.pending_initial.is_some(),
        "the deferred turn is returned to the caller that runs it"
    );

    // `#[serde(skip)]` keeps execution state out of the actor envelope: the
    // gateway deserializes the reply and must not see a pending turn it would
    // wrongly run (`design §4.3`).
    let value = serde_json::to_value(&created).expect("serialize");
    assert!(
        value.get("pendingInitial").is_none(),
        "the envelope carries no execution state: {value}"
    );
    let decoded: CreateSessionResult = serde_json::from_value(value).expect("deserialize");
    assert!(
        decoded.pending_initial.is_none(),
        "a decoded create reply never carries a pending turn"
    );
    assert!(decoded.created, "the durable fields survive the round trip");
}

#[tokio::test]
async fn deferred_first_turn_streams_the_complete_turn_when_run() {
    let service = InMemorySwarmService::in_memory(ServiceConfig::default());
    let id = LogicalId::parse("agent-01").expect("id");
    let subscription = service.open_stream(&id).await;
    let mut command = create_command("agent-01", "/tmp/agent-01");
    command.initial_input = Some("hello".to_owned());
    command.defer_initial_turn = true;
    let created = service.create_session(command).await.expect("create");
    let pending = created.pending_initial.expect("deferred turn");

    service.run_pending_turn(pending).await.expect("run");

    let (backlog, mut live) = subscription.into_parts();
    assert!(backlog.is_empty());
    let mut events = Vec::new();
    while let Ok(event) = live.try_recv() {
        events.push(event);
    }
    assert_eq!(
        events
            .iter()
            .filter_map(StreamEvent::item_seq)
            .collect::<Vec<_>>(),
        [1, 2],
        "the user item, then the assistant completion"
    );
    let official_names: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Official(official) => Some(official.event_type().to_owned()),
            _ => None,
        })
        .collect();
    assert_eq!(
        official_names,
        [
            "agent.session.in_progress",
            "agent.session.turn.created",
            "agent.session.turn.in_progress",
            "agent.session.turn.item.added",
            "agent.session.turn.item.added",
            "agent.session.turn.content_part.added",
            "agent.session.turn.output_text.done",
            "agent.session.turn.content_part.done",
            "agent.session.turn.item.done",
            "agent.session.turn.completed",
            "agent.session.idle",
        ],
        "the official sequence is complete"
    );
    assert!(matches!(
        events[5],
        StreamEvent::Turn(TurnEvent::Started { .. })
    ));
    assert!(matches!(
        events[12],
        StreamEvent::Turn(TurnEvent::Completed { .. })
    ));
    assert!(matches!(
        events[14],
        StreamEvent::Official(OfficialEvent::SessionIdle { .. })
    ));
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
    append_message_from(service, logical_id, item_id, ItemSource::root(), payload).await
}

async fn append_message_from(
    service: &InMemorySwarmService,
    logical_id: &str,
    item_id: &str,
    source: ItemSource,
    payload: serde_json::Value,
) -> u64 {
    let id = LogicalId::parse(logical_id).expect("logical id");
    service
        .append_item_with_id(
            &id,
            ItemKind::Message,
            source,
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
            "agent.session.in_progress",
            "agent.session.turn.created",
            "agent.session.turn.in_progress",
            "agent.session.turn.item.added",
            "agent.session.turn.item.added",
            "agent.session.turn.content_part.added",
            "agent.session.turn.output_text.done",
            "agent.session.turn.content_part.done",
            "agent.session.turn.item.done",
            "agent.session.turn.item.added",
            "agent.session.turn.content_part.added",
            "agent.session.turn.output_text.done",
            "agent.session.turn.content_part.done",
            "agent.session.turn.item.done",
            "agent.session.turn.completed",
            "agent.session.idle",
        ],
        "{ids:?}"
    );
    let turn_id = "turn_input_x";
    assert_eq!(ids[0].1, "evt_turn_input_x_session_in_progress");
    assert_eq!(ids[1].1, "evt_turn_input_x_created");
    assert_eq!(ids[2].1, "evt_turn_input_x_in_progress");
    assert_eq!(ids[3].1, "evt_item_1_added");
    assert_eq!(ids[4].1, "evt_item_2_added");
    assert_eq!(ids[8].1, "evt_item_2_done");
    assert_eq!(ids[9].1, "evt_item_3_added");
    assert_eq!(ids[13].1, "evt_item_3_done");
    assert_eq!(ids[14].1, "evt_turn_input_x_completed");
    assert_eq!(ids[15].1, "evt_turn_input_x_idle");
    let added: Vec<_> = backlog
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Official(OfficialEvent::TurnItemAdded { item, .. }) => Some(item),
            _ => None,
        })
        .collect();
    assert_eq!(added.len(), 3);
    assert!(added.iter().all(|item| item.turn_id == turn_id));
    // Old rows carry no phase/outputIndex: replay keeps the legacy default and
    // numbers the completed messages in row order.
    let done: Vec<_> = backlog
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Official(OfficialEvent::TurnItemDone {
                output_index, item, ..
            }) => Some((*output_index, item.phase.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        done,
        [
            (0, Some("final_answer".to_owned())),
            (1, Some("final_answer".to_owned())),
        ],
        "legacy payloads replay as final answers with sequential indices"
    );
}

#[tokio::test]
async fn replay_reads_the_persisted_phase_and_output_index() {
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
        "itim_msg_b",
        json!({
            "role": "assistant",
            "messageId": "msg_b",
            "text": "working",
            "complete": true,
            "phase": "commentary",
            "outputIndex": 0,
        }),
    )
    .await;
    append_message(
        &service,
        "agent-01",
        "itim_msg_a",
        json!({
            "role": "assistant",
            "messageId": "msg_a",
            "text": "answer",
            "complete": true,
            "phase": "final_answer",
            "outputIndex": 1,
        }),
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
    let done: Vec<_> = backlog
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Official(OfficialEvent::TurnItemDone {
                output_index, item, ..
            }) => Some((*output_index, item.phase.clone(), item.id.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        done,
        [
            (0, Some("commentary".to_owned()), "itim_msg_b".to_owned()),
            (1, Some("final_answer".to_owned()), "itim_msg_a".to_owned()),
        ],
        "replay must read the persisted facts, not re-derive them"
    );
}

#[tokio::test]
async fn replay_ignores_child_messages_for_official_items_and_output_index() {
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
        "itim_msg_root",
        json!({
            "role": "assistant",
            "messageId": "msg_root",
            "text": "root answer",
            "complete": true,
            "phase": "final_answer",
            "outputIndex": 0,
        }),
    )
    .await;
    // A child completion is a persisted row but not a root output: replay must
    // not turn it into an official item or let it claim the final phase.
    append_message_from(
        &service,
        "agent-01",
        "itim_msg_child",
        ItemSource {
            agent_id: "child:child-1".to_owned(),
            task_id: None,
        },
        json!({
            "role": "assistant",
            "messageId": "msg_child",
            "text": "child answer",
            "complete": true,
        }),
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
    let done: Vec<_> = backlog
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Official(OfficialEvent::TurnItemDone {
                output_index, item, ..
            }) => Some((*output_index, item.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(done.len(), 1, "{backlog:?}");
    assert_eq!(done[0].0, 0, "the child row must not consume output 0");
    assert_eq!(done[0].1.id, "itim_msg_root");
    assert_eq!(done[0].1.phase.as_deref(), Some("final_answer"));
    assert!(
        !backlog.iter().any(|event| matches!(
            event,
            StreamEvent::Official(OfficialEvent::TurnItemAdded { item, .. })
                if item.id == "itim_msg_child"
        )),
        "child rows must not produce official item.added"
    );
    // The custom item surface still carries the child row.
    assert!(backlog.iter().any(|event| matches!(
        event,
        StreamEvent::Item(item) if item.item_id == "itim_msg_child"
    )));
}

#[tokio::test]
async fn replay_lookback_ignores_child_output_when_deciding_steer() {
    let service = InMemorySwarmService::in_memory(ServiceConfig::default());
    seed_session(&service, "agent-01").await;
    append_message(
        &service,
        "agent-01",
        "itim_input_x",
        json!({"role": "user", "text": "hi"}),
    )
    .await;
    let child_seq = append_message_from(
        &service,
        "agent-01",
        "itim_msg_child",
        ItemSource {
            agent_id: "child:child-1".to_owned(),
            task_id: None,
        },
        json!({
            "role": "assistant",
            "messageId": "msg_child",
            "text": "child answer",
            "complete": true,
        }),
    )
    .await;
    append_message(
        &service,
        "agent-01",
        "itim_input_y",
        json!({"role": "user", "text": "adjust"}),
    )
    .await;
    set_state(&service, "agent-01", SessionState::Idle).await;

    // Child output must not close the root turn. The window starts at the
    // second user item; the lookback must still attach it to `turn_input_x`.
    let backlog = service
        .replay_after(
            &LogicalId::parse("agent-01").expect("id"),
            EventCursor::after(child_seq),
        )
        .await
        .expect("replay");
    let steer = backlog
        .iter()
        .find_map(|event| match event {
            StreamEvent::Official(OfficialEvent::TurnItemAdded { turn_id, item, .. })
                if item.role == "user" =>
            {
                Some(turn_id.clone())
            }
            _ => None,
        })
        .expect("user item added");
    assert_eq!(
        steer,
        Some("turn_input_x".to_owned()),
        "child output must not make the next user item a new turn: {backlog:?}"
    );
}

#[tokio::test]
async fn replay_session_frames_carry_persisted_metadata_and_prompt_cache() {
    let service = InMemorySwarmService::in_memory(ServiceConfig::default());
    let mut command = create_command("agent-01", "/tmp");
    command.metadata = UpdateField::Set([("team".to_owned(), "alpha".to_owned())].into());
    command.prompt_cache_options = UpdateField::Set(crate::domain::PromptCacheOptions::new(
        crate::domain::PromptCacheTtl::TwelveHours,
    ));
    service.create_session(command).await.expect("create");
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

    let backlog = service
        .replay_after(
            &LogicalId::parse("agent-01").expect("id"),
            EventCursor::START,
        )
        .await
        .expect("replay");
    let sessions: Vec<_> = backlog
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Official(OfficialEvent::SessionInProgress { session, .. })
            | StreamEvent::Official(OfficialEvent::SessionIdle { session, .. }) => Some(session),
            _ => None,
        })
        .collect();
    assert_eq!(sessions.len(), 2, "in_progress and idle: {backlog:?}");
    for session in sessions {
        assert_eq!(
            session.metadata,
            [("team".to_owned(), "alpha".to_owned())].into(),
            "replay must carry the persisted metadata"
        );
        assert_eq!(
            session.prompt_cache_options,
            Some(crate::domain::PromptCacheOptions::new(
                crate::domain::PromptCacheTtl::TwelveHours
            )),
            "replay must carry the persisted prompt cache options"
        );
    }
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
            "agent.session.in_progress",
            "agent.session.turn.created",
            "agent.session.turn.in_progress",
            "agent.session.turn.item.added",
            "agent.session.turn.item.added",
            "agent.session.turn.content_part.added",
            "agent.session.turn.output_text.done",
            "agent.session.turn.content_part.done",
            "agent.session.turn.item.done",
            "agent.session.turn.failed",
            "error",
            "agent.session.failed",
        ],
        "a persisted assistant frame must not turn a failed turn into a completion: {ids:?}"
    );
    assert_eq!(ids[9].1, "evt_turn_input_x_failed");
    assert_eq!(ids[10].1, "evt_turn_input_x_error");
    assert_eq!(ids[11].1, "evt_turn_input_x_session_failed");
    let error = backlog
        .iter()
        .find_map(|event| match event {
            StreamEvent::Official(OfficialEvent::Error { error, .. }) => Some(error),
            _ => None,
        })
        .expect("error frame");
    // The turn row is not read during replay: the category is the only code
    // this gateway writes, and the message stays empty instead of invented.
    assert_eq!(error.code, "internal_error");
    assert_eq!(error.kind, "internal_error");
    assert_eq!(error.message, "");
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
            "agent.session.turn.content_part.added",
            "agent.session.turn.output_text.done",
            "agent.session.turn.content_part.done",
            "agent.session.turn.item.done",
            "agent.session.turn.completed",
            "agent.session.idle",
        ],
        "{ids:?}"
    );
    assert_eq!(ids[0].1, "evt_item_3_added");
    assert_eq!(ids[4].1, "evt_item_3_done");
    assert_eq!(ids[5].1, "evt_turn_input_x_completed");
    let item = backlog
        .iter()
        .find_map(|event| match event {
            StreamEvent::Item(item) if item.payload["complete"] == json!(true) => Some(item),
            _ => None,
        })
        .expect("the final assistant item");
    assert_eq!(item.seq, 3);
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
            "agent.session.in_progress",
            "agent.session.turn.created",
            "agent.session.turn.in_progress",
            "agent.session.turn.item.added",
            "agent.session.turn.item.added",
            "agent.session.turn.content_part.added",
            "agent.session.turn.output_text.done",
            "agent.session.turn.content_part.done",
            "agent.session.turn.item.done",
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
            "agent.session.turn.content_part.added",
            "agent.session.turn.output_text.done",
            "agent.session.turn.content_part.done",
            "agent.session.turn.item.done",
            "agent.session.turn.completed",
            "agent.session.idle",
        ],
        "{ids:?}"
    );
    assert_eq!(ids[0].1, "evt_item_3_added");
    assert_eq!(ids[1].1, "evt_item_4_added");
    assert_eq!(
        ids[6].1, "evt_turn_input_x_completed",
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
            "agent.session.in_progress",
            "agent.session.turn.created",
            "agent.session.turn.in_progress",
            "agent.session.turn.item.added",
            "agent.session.turn.completed",
            "agent.session.idle",
        ],
        "{ids:?}"
    );
    assert_eq!(ids[0].1, "evt_turn_input_y_session_in_progress");
    assert_eq!(ids[1].1, "evt_turn_input_y_created");
    assert_eq!(ids[4].1, "evt_turn_input_y_completed");
}

/// A recovered row belongs to no turn, so it is not the first turn row of the
/// replay window: the user item behind it still opens the next turn instead of
/// making replay resend the previous turn's terminal (`first_turn_row_role`).
#[tokio::test]
async fn a_recovered_row_is_not_the_first_turn_row_of_the_window() {
    let service = InMemorySwarmService::in_memory(ServiceConfig::default());
    seed_session(&service, "agent-01").await;
    append_message(
        &service,
        "agent-01",
        "itim_input_x",
        json!({"role": "user", "text": "first"}),
    )
    .await;
    let cursor = append_message(
        &service,
        "agent-01",
        "itim_msg_1",
        json!({"role": "assistant", "messageId": "msg_1", "text": "done", "complete": true}),
    )
    .await;
    // Recovery flushed root output after the turn closed: custom item only.
    append_message(
        &service,
        "agent-01",
        "itim_msg_recovered",
        json!({
            "role": "assistant",
            "messageId": "msg_recovered",
            "text": "recovered text",
            "complete": true,
            "recovered": true,
        }),
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

    // The window is [recovered row, user y]. Its first turn row is the user
    // item and the previous turn already produced output, so the window opens
    // turn y and does not attach itself to turn x.
    let backlog = service
        .replay_after(
            &LogicalId::parse("agent-01").expect("id"),
            EventCursor::after(cursor),
        )
        .await
        .expect("replay");
    let ids = official_ids(&backlog);
    assert_eq!(
        ids.iter()
            .map(|(kind, _)| kind.as_str())
            .collect::<Vec<_>>(),
        [
            "agent.session.in_progress",
            "agent.session.turn.created",
            "agent.session.turn.in_progress",
            "agent.session.turn.item.added",
            "agent.session.turn.completed",
            "agent.session.idle",
        ],
        "{ids:?}"
    );
    assert_eq!(ids[1].1, "evt_turn_input_y_created");
    assert!(
        ids.iter()
            .all(|(_, event_id)| event_id != "evt_turn_input_x_completed"),
        "the recovered row must not replay as turn x's completion: {ids:?}"
    );
}

/// The lookback must not take a recovered row for the completion of the turn
/// before it (`turn_at_cursor`). Without the skip, a cursor after the recovered
/// row closes turn x with it and the next input mints a second turn.
#[tokio::test]
async fn a_recovered_row_after_the_cursor_does_not_close_the_open_turn() {
    let service = InMemorySwarmService::in_memory(ServiceConfig::default());
    seed_session(&service, "agent-01").await;
    append_message(
        &service,
        "agent-01",
        "itim_input_x",
        json!({"role": "user", "text": "first"}),
    )
    .await;
    // Turn x produced no final assistant row; recovery then wrote root output
    // that belongs to no turn, and the client reconnects after that row.
    let cursor = append_message(
        &service,
        "agent-01",
        "itim_msg_recovered",
        json!({
            "role": "assistant",
            "messageId": "msg_recovered",
            "text": "recovered text",
            "complete": true,
            "recovered": true,
        }),
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

    let backlog = service
        .replay_after(
            &LogicalId::parse("agent-01").expect("id"),
            EventCursor::after(cursor),
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
    assert_eq!(ids[1].1, "evt_turn_input_x_completed");
}

#[tokio::test]
async fn live_and_replay_agree_on_the_official_type_and_event_id_sequence() {
    let service = InMemorySwarmService::in_memory(ServiceConfig::default());
    seed_session(&service, "agent-01").await;
    let id = LogicalId::parse("agent-01").expect("id");
    let subscription = service
        .subscribe(&id, EventCursor::Tail)
        .await
        .expect("subscribe before the turn");

    submit(&service, "agent-01", "input-1", "hello").await;

    let (_, mut live) = subscription.into_parts();
    let mut live_events = Vec::new();
    while let Ok(event) = live.try_recv() {
        live_events.push(event);
    }
    let backlog = service
        .replay_after(&id, EventCursor::START)
        .await
        .expect("replay");
    assert_eq!(
        official_ids(&live_events),
        official_ids(&backlog),
        "live and replay must deliver the same identities: live={live_events:?}"
    );
    assert!(
        official_ids(&live_events).len() > 5,
        "the parity covers the completion frames, not only the terminal"
    );
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
