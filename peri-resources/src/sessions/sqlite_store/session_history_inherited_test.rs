//! S7.4: inherited 冻结前缀的 bounds/page/seek/body 与父变更隔离。

use super::*;

use peri_acp_types::history_window::{
    HistorySource, HistoryWindowDirection, HISTORY_WINDOW_DEFAULT_BYTE_BUDGET,
};
use peri_acp_types::session_resources::{
    HistoryWindowEntry, HistoryWindowPagePlan, HistoryWindowReadIntent,
    HistoryWindowTailExpectation, InheritedHistoryEntry,
};
use peri_acp_types::store::{serialize_persisted_payload, InheritedContext};

const WINDOW_SESSION: &str = "window-session";
const BUDGET: usize = HISTORY_WINDOW_DEFAULT_BYTE_BUDGET as usize;

async fn session_with_history(
    history: &[PersistedPayload],
) -> (SqliteThreadStore, SqliteSessionData, TempDir) {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    data.save_new_session(&session(
        WINDOW_SESSION,
        workspace.cwd.to_str().unwrap(),
        &workspace,
        frozen(WINDOW_SESSION),
    ))
    .await
    .unwrap();
    data.append_history(&WINDOW_SESSION.to_owned(), history)
        .await
        .unwrap();
    (store, data, directory)
}

async fn set_inherited_context(
    store: &SqliteThreadStore,
    id: &str,
    payloads: &[PersistedPayload],
    ancestor: &str,
) {
    let context = InheritedContext {
        payloads: payloads.to_vec(),
        flags: std::collections::HashMap::new(),
    }
    .to_json()
    .unwrap();
    sqlx::query("UPDATE threads SET inherited_context = ?1, parent_thread_id = ?2 WHERE id = ?3")
        .bind(context)
        .bind(ancestor)
        .bind(id)
        .execute(&store.database.pool)
        .await
        .unwrap();
}

fn inherited_source(
    snapshot: &peri_acp_types::history_window::HistoryInheritedSnapshot,
) -> HistorySource {
    HistorySource::inherited_with_ancestor(
        snapshot.snapshot_id.clone(),
        snapshot.ancestor_thread_id.clone(),
    )
}

#[tokio::test]
async fn inherited_bounds_report_the_frozen_range_and_snapshot_identity() {
    let history = payloads(2);
    let (store, data, _directory) = session_with_history(&history).await;
    let id = WINDOW_SESSION.to_owned();
    let inherited = payloads(3);
    set_inherited_context(&store, &id, &inherited, "parent-session").await;

    let bounds = data.history_window_bounds(&id).await.unwrap();
    let snapshot = bounds
        .inherited
        .snapshot()
        .expect("inherited prefix must be reported");
    assert_eq!(snapshot.entry_count, inherited.len());
    assert_eq!(snapshot.first_entry, Some(inherited[0].id()));
    assert_eq!(snapshot.last_entry, Some(inherited[2].id()));
    assert_eq!(
        snapshot.ancestor_thread_id.as_deref(),
        Some("parent-session")
    );
    assert!(snapshot.snapshot_id.starts_with("inherited:"));
}

#[tokio::test]
async fn inherited_page_seek_and_chunks_are_bounded_and_isolated_from_parent_changes() {
    let history = payloads(2);
    let (store, data, directory) = session_with_history(&history).await;
    let id = WINDOW_SESSION.to_owned();
    let inherited = payloads(4);
    set_inherited_context(&store, &id, &inherited, "parent-session").await;
    // 真实父会话：后续 append/compact/rewind 都发生在父行上，子会话冻结列不变。
    let parent_workspace = workspace(&store, directory.path()).await;
    data.save_new_session(&session(
        "parent-session",
        parent_workspace.cwd.to_str().unwrap(),
        &parent_workspace,
        frozen("parent-session"),
    ))
    .await
    .unwrap();
    data.append_history(&"parent-session".to_owned(), &inherited)
        .await
        .unwrap();
    let bounds = data.history_window_bounds(&id).await.unwrap();
    let snapshot = bounds.inherited.snapshot().unwrap().clone();
    let source = inherited_source(&snapshot);

    let first = data
        .load_history_window_page(
            &id,
            &HistoryWindowPagePlan {
                intent: HistoryWindowReadIntent::Open,
                source: source.clone(),
                anchor: None,
                direction: HistoryWindowDirection::Before,
                include_anchor: false,
                before_limit: 2,
                after_limit: 0,
                byte_budget: BUDGET,
                tail: HistoryWindowTailExpectation::Fixed(None),
            },
        )
        .await
        .unwrap();
    assert!(first.entries.is_empty());
    assert_eq!(
        first
            .inherited_entries
            .iter()
            .map(InheritedHistoryEntry::entry_id)
            .collect::<Vec<_>>(),
        vec![inherited[2].id(), inherited[3].id()]
    );
    assert_eq!(
        first.inherited_snapshot_id.as_deref(),
        Some(snapshot.snapshot_id.as_str())
    );
    assert_eq!(first.before_cursor_anchor, Some(inherited[2].id()));

    let second = data
        .load_history_window_page(
            &id,
            &HistoryWindowPagePlan {
                intent: HistoryWindowReadIntent::Page,
                source: source.clone(),
                anchor: first.before_cursor_anchor,
                direction: HistoryWindowDirection::Before,
                include_anchor: false,
                before_limit: 2,
                after_limit: 0,
                byte_budget: BUDGET,
                tail: HistoryWindowTailExpectation::Fixed(None),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        second
            .inherited_entries
            .iter()
            .map(InheritedHistoryEntry::entry_id)
            .collect::<Vec<_>>(),
        vec![inherited[0].id(), inherited[1].id()]
    );

    let sought = data
        .load_history_window_page(
            &id,
            &HistoryWindowPagePlan {
                intent: HistoryWindowReadIntent::Seek,
                source: source.clone(),
                anchor: Some(inherited[2].id()),
                direction: HistoryWindowDirection::Around,
                include_anchor: true,
                before_limit: 1,
                after_limit: 1,
                byte_budget: BUDGET,
                tail: HistoryWindowTailExpectation::Fixed(None),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        sought
            .inherited_entries
            .iter()
            .map(InheritedHistoryEntry::entry_id)
            .collect::<Vec<_>>(),
        vec![inherited[2].id(), inherited[3].id()]
    );
    let entry = sought.inherited_entries.first().unwrap();
    let entry_id = entry.entry_id();
    let expected = serialize_persisted_payload(&inherited[2]).unwrap();
    let chunk = data
        .load_history_body_chunk(
            &id,
            source.clone(),
            entry_id,
            0,
            100_000,
            None,
            &peri_acp_types::history_window::HistoryReadTicket::new("ticket"),
        )
        .await
        .unwrap();
    assert_eq!(chunk.bytes, expected.as_bytes());
    assert!(chunk.complete);

    // 父侧变更不改变冻结前缀。
    data.append_history(&"parent-session".to_owned(), &payloads(2))
        .await
        .unwrap();
    data.apply_compaction(
        &"parent-session".to_owned(),
        &CompactionChange {
            flag_updates: Vec::new(),
            appended_messages: vec![BaseMessage::ai("parent summary")],
        },
    )
    .await
    .unwrap();
    data.rewind_history(
        &"parent-session".to_owned(),
        RewindBoundary::KeepThrough(inherited[1].id()),
        &[],
    )
    .await
    .unwrap();
    let again = data
        .load_history_window_page(
            &id,
            &HistoryWindowPagePlan {
                intent: HistoryWindowReadIntent::Seek,
                source,
                anchor: Some(entry_id),
                direction: HistoryWindowDirection::Around,
                include_anchor: true,
                before_limit: 2,
                after_limit: 2,
                byte_budget: BUDGET,
                tail: HistoryWindowTailExpectation::Fixed(None),
            },
        )
        .await
        .unwrap();
    let content = again
        .inherited_entries
        .iter()
        .find_map(|entry| match entry {
            InheritedHistoryEntry::Loaded(item) if item.id == entry_id => {
                Some(item.payload.as_message().unwrap().content())
            }
            _ => None,
        })
        .expect("same inherited entry remains readable");
    assert_eq!(content, inherited[2].as_message().unwrap().content());
}

#[tokio::test]
async fn inherited_snapshot_mismatch_and_absent_prefix_fail_closed() {
    let history = payloads(2);
    let (store, data, _directory) = session_with_history(&history).await;
    let id = WINDOW_SESSION.to_owned();
    let inherited = payloads(2);
    set_inherited_context(&store, &id, &inherited, "parent-session").await;
    let snapshot = data
        .history_window_bounds(&id)
        .await
        .unwrap()
        .inherited
        .snapshot()
        .unwrap()
        .clone();

    let mismatch = data
        .load_history_window_page(
            &id,
            &HistoryWindowPagePlan {
                intent: HistoryWindowReadIntent::Open,
                source: HistorySource::inherited_with_ancestor(
                    "inherited:wrong",
                    snapshot.ancestor_thread_id.clone(),
                ),
                anchor: None,
                direction: HistoryWindowDirection::Before,
                include_anchor: false,
                before_limit: 1,
                after_limit: 0,
                byte_budget: BUDGET,
                tail: HistoryWindowTailExpectation::Fixed(None),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        mismatch.history_window_error().map(|error| error.code()),
        Some(-32015)
    );

    let empty = InheritedContext::default().to_json().unwrap();
    sqlx::query("UPDATE threads SET inherited_context = ?1 WHERE id = ?2")
        .bind(empty)
        .bind(&id)
        .execute(&store.database.pool)
        .await
        .unwrap();
    let bounds = data.history_window_bounds(&id).await.unwrap();
    assert!(!bounds.inherited.is_present());
    let absent = data
        .load_history_window_page(
            &id,
            &HistoryWindowPagePlan {
                intent: HistoryWindowReadIntent::Open,
                source: inherited_source(&snapshot),
                anchor: None,
                direction: HistoryWindowDirection::Before,
                include_anchor: false,
                before_limit: 1,
                after_limit: 0,
                byte_budget: BUDGET,
                tail: HistoryWindowTailExpectation::Fixed(None),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        absent.history_window_error().map(|error| error.code()),
        Some(-32012)
    );
}

#[tokio::test]
async fn inherited_prefix_is_not_part_of_own_pages() {
    let history = payloads(2);
    let (store, data, _directory) = session_with_history(&history).await;
    let id = WINDOW_SESSION.to_owned();
    let inherited = payloads(2);
    set_inherited_context(&store, &id, &inherited, "parent-session").await;

    let bounds = data.history_window_bounds(&id).await.unwrap();
    assert!(bounds.inherited.is_present());
    let own = data
        .load_history_window_page(
            &id,
            &HistoryWindowPagePlan::open(
                HistorySource::Own,
                None,
                HistoryWindowDirection::Before,
                10,
                0,
                BUDGET,
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        own.entries
            .iter()
            .map(HistoryWindowEntry::entry_id)
            .collect::<Vec<_>>(),
        history.iter().map(PersistedPayload::id).collect::<Vec<_>>()
    );
    assert!(own.inherited_entries.is_empty());
}

fn inherited_ids(
    rows: &peri_acp_types::session_resources::HistoryWindowRows,
) -> Vec<peri_acp_types::messages::MessageId> {
    rows.inherited_entries
        .iter()
        .map(InheritedHistoryEntry::entry_id)
        .collect()
}

async fn inherited_snapshot_source(
    data: &SqliteSessionData,
    id: &str,
) -> peri_acp_types::history_window::HistorySource {
    let snapshot = data
        .history_window_bounds(&id.to_owned())
        .await
        .unwrap()
        .inherited
        .snapshot()
        .unwrap()
        .clone();
    inherited_source(&snapshot)
}

#[tokio::test]
async fn inherited_before_page_exposes_a_later_cursor() {
    let history = payloads(2);
    let (store, data, _directory) = session_with_history(&history).await;
    let id = WINDOW_SESSION.to_owned();
    let inherited = payloads(4);
    set_inherited_context(&store, &id, &inherited, "parent-session").await;
    let source = inherited_snapshot_source(&data, &id).await;

    let opened = data
        .load_history_window_page(
            &id,
            &HistoryWindowPagePlan {
                intent: HistoryWindowReadIntent::Open,
                source: source.clone(),
                anchor: None,
                direction: HistoryWindowDirection::Before,
                include_anchor: false,
                before_limit: 2,
                after_limit: 0,
                byte_budget: BUDGET,
                tail: HistoryWindowTailExpectation::Fixed(None),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        inherited_ids(&opened),
        vec![inherited[2].id(), inherited[3].id()]
    );
    assert_eq!(
        opened.after_cursor_anchor, None,
        "页已到继承快照末尾：更晚方向没有可读行"
    );

    let older = data
        .load_history_window_page(
            &id,
            &HistoryWindowPagePlan {
                intent: HistoryWindowReadIntent::Page,
                source,
                anchor: Some(inherited[2].id()),
                direction: HistoryWindowDirection::Before,
                include_anchor: false,
                before_limit: 1,
                after_limit: 0,
                byte_budget: BUDGET,
                tail: HistoryWindowTailExpectation::Fixed(None),
            },
        )
        .await
        .unwrap();
    assert_eq!(inherited_ids(&older), vec![inherited[1].id()]);
    assert_eq!(older.before_cursor_anchor, Some(inherited[1].id()));
    assert_eq!(
        older.after_cursor_anchor,
        Some(inherited[1].id()),
        "继承区更早页也必须暴露更晚方向的内侧锚点"
    );
}

#[tokio::test]
async fn inherited_open_entry_after_includes_the_anchor() {
    let history = payloads(2);
    let (store, data, _directory) = session_with_history(&history).await;
    let id = WINDOW_SESSION.to_owned();
    let inherited = payloads(4);
    set_inherited_context(&store, &id, &inherited, "parent-session").await;
    let source = inherited_snapshot_source(&data, &id).await;

    let opened = data
        .load_history_window_page(
            &id,
            &HistoryWindowPagePlan {
                intent: HistoryWindowReadIntent::Open,
                source,
                anchor: Some(inherited[1].id()),
                direction: HistoryWindowDirection::After,
                include_anchor: true,
                before_limit: 0,
                after_limit: 2,
                byte_budget: BUDGET,
                tail: HistoryWindowTailExpectation::Fixed(None),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        inherited_ids(&opened),
        vec![inherited[1].id(), inherited[2].id()],
        "after 半段必须包含锚点自身，且只出现一次"
    );
    assert_eq!(opened.before_cursor_anchor, Some(inherited[1].id()));
    assert_eq!(opened.after_cursor_anchor, Some(inherited[2].id()));
}

#[tokio::test]
async fn inherited_after_page_exposes_an_earlier_cursor() {
    let history = payloads(2);
    let (store, data, _directory) = session_with_history(&history).await;
    let id = WINDOW_SESSION.to_owned();
    let inherited = payloads(4);
    set_inherited_context(&store, &id, &inherited, "parent-session").await;
    let source = inherited_snapshot_source(&data, &id).await;

    let newer = data
        .load_history_window_page(
            &id,
            &HistoryWindowPagePlan {
                intent: HistoryWindowReadIntent::Page,
                source,
                anchor: Some(inherited[0].id()),
                direction: HistoryWindowDirection::After,
                include_anchor: false,
                before_limit: 0,
                after_limit: 2,
                byte_budget: BUDGET,
                tail: HistoryWindowTailExpectation::Fixed(None),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        inherited_ids(&newer),
        vec![inherited[1].id(), inherited[2].id()]
    );
    assert_eq!(
        newer.before_cursor_anchor,
        Some(inherited[1].id()),
        "继承区更晚页也必须暴露更早方向的内侧锚点"
    );
    assert_eq!(newer.after_cursor_anchor, Some(inherited[2].id()));
}
