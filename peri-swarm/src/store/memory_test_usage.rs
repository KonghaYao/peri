//! Usage/token persistence tests for the in-memory store.
//!
//! Split out of `memory_test.rs` to keep that file within the source-size gate;
//! the parent test module supplies the shared row builders via `use super::*`.

use super::*;

/// Parity with PostgreSQL's `COALESCE(excluded.usage_json, stored)`: a write
/// that carries no usage keeps the fact an earlier observation persisted.
#[tokio::test]
async fn a_turn_write_without_usage_keeps_the_stored_value() {
    let store = MemorySwarmStore::new();
    store
        .swarm_upsert_session(session_row("agent-u", "/tmp"))
        .await
        .expect("session");
    let mut opening = turn_row("agent-u", "turn-u", 10, TurnStatus::InProgress);
    opening.usage = Some(json!({ "input_tokens": 7, "total_tokens": 7 }));
    store.swarm_upsert_turn(opening).await.expect("usage");

    store
        .swarm_upsert_turn(turn_row("agent-u", "turn-u", 10, TurnStatus::Completed))
        .await
        .expect("terminal without usage");
    let row = store
        .swarm_get_turn("agent-u", "turn-u")
        .await
        .expect("get")
        .expect("turn");
    assert_eq!(row.status, TurnStatus::Completed);
    assert_eq!(
        row.usage.expect("kept usage")["input_tokens"],
        json!(7),
        "a usage-less write must not erase an earlier observation"
    );
}

/// A usage observation touches only `usage_json`: it creates the row it
/// belongs to, refreshes an open row, and never revives a closed one or
/// replaces the usage its terminal write recorded.
#[tokio::test]
async fn a_usage_observation_never_revives_a_closed_turn() {
    let store = MemorySwarmStore::new();
    store
        .swarm_upsert_session(session_row("agent-r", "/tmp"))
        .await
        .expect("session");

    store
        .swarm_record_turn_usage("agent-r", "turn-r", 10, None, &json!({ "total_tokens": 4 }))
        .await
        .expect("observation before the opening write");
    let early = store
        .swarm_get_turn("agent-r", "turn-r")
        .await
        .expect("get")
        .expect("row");
    assert_eq!(
        early.status,
        TurnStatus::InProgress,
        "a missing row is created open"
    );
    assert_eq!(early.usage.expect("usage")["total_tokens"], json!(4));

    let mut terminal = turn_row("agent-r", "turn-r", 10, TurnStatus::Completed);
    terminal.usage = Some(json!({ "total_tokens": 9 }));
    store.swarm_upsert_turn(terminal).await.expect("terminal");

    store
        .swarm_record_turn_usage(
            "agent-r",
            "turn-r",
            10,
            None,
            &json!({ "total_tokens": 99 }),
        )
        .await
        .expect("late observation");
    let closed = store
        .swarm_get_turn("agent-r", "turn-r")
        .await
        .expect("get")
        .expect("row");
    assert_eq!(
        closed.status,
        TurnStatus::Completed,
        "a late observation must not revert the status to in_progress"
    );
    assert_eq!(
        closed.completed_at,
        Some(10),
        "a late observation must not reopen the completion time"
    );
    assert_eq!(
        closed.usage.expect("usage")["total_tokens"],
        json!(9),
        "the terminal usage wins over a later observation"
    );
}

#[tokio::test]
async fn usage_reads_are_newest_first_and_bounded_by_the_window() {
    let store = MemorySwarmStore::new();
    let mut session = session_row("agent-one", "/tmp");
    session.model = Some("claude-test".to_owned());
    session.last_activity_at = "2026-01-02T00:00:00Z".to_owned();
    store
        .swarm_upsert_session(session)
        .await
        .expect("session one");
    store
        .swarm_upsert_session(session_row("agent-two", "/tmp"))
        .await
        .expect("session two");
    for (logical_id, created_at, usage) in [
        ("agent-one", 100, Some(json!({ "total_tokens": 10 }))),
        ("agent-one", 200, Some(json!({ "total_tokens": 20 }))),
        ("agent-two", 150, None),
        ("agent-two", 300, Some(json!({ "total_tokens": 30 }))),
    ] {
        let mut row = turn_row(
            logical_id,
            &format!("turn-{logical_id}-{created_at}"),
            created_at,
            TurnStatus::Completed,
        );
        row.usage = usage;
        store.swarm_upsert_turn(row).await.expect("turn");
    }

    let window = store
        .swarm_turn_usage_window(150, 300, 10)
        .await
        .expect("window");
    assert_eq!(
        window
            .iter()
            .map(|row| row.turn_id.as_str())
            .collect::<Vec<_>>(),
        [
            "turn-agent-two-300",
            "turn-agent-one-200",
            "turn-agent-two-150"
        ],
        "the window is inclusive and newest first"
    );
    assert_eq!(
        window[2].usage, None,
        "a turn without usage is returned; the caller decides what it can report"
    );
    assert_eq!(
        store
            .swarm_turn_usage_window(0, 1_000, 2)
            .await
            .expect("bounded")
            .len(),
        2,
        "the scan is bounded"
    );

    let session_rows = store
        .swarm_session_usage_turns("agent-one", 10)
        .await
        .expect("session rows");
    assert_eq!(
        session_rows
            .iter()
            .map(|row| row.turn_id.as_str())
            .collect::<Vec<_>>(),
        ["turn-agent-one-200", "turn-agent-one-100"]
    );
    assert_eq!(session_rows[0].model.as_deref(), Some("claude-test"));
    assert_eq!(session_rows[0].last_active_at, "2026-01-02T00:00:00Z");
    assert!(store
        .swarm_session_usage_turns("agent-missing", 10)
        .await
        .expect("missing session")
        .is_empty());
}
