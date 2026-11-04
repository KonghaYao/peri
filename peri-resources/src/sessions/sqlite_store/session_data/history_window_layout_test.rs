//! own/inherited 窗口读取的分配回归与布局语义。
//!
//! 默认测试只覆盖布局分页行为；16 MiB 夹具与 SQLite 分配测量标记 `#[ignore]`，命令见
//! 各用例文档。分配口径：`sqlite3_memory_highwater`（SQLite 自身分配，不含 Rust Vec），
//! 与审查报告一致；测试串行执行（`--test-threads=1`）。

use super::*;
use crate::sessions::data::SessionDataPort;
use crate::sessions::sqlite_store::SqliteThreadStore;
use peri_acp_types::history_common::{
    HistoryReadTicket, HistorySource, HistoryWindowDirection, HistoryWindowError,
};
use peri_acp_types::messages::{BaseMessage, MessageId};
use peri_acp_types::session_resources::{
    FrozenSnapshotBytes, HistoryWindowEntry, HistoryWindowPagePlan, HistoryWindowReadIntent,
    HistoryWindowTailExpectation, InheritedHistoryEntry, NewSession, NewSessionMeta,
};
use peri_acp_types::store::{
    serialize_persisted_payload, InheritedContext, PersistedPayload, ThreadStore,
};
use peri_acp_types::workspace::{ResolvedWorkspace, SessionBinding};
use std::collections::HashMap;
use tempfile::TempDir;

const LARGE_SESSION: &str = "bounded-read-large";
const LARGE_BYTES: usize = 16 * 1024 * 1024;
const CHUNK_BYTES: u32 = 64 * 1024;

extern "C" {
    fn sqlite3_memory_highwater(reset: i32) -> i64;
    fn sqlite3_memory_used() -> i64;
}

fn sqlite_peak_reset() -> i64 {
    // SAFETY: SQLite 的全局分配计数；测试串行执行。
    let before = unsafe { sqlite3_memory_used() };
    unsafe {
        sqlite3_memory_highwater(1);
    }
    before
}

fn sqlite_peak_extra(before: i64) -> i64 {
    // SAFETY: 同上；只读取计数器。
    let peak = unsafe { sqlite3_memory_highwater(0) };
    (peak - before).max(0)
}

async fn database() -> (SqliteThreadStore, SqliteSessionData, TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteThreadStore::new(directory.path().join("threads.db"))
        .await
        .unwrap();
    let data = SqliteSessionData::new(Arc::clone(&store.database));
    (store, data, directory)
}

fn frozen(marker: &str) -> FrozenSnapshotBytes {
    FrozenSnapshotBytes::new(format!(r#"{{"version":1,"marker":"{marker}"}}"#))
}

fn binding_of(workspace: &ResolvedWorkspace) -> SessionBinding {
    SessionBinding::from_workspace(workspace)
}

async fn workspace(store: &SqliteThreadStore, cwd: &std::path::Path) -> ResolvedWorkspace {
    store.resolve_workspace(cwd).await.unwrap()
}

fn session(
    id: &str,
    cwd: &str,
    workspace: &ResolvedWorkspace,
    frozen: FrozenSnapshotBytes,
) -> NewSession {
    NewSession {
        thread_id: id.to_owned(),
        created_at: "2026-10-18T00:00:00Z".to_owned(),
        meta: NewSessionMeta {
            title: Some(format!("session {id}")),
            cwd: cwd.to_owned(),
            parent_thread_id: None,
            hidden: false,
            cancel_policy: Default::default(),
            snapshot_at_message_id: None,
        },
        binding: binding_of(workspace),
        frozen,
    }
}

async fn session_with_own(
    payloads: &[PersistedPayload],
) -> (SqliteThreadStore, SqliteSessionData, TempDir, String) {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let id = LARGE_SESSION.to_owned();
    data.save_new_session(&session(
        &id,
        workspace.cwd.to_str().unwrap(),
        &workspace,
        frozen(&id),
    ))
    .await
    .unwrap();
    data.append_history(&id, payloads).await.unwrap();
    (store, data, directory, id)
}

async fn session_with_inherited(
    payloads: &[PersistedPayload],
    ancestor: &str,
) -> (SqliteThreadStore, SqliteSessionData, TempDir, String) {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let id = LARGE_SESSION.to_owned();
    data.save_new_session(&session(
        &id,
        workspace.cwd.to_str().unwrap(),
        &workspace,
        frozen(&id),
    ))
    .await
    .unwrap();
    let context = InheritedContext {
        payloads: payloads.to_vec(),
        flags: HashMap::new(),
    }
    .to_json()
    .unwrap();
    sqlx::query("UPDATE threads SET inherited_context = ?1, parent_thread_id = ?2 WHERE id = ?3")
        .bind(context)
        .bind(ancestor)
        .bind(&id)
        .execute(&store.database.pool)
        .await
        .unwrap();
    (store, data, directory, id)
}

fn inherited_source(
    source: &peri_acp_types::history_common::HistoryInheritedSnapshot,
) -> HistorySource {
    HistorySource::inherited_with_ancestor(
        source.snapshot_id.clone(),
        source.ancestor_thread_id.clone(),
    )
}

/// own 16 MiB 正文、64 KiB 分块：字节级重组必须与序列化原文一致，SQLite 分配不随正文增长；
/// 同一夹具上的旧 SQL（`length(CAST(...)) + substr(CAST(...))`）必须物化整条正文。
///
/// ```text
/// ./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- \
///     own_large_body_chunks_stay_bounded_and_exact --ignored --nocapture --test-threads=1
/// ```
#[tokio::test]
#[ignore = "16 MiB 夹具与分配测量；显式运行，见用例文档"]
async fn own_large_body_chunks_stay_bounded_and_exact() {
    let large = PersistedPayload::Message(BaseMessage::human("C".repeat(LARGE_BYTES)));
    let expected = serialize_persisted_payload(&large).unwrap();
    let (_store, data, _directory, id) = session_with_own(std::slice::from_ref(&large)).await;
    let tail = data
        .history_window_bounds(&id)
        .await
        .unwrap()
        .own_latest
        .unwrap();
    let ticket = HistoryReadTicket::new("ticket");
    let start = 8 * 1024 * 1024;

    let before = sqlite_peak_reset();
    let measured = data
        .load_history_body_chunk(
            &id,
            HistorySource::Own,
            large.id(),
            start as u64,
            CHUNK_BYTES,
            Some(&tail),
            &ticket,
        )
        .await
        .unwrap();
    let incremental_extra = sqlite_peak_extra(before);
    assert_eq!(
        measured.bytes,
        expected.as_bytes()[start..start + CHUNK_BYTES as usize]
    );

    let before = sqlite_peak_reset();
    let legacy: (i64, Vec<u8>) = sqlx::query_as(
        "SELECT length(CAST(content AS BLOB)), substr(CAST(content AS BLOB), ?3 + 1, ?4)
         FROM messages
         WHERE thread_id = ?1 AND message_id = ?2 AND rowid <= ?5",
    )
    .bind(&id)
    .bind(large.id().as_uuid().to_string())
    .bind(start as i64)
    .bind(i64::from(CHUNK_BYTES))
    .bind(i64::try_from(tail.position).unwrap())
    .fetch_one(&data.database.pool)
    .await
    .unwrap();
    let legacy_extra = sqlite_peak_extra(before);
    assert_eq!(legacy.0 as usize, expected.len());
    assert_eq!(
        legacy.1,
        expected.as_bytes()[start..start + CHUNK_BYTES as usize]
    );

    println!(
        "own_chunk bytes={} chunk={} incremental_sqlite_extra={} MiB legacy_sqlite_extra={} MiB",
        expected.len(),
        CHUNK_BYTES,
        incremental_extra as f64 / (1024.0 * 1024.0),
        legacy_extra as f64 / (1024.0 * 1024.0),
    );
    assert!(
        incremental_extra < 8 * 1024 * 1024,
        "own chunk read must stay bounded, got {incremental_extra} bytes"
    );
    assert!(
        legacy_extra > 16 * 1024 * 1024,
        "legacy SQL must materialize the body, got {legacy_extra} bytes"
    );

    // 完整重组：每个请求都重新打开 blob 句柄，但区间必须严格推进。
    let mut offset = 0u64;
    let mut collected = Vec::with_capacity(expected.len());
    loop {
        let chunk = data
            .load_history_body_chunk(
                &id,
                HistorySource::Own,
                large.id(),
                offset,
                CHUNK_BYTES,
                Some(&tail),
                &ticket,
            )
            .await
            .unwrap();
        assert!(chunk.bytes.len() <= CHUNK_BYTES as usize);
        collected.extend_from_slice(&chunk.bytes);
        match chunk.next_offset {
            Some(next) => {
                assert!(next > offset);
                offset = next;
            }
            None => break,
        }
    }
    assert_eq!(collected, expected.into_bytes(), "own body reassembly");
}

/// own 分页候选：16 MiB 行只作为 descriptor，阶段一不得用 `length(CAST(content))`
/// 物化整条正文；小行正文仍精确。
///
/// ```text
/// ./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- \
///     own_page_candidate_lengths_avoid_materializing --ignored --nocapture --test-threads=1
/// ```
#[tokio::test]
#[ignore = "16 MiB 夹具与分配测量；显式运行，见用例文档"]
async fn own_page_candidate_lengths_avoid_materializing_oversize_rows() {
    const BUDGET: usize = 512 * 1024;
    let large = PersistedPayload::Message(BaseMessage::human("D".repeat(LARGE_BYTES)));
    let small = PersistedPayload::Message(BaseMessage::human("small own body"));
    let expected_large = serialize_persisted_payload(&large).unwrap();
    let expected_small = serialize_persisted_payload(&small).unwrap();
    let (_store, data, _directory, id) = session_with_own(&[small.clone(), large.clone()]).await;
    let tail = data
        .history_window_bounds(&id)
        .await
        .unwrap()
        .own_latest
        .unwrap();

    let plan = HistoryWindowPagePlan {
        intent: HistoryWindowReadIntent::Seek,
        source: HistorySource::Own,
        anchor: Some(large.id()),
        direction: HistoryWindowDirection::Before,
        include_anchor: true,
        before_limit: 1,
        after_limit: 0,
        byte_budget: BUDGET,
        tail: HistoryWindowTailExpectation::Fixed(Some(tail.clone())),
    };
    let before = sqlite_peak_reset();
    let rows = data.load_history_window_page(&id, &plan).await.unwrap();
    let incremental_extra = sqlite_peak_extra(before);
    let oversized: Vec<&HistoryWindowEntry> = rows
        .entries
        .iter()
        .filter(|entry| matches!(entry, HistoryWindowEntry::Oversized { .. }))
        .collect();
    assert_eq!(oversized.len(), 1, "16 MiB row must be a descriptor");
    let HistoryWindowEntry::Oversized { total_bytes, .. } = oversized[0] else {
        unreachable!()
    };
    assert_eq!(*total_bytes as usize, expected_large.len());

    // 小行仍能按同一套 blob 长度正常取出正文。
    let small_plan = HistoryWindowPagePlan {
        intent: HistoryWindowReadIntent::Seek,
        source: HistorySource::Own,
        anchor: Some(small.id()),
        direction: HistoryWindowDirection::Before,
        include_anchor: true,
        before_limit: 1,
        after_limit: 0,
        byte_budget: BUDGET,
        tail: HistoryWindowTailExpectation::Fixed(Some(tail.clone())),
    };
    let rows = data
        .load_history_window_page(&id, &small_plan)
        .await
        .unwrap();
    let loaded: Vec<&HistoryWindowEntry> = rows
        .entries
        .iter()
        .filter(|entry| matches!(entry, HistoryWindowEntry::Loaded(_)))
        .collect();
    assert_eq!(loaded.len(), 1);
    let HistoryWindowEntry::Loaded(item) = loaded[0] else {
        unreachable!()
    };
    assert_eq!(
        serialize_persisted_payload(&item.payload).unwrap(),
        expected_small
    );

    // 对照：旧阶段一语句对同一批候选物化 16 MiB 正文。
    let before = sqlite_peak_reset();
    let legacy: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT message_id, rowid, length(CAST(content AS BLOB))
         FROM messages
         WHERE thread_id = ?1 AND rowid <= ?2
           AND (?3 IS NULL OR rowid < ?3 OR (?4 <> 0 AND rowid = ?3))
         ORDER BY rowid DESC LIMIT ?5",
    )
    .bind(&id)
    .bind(i64::try_from(tail.position).unwrap())
    .bind(i64::try_from(tail.position).unwrap())
    .bind(1_i64)
    .bind(2_i64)
    .fetch_all(&data.database.pool)
    .await
    .unwrap();
    let legacy_extra = sqlite_peak_extra(before);
    assert!(!legacy.is_empty());

    println!(
        "own_page_candidates incremental_sqlite_extra={} MiB legacy_sqlite_extra={} MiB",
        incremental_extra as f64 / (1024.0 * 1024.0),
        legacy_extra as f64 / (1024.0 * 1024.0),
    );
    assert!(
        incremental_extra < 8 * 1024 * 1024,
        "own page candidates must not materialize oversize rows, got {incremental_extra} bytes"
    );
    assert!(
        legacy_extra > 16 * 1024 * 1024,
        "legacy candidate SQL must materialize the body, got {legacy_extra} bytes"
    );
}

/// inherited 分页：canonical 布局路径只读 blob 窗口，候选长度是 JSON 解码后的逻辑长度；
/// 同一夹具上的旧 `json_each + length(CAST(je.value AS BLOB))` 必须物化整条正文。
///
/// ```text
/// ./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- \
///     inherited_page_candidates_use_layout --ignored --nocapture --test-threads=1
/// ```
#[tokio::test]
#[ignore = "16 MiB 夹具与分配测量；显式运行，见用例文档"]
async fn inherited_page_candidates_use_layout_and_stay_bounded() {
    const BUDGET: usize = 512 * 1024;
    let large = PersistedPayload::Message(BaseMessage::human("E".repeat(LARGE_BYTES)));
    let tail = PersistedPayload::Message(BaseMessage::human("inherited tail"));
    let expected_large = serialize_persisted_payload(&large).unwrap();
    let (_store, data, _directory, id) =
        session_with_inherited(&[large.clone(), tail], "parent").await;
    let snapshot = data
        .history_window_bounds(&id)
        .await
        .unwrap()
        .inherited
        .snapshot()
        .unwrap()
        .clone();
    let source = inherited_source(&snapshot);

    let plan = HistoryWindowPagePlan {
        intent: HistoryWindowReadIntent::Seek,
        source,
        anchor: Some(large.id()),
        direction: HistoryWindowDirection::Before,
        include_anchor: true,
        before_limit: 1,
        after_limit: 0,
        byte_budget: BUDGET,
        tail: HistoryWindowTailExpectation::Current,
    };
    let before = sqlite_peak_reset();
    let rows = data.load_history_window_page(&id, &plan).await.unwrap();
    let incremental_extra = sqlite_peak_extra(before);
    let oversized: Vec<&InheritedHistoryEntry> = rows
        .inherited_entries
        .iter()
        .filter(|entry| matches!(entry, InheritedHistoryEntry::Oversized { .. }))
        .collect();
    assert_eq!(
        oversized.len(),
        1,
        "16 MiB inherited row must be a descriptor"
    );
    let InheritedHistoryEntry::Oversized { total_bytes, .. } = oversized[0] else {
        unreachable!()
    };
    assert_eq!(
        *total_bytes as usize,
        expected_large.len(),
        "descriptor length must be the decoded logical length, not the escaped span"
    );

    // 对照：旧候选语句把整条 payload 物化后再取长度。
    let before = sqlite_peak_reset();
    let legacy: Vec<(i64, Option<String>, i64)> = sqlx::query_as(
        "SELECT je.key,
                COALESCE(json_extract(je.value, '$.message.id'), json_extract(je.value, '$.id')),
                length(CAST(je.value AS BLOB))
         FROM threads t, json_each(t.inherited_context, '$.payloads') je
         WHERE t.id = ?1
           AND (?2 IS NULL OR je.key < ?2 OR (?3 <> 0 AND je.key = ?2))
         ORDER BY je.key DESC LIMIT ?4",
    )
    .bind(&id)
    .bind(0_i64)
    .bind(1_i64)
    .bind(2_i64)
    .fetch_all(&data.database.pool)
    .await
    .unwrap();
    let legacy_extra = sqlite_peak_extra(before);
    assert_eq!(legacy.len(), 1);
    assert_eq!(legacy[0].2 as usize, expected_large.len());

    println!(
        "inherited_page_candidates incremental_sqlite_extra={} MiB legacy_sqlite_extra={} MiB",
        incremental_extra as f64 / (1024.0 * 1024.0),
        legacy_extra as f64 / (1024.0 * 1024.0),
    );
    assert!(
        incremental_extra < 8 * 1024 * 1024,
        "inherited layout candidates must stay bounded, got {incremental_extra} bytes"
    );
    assert!(
        legacy_extra > 32 * 1024 * 1024,
        "legacy json_each candidate SQL must materialize the payload, got {legacy_extra} bytes"
    );
}

/// bounds 语句是窗口打开与 Full compact 的公共入口，不得物化冻结前缀。
///
/// 旧语句里的 `t.inherited_context IS NULL OR t.inherited_context = ''` 会让 SQLite 为
/// 16 MiB 列额外分配约 16 MiB（即使结果列根本未被使用）。这里把 `history_window_bounds`
/// 整体放进测量区间：边界 SQL 与增量 inherited 事实读取都必须与正文大小无关。
///
/// ```text
/// ./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- \
///     bounds_query_does_not_materialize_inherited_context --ignored --nocapture --test-threads=1
/// ```
#[tokio::test]
#[ignore = "16 MiB 夹具与分配测量；显式运行，见用例文档"]
async fn bounds_query_does_not_materialize_inherited_context() {
    let large = PersistedPayload::Message(BaseMessage::human("F".repeat(LARGE_BYTES)));
    let (_store, data, _directory, id) = session_with_inherited(&[large], "parent").await;

    let before = sqlite_peak_reset();
    let bounds = data.history_window_bounds(&id).await.unwrap();
    let incremental_extra = sqlite_peak_extra(before);

    assert!(
        bounds.own_latest.is_none(),
        "fixture stores the payload only in the inherited context"
    );
    assert!(
        bounds.inherited.snapshot().is_some(),
        "incremental facts path must still report the frozen prefix"
    );
    println!(
        "bounds incremental_sqlite_extra={} MiB",
        incremental_extra as f64 / (1024.0 * 1024.0),
    );
    assert!(
        incremental_extra < 8 * 1024 * 1024,
        "bounds query must not materialize inherited_context, got {incremental_extra} bytes"
    );
}

/// 静态守卫：bounds SQL 不得再引用 `inherited_context` 列。
#[test]
fn bounds_sql_does_not_reference_inherited_context() {
    assert!(
        !super::history::HISTORY_WINDOW_BOUNDS_SQL.contains("inherited_context"),
        "bounds SQL must not touch the inherited_context column: {}",
        super::history::HISTORY_WINDOW_BOUNDS_SQL
    );
}

/// inherited 布局分页行为：锚点、方向、include_anchor 与游标语义。
#[tokio::test]
async fn inherited_page_layout_matches_anchor_and_direction() {
    let payloads = [
        PersistedPayload::Message(BaseMessage::human("a")),
        PersistedPayload::Message(BaseMessage::human("b")),
        PersistedPayload::Message(BaseMessage::human("c")),
        PersistedPayload::Message(BaseMessage::human("d")),
    ];
    let (_store, data, _directory, id) = session_with_inherited(&payloads, "parent").await;
    let snapshot = data
        .history_window_bounds(&id)
        .await
        .unwrap()
        .inherited
        .snapshot()
        .unwrap()
        .clone();
    let plan = HistoryWindowPagePlan {
        intent: HistoryWindowReadIntent::Seek,
        source: inherited_source(&snapshot),
        anchor: Some(payloads[1].id()),
        direction: HistoryWindowDirection::Around,
        include_anchor: true,
        before_limit: 2,
        after_limit: 1,
        byte_budget: 512 * 1024,
        tail: HistoryWindowTailExpectation::Current,
    };
    let rows = data.load_history_window_page(&id, &plan).await.unwrap();
    let ids: Vec<MessageId> = rows
        .inherited_entries
        .iter()
        .map(|entry| match entry {
            InheritedHistoryEntry::Loaded(item) => item.id,
            InheritedHistoryEntry::Oversized { entry_id, .. } => *entry_id,
        })
        .collect();
    assert_eq!(
        ids,
        vec![payloads[0].id(), payloads[1].id(), payloads[2].id()],
        "before 必须按历史正序结束于锚点，after 从锚点之后继续"
    );
    assert!(rows.before_cursor_anchor.is_none());
    assert_eq!(rows.after_cursor_anchor, Some(payloads[2].id()));
    assert_eq!(
        rows.inherited_snapshot_id.as_deref(),
        Some(snapshot.snapshot_id.as_str())
    );

    // 锚点缺失失败关闭（seek 语义），不回退到首尾。
    let missing = HistoryWindowPagePlan {
        intent: HistoryWindowReadIntent::Seek,
        source: inherited_source(&snapshot),
        anchor: Some(MessageId::new()),
        direction: HistoryWindowDirection::Around,
        include_anchor: true,
        before_limit: 1,
        after_limit: 1,
        byte_budget: 512 * 1024,
        tail: HistoryWindowTailExpectation::Current,
    };
    let error = data
        .load_history_window_page(&id, &missing)
        .await
        .unwrap_err();
    assert_eq!(
        error.history_window_error().map(HistoryWindowError::code),
        Some(-32016)
    );
}
