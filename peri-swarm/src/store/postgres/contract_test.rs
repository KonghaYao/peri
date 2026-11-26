//! Item, turn and environment-binding contract tests for the PostgreSQL state
//! store (real server, private schema per test from [`super::test_fixture`]).
//!
//! These pin the properties the actor and the SSE replay depend on: strictly
//! increasing `seq`, item-grouped cursors, one write unit per append run,
//! `created_at` kept across the turn upsert, and the binding hand-over.

use std::sync::Arc;

use crate::domain::{
    EnvironmentKind, EnvironmentRef, ItemOrder, SessionState, TurnStatus, TurnsWindow,
};
use crate::store::{StoreError, SwarmSessionUpdate, SwarmStore};

use super::test_fixture::{binding, input, item, now, session, turn, TestSchema};

// ── items ───────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
async fn items_page_by_item_and_advance_the_cursor() {
    let Some(fixture) = TestSchema::create().await else {
        return;
    };
    let store = fixture.store().await;
    store
        .swarm_upsert_session(session("logical-1", "/workspace/a"))
        .await
        .expect("seed");

    // Two streamed messages: item `a` has two frames, item `b` one.
    for (seq, item_id, text) in [(1, "a", "a0"), (2, "b", "b0"), (3, "a", "a1")] {
        store
            .swarm_append_item(item("logical-1", seq, item_id, text))
            .await
            .expect("append");
    }
    assert_eq!(
        store.swarm_last_seq("logical-1").await.expect("last seq"),
        3
    );

    let after: Vec<u64> = store
        .swarm_items_after("logical-1", 1, 10)
        .await
        .expect("after")
        .iter()
        .map(|row| row.seq)
        .collect();
    assert_eq!(after, vec![2, 3]);

    let before: Vec<u64> = store
        .swarm_items_before("logical-1", Some(3), 10)
        .await
        .expect("before")
        .iter()
        .map(|row| row.seq)
        .collect();
    assert_eq!(before, vec![2, 1]);
    let tail: Vec<u64> = store
        .swarm_items_before("logical-1", None, 2)
        .await
        .expect("tail")
        .iter()
        .map(|row| row.seq)
        .collect();
    assert_eq!(tail, vec![3, 2]);

    // A cursor is an item boundary: `after=1` selects whole items `b` and `a`
    // (the latter at its greatest seq), and reports the page cursor.
    let page = store
        .swarm_item_page("logical-1", ItemOrder::Asc, Some(1), 10)
        .await
        .expect("ascending page");
    assert_eq!(
        page.items.iter().map(|row| row.seq).collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert!(!page.has_more);
    assert_eq!(page.next_cursor, None);
    assert_eq!(
        page.rows.iter().map(|row| row.seq).collect::<Vec<_>>(),
        vec![1, 2, 3],
        "every row of a selected item belongs to the page, including seq 1 of item `a`"
    );

    // One item per page, newest item first: the page holds both frames of the
    // streamed message that started at seq 1, plus its terminal at seq 3.
    let first = store
        .swarm_item_page("logical-1", ItemOrder::Desc, None, 1)
        .await
        .expect("first item only");
    assert_eq!(
        first
            .items
            .iter()
            .map(|row| row.item_id.as_str())
            .collect::<Vec<_>>(),
        vec!["a"],
        "an item is returned whole: both of its frames are in `rows`"
    );
    assert_eq!(
        first.rows.iter().map(|row| row.seq).collect::<Vec<_>>(),
        vec![3, 1]
    );
    assert!(first.has_more);
    assert_eq!(
        first.next_cursor,
        Some(3),
        "the cursor names the last item's greatest seq, not a row inside it"
    );

    // The next descending page (before the cursor) is the remaining item.
    let rest = store
        .swarm_item_page("logical-1", ItemOrder::Desc, first.next_cursor, 1)
        .await
        .expect("remaining item");
    assert_eq!(
        rest.items
            .iter()
            .map(|row| row.item_id.as_str())
            .collect::<Vec<_>>(),
        vec!["b"]
    );
    assert_eq!(
        rest.rows.iter().map(|row| row.seq).collect::<Vec<_>>(),
        vec![2]
    );
    assert!(!rest.has_more);

    let descending = store
        .swarm_item_page("logical-1", ItemOrder::Desc, None, 10)
        .await
        .expect("descending page");
    assert_eq!(
        descending
            .items
            .iter()
            .map(|row| row.item_id.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b"]
    );

    // `get_item` prefers the greatest seq of a repeated id.
    let greatest = store
        .swarm_get_item("logical-1", "a")
        .await
        .expect("get item")
        .expect("item");
    assert_eq!(greatest.seq, 3);

    // A repeated (logical_id, seq) is a conflict, not a renumbering.
    let conflict = store
        .swarm_append_item(item("logical-1", 3, "a", "duplicate"))
        .await
        .expect_err("duplicate seq");
    assert!(matches!(conflict, StoreError::Conflict(_)), "{conflict:?}");

    store.close().await.expect("close");
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
async fn append_items_is_one_write_unit() {
    let Some(fixture) = TestSchema::create().await else {
        return;
    };
    let store = fixture.store().await;
    store
        .swarm_upsert_session(session("logical-1", "/workspace/a"))
        .await
        .expect("seed");

    // A conflicting row in the middle leaves none of the run persisted.
    let run = vec![
        item("logical-1", 1, "a", "a0"),
        item("logical-1", 2, "b", "b0"),
        item("logical-1", 2, "c", "c0"),
    ];
    let conflict = store
        .swarm_append_items(&run, None)
        .await
        .expect_err("duplicate seq inside the run");
    assert!(matches!(conflict, StoreError::Conflict(_)), "{conflict:?}");
    assert_eq!(
        fixture.count("swarm_items").await,
        0,
        "a failed run leaves no row behind"
    );

    // The progress update is part of the same unit: a missing session rolls the
    // items back too.
    let missing = store
        .swarm_append_items(
            &[item("logical-1", 1, "a", "a0")],
            Some(SwarmSessionUpdate {
                logical_id: "logical-absent".to_owned(),
                state: SessionState::Running,
                last_item_seq: 1,
                last_activity_at: now(),
            }),
        )
        .await
        .expect_err("the session does not exist");
    assert!(matches!(missing, StoreError::NotFound(_)), "{missing:?}");
    assert_eq!(fixture.count("swarm_items").await, 0);

    // A successful run commits items and the cursor together.
    store
        .swarm_append_items(
            &[item("logical-1", 1, "a", "a0")],
            Some(SwarmSessionUpdate {
                logical_id: "logical-1".to_owned(),
                state: SessionState::Running,
                last_item_seq: 1,
                last_activity_at: now(),
            }),
        )
        .await
        .expect("append run");
    let session_row = store
        .swarm_get_session("logical-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(session_row.state, SessionState::Running);
    assert_eq!(session_row.last_item_seq, 1);
    assert_eq!(fixture.count("swarm_items").await, 1);

    store.close().await.expect("close");
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
async fn concurrent_appends_leave_one_row_and_a_conflict() {
    let Some(fixture) = TestSchema::create().await else {
        return;
    };
    let store = Arc::new(fixture.store().await);
    store
        .swarm_upsert_session(session("logical-1", "/workspace/a"))
        .await
        .expect("seed");

    let first = {
        let store = Arc::clone(&store);
        tokio::spawn(async move {
            store
                .swarm_append_item(item("logical-1", 1, "a", "a0"))
                .await
        })
    };
    let second = {
        let store = Arc::clone(&store);
        tokio::spawn(async move {
            store
                .swarm_append_item(item("logical-1", 1, "b", "b0"))
                .await
        })
    };
    let outcomes = [first.await.expect("join"), second.await.expect("join")];
    assert_eq!(
        outcomes.iter().filter(|outcome| outcome.is_ok()).count(),
        1,
        "the primary key arbitrates the race: {outcomes:?}"
    );
    assert!(
        outcomes
            .iter()
            .any(|outcome| matches!(outcome, Err(StoreError::Conflict(_)))),
        "the loser sees a conflict so it can reload MAX(seq): {outcomes:?}"
    );
    assert_eq!(fixture.count("swarm_items").await, 1);

    store.close().await.expect("close");
    fixture.close().await;
}

// ── turns, bindings, hosted sessions ────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
async fn turns_keep_created_at_and_close_open_rows() {
    let Some(fixture) = TestSchema::create().await else {
        return;
    };
    let store = fixture.store().await;
    store
        .swarm_upsert_session(session("logical-1", "/workspace/a"))
        .await
        .expect("seed");

    store
        .swarm_upsert_turn(turn("logical-1", "turn-1", TurnStatus::InProgress, 100))
        .await
        .expect("open turn");
    assert!(store
        .swarm_has_open_turn("logical-1")
        .await
        .expect("open probe"));

    let mut completed = turn("logical-1", "turn-1", TurnStatus::Completed, 999);
    completed.started_at = Some(100);
    completed.completed_at = Some(101);
    completed.error = Some(serde_json::json!({"code": "none"}));
    completed.usage = Some(serde_json::json!({"input_tokens": 3}));
    store
        .swarm_upsert_turn(completed.clone())
        .await
        .expect("terminal write");
    let stored = store
        .swarm_get_turn("logical-1", "turn-1")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(stored.status, TurnStatus::Completed);
    assert_eq!(
        stored.created_at, 100,
        "the terminal write keeps the opening created_at"
    );
    assert_eq!(stored.started_at, Some(100));
    assert_eq!(stored.completed_at, Some(101));
    assert_eq!(stored.error, completed.error);
    assert_eq!(stored.usage, completed.usage);
    assert!(!store
        .swarm_has_open_turn("logical-1")
        .await
        .expect("open probe"));

    let page = store
        .swarm_turn_page(
            "logical-1",
            &TurnsWindow {
                order: ItemOrder::Asc,
                after: None,
                limit: 10,
            },
        )
        .await
        .expect("turn page");
    assert_eq!(page.len(), 1);

    // Crash reconciliation closes every non-terminal row of the session.
    store
        .swarm_upsert_turn(turn("logical-1", "turn-2", TurnStatus::Waiting, 200))
        .await
        .expect("open second turn");
    let closed = store
        .swarm_close_open_turns(
            "logical-1",
            TurnStatus::Failed,
            300,
            Some(serde_json::json!({"code": "host_restarted"})),
        )
        .await
        .expect("close open turns");
    assert_eq!(closed, 1);
    let stored = store
        .swarm_get_turn("logical-1", "turn-2")
        .await
        .expect("read")
        .expect("row");
    assert_eq!(stored.status, TurnStatus::Failed);
    assert_eq!(stored.completed_at, Some(300));
    assert_eq!(
        store
            .swarm_close_open_turns("logical-1", TurnStatus::Failed, 301, None)
            .await
            .expect("no open turns"),
        0
    );

    store.close().await.expect("close");
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
async fn delete_session_is_atomic_and_reassigns_bindings() {
    let Some(fixture) = TestSchema::create().await else {
        return;
    };
    let store = fixture.store().await;
    let shared = EnvironmentRef {
        kind: EnvironmentKind::OpenAiHosted,
        id: Some("logical-env-1".to_owned()),
    };
    let mut first = session("logical-1", "/workspace/a");
    first.environment = shared.clone();
    let mut second = session("logical-2", "/workspace/a");
    second.environment = shared;
    for row in [first, second] {
        store.swarm_upsert_session(row).await.expect("seed");
    }
    store
        .swarm_append_item(item("logical-1", 1, "a", "a0"))
        .await
        .expect("item");
    store
        .swarm_put_input(input("logical-1", "input-1", "fp"))
        .await
        .expect("input");
    store
        .swarm_upsert_turn(turn("logical-1", "turn-1", TurnStatus::Completed, 1))
        .await
        .expect("turn");
    store
        .swarm_put_binding(binding("logical-env-1", "logical-1"))
        .await
        .expect("binding");

    store
        .swarm_delete_session("logical-1")
        .await
        .expect("delete");

    for table in ["swarm_items", "swarm_inputs", "swarm_turns"] {
        assert_eq!(
            fixture.count(table).await,
            0,
            "{table} rows of the deleted session are gone"
        );
    }
    assert_eq!(
        store
            .swarm_get_binding("logical-env-1")
            .await
            .expect("binding")
            .expect("the shared environment still exists")
            .logical_id,
        "logical-2",
        "the binding is handed to the remaining user of the environment"
    );
    assert!(store
        .swarm_get_session("logical-1")
        .await
        .expect("read")
        .is_none());

    // Deleting the last user removes the binding as well.
    store
        .swarm_delete_session("logical-2")
        .await
        .expect("delete");
    assert!(store
        .swarm_get_binding("logical-env-1")
        .await
        .expect("binding")
        .is_none());

    store.close().await.expect("close");
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
async fn hosted_session_requires_a_matching_binding() {
    let Some(fixture) = TestSchema::create().await else {
        return;
    };
    let store = fixture.store().await;
    let mut row = session("logical-1", "/workspace/a");
    row.environment = EnvironmentRef {
        kind: EnvironmentKind::OpenAiHosted,
        id: Some("logical-env-1".to_owned()),
    }
    .normalized();
    let existing = binding("logical-env-1", "logical-0");
    store
        .swarm_put_binding(existing.clone())
        .await
        .expect("binding");

    // The sandbox no longer matches: the session is not created and the binding
    // keeps pointing at its previous session.
    let mut mismatched = existing.clone();
    mismatched.sandbox_id = "sandbox-2".to_owned();
    mismatched.last_used_at = "2099-01-01T00:00:00Z".to_owned();
    let error = store
        .swarm_create_hosted_session(row.clone(), mismatched, true)
        .await
        .expect_err("the binding does not match");
    assert!(matches!(error, StoreError::NotFound(_)), "{error:?}");
    assert_eq!(
        store
            .swarm_get_binding("logical-env-1")
            .await
            .expect("binding"),
        Some(existing.clone()),
        "a rejected attach changes nothing"
    );
    assert!(store
        .swarm_get_session("logical-1")
        .await
        .expect("read")
        .is_none());

    // A matching attach writes the session and moves the binding. The caller
    // supplies the binding row of the **new** session.
    let mut attached = existing.clone();
    attached.logical_id = "logical-1".to_owned();
    attached.workspace_path = "/workspace/cwd".to_owned();
    store
        .swarm_create_hosted_session(row.clone(), attached.clone(), true)
        .await
        .expect("attach");
    assert_eq!(
        store
            .swarm_get_binding("logical-env-1")
            .await
            .expect("binding")
            .expect("binding")
            .logical_id,
        "logical-1"
    );
    assert!(store
        .swarm_get_session("logical-1")
        .await
        .expect("read")
        .is_some());

    // A new environment upserts the binding and inserts the session together.
    let mut fresh = session("logical-2", "/workspace/a");
    fresh.environment = EnvironmentRef {
        kind: EnvironmentKind::OpenAiHosted,
        id: Some("logical-env-2".to_owned()),
    }
    .normalized();
    store
        .swarm_create_hosted_session(fresh, binding("logical-env-2", "logical-2"), false)
        .await
        .expect("new hosted session");
    assert!(store
        .swarm_get_session("logical-2")
        .await
        .expect("read")
        .is_some());

    // An identity conflict rolls the binding update back.
    let mut conflicting = row;
    conflicting.cwd = "/workspace/b".to_owned();
    let conflict = store
        .swarm_create_hosted_session(conflicting, attached, true)
        .await
        .expect_err("identity conflict");
    assert!(matches!(conflict, StoreError::Conflict(_)), "{conflict:?}");
    assert_eq!(
        store
            .swarm_get_binding("logical-env-1")
            .await
            .expect("binding")
            .expect("binding")
            .logical_id,
        "logical-1",
        "the binding update is part of the rolled-back transaction"
    );

    store.close().await.expect("close");
    fixture.close().await;
}

// ── concurrent delete of one hosted environment ─────────────────────────────

/// Two sessions of one hosted environment, both with rows a delete must carry.
async fn seed_shared_environment(
    fixture: &TestSchema,
    store: &crate::store::SwarmStateStore,
    run: usize,
) -> (String, String, String) {
    let first = format!("logical-a-{run}");
    let second = format!("logical-b-{run}");
    let environment = format!("logical-env-{run}");
    for (logical_id, root) in [
        (&first, format!("root-a-{run}")),
        (&second, format!("root-b-{run}")),
    ] {
        let mut row = session(logical_id, "/workspace/a");
        row.root_session_id = root;
        row.environment = EnvironmentRef {
            kind: EnvironmentKind::OpenAiHosted,
            id: Some(environment.clone()),
        };
        store.swarm_upsert_session(row).await.expect("seed session");
        store
            .swarm_append_item(item(logical_id, 1, "i", "t"))
            .await
            .expect("seed item");
        store
            .swarm_put_input(input(logical_id, "input-1", "fp"))
            .await
            .expect("seed receipt");
    }
    store
        .swarm_put_binding(binding(&environment, &first))
        .await
        .expect("seed binding");
    fixture.execute("SELECT 1").await;
    (first, second, environment)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
async fn concurrent_deletes_of_one_environment_leave_no_dangling_binding() {
    let Some(fixture) = TestSchema::create().await else {
        return;
    };
    let store = std::sync::Arc::new(fixture.store().await);

    // Repeated rounds: the hand-over between the two deleters is a race window,
    // and every round must end with both sessions, their rows and the binding
    // gone. A dangling binding would make the next create reuse a dead session.
    for run in 0..25 {
        let (first, second, environment) = seed_shared_environment(&fixture, &store, run).await;
        let left = {
            let store = std::sync::Arc::clone(&store);
            let first = first.clone();
            tokio::spawn(async move { store.swarm_delete_session(&first).await })
        };
        let right = {
            let store = std::sync::Arc::clone(&store);
            let second = second.clone();
            tokio::spawn(async move { store.swarm_delete_session(&second).await })
        };
        left.await.expect("join").expect("delete the first session");
        right
            .await
            .expect("join")
            .expect("delete the second session");
        assert!(
            store
                .swarm_get_binding(&environment)
                .await
                .expect("binding")
                .is_none(),
            "round {run}: the binding outlived every session of its environment"
        );
        assert!(
            store
                .swarm_get_session(&first)
                .await
                .expect("session")
                .is_none()
                && store
                    .swarm_get_session(&second)
                    .await
                    .expect("session")
                    .is_none(),
            "round {run}: both sessions are gone"
        );
        assert_eq!(fixture.count("swarm_items").await, 0, "round {run}");
        assert_eq!(fixture.count("swarm_inputs").await, 0, "round {run}");
    }

    store.close().await.expect("close");
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires PERI_TEST_POSTGRES_URL"]
async fn a_foreign_writer_holding_the_binding_serializes_the_delete() {
    let Some(fixture) = TestSchema::create().await else {
        return;
    };
    let store = std::sync::Arc::new(fixture.store().await);
    let (first, second, environment) = seed_shared_environment(&fixture, &store, 0).await;

    // A competing writer locks the environment binding and then removes the
    // session that a naive hand-over would pick as the successor.
    let mut competitor = fixture.pool().await.begin().await.expect("begin");
    let locked = sqlx::query("SELECT 1 FROM swarm_bindings WHERE logical_env_id = $1 FOR UPDATE")
        .bind(environment.clone())
        .fetch_optional(&mut *competitor)
        .await
        .expect("lock the binding");
    assert!(locked.is_some(), "the fixture owns the binding");

    let deletion = {
        let store = std::sync::Arc::clone(&store);
        let second = second.clone();
        tokio::spawn(async move { store.swarm_delete_session(&second).await })
    };
    // The delete must wait for the binding lock instead of handing the binding to
    // a session that is already gone.
    peri_time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(
        !deletion.is_finished(),
        "delete_session must wait for the binding row lock"
    );

    sqlx::query("DELETE FROM swarm_sessions WHERE logical_id = $1")
        .bind(first.clone())
        .execute(&mut *competitor)
        .await
        .expect("competitor deletes the successor");
    competitor.commit().await.expect("commit");

    deletion
        .await
        .expect("join")
        .expect("delete the second session");
    assert!(
        store
            .swarm_get_binding(&environment)
            .await
            .expect("binding")
            .is_none(),
        "the binding must not point at a deleted session"
    );
    assert!(store
        .swarm_get_session(&first)
        .await
        .expect("session")
        .is_none());
    assert!(store
        .swarm_get_session(&second)
        .await
        .expect("session")
        .is_none());

    store.close().await.expect("close");
    fixture.close().await;
}
