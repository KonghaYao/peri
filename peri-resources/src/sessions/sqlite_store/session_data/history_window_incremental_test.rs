//! inherited 增量读取与 checkpoint 快路径的回归 + 测量。
//!
//! 默认测试只覆盖行为；大夹具与分配测量标记 `#[ignore]`，命令见各用例文档。
//! 分配口径：`sqlite3_memory_highwater` 是 SQLite 自己的分配器计数（与审查报告同口径），
//! 不含 Rust `String`/`Vec` 的分配；进程峰值未接入 allocator 账单，报告里标 n/a。

use super::history::{HISTORY_WINDOW_CHECKPOINT_FAST_SQL, HISTORY_WINDOW_CHECKPOINT_SQL};
use super::*;
use crate::sessions::data::SessionDataPort;
use crate::sessions::sqlite_store::SqliteThreadStore;
use peri_acp_types::history_window::{HistoryReadTicket, HistorySource, HistoryWindowError};
use peri_acp_types::messages::{BaseMessage, MessageId};
use peri_acp_types::projection::MessageProjectionDirective;
use peri_acp_types::session_resources::{
    FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResourceErrorKind,
};
use peri_acp_types::store::{
    serialize_persisted_payload, CompactionChange, InheritedContext, MessageFlags,
    PersistedPayload, ThreadStore,
};
use peri_acp_types::workspace::{ResolvedWorkspace, SessionBinding};
use sqlx::{Row, SqlitePool};
use std::collections::HashMap;
use std::time::Instant;
use tempfile::TempDir;

const LARGE_SESSION: &str = "incremental-large";
const LARGE_BYTES: usize = 16 * 1024 * 1024;
const CHUNK_BYTES: u32 = 64 * 1024;

extern "C" {
    fn sqlite3_memory_highwater(reset: i32) -> i64;
    fn sqlite3_memory_used() -> i64;
}

fn sqlite_peak_reset() -> i64 {
    // SAFETY: SQLite 的全局分配计数；测试串行执行（--test-threads=1）。
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
        created_at: "2026-10-17T00:00:00Z".to_owned(),
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
    set_inherited_context(&store, &id, payloads, ancestor).await;
    (store, data, directory, id)
}

async fn set_inherited_context(
    store: &SqliteThreadStore,
    id: &str,
    payloads: &[PersistedPayload],
    ancestor: &str,
) {
    let context = InheritedContext {
        payloads: payloads.to_vec(),
        flags: HashMap::new(),
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
    source: &peri_acp_types::history_window::HistoryInheritedSnapshot,
) -> HistorySource {
    HistorySource::inherited_with_ancestor(
        source.snapshot_id.clone(),
        source.ancestor_thread_id.clone(),
    )
}

/// 16 MiB 继承正文、64 KiB 分块：字节级重组必须与序列化原文一致，且 SQLite 分配不随正文增长。
///
/// ```text
/// ./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib -- \
///     incremental_large_body --ignored --nocapture --test-threads=1
/// ```
#[tokio::test]
#[ignore = "16 MiB 夹具与分配测量；显式运行，见用例文档"]
async fn incremental_large_body_chunks_stay_bounded_and_exact() {
    let body = "A".repeat(LARGE_BYTES);
    let large = PersistedPayload::Message(BaseMessage::human(body));
    let tail = PersistedPayload::Message(BaseMessage::human("tail"));
    let expected = serialize_persisted_payload(&large).unwrap();
    assert!(expected.len() > LARGE_BYTES);

    let (_store, data, _directory, id) =
        session_with_inherited(&[large.clone(), tail], "parent").await;
    let bounds = data.history_window_bounds(&id).await.unwrap();
    let snapshot = bounds
        .inherited
        .snapshot()
        .expect("inherited prefix is present")
        .clone();
    assert_eq!(snapshot.entry_count, 2);
    assert_eq!(snapshot.first_entry, Some(large.id()));
    let source = inherited_source(&snapshot);
    let ticket = HistoryReadTicket::new("ticket");

    // 冷路径：布局扫描 + 检查点构建都走 16 KiB blob 窗口。
    let first = data
        .load_history_body_chunk(
            &id,
            source.clone(),
            large.id(),
            0,
            CHUNK_BYTES,
            None,
            &ticket,
        )
        .await
        .unwrap();
    assert_eq!(first.bytes, expected.as_bytes()[..CHUNK_BYTES as usize]);

    // 热路径：深偏移只回退一个检查点区间重新解码。
    let deep_offset = 15 * 1024 * 1024;
    let deep = data
        .load_history_body_chunk(
            &id,
            source.clone(),
            large.id(),
            deep_offset as u64,
            CHUNK_BYTES,
            None,
            &ticket,
        )
        .await
        .unwrap();
    assert_eq!(
        deep.bytes,
        expected.as_bytes()[deep_offset..deep_offset + CHUNK_BYTES as usize]
    );

    // 分配测量 1：生产增量路径（热缓存）。
    let before = sqlite_peak_reset();
    let measured = data
        .load_history_body_chunk(
            &id,
            source.clone(),
            large.id(),
            8 * 1024 * 1024,
            CHUNK_BYTES,
            None,
            &ticket,
        )
        .await
        .unwrap();
    let incremental_extra = sqlite_peak_extra(before);
    assert_eq!(measured.bytes.len(), CHUNK_BYTES as usize);

    // 分配测量 2：原 SQL 路径（对照实现，证明它物化整条 16 MiB 元素）。
    let before = sqlite_peak_reset();
    let row: (i64, Vec<u8>) = sqlx::query_as(
        "SELECT length(CAST(je.value AS BLOB)),
                substr(CAST(je.value AS BLOB), 1, ?2)
         FROM threads t, json_each(t.inherited_context, '$.payloads') je
         WHERE t.id = ?1 AND je.key = 0",
    )
    .bind(&id)
    .bind(i64::from(CHUNK_BYTES))
    .fetch_one(&data.database.pool)
    .await
    .unwrap();
    let legacy_extra = sqlite_peak_extra(before);
    assert_eq!(row.0 as usize, expected.len());
    assert_eq!(row.1, expected.as_bytes()[..CHUNK_BYTES as usize]);

    println!(
        "inherited_chunk bytes={} chunk={} incremental_sqlite_extra={} MiB \
         legacy_sqlite_extra={} MiB process_peak=n/a",
        expected.len(),
        CHUNK_BYTES,
        incremental_extra as f64 / (1024.0 * 1024.0),
        legacy_extra as f64 / (1024.0 * 1024.0),
    );
    assert!(
        incremental_extra < 8 * 1024 * 1024,
        "incremental chunk read must stay bounded, got {incremental_extra} bytes"
    );
    assert!(
        legacy_extra > 16 * 1024 * 1024,
        "legacy SQL path must materialize the payload, got {legacy_extra} bytes"
    );

    // 完整重组：32 位字节块不得丢字符。
    let mut offset = 0u64;
    let mut collected = Vec::with_capacity(expected.len());
    loop {
        let chunk = data
            .load_history_body_chunk(
                &id,
                source.clone(),
                large.id(),
                offset,
                CHUNK_BYTES,
                None,
                &ticket,
            )
            .await
            .unwrap();
        chunk
            .validate(
                &peri_acp_types::history_window::HistoryEntryId::new(&id, large.id()).encode(),
                offset,
                CHUNK_BYTES,
            )
            .unwrap();
        collected.extend_from_slice(&chunk.bytes);
        match chunk.next_offset {
            Some(next) => offset = next,
            None => break,
        }
    }
    let expected = expected.into_bytes();
    assert_eq!(collected.len(), expected.len(), "reassembled length");
    if let Some(index) = collected
        .iter()
        .zip(&expected)
        .position(|(actual, wanted)| actual != wanted)
    {
        let from = index.saturating_sub(16);
        let to = (index + 16).min(expected.len());
        panic!(
            "reassembly mismatch at byte {index}: collected={:?} expected={:?}",
            &collected[from..to.min(collected.len())],
            &expected[from..to],
        );
    }
}

/// 转义与多字节字符在任意块边界上都必须按字节重组，不按字符裁断。
#[tokio::test]
async fn inherited_chunks_reassemble_escapes_and_multibyte_bodies() {
    let content = "quote:\" slash:\\ newline:\n tab:\t ctrl:\u{1} 中文 emoji:😀 tail";
    let payload = PersistedPayload::Message(BaseMessage::human(content));
    let expected = serialize_persisted_payload(&payload).unwrap();
    let (_store, data, _directory, id) =
        session_with_inherited(std::slice::from_ref(&payload), "parent").await;
    let bounds = data.history_window_bounds(&id).await.unwrap();
    let snapshot = bounds.inherited.snapshot().unwrap().clone();
    assert_eq!(snapshot.first_entry, Some(payload.id()));
    assert_eq!(snapshot.last_entry, Some(payload.id()));
    let source = inherited_source(&snapshot);
    let ticket = HistoryReadTicket::new("ticket");

    for max_bytes in [1u32, 3, 7, 64] {
        let mut offset = 0u64;
        let mut collected = Vec::new();
        loop {
            let chunk = data
                .load_history_body_chunk(
                    &id,
                    source.clone(),
                    payload.id(),
                    offset,
                    max_bytes,
                    None,
                    &ticket,
                )
                .await
                .unwrap();
            assert!(chunk.bytes.len() <= max_bytes as usize);
            collected.extend_from_slice(&chunk.bytes);
            match chunk.next_offset {
                Some(next) => {
                    assert!(next > offset);
                    offset = next;
                }
                None => {
                    assert!(chunk.complete);
                    break;
                }
            }
        }
        assert_eq!(
            collected,
            expected.clone().into_bytes(),
            "max_bytes={max_bytes}"
        );
    }

    let beyond = data
        .load_history_body_chunk(
            &id,
            source.clone(),
            payload.id(),
            expected.len() as u64 + 1,
            CHUNK_BYTES,
            None,
            &ticket,
        )
        .await
        .unwrap_err();
    assert_eq!(
        beyond.history_window_error().map(HistoryWindowError::code),
        Some(-32602)
    );

    let foreign = HistorySource::inherited_with_ancestor("inherited:foreign", None);
    let mismatch = data
        .load_history_body_chunk(&id, foreign, payload.id(), 0, CHUNK_BYTES, None, &ticket)
        .await
        .unwrap_err();
    assert_eq!(
        mismatch
            .history_window_error()
            .map(HistoryWindowError::code),
        Some(-32015)
    );
}

/// 非 canonical 信封（字段顺序不同）回退 SQL 路径，语义与增量路径一致。
#[tokio::test]
async fn non_canonical_inherited_envelope_falls_back_to_sql() {
    let payload = PersistedPayload::Message(BaseMessage::human("fallback body"));
    let expected = serialize_persisted_payload(&payload).unwrap();
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let id = "fallback-session".to_owned();
    data.save_new_session(&session(
        &id,
        workspace.cwd.to_str().unwrap(),
        &workspace,
        frozen(&id),
    ))
    .await
    .unwrap();
    let encoded = serde_json::to_string(&expected).unwrap();
    // payloads 先于 version：canonical 扫描器拒绝，走 `HISTORY_WINDOW_INHERITED_*` SQL。
    let envelope = format!(r#"{{"payloads":[{encoded}],"version":1,"flags":{{}}}}"#);
    sqlx::query("UPDATE threads SET inherited_context = ?1, parent_thread_id = ?2 WHERE id = ?3")
        .bind(envelope)
        .bind("parent")
        .bind(&id)
        .execute(&store.database.pool)
        .await
        .unwrap();

    let bounds = data.history_window_bounds(&id).await.unwrap();
    let snapshot = bounds.inherited.snapshot().unwrap().clone();
    assert_eq!(snapshot.first_entry, Some(payload.id()));
    assert_eq!(snapshot.last_entry, Some(payload.id()));
    let source = inherited_source(&snapshot);
    let chunk = data
        .load_history_body_chunk(
            &id,
            source,
            payload.id(),
            0,
            CHUNK_BYTES,
            None,
            &HistoryReadTicket::new("ticket"),
        )
        .await
        .unwrap();
    assert_eq!(chunk.bytes, expected.into_bytes());
}

/// checkpoint 快路径与全归档推导在 legacy/悬挂引用下必须给出同一答案。
#[tokio::test]
async fn checkpoint_fast_path_falls_back_without_changing_the_answer() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let id = "checkpoint-fallback".to_owned();
    data.save_new_session(&session(
        &id,
        workspace.cwd.to_str().unwrap(),
        &workspace,
        frozen(&id),
    ))
    .await
    .unwrap();
    let history: Vec<PersistedPayload> = (0..3)
        .map(|index| PersistedPayload::Message(BaseMessage::human(format!("m{index}"))))
        .collect();
    data.append_history(&id, &history).await.unwrap();
    assert_fast_checkpoint_matches_full(&store.database.pool, &id).await;

    let summary_one = BaseMessage::ai("summary one");
    let summary_one_id = summary_one.id();
    apply_full_compaction(&data, &id, &[history[0].id(), history[1].id()], summary_one).await;
    assert_fast_checkpoint_matches_full(&store.database.pool, &id).await;
    let bounds = data.history_window_bounds(&id).await.unwrap();
    assert_eq!(
        bounds.checkpoint.as_ref().and_then(|c| c.summary_entry_id),
        Some(summary_one_id)
    );

    // 第二次 Full：旧摘要与剩余历史一起归档，指向新摘要。
    let summary_two = BaseMessage::ai("summary two");
    let summary_two_id = summary_two.id();
    apply_full_compaction(&data, &id, &[history[2].id(), summary_one_id], summary_two).await;
    assert_fast_checkpoint_matches_full(&store.database.pool, &id).await;
    let bounds = data.history_window_bounds(&id).await.unwrap();
    assert_eq!(
        bounds.checkpoint.as_ref().and_then(|c| c.summary_entry_id),
        Some(summary_two_id)
    );

    // 悬挂：删掉最新摘要后，全归档推导应回到仍然存在的 summary one。
    sqlx::query("DELETE FROM messages WHERE thread_id = ?1 AND message_id = ?2")
        .bind(&id)
        .bind(summary_two_id.as_uuid().to_string())
        .execute(&store.database.pool)
        .await
        .unwrap();
    assert_fast_checkpoint_matches_full(&store.database.pool, &id).await;
    let bounds = data.history_window_bounds(&id).await.unwrap();
    assert_eq!(
        bounds.checkpoint.as_ref().and_then(|c| c.summary_entry_id),
        Some(summary_one_id),
        "dangling latest pointer must fall back to the surviving checkpoint"
    );

    // legacy 投影：最新归档行没有 projection，快路径无行，回退后仍是 summary one。
    sqlx::query(
        "UPDATE messages SET projection = NULL
         WHERE thread_id = ?1 AND message_id = ?2",
    )
    .bind(&id)
    .bind(summary_one_id.as_uuid().to_string())
    .execute(&store.database.pool)
    .await
    .unwrap();
    assert_fast_checkpoint_matches_full(&store.database.pool, &id).await;
    let bounds = data.history_window_bounds(&id).await.unwrap();
    assert_eq!(
        bounds.checkpoint.as_ref().and_then(|c| c.summary_entry_id),
        Some(summary_one_id),
        "legacy projection must not hide the surviving checkpoint"
    );
}

async fn checkpoint_choice(pool: &SqlitePool, sql: &'static str, id: &str) -> Option<String> {
    let row: Option<(String, i64)> = sqlx::query_as(sql)
        .bind(id)
        .fetch_optional(pool)
        .await
        .unwrap();
    row.map(|(message_id, _)| message_id)
}

/// 快路径只要给出答案，就必须与全归档推导一致；没有答案时生产路径才回退全量。
async fn assert_fast_checkpoint_matches_full(pool: &SqlitePool, id: &str) {
    let full = checkpoint_choice(pool, HISTORY_WINDOW_CHECKPOINT_SQL, id).await;
    let fast = checkpoint_choice(pool, HISTORY_WINDOW_CHECKPOINT_FAST_SQL, id).await;
    if let Some(fast) = fast {
        assert_eq!(
            fast,
            full.expect("full derivation must contain the fast checkpoint"),
        );
    }
}

async fn apply_full_compaction(
    data: &SqliteSessionData,
    id: &str,
    targets: &[MessageId],
    summary: BaseMessage,
) {
    let summary_id = summary.id();
    let directive = MessageProjectionDirective {
        policy_version: 1,
        entries: Vec::new(),
        superseded_by: Some(summary_id),
    };
    data.apply_compaction(
        &id.to_owned(),
        &CompactionChange {
            flag_updates: targets
                .iter()
                .map(|target| {
                    (
                        *target,
                        MessageFlags {
                            excluded: true,
                            projection: Some(directive.clone()),
                            ..Default::default()
                        },
                    )
                })
                .collect(),
            appended_messages: vec![summary],
        },
    )
    .await
    .unwrap();
}

/// 快路径的计划必须走 `idx_messages_active` 且没有排序；全归档语句才需要临时 B-tree。
#[tokio::test]
async fn checkpoint_fast_query_uses_the_active_index_without_sort() {
    let (_store, data, _directory) = database().await;
    let fast = explain_plan(
        &data.database.pool,
        HISTORY_WINDOW_CHECKPOINT_FAST_SQL,
        "thread",
    )
    .await;
    assert!(
        fast.iter().any(|line| line.contains("idx_messages_active")),
        "fast plan must bound by the active index: {fast:?}"
    );
    assert!(
        fast.iter().all(|line| !line.contains("TEMP B-TREE")),
        "fast plan must not sort the archive: {fast:?}"
    );
    // 全归档语句是回退/修复路径：这里只要求它能执行，不把计划形状写成契约。
    let full = explain_plan(&data.database.pool, HISTORY_WINDOW_CHECKPOINT_SQL, "thread").await;
    assert!(
        !full.is_empty(),
        "full reconstruction plan must remain executable"
    );
}

async fn explain_plan(pool: &SqlitePool, sql: &str, thread: &str) -> Vec<String> {
    let mut query = sqlx::query(sqlx::AssertSqlSafe(format!("EXPLAIN QUERY PLAN {sql}")));
    query = query.bind(thread);
    let rows = query.fetch_all(pool).await.expect("EXPLAIN QUERY PLAN");
    rows.iter()
        .map(|row| {
            let id: i64 = row.try_get(0).unwrap_or(-1);
            let detail: String = row.try_get(3).unwrap_or_default();
            format!("#{id} {detail}")
        })
        .collect()
}

/// 1 万/10 万归档行下的五次中位数：快路径应保持常数，全归档推导线性增长。
///
/// ```text
/// PERI_PERF_ROWS=10000,100000 ./scripts/cargo-rmcp-patched.sh test --locked \
///     -p peri-resources --lib -- checkpoint_scale --ignored --nocapture --test-threads=1
/// ```
#[tokio::test]
#[ignore = "1 万/10 万行夹具与计时；显式运行，见用例文档"]
async fn checkpoint_scale_is_constant_on_the_fast_path() {
    let scales = std::env::var("PERI_PERF_ROWS")
        .ok()
        .map(|raw| {
            raw.split(',')
                .filter_map(|item| item.trim().parse::<usize>().ok())
                .collect::<Vec<_>>()
        })
        .filter(|rows| !rows.is_empty())
        .unwrap_or_else(|| vec![10_000, 100_000]);
    for rows in scales {
        let (store, data, directory) = database().await;
        let workspace = workspace(&store, directory.path()).await;
        let id = format!("checkpoint-scale-{rows}");
        data.save_new_session(&session(
            &id,
            workspace.cwd.to_str().unwrap(),
            &workspace,
            frozen(&id),
        ))
        .await
        .unwrap();
        let summary = insert_archive(&store.database.pool, &id, rows).await;

        let fast = median_ms(
            &store.database.pool,
            HISTORY_WINDOW_CHECKPOINT_FAST_SQL,
            &id,
            5,
        )
        .await;
        let full = median_ms(&store.database.pool, HISTORY_WINDOW_CHECKPOINT_SQL, &id, 5).await;
        let bounds = data.history_window_bounds(&id).await.unwrap();
        assert_eq!(
            bounds.checkpoint.as_ref().and_then(|c| c.summary_entry_id),
            Some(summary)
        );
        println!("rows={rows} fast_median_ms={fast:.3} full_median_ms={full:.3}");
        assert!(
            fast < full,
            "fast checkpoint lookup must not scan the archive (rows={rows}, fast={fast}, full={full})"
        );
    }
}

async fn insert_archive(pool: &SqlitePool, id: &str, rows: usize) -> MessageId {
    let summary = MessageId::new();
    let projection = serde_json::to_string(&MessageProjectionDirective {
        policy_version: 1,
        entries: Vec::new(),
        superseded_by: Some(summary),
    })
    .unwrap();
    let mut tx = pool.begin().await.unwrap();
    for _ in 0..rows {
        sqlx::query(
            "INSERT INTO messages (message_id, thread_id, role, content, excluded, projection)
             VALUES (?1, ?2, 'assistant', '{}', 1, ?3)",
        )
        .bind(uuid::Uuid::now_v7().to_string())
        .bind(id)
        .bind(&projection)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO messages (message_id, thread_id, role, content, excluded, projection)
         VALUES (?1, ?2, 'assistant', '{}', 0, NULL)",
    )
    .bind(summary.as_uuid().to_string())
    .bind(id)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    summary
}

async fn median_ms(pool: &SqlitePool, sql: &'static str, id: &str, runs: usize) -> f64 {
    let mut samples = Vec::with_capacity(runs);
    for _ in 0..runs {
        let started = Instant::now();
        let row: Option<(String, i64)> = sqlx::query_as(sql)
            .bind(id)
            .fetch_optional(pool)
            .await
            .unwrap();
        assert!(row.is_some(), "checkpoint query must find the summary row");
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(|left, right| left.total_cmp(right));
    samples[samples.len() / 2]
}

#[test]
fn decoder_reassembles_escapes_across_arbitrary_offsets() {
    let raw = br#"quote:\" slash:\\ newline:\n tab:\t unicode:\u4e2d emoji:\ud83d\ude00"#;
    let decoded = super::inherited_blob::decode_prefix(raw, usize::MAX).unwrap();
    assert_eq!(
        String::from_utf8(decoded).unwrap(),
        "quote:\" slash:\\ newline:\n tab:\t unicode:中 emoji:😀"
    );
}

#[test]
fn decoder_unit_never_splits_a_multibyte_escape() {
    let raw = br#"\ud83d\ude00x"#;
    let mut pos = 0usize;
    let unit = super::inherited_blob::next_unit(raw, &mut pos)
        .unwrap()
        .unwrap();
    assert_eq!(&unit.bytes[..unit.decoded_len], "😀".as_bytes());
    assert_eq!(pos, 12);
    let unit = super::inherited_blob::next_unit(raw, &mut pos)
        .unwrap()
        .unwrap();
    assert_eq!(&unit.bytes[..unit.decoded_len], b"x");
}

/// NULL/空列报告 absent；损坏 JSON 失败关闭。任何一步都不得 panic。
#[tokio::test]
async fn absent_and_corrupt_inherited_context_fail_closed() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let ticket = HistoryReadTicket::new("ticket");
    for (id, context) in [("inherited-null", None), ("inherited-empty", Some(""))] {
        let thread_id = id.to_owned();
        data.save_new_session(&session(
            &thread_id,
            workspace.cwd.to_str().unwrap(),
            &workspace,
            frozen(&thread_id),
        ))
        .await
        .unwrap();
        if let Some(context) = context {
            sqlx::query("UPDATE threads SET inherited_context = ?1 WHERE id = ?2")
                .bind(context)
                .bind(&thread_id)
                .execute(&store.database.pool)
                .await
                .unwrap();
        }
        let bounds = data.history_window_bounds(&thread_id).await.unwrap();
        assert!(bounds.inherited.snapshot().is_none(), "{id}");
        let error = data
            .load_history_body_chunk(
                &thread_id,
                HistorySource::inherited("inherited:absent"),
                MessageId::new(),
                0,
                CHUNK_BYTES,
                None,
                &ticket,
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.history_window_error().map(HistoryWindowError::code),
            Some(-32012),
            "{id}"
        );
    }

    let id = "inherited-corrupt".to_owned();
    data.save_new_session(&session(
        &id,
        workspace.cwd.to_str().unwrap(),
        &workspace,
        frozen(&id),
    ))
    .await
    .unwrap();
    for context in ["{not json", r#"{"version":1,"payloads":["#] {
        sqlx::query("UPDATE threads SET inherited_context = ?1 WHERE id = ?2")
            .bind(context)
            .bind(&id)
            .execute(&store.database.pool)
            .await
            .unwrap();
        assert!(
            data.history_window_bounds(&id).await.is_err(),
            "corrupt inherited context must fail closed: {context}"
        );
    }
}

/// canonical 信封的 payload 字符串里含非法转义（`\q`）：读取失败关闭，不得 panic。
///
/// 手工构造信封而不是走 `serde_json::json!`：后者的 Map 实现由 `preserve_order` 特性
/// 决定（workspace 里的 ACP 依赖会启用它），字段顺序会改变扫描路径。固定 canonical
/// 顺序才能让本用例在所有构建特性下都覆盖增量解码，而不是偶然回退到 SQL。
#[tokio::test]
async fn invalid_escape_in_canonical_payload_fails_closed() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let id = "inherited-bad-escape".to_owned();
    data.save_new_session(&session(
        &id,
        workspace.cwd.to_str().unwrap(),
        &workspace,
        frozen(&id),
    ))
    .await
    .unwrap();
    let raw_payload = format!(
        r#"{{"role":"user","id":"{}","content":"bad\q"}}"#,
        MessageId::new().as_uuid()
    );
    // 只转义双引号、保留单个 `\q`：外层 JSON 的转义因此非法，正是解码器要拒绝的形状。
    let encoded_payload = raw_payload.replace('"', "\\\"");
    let envelope = format!(r#"{{"version":1,"payloads":["{encoded_payload}"],"flags":{{}}}}"#);
    assert!(
        envelope.starts_with(r#"{"version":1,"payloads":["{\"role\":\"user\""#),
        "fixture must stay in canonical order: {envelope}"
    );
    sqlx::query("UPDATE threads SET inherited_context = ?1 WHERE id = ?2")
        .bind(envelope)
        .bind(&id)
        .execute(&store.database.pool)
        .await
        .unwrap();
    let error = data.history_window_bounds(&id).await.unwrap_err();
    match error.kind() {
        SessionResourceErrorKind::Corrupt { detail } => assert!(
            detail.contains("invalid escape"),
            "blob decoder must reject the invalid escape, got: {detail}"
        ),
        other => panic!("invalid payload escape must fail closed, got: {other:?}"),
    }
}

/// 非 canonical（payloads 在前）信封 + 不可反序列化的 payload：blob 扫描拒绝结构，
/// SQL 回退在解码首个 payload 时失败关闭。外层 JSON 合法，夹具与 `preserve_order` 无关。
#[tokio::test]
async fn non_canonical_envelope_with_undecodable_payload_fails_closed() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let id = "inherited-bad-payload".to_owned();
    data.save_new_session(&session(
        &id,
        workspace.cwd.to_str().unwrap(),
        &workspace,
        frozen(&id),
    ))
    .await
    .unwrap();
    let raw_payload = format!(
        r#"{{"role":"user","id":"{}","content":"bad\q"}}"#,
        MessageId::new().as_uuid()
    );
    // `serde_json` 会把正文里的 `\` 转义成 `\\`，信封本身是合法 JSON；payloads 在前
    // 让 canonical 扫描器拒绝结构，强制走 JSON1 SQL 回退。
    let encoded_payload = serde_json::to_string(&raw_payload).unwrap();
    let envelope = format!(r#"{{"payloads":[{encoded_payload}],"version":1,"flags":{{}}}}"#);
    assert!(
        envelope.starts_with(r#"{"payloads":["{\"role\":\"user\""#),
        "fixture must keep the non-canonical payloads-first order: {envelope}"
    );
    sqlx::query("UPDATE threads SET inherited_context = ?1 WHERE id = ?2")
        .bind(envelope)
        .bind(&id)
        .execute(&store.database.pool)
        .await
        .unwrap();
    let error = data.history_window_bounds(&id).await.unwrap_err();
    match error.kind() {
        SessionResourceErrorKind::Corrupt { detail } => assert!(
            detail.contains("not decodable"),
            "SQL fallback must reject the undecodable payload, got: {detail}"
        ),
        other => panic!("undecodable payload must fail closed, got: {other:?}"),
    }
}

/// JSONB 列（BLOB）不是 canonical 文本信封：blob 扫描拒绝后回退 JSON1 SQL，语义不变。
#[tokio::test]
async fn jsonb_inherited_context_falls_back_to_sql() {
    let payload = PersistedPayload::Message(BaseMessage::human("jsonb body"));
    let expected = serialize_persisted_payload(&payload).unwrap();
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let id = "inherited-jsonb".to_owned();
    data.save_new_session(&session(
        &id,
        workspace.cwd.to_str().unwrap(),
        &workspace,
        frozen(&id),
    ))
    .await
    .unwrap();
    let context = InheritedContext {
        payloads: vec![payload.clone()],
        flags: HashMap::new(),
    }
    .to_json()
    .unwrap();
    sqlx::query(
        "UPDATE threads SET inherited_context = jsonb(?1), parent_thread_id = ?2 WHERE id = ?3",
    )
    .bind(&context)
    .bind("parent")
    .bind(&id)
    .execute(&store.database.pool)
    .await
    .unwrap();
    let stored: String =
        sqlx::query_scalar("SELECT typeof(inherited_context) FROM threads WHERE id = ?1")
            .bind(&id)
            .fetch_one(&store.database.pool)
            .await
            .unwrap();
    assert_eq!(stored, "blob");

    let bounds = data.history_window_bounds(&id).await.unwrap();
    let snapshot = bounds.inherited.snapshot().unwrap().clone();
    assert_eq!(snapshot.first_entry, Some(payload.id()));
    assert_eq!(snapshot.last_entry, Some(payload.id()));
    let chunk = data
        .load_history_body_chunk(
            &id,
            inherited_source(&snapshot),
            payload.id(),
            0,
            CHUNK_BYTES,
            None,
            &HistoryReadTicket::new("ticket"),
        )
        .await
        .unwrap();
    assert_eq!(chunk.bytes, expected.into_bytes());
}
