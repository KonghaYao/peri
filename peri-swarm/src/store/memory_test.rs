use serde_json::json;

use super::*;
use crate::domain::{
    EnvironmentKind, EnvironmentRef, InputState, ItemKind, ItemSource, McpHeader, McpToolSpec,
    SessionState,
};

fn session_row(logical_id: &str, cwd: &str) -> SwarmSessionRow {
    SwarmSessionRow {
        logical_id: logical_id.to_owned(),
        root_session_id: "0192f0c4-0000-7000-8000-000000000001".to_owned(),
        actor_key: format!("agent/{logical_id}"),
        cwd: cwd.to_owned(),
        state: SessionState::Idle,
        model: None,
        instructions: None,
        mcp_tools: Vec::new(),
        environment: EnvironmentRef {
            kind: EnvironmentKind::None,
            id: Some(logical_id.to_owned()),
        },
        created_at: "2026-01-01T00:00:00.000000Z".to_owned(),
        last_activity_at: "2026-01-01T00:00:00.000000Z".to_owned(),
        last_item_seq: 0,
    }
}

fn item_row(logical_id: &str, seq: u64) -> SwarmItemRow {
    SwarmItemRow {
        logical_id: logical_id.to_owned(),
        seq,
        item_id: format!("itim_test_{seq}"),
        kind: ItemKind::Message,
        source: ItemSource::root(),
        payload: json!({ "text": format!("item {seq}") }),
        created_at: "2026-01-01T00:00:00.000000Z".to_owned(),
        truncated: false,
    }
}

fn input_row(logical_id: &str, input_id: &str, fingerprint: &str) -> SwarmInputRow {
    SwarmInputRow {
        logical_id: logical_id.to_owned(),
        input_id: input_id.to_owned(),
        fingerprint: fingerprint.to_owned(),
        state: InputState::Accepted,
        created_at: "2026-01-01T00:00:00.000000Z".to_owned(),
    }
}

#[tokio::test]
async fn items_after_is_exclusive_and_bounded() {
    let store = MemorySwarmStore::new();
    for seq in 1..=5 {
        store
            .swarm_append_item(item_row("agent", seq))
            .await
            .expect("append");
    }

    let first = store.swarm_items_after("agent", 0, 2).await.expect("page");
    assert_eq!(first.iter().map(|row| row.seq).collect::<Vec<_>>(), [1, 2]);

    let middle = store.swarm_items_after("agent", 3, 10).await.expect("page");
    assert_eq!(middle.iter().map(|row| row.seq).collect::<Vec<_>>(), [4, 5]);

    let at_tail = store.swarm_items_after("agent", 5, 10).await.expect("page");
    assert!(at_tail.is_empty());

    let beyond = store
        .swarm_items_after("agent", 99, 10)
        .await
        .expect("page");
    assert!(beyond.is_empty());

    let unknown = store.swarm_items_after("other", 0, 10).await.expect("page");
    assert!(unknown.is_empty());
}

#[tokio::test]
async fn items_before_returns_newest_first() {
    let store = MemorySwarmStore::new();
    for seq in 1..=5 {
        store
            .swarm_append_item(item_row("agent", seq))
            .await
            .expect("append");
    }

    let newest = store
        .swarm_items_before("agent", None, 2)
        .await
        .expect("page");
    assert_eq!(newest.iter().map(|row| row.seq).collect::<Vec<_>>(), [5, 4]);

    let older = store
        .swarm_items_before("agent", Some(4), 2)
        .await
        .expect("page");
    assert_eq!(older.iter().map(|row| row.seq).collect::<Vec<_>>(), [3, 2]);

    let none = store
        .swarm_items_before("agent", Some(1), 2)
        .await
        .expect("page");
    assert!(none.is_empty());
}

#[tokio::test]
async fn last_seq_reads_the_item_table_not_the_session_projection() {
    let store = MemorySwarmStore::new();
    let mut row = session_row("agent", "/tmp/agent");
    row.last_item_seq = 9;
    store.swarm_upsert_session(row).await.expect("session");
    assert_eq!(store.swarm_last_seq("agent").await.expect("seq"), 0);

    store
        .swarm_append_item(item_row("agent", 3))
        .await
        .expect("append");
    store
        .swarm_append_item(item_row("agent", 7))
        .await
        .expect("append");
    assert_eq!(store.swarm_last_seq("agent").await.expect("seq"), 7);
}

#[tokio::test]
async fn item_page_groups_rows_by_item_and_advances_past_a_whole_item() {
    let store = MemorySwarmStore::new();
    // Two streamed items interleaved by seq, plus one single-row item.
    let rows = [
        (1, "itim_a"),
        (2, "itim_b"),
        (3, "itim_a"),
        (4, "itim_b"),
        (5, "itim_c"),
    ];
    for (seq, item_id) in rows {
        let mut row = item_row("agent", seq);
        row.item_id = item_id.to_owned();
        store.swarm_append_item(row).await.expect("append");
    }

    // `limit` counts items: the first item owns two rows but is one entry.
    let first = store
        .swarm_item_page("agent", crate::domain::ItemOrder::Asc, None, 1)
        .await
        .expect("page");
    assert_eq!(
        first.rows.iter().map(|row| row.seq).collect::<Vec<_>>(),
        [1, 3],
        "every row of the page's item is returned"
    );
    assert_eq!(
        first.items.iter().map(|row| row.seq).collect::<Vec<_>>(),
        [3],
        "`data` keeps one row per item: its greatest seq"
    );
    assert!(first.has_more);
    assert_eq!(first.next_cursor, Some(3));

    // `after` is the item's greatest seq, so the same item cannot repeat.
    let second = store
        .swarm_item_page("agent", crate::domain::ItemOrder::Asc, Some(3), 1)
        .await
        .expect("page");
    assert_eq!(
        second.items.iter().map(|row| row.seq).collect::<Vec<_>>(),
        [4]
    );
    assert_eq!(second.next_cursor, Some(4));

    let third = store
        .swarm_item_page("agent", crate::domain::ItemOrder::Asc, Some(4), 1)
        .await
        .expect("page");
    assert_eq!(
        third.items.iter().map(|row| row.seq).collect::<Vec<_>>(),
        [5]
    );
    assert!(!third.has_more);
    assert_eq!(third.next_cursor, None);

    // Descending membership uses the same item boundary: strictly below the
    // item's greatest seq.
    let newest = store
        .swarm_item_page("agent", crate::domain::ItemOrder::Desc, None, 1)
        .await
        .expect("page");
    assert_eq!(
        newest.rows.iter().map(|row| row.seq).collect::<Vec<_>>(),
        [5]
    );
    let middle = store
        .swarm_item_page("agent", crate::domain::ItemOrder::Desc, Some(5), 1)
        .await
        .expect("page");
    assert_eq!(
        middle.rows.iter().map(|row| row.seq).collect::<Vec<_>>(),
        [4, 2]
    );
    assert_eq!(
        middle.items.iter().map(|row| row.seq).collect::<Vec<_>>(),
        [4]
    );
    let oldest = store
        .swarm_item_page("agent", crate::domain::ItemOrder::Desc, Some(4), 1)
        .await
        .expect("page");
    assert_eq!(
        oldest.rows.iter().map(|row| row.seq).collect::<Vec<_>>(),
        [3, 1]
    );
    assert!(!oldest.has_more);
}

#[tokio::test]
async fn duplicate_seq_is_a_visible_conflict() {
    let store = MemorySwarmStore::new();
    store
        .swarm_append_item(item_row("agent", 1))
        .await
        .expect("append");
    let error = store
        .swarm_append_item(item_row("agent", 1))
        .await
        .expect_err("seq conflict");
    assert!(matches!(error, StoreError::Conflict(_)));
}

#[tokio::test]
async fn put_input_is_insert_if_absent() {
    let store = MemorySwarmStore::new();
    let inserted = store
        .swarm_put_input(input_row("agent", "input-1", "fingerprint-a"))
        .await
        .expect("insert");
    assert_eq!(inserted, InputPut::Inserted);

    let existing = store
        .swarm_put_input(input_row("agent", "input-1", "fingerprint-b"))
        .await
        .expect("existing");
    match existing {
        InputPut::Existing(row) => {
            assert_eq!(row.fingerprint, "fingerprint-a");
            assert_eq!(row.state, InputState::Accepted);
        }
        InputPut::Inserted => panic!("second put must not insert"),
    }

    let fetched = store
        .swarm_get_input("agent", "input-1")
        .await
        .expect("get")
        .expect("row");
    assert_eq!(fetched.fingerprint, "fingerprint-a");
}

#[tokio::test]
async fn session_upsert_rejects_identity_change() {
    let store = MemorySwarmStore::new();
    store
        .swarm_upsert_session(session_row("agent", "/tmp/agent"))
        .await
        .expect("insert");

    store
        .swarm_upsert_session(session_row("agent", "/tmp/agent"))
        .await
        .expect("same identity");

    let error = store
        .swarm_upsert_session(session_row("agent", "/tmp/other"))
        .await
        .expect_err("cwd change");
    assert!(matches!(error, StoreError::Conflict(_)));
}

#[tokio::test]
async fn session_rows_keep_the_mcp_declaration_field_by_field() {
    let store = MemorySwarmStore::new();
    let mut row = session_row("agent", "/tmp/agent");
    row.mcp_tools = vec![McpToolSpec {
        headers: vec![McpHeader {
            name: "Authorization".to_owned(),
            value: "Bearer caller".to_owned(),
        }],
        require_approval: Some(json!({ "always": { "tool_names": ["write"] } })),
        allowed_tools: Some(json!(["read"])),
        extra: serde_json::Map::from_iter([("defer_loading".to_owned(), json!(true))]),
        ..McpToolSpec::new("deepwiki", "https://mcp.example.test/mcp")
    }];
    store
        .swarm_upsert_session(row.clone())
        .await
        .expect("insert");

    assert_eq!(
        store.swarm_get_session("agent").await.expect("get"),
        Some(row)
    );
}

#[tokio::test]
async fn update_session_persists_state_and_projection() {
    let store = MemorySwarmStore::new();
    store
        .swarm_upsert_session(session_row("agent", "/tmp/agent"))
        .await
        .expect("insert");
    store
        .swarm_update_session(SwarmSessionUpdate {
            logical_id: "agent".to_owned(),
            state: SessionState::Running,
            last_item_seq: 4,
            last_activity_at: "2026-01-01T00:00:01.000000Z".to_owned(),
        })
        .await
        .expect("update");

    let row = store
        .swarm_get_session("agent")
        .await
        .expect("get")
        .expect("row");
    assert_eq!(row.state, SessionState::Running);
    assert_eq!(row.last_item_seq, 4);
    assert_eq!(row.last_activity_at, "2026-01-01T00:00:01.000000Z");

    let missing = store
        .swarm_update_session(SwarmSessionUpdate {
            logical_id: "missing".to_owned(),
            state: SessionState::Idle,
            last_item_seq: 0,
            last_activity_at: String::new(),
        })
        .await
        .expect_err("missing session");
    assert!(matches!(missing, StoreError::NotFound(_)));
}

#[tokio::test]
async fn bindings_roundtrip_without_token_values() {
    let store = MemorySwarmStore::new();
    let row = SwarmBindingRow {
        logical_env_id: "agent-01".to_owned(),
        logical_id: "agent".to_owned(),
        sandbox_id: "sbx_1".to_owned(),
        workspace_path: "/tmp/agent".to_owned(),
        sandbox_root: "/tmp/sandboxes".to_owned(),
        settings_profile: None,
        token_file: Some("/tmp/sandboxes/agent-01/token".to_owned()),
        last_run_id: Some("run_1".to_owned()),
        last_used_at: "2026-01-01T00:00:00.000000Z".to_owned(),
    };
    store.swarm_put_binding(row.clone()).await.expect("put");
    assert_eq!(
        store
            .swarm_get_binding("agent-01")
            .await
            .expect("get")
            .expect("row"),
        row
    );
    assert!(store
        .swarm_get_binding("missing")
        .await
        .expect("get")
        .is_none());
}
