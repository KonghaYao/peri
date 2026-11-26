//! Real ACP store and model transport with the persistent state of a lost actor.
use super::*;
use peri_acp_types::messages::{BaseMessage, MessageId};
use peri_acp_types::session::{MessageKind, MessagePolicy, MessageSource, MqInput};
use peri_acp_types::session_resources::SessionStoreShutdownPort;
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::thread::WorkState;
use peri_resources::Resources;
use peri_swarm::store::SwarmSessionUpdate;

async fn replay_terminals(
    service: &AcpSwarmService,
    logical: &LogicalId,
    cursor: EventCursor,
) -> Vec<(String, String)> {
    use peri_swarm::domain::OfficialEvent;
    service
        .replay_after(logical, cursor)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|event| match event {
            StreamEvent::Official(
                OfficialEvent::TurnCompleted { turn, .. } | OfficialEvent::TurnFailed { turn, .. },
            ) => Some((turn.id, turn.status)),
            _ => None,
        })
        .collect()
}

async fn exercise_interrupted_entry(durable_work: bool) {
    clear_telemetry_env();
    let (base_url, stub) = start_stub().await;
    let tmp = tempfile::tempdir().expect("workspace");
    let cfg = service_config(&base_url, &tmp);
    let store = Arc::new(MemorySwarmStore::new());
    let logical = LogicalId::parse("interrupted").expect("logical id");
    let first = AcpSwarmService::new(store.clone(), cfg.clone());
    let created = first
        .create_session(create_command(&logical, tmp.path(), None))
        .await
        .unwrap();
    let root = created.session.root_session_id;
    let opener = user_input(
        &uuid(94),
        InputMode::Turn,
        "original side-effecting request",
    );
    let accepted = first.accept_input(&logical, opener.clone()).await.unwrap();
    let old_turn = accepted.pending_turn.unwrap().turn_id;
    first.shutdown_all().await;
    // The external process-kill verifier covers the actual crash. Here we set
    // both owners' records as they would be after prompt admission.
    store
        .swarm_upsert_turn(SwarmTurnRow {
            logical_id: logical.as_str().to_owned(),
            turn_id: old_turn.clone(),
            status: peri_swarm::domain::TurnStatus::InProgress,
            created_at: 100,
            started_at: Some(100),
            completed_at: None,
            error: None,
            usage: None,
            subagent_id: None,
        })
        .await
        .unwrap();
    store
        .swarm_update_session(SwarmSessionUpdate {
            logical_id: logical.as_str().to_owned(),
            state: SessionState::Running,
            last_item_seq: 1,
            last_activity_at: peri_time::now_utc_rfc3339(),
        })
        .await
        .unwrap();
    let (resources, owner) = Resources::open_with(Some(session_store_path(&tmp)))
        .await
        .unwrap()
        .into_parts();
    resources
        .set_work_state(&root, WorkState::Working)
        .await
        .unwrap();
    let id = MessageId::new();
    if durable_work {
        resources
            .mq_enqueue(
                &root,
                MqInput {
                    id,
                    kind: MessageKind::Defer,
                    source: MessageSource::SubAgentComplete,
                    payload: PersistedPayload::Message(BaseMessage::Human {
                        id,
                        content: "previously queued result".into(),
                    }),
                    policy: MessagePolicy::ensure_processing(),
                    original_draft: None,
                    plugin_selections: Vec::new(),
                },
            )
            .await
            .unwrap();
    }
    owner.shutdown().await.unwrap();
    drop(resources);

    let next = AcpSwarmService::new(store.clone(), cfg);
    assert!(replay_terminals(&next, &logical, EventCursor::START)
        .await
        .is_empty());
    for _ in 0..2 {
        assert_eq!(
            next.get_turn(&logical, &old_turn).await.unwrap().status,
            "in_progress"
        );
        assert_eq!(
            next.list_turns(
                &logical,
                TurnsWindow {
                    order: ItemOrder::Asc,
                    after: None,
                    limit: 10
                }
            )
            .await
            .unwrap()
            .data[0]
                .status,
            "in_progress"
        );
        assert_eq!(
            next.get_session(&logical).await.unwrap().state,
            SessionState::Running
        );
        assert_eq!(
            next.accept_input(&logical, opener.clone())
                .await
                .unwrap()
                .outcome
                .disposition,
            Disposition::Existing
        );
    }
    next.accept_input(&logical, control_input(&uuid(95), "cancel"))
        .await
        .unwrap();
    let retry = next
        .create_session(create_command(
            &logical,
            tmp.path(),
            Some("original side-effecting request"),
        ))
        .await
        .unwrap();
    assert!(retry.pending_initial.is_none());
    assert!(!next.is_live(&logical));
    assert_eq!(stub.main_calls(), 0);

    let input = user_input(&uuid(96), InputMode::Turn, "fresh explicit prompt");
    let (a, b) = tokio::join!(
        next.accept_input(&logical, input.clone()),
        next.accept_input(&logical, input)
    );
    let receipts = [a.unwrap(), b.unwrap()];
    let mut pending = receipts
        .into_iter()
        .filter_map(|accepted| accepted.pending_turn);
    let new_turn = pending.next().expect("one new turn");
    assert!(pending.next().is_none(), "concurrent retry duplicated turn");
    let failed = next.get_turn(&logical, &old_turn).await.unwrap();
    assert_eq!(failed.status, "failed");
    assert_eq!(failed.error.unwrap()["code"], json!("internal_error"));
    assert_eq!(
        stub.main_calls(),
        usize::from(durable_work),
        "only a durable queue may run before the new prompt"
    );
    if durable_work {
        assert!(stub
            .delivered_user_texts()
            .iter()
            .any(|s| s.contains("previously queued result")));
    }
    let new_id = new_turn.turn_id.clone();
    next.run_pending_turn(new_turn).await.unwrap();
    // session/prompt 可能先于终态通知返回，终态从调用方看是异步的；与既有
    // `wait_turn` 契约一致，等待 completed 落盘后再断言。
    wait_turn(&next, &logical, &new_id, "completed").await;
    assert_eq!(stub.main_calls(), usize::from(durable_work) + 1);
    assert!(!stub
        .delivered_user_texts()
        .iter()
        .any(|s| s.contains("original side-effecting request")));
    for cursor in [EventCursor::START, EventCursor::after(1)] {
        assert_eq!(
            replay_terminals(&next, &logical, cursor).await,
            vec![
                (old_turn.clone(), "failed".into()),
                (new_id.clone(), "completed".into()),
            ]
        );
    }
    next.shutdown_all().await;
    let (resources, owner) = Resources::open_with(Some(session_store_path(&tmp)))
        .await
        .unwrap()
        .into_parts();
    assert_eq!(
        resources.mq_history_contains(&root, id).await.unwrap(),
        durable_work
    );
    owner.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupted_synchronous_turn_is_failed_only_on_fresh_user_input() {
    exercise_interrupted_entry(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupted_session_hands_durable_work_to_acp_before_fresh_input() {
    exercise_interrupted_entry(true).await;
}

/// ILOG-5: a crash after the initial input's user item must not record it twice.
///
/// The external process-kill verifier covers the real crash; here the dead
/// attempt is reproduced from its durable half: the create had persisted its
/// reserved identity, the user item (`seq = 1`) and nothing else.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crash_after_user_item_keeps_initial_input_once() {
    clear_telemetry_env();
    let (base_url, stub) = start_stub().await;
    let tmp = tempfile::tempdir().expect("workspace");
    let cfg = service_config(&base_url, &tmp);
    let store = Arc::new(MemorySwarmStore::new());
    let logical = LogicalId::parse("interrupted-initial").expect("logical id");

    let first = AcpSwarmService::new(store.clone(), cfg.clone());
    let mut command = create_command(&logical, tmp.path(), Some("side-effecting initial input"));
    command.defer_initial_turn = true;
    let created = first.create_session(command).await.expect("create");
    let deferred = created.pending_initial.expect("the turn is deferred");
    // The durable half of the create ran; the prompt never reached the model.
    let admitted = items_of(&first, &logical).await;
    assert_eq!(
        admitted.len(),
        1,
        "the create recorded one user item: {admitted:?}"
    );
    assert_eq!(admitted[0].0, 1);
    assert_eq!(stub.main_calls(), 0, "no turn was executed");
    drop(deferred);
    first.shutdown_all().await;

    // A crash before the create replied leaves the row `Creating`.
    store
        .swarm_update_session(SwarmSessionUpdate {
            logical_id: logical.as_str().to_owned(),
            state: SessionState::Creating,
            last_item_seq: 1,
            last_activity_at: peri_time::now_utc_rfc3339(),
        })
        .await
        .expect("rewrite the interrupted row");

    let next = AcpSwarmService::new(store.clone(), cfg);
    let retried = next
        .create_session(create_command(
            &logical,
            tmp.path(),
            Some("side-effecting initial input"),
        ))
        .await
        .expect("recover the interrupted create");
    assert!(
        retried.pending_initial.is_none(),
        "a persisted user item means the initial input is already recorded"
    );
    assert_eq!(
        items_of(&next, &logical).await,
        admitted,
        "recovery does not append a second user item"
    );
    assert_eq!(
        stub.main_calls(),
        0,
        "recovery must not replay the initial prompt into the model"
    );
    next.shutdown_all().await;
}
