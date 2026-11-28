use serde_json::json;

use super::*;
use crate::domain::{
    EnvironmentKind, EnvironmentRef, InputState, ItemKind, ItemSource, McpHeader, McpToolSpec,
    SessionState,
};

#[path = "memory_test_usage.rs"]
mod usage;

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
        metadata: Default::default(),
        prompt_cache_options: None,
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
async fn append_items_rejects_a_run_that_conflicts_without_writing_any_row() {
    let store = MemorySwarmStore::new();
    store
        .swarm_append_item(item_row("agent", 1))
        .await
        .expect("append");
    let error = store
        .swarm_append_items(&[item_row("agent", 2), item_row("agent", 1)], None)
        .await
        .expect_err("the second row conflicts with the existing seq");
    assert!(matches!(error, StoreError::Conflict(_)), "{error:?}");
    let rows = store
        .swarm_items_after("agent", 0, 10)
        .await
        .expect("read rows");
    assert_eq!(
        rows.iter().map(|row| row.seq).collect::<Vec<_>>(),
        [1],
        "a rejected run must leave no row behind: {rows:?}"
    );
}

#[tokio::test]
async fn append_items_with_a_missing_progress_session_writes_nothing() {
    let store = MemorySwarmStore::new();
    let error = store
        .swarm_append_items(
            &[item_row("agent", 1), item_row("agent", 2)],
            Some(SwarmSessionUpdate {
                logical_id: "agent".to_owned(),
                state: SessionState::Running,
                last_item_seq: 2,
                last_activity_at: "2026-01-01T00:00:01.000000Z".to_owned(),
            }),
        )
        .await
        .expect_err("the progress session does not exist");
    assert!(matches!(error, StoreError::NotFound(_)), "{error:?}");
    let rows = store
        .swarm_items_after("agent", 0, 10)
        .await
        .expect("read rows");
    assert!(
        rows.is_empty(),
        "a missing progress session must roll back the run: {rows:?}"
    );
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

#[tokio::test]
async fn item_page_spends_limit_as_an_aggregate_row_budget() {
    let store = MemorySwarmStore::new();
    for (seq, item_id) in [(1, "itim_a"), (2, "itim_a"), (3, "itim_a"), (4, "itim_b")] {
        let mut row = item_row("agent", seq);
        row.item_id = item_id.to_owned();
        store.swarm_append_item(row).await.expect("append");
    }

    // The first item already spends the two-row budget, so the next item waits
    // for the following page even though the item cap would allow it.
    let first = store
        .swarm_item_page("agent", crate::domain::ItemOrder::Asc, None, 2)
        .await
        .expect("page");
    assert_eq!(
        first.rows.iter().map(|row| row.seq).collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert_eq!(
        first.items.iter().map(|row| row.seq).collect::<Vec<_>>(),
        [3]
    );
    assert!(first.has_more);
    assert_eq!(first.next_cursor, Some(3));

    let second = store
        .swarm_item_page("agent", crate::domain::ItemOrder::Asc, Some(3), 2)
        .await
        .expect("page");
    assert_eq!(
        second.rows.iter().map(|row| row.seq).collect::<Vec<_>>(),
        [4]
    );
    assert!(!second.has_more);
    assert_eq!(second.next_cursor, None);

    // A single item larger than the budget is still returned whole, so its
    // final frame cannot be lost.
    let whole = store
        .swarm_item_page("agent", crate::domain::ItemOrder::Asc, None, 1)
        .await
        .expect("page");
    assert_eq!(
        whole.rows.iter().map(|row| row.seq).collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert!(whole.has_more);
}

// ── official session resources (sessions.list/update/delete, turns) ─────────

fn window(
    order: crate::domain::ItemOrder,
    after: Option<&str>,
    limit: u32,
    metadata: &[(&str, &str)],
) -> SessionsWindow {
    SessionsWindow {
        order,
        after: after.map(str::to_owned),
        limit,
        agent_id: None,
        metadata: metadata
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect(),
    }
}

fn turn_row(logical_id: &str, turn_id: &str, created_at: i64, status: TurnStatus) -> SwarmTurnRow {
    SwarmTurnRow {
        logical_id: logical_id.to_owned(),
        turn_id: turn_id.to_owned(),
        status,
        created_at,
        started_at: Some(created_at),
        completed_at: if status.is_open() {
            None
        } else {
            Some(created_at)
        },
        error: None,
        usage: None,
        subagent_id: None,
    }
}

#[tokio::test]
async fn session_list_orders_by_id_and_matches_metadata_exactly() {
    let store = MemorySwarmStore::new();
    for (id, team) in [
        ("agent-a", "alpha"),
        ("agent-b", "beta"),
        ("agent-c", "alpha"),
    ] {
        let mut row = session_row(id, "/tmp");
        row.metadata.insert("team".to_owned(), team.to_owned());
        store.swarm_upsert_session(row).await.expect("upsert");
    }

    let desc = store
        .swarm_list_sessions(&window(ItemOrder::Desc, None, 10, &[]))
        .await
        .expect("list");
    assert_eq!(
        desc.iter()
            .map(|row| row.logical_id.as_str())
            .collect::<Vec<_>>(),
        ["agent-c", "agent-b", "agent-a"]
    );

    let asc = store
        .swarm_list_sessions(&window(ItemOrder::Asc, None, 10, &[]))
        .await
        .expect("list");
    assert_eq!(
        asc.iter()
            .map(|row| row.logical_id.as_str())
            .collect::<Vec<_>>(),
        ["agent-a", "agent-b", "agent-c"]
    );

    let after = store
        .swarm_list_sessions(&window(ItemOrder::Desc, Some("agent-b"), 10, &[]))
        .await
        .expect("list");
    assert_eq!(
        after
            .iter()
            .map(|row| row.logical_id.as_str())
            .collect::<Vec<_>>(),
        ["agent-a"],
        "after is exclusive in the selected order"
    );

    let filtered = store
        .swarm_list_sessions(&window(ItemOrder::Asc, None, 10, &[("team", "alpha")]))
        .await
        .expect("list");
    assert_eq!(
        filtered
            .iter()
            .map(|row| row.logical_id.as_str())
            .collect::<Vec<_>>(),
        ["agent-a", "agent-c"]
    );

    let missing = store
        .swarm_list_sessions(&window(ItemOrder::Asc, None, 10, &[("team", "missing")]))
        .await
        .expect("list");
    assert!(missing.is_empty());

    let mut other_agent = window(ItemOrder::Asc, None, 10, &[]);
    other_agent.agent_id = Some("someone-else".to_owned());
    assert!(store
        .swarm_list_sessions(&other_agent)
        .await
        .expect("list")
        .is_empty());

    let probe = store
        .swarm_list_sessions(&window(ItemOrder::Asc, None, 2, &[]))
        .await
        .expect("list");
    assert_eq!(
        probe.len(),
        3,
        "the store returns limit + 1 rows for has_more"
    );
}

#[tokio::test]
async fn session_patch_replaces_and_clears_official_fields() {
    let store = MemorySwarmStore::new();
    let mut row = session_row("agent-p", "/tmp");
    row.metadata.insert("old".to_owned(), "value".to_owned());
    row.prompt_cache_options = Some(crate::domain::PromptCacheOptions::new(
        crate::domain::PromptCacheTtl::ThirtyMinutes,
    ));
    store.swarm_upsert_session(row).await.expect("upsert");

    let tool = McpToolSpec::new("deepwiki", "https://mcp.example.test/mcp");
    store
        .swarm_patch_session(SwarmSessionPatch {
            logical_id: "agent-p".to_owned(),
            model: Some("model-2".to_owned()),
            mcp_tools: Some(vec![tool.clone()]),
            metadata: Some([("new".to_owned(), "value".to_owned())].into()),
            prompt_cache_options: Some(Some(crate::domain::PromptCacheOptions::new(
                crate::domain::PromptCacheTtl::TwelveHours,
            ))),
            last_activity_at: None,
        })
        .await
        .expect("patch");

    let row = store
        .swarm_get_session("agent-p")
        .await
        .expect("get")
        .expect("row");
    assert_eq!(row.model.as_deref(), Some("model-2"));
    assert_eq!(row.mcp_tools, vec![tool]);
    assert_eq!(row.metadata.keys().collect::<Vec<_>>(), [&"new".to_owned()]);
    assert_eq!(
        row.prompt_cache_options.expect("ttl").ttl,
        crate::domain::PromptCacheTtl::TwelveHours
    );

    store
        .swarm_patch_session(SwarmSessionPatch {
            logical_id: "agent-p".to_owned(),
            model: None,
            mcp_tools: Some(Vec::new()),
            metadata: Some(Default::default()),
            prompt_cache_options: Some(None),
            last_activity_at: None,
        })
        .await
        .expect("clear");
    let row = store
        .swarm_get_session("agent-p")
        .await
        .expect("get")
        .expect("row");
    assert_eq!(row.model.as_deref(), Some("model-2"), "omitted model stays");
    assert!(row.mcp_tools.is_empty());
    assert!(row.metadata.is_empty());
    assert_eq!(row.prompt_cache_options, None);

    assert!(matches!(
        store
            .swarm_patch_session(SwarmSessionPatch {
                logical_id: "missing".to_owned(),
                model: None,
                mcp_tools: None,
                metadata: None,
                prompt_cache_options: None,
                last_activity_at: None,
            })
            .await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn deleting_either_shared_session_keeps_the_environment_until_its_last_user_leaves() {
    for (first, survivor) in [("agent-a", "agent-c"), ("agent-c", "agent-a")] {
        let store = MemorySwarmStore::new();
        for id in ["agent-a", "agent-c"] {
            let mut row = session_row(id, "/work");
            row.root_session_id = format!("root-{id}");
            row.environment = EnvironmentRef {
                kind: EnvironmentKind::OpenAiHosted,
                id: Some("shared".into()),
            };
            let mut binding = binding_for(id);
            binding.logical_env_id = "shared".into();
            binding.sandbox_id = "stable".into();
            binding.workspace_path = "/work/shared".into();
            store
                .swarm_create_hosted_session(row, binding, id == "agent-c")
                .await
                .expect("attach");
        }
        let mut late = session_row("late", "/work");
        late.root_session_id = "another-root".into();
        let mut changed = store
            .swarm_get_binding("shared")
            .await
            .expect("get")
            .expect("shared");
        changed.logical_id = late.logical_id.clone();
        changed.workspace_path = "/different".into();
        assert!(matches!(
            store
                .swarm_create_hosted_session(late.clone(), changed.clone(), true)
                .await,
            Err(StoreError::NotFound(_))
        ));
        assert!(store
            .swarm_get_session("late")
            .await
            .expect("get")
            .is_none());
        let mut conflicting = session_row("agent-c", "/other");
        conflicting.environment = EnvironmentRef {
            kind: EnvironmentKind::OpenAiHosted,
            id: Some("unclaimed".into()),
        };
        let mut unrelated = binding_for("agent-c");
        unrelated.logical_env_id = "unclaimed".into();
        assert!(matches!(
            store
                .swarm_create_hosted_session(conflicting, unrelated, false)
                .await,
            Err(StoreError::Conflict(_))
        ));
        assert!(store
            .swarm_get_binding("unclaimed")
            .await
            .expect("get")
            .is_none());

        store
            .swarm_delete_session(first)
            .await
            .expect("delete first");
        let active = store
            .swarm_get_binding("shared")
            .await
            .expect("get")
            .expect("retained");
        assert_eq!(active.logical_id, survivor);
        assert!(store
            .swarm_get_session(survivor)
            .await
            .expect("get")
            .is_some());
        store
            .swarm_delete_session(survivor)
            .await
            .expect("delete last");
        changed.workspace_path = "/work/shared".into();
        assert!(matches!(
            store.swarm_create_hosted_session(late, changed, true).await,
            Err(StoreError::NotFound(_))
        ));
        assert!(store
            .swarm_get_session("late")
            .await
            .expect("get")
            .is_none());

        assert!(store
            .swarm_get_binding("shared")
            .await
            .expect("get")
            .is_none());
    }
}

#[tokio::test]
async fn delete_session_removes_every_scoped_row() {
    let store = MemorySwarmStore::new();
    store
        .swarm_upsert_session(session_row("agent-d", "/tmp"))
        .await
        .expect("session");
    store
        .swarm_append_item(item_row("agent-d", 1))
        .await
        .expect("item");
    store
        .swarm_put_input(sample_input_for("agent-d"))
        .await
        .expect("input");
    store
        .swarm_put_binding(binding_for("agent-d"))
        .await
        .expect("binding");
    store
        .swarm_upsert_turn(turn_row("agent-d", "turn-1", 10, TurnStatus::Completed))
        .await
        .expect("turn");

    store.swarm_delete_session("agent-d").await.expect("delete");
    assert!(store
        .swarm_get_session("agent-d")
        .await
        .expect("get")
        .is_none());
    assert_eq!(store.swarm_last_seq("agent-d").await.expect("seq"), 0);
    assert!(store
        .swarm_get_input("agent-d", "input-1")
        .await
        .expect("input")
        .is_none());
    assert!(store
        .swarm_get_binding("env-agent-d")
        .await
        .expect("binding")
        .is_none());
    assert!(store
        .swarm_get_turn("agent-d", "turn-1")
        .await
        .expect("turn")
        .is_none());
}

#[tokio::test]
async fn turn_page_is_ordered_and_after_is_exclusive_in_both_orders() {
    let store = MemorySwarmStore::new();
    store
        .swarm_upsert_session(session_row("agent-t", "/tmp"))
        .await
        .expect("session");
    store
        .swarm_upsert_turn(turn_row("agent-t", "turn-b", 10, TurnStatus::Completed))
        .await
        .expect("first");
    store
        .swarm_upsert_turn(turn_row("agent-t", "turn-a", 10, TurnStatus::Completed))
        .await
        .expect("tie-break");
    store
        .swarm_upsert_turn(turn_row("agent-t", "turn-c", 11, TurnStatus::Failed))
        .await
        .expect("last");

    let asc = store
        .swarm_turn_page(
            "agent-t",
            &TurnsWindow {
                order: ItemOrder::Asc,
                after: None,
                limit: 10,
            },
        )
        .await
        .expect("page");
    assert_eq!(
        asc.iter()
            .map(|row| row.turn_id.as_str())
            .collect::<Vec<_>>(),
        ["turn-a", "turn-b", "turn-c"],
        "same-second turns use the turn id as tie-break"
    );

    let desc = store
        .swarm_turn_page(
            "agent-t",
            &TurnsWindow {
                order: ItemOrder::Desc,
                after: None,
                limit: 1,
            },
        )
        .await
        .expect("page");
    assert_eq!(
        desc.iter()
            .map(|row| row.turn_id.as_str())
            .collect::<Vec<_>>(),
        ["turn-c", "turn-b"],
        "the store returns limit + 1 rows for has_more"
    );

    let after = store
        .swarm_turn_page(
            "agent-t",
            &TurnsWindow {
                order: ItemOrder::Desc,
                after: Some("turn-b".to_owned()),
                limit: 2,
            },
        )
        .await
        .expect("page");
    assert_eq!(
        after
            .iter()
            .map(|row| row.turn_id.as_str())
            .collect::<Vec<_>>(),
        ["turn-a"]
    );

    // A terminal write keeps the opening `created_at`.
    let mut completed = turn_row("agent-t", "turn-b", 999, TurnStatus::Completed);
    completed.completed_at = Some(12);
    store.swarm_upsert_turn(completed).await.expect("update");
    let row = store
        .swarm_get_turn("agent-t", "turn-b")
        .await
        .expect("get")
        .expect("turn");
    assert_eq!(row.created_at, 10);
    assert_eq!(row.completed_at, Some(12));
}

#[tokio::test]
async fn close_open_turns_only_touches_non_terminal_rows() {
    let store = MemorySwarmStore::new();
    store
        .swarm_upsert_session(session_row("agent-c", "/tmp"))
        .await
        .expect("session");
    store
        .swarm_upsert_turn(turn_row("agent-c", "open", 10, TurnStatus::InProgress))
        .await
        .expect("open");
    store
        .swarm_upsert_turn(turn_row("agent-c", "done", 11, TurnStatus::Completed))
        .await
        .expect("done");

    let closed = store
        .swarm_close_open_turns(
            "agent-c",
            TurnStatus::Failed,
            12,
            Some(json!({ "code": "internal_error", "message": "interrupted" })),
        )
        .await
        .expect("close");
    assert_eq!(closed, 1);

    let open = store
        .swarm_get_turn("agent-c", "open")
        .await
        .expect("get")
        .expect("turn");
    assert_eq!(open.status, TurnStatus::Failed);
    assert_eq!(open.completed_at, Some(12));
    assert_eq!(open.error.expect("error")["code"], json!("internal_error"));

    let done = store
        .swarm_get_turn("agent-c", "done")
        .await
        .expect("get")
        .expect("turn");
    assert_eq!(done.status, TurnStatus::Completed);
    assert_eq!(done.completed_at, Some(11));
}

fn sample_input_for(logical_id: &str) -> SwarmInputRow {
    SwarmInputRow {
        logical_id: logical_id.to_owned(),
        input_id: "input-1".to_owned(),
        fingerprint: "sha256:abc".to_owned(),
        state: InputState::Accepted,
        created_at: "2026-01-01T00:00:00Z".to_owned(),
    }
}

fn binding_for(logical_id: &str) -> SwarmBindingRow {
    SwarmBindingRow {
        logical_env_id: format!("env-{logical_id}"),
        logical_id: logical_id.to_owned(),
        sandbox_id: "sandbox-1".to_owned(),
        workspace_path: "/tmp/ws".to_owned(),
        sandbox_root: "/tmp".to_owned(),
        settings_profile: None,
        token_file: None,
        last_run_id: None,
        last_used_at: "2026-01-01T00:00:00Z".to_owned(),
    }
}
