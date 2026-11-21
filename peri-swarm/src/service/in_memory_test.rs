use std::sync::Arc;

use super::*;
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
        .subscribe(&id, Cursor::new(1))
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
    let started = live.recv().await.expect("started");
    assert!(matches!(
        started,
        StreamEvent::Turn(TurnEvent::Started { .. })
    ));
    let item = live.recv().await.expect("assistant item");
    assert_eq!(item.item_seq(), Some(2));
    let completed = live.recv().await.expect("completed");
    assert!(matches!(
        completed,
        StreamEvent::Turn(TurnEvent::Completed { .. })
    ));

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
            .subscribe(&id, Cursor::START)
            .await
            .expect_err("subscribe")
            .code(),
        ErrorCode::UnknownAgent
    );
}
