//! R2：inherited 派生索引缺失/不完整时的有界门禁。
//!
//! 大于 8 MiB 或超过 4096 条 payload 的快照必须依赖派生索引；索引不可用时读取显式
//! 返回 `index_not_ready`，不得回退成全列扫描、`NotFound` 或空页。小快照保留原有
//! 有界回退行为。

use super::inherited_facts::{fallback_scan_count, reset_fallback_scan_probe};
use super::inherited_read::{
    InheritedFactsRead, INHERITED_FALLBACK_MAX_BYTES, INHERITED_FALLBACK_MAX_PAYLOADS,
};
use super::*;
use crate::sessions::data::SessionDataPort;
use crate::sessions::sqlite_store::SqliteThreadStore;
use peri_acp_types::history_window::{HistorySource, HistoryWindowDirection};
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::session_resources::{
    HistoryWindowPagePlan, HistoryWindowReadIntent, HistoryWindowTailExpectation,
    SessionResourceErrorKind,
};
use peri_acp_types::store::{InheritedContext, PersistedPayload, ThreadStore};
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;

fn payloads(count: usize) -> Vec<PersistedPayload> {
    (0..count)
        .map(|index| PersistedPayload::Message(BaseMessage::human(format!("inherited-{index}"))))
        .collect()
}

fn large_payload() -> PersistedPayload {
    PersistedPayload::Message(BaseMessage::human(
        "x".repeat(INHERITED_FALLBACK_MAX_BYTES as usize + 1024 * 1024),
    ))
}

fn context(payloads: &[PersistedPayload]) -> InheritedContext {
    InheritedContext {
        payloads: payloads.to_vec(),
        flags: HashMap::new(),
    }
}

async fn store(tmp: &TempDir) -> SqliteThreadStore {
    SqliteThreadStore::new(tmp.path().join("threads.db"))
        .await
        .unwrap()
}

async fn child_thread(store: &SqliteThreadStore, cwd: &str) -> (ThreadId, String) {
    let parent = store
        .create_thread(ThreadMeta::new_at(cwd.to_owned(), peri_time::now_wall()))
        .await
        .unwrap();
    let mut meta = ThreadMeta::new_at(cwd.to_owned(), peri_time::now_wall());
    meta.parent_thread_id = Some(parent.clone());
    let child = store.create_thread(meta).await.unwrap();
    (child, parent.as_str().to_owned())
}

async fn delete_index(store: &SqliteThreadStore, thread: &ThreadId) {
    sqlx::query(
        "DELETE FROM inherited_payload_index WHERE snapshot_id IN \
         (SELECT snapshot_id FROM inherited_snapshot_index WHERE thread_id = ?1)",
    )
    .bind(thread.as_str())
    .execute(&store.database.pool)
    .await
    .unwrap();
    sqlx::query("DELETE FROM inherited_snapshot_index WHERE thread_id = ?1")
        .bind(thread.as_str())
        .execute(&store.database.pool)
        .await
        .unwrap();
}

fn assert_index_not_ready(error: SessionResourceError) {
    assert!(
        matches!(error.kind(), SessionResourceErrorKind::IndexNotReady { .. }),
        "expected index_not_ready, got {error:?}"
    );
}

fn inherited_plan(
    snapshot: &peri_acp_types::history_window::HistoryInheritedSnapshot,
) -> HistoryWindowPagePlan {
    HistoryWindowPagePlan {
        intent: HistoryWindowReadIntent::Page,
        source: HistorySource::Inherited {
            frozen_snapshot_id: snapshot.snapshot_id.clone(),
            ancestor_thread_id: snapshot.ancestor_thread_id.clone(),
        },
        anchor: None,
        direction: HistoryWindowDirection::Before,
        include_anchor: false,
        before_limit: 50,
        after_limit: 0,
        byte_budget: 64 * 1024,
        tail: HistoryWindowTailExpectation::Fixed(None),
    }
}

/// 门禁边界：8 MiB / 4096 条本身仍允许回退，超出才拒绝。
#[test]
fn fallback_bound_is_inclusive_at_the_documented_limits() {
    assert!(!super::inherited_read::fallback_exceeds_bound(
        INHERITED_FALLBACK_MAX_BYTES,
        Some(INHERITED_FALLBACK_MAX_PAYLOADS)
    ));
    assert!(super::inherited_read::fallback_exceeds_bound(
        INHERITED_FALLBACK_MAX_BYTES + 1,
        Some(1)
    ));
    assert!(super::inherited_read::fallback_exceeds_bound(
        1,
        Some(INHERITED_FALLBACK_MAX_PAYLOADS + 1)
    ));
    assert!(!super::inherited_read::fallback_exceeds_bound(1, None));
    assert!(super::inherited_read::fallback_exceeds_bound(
        INHERITED_FALLBACK_MAX_BYTES + 1,
        None
    ));
}

/// 大快照 + 索引缺失：open/bounds 与 page 都在回退扫描之前失败。
#[tokio::test]
async fn large_snapshot_without_index_fails_before_fallback_scan() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store(&tmp).await;
    let cwd = tmp.path().to_string_lossy().into_owned();
    let (child, _parent) = child_thread(&store, &cwd).await;
    store
        .store_inherited_context(&child, &context(&[large_payload()]))
        .await
        .unwrap();

    // 索引健康时先取得冻结快照身份（open 的正常路径）。
    let data = SqliteSessionData::new(Arc::clone(&store.database));
    let snapshot = data
        .read_history_window_bounds(&child)
        .await
        .unwrap()
        .inherited
        .snapshot()
        .cloned()
        .expect("inherited snapshot");
    delete_index(&store, &child).await;

    // 新句柄 = 空内存缓存；索引确实不可用。
    let data = SqliteSessionData::new(Arc::clone(&store.database));
    reset_fallback_scan_probe();

    assert_index_not_ready(data.read_history_window_bounds(&child).await.unwrap_err());
    assert_eq!(
        fallback_scan_count(),
        0,
        "bounds 不得在门禁之前扫描 inherited 列"
    );

    assert_index_not_ready(
        data.load_history_window_page(&child, &inherited_plan(&snapshot))
            .await
            .unwrap_err(),
    );
    assert_eq!(
        fallback_scan_count(),
        0,
        "page 不得在门禁之前扫描 inherited 列"
    );

    let mut connection = data.database.pool.acquire().await.unwrap();
    match data
        .read_inherited_layout_cached(&mut connection, &child)
        .await
    {
        Err(error) => assert_index_not_ready(error),
        Ok(_) => panic!("large snapshot without a usable index must not fall back"),
    }
    assert_eq!(
        fallback_scan_count(),
        0,
        "layout 读取不得在门禁之前扫描 inherited 列"
    );
}

/// 索引头存在但行数与 entry_count 不一致：按缺失处理；大快照直接拒绝。
#[tokio::test]
async fn inconsistent_index_rows_are_treated_as_missing_for_large_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store(&tmp).await;
    let cwd = tmp.path().to_string_lossy().into_owned();
    let (child, _parent) = child_thread(&store, &cwd).await;
    let body = "y".repeat(5 * 1024 * 1024);
    let entries = [
        PersistedPayload::Message(BaseMessage::human(body.clone())),
        PersistedPayload::Message(BaseMessage::ai(body)),
    ];
    store
        .store_inherited_context(&child, &context(&entries))
        .await
        .unwrap();
    sqlx::query("DELETE FROM inherited_payload_index WHERE ordinal = 0")
        .execute(&store.database.pool)
        .await
        .unwrap();

    let data = SqliteSessionData::new(Arc::clone(&store.database));
    reset_fallback_scan_probe();
    let mut connection = data.database.pool.acquire().await.unwrap();
    match data
        .read_inherited_layout_cached(&mut connection, &child)
        .await
    {
        Err(error) => assert_index_not_ready(error),
        Ok(_) => panic!("large snapshot without a usable index must not fall back"),
    }
    assert_eq!(
        fallback_scan_count(),
        0,
        "索引行数不符不得触发大快照全列扫描"
    );
}

/// 小快照缺索引：保持原有回退行为（有界扫描 + 回填）。
#[tokio::test]
async fn small_snapshot_without_index_keeps_bounded_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store(&tmp).await;
    let cwd = tmp.path().to_string_lossy().into_owned();
    let (child, _parent) = child_thread(&store, &cwd).await;
    let entries = payloads(3);
    store
        .store_inherited_context(&child, &context(&entries))
        .await
        .unwrap();
    delete_index(&store, &child).await;

    let data = SqliteSessionData::new(Arc::clone(&store.database));
    reset_fallback_scan_probe();
    let mut connection = data.database.pool.acquire().await.unwrap();
    let read = data
        .read_inherited_layout_cached(&mut connection, &child)
        .await
        .unwrap();
    match read {
        InheritedFactsRead::Facts {
            layout: Some(layout),
            ..
        } => assert_eq!(layout.len(), entries.len()),
        _ => panic!("small snapshot must keep the bounded fallback"),
    }
    assert_eq!(fallback_scan_count(), 1, "小快照只允许一次有界全列扫描");
}

/// 索引完整的大快照：保持快速路径，不触发回退扫描。
#[tokio::test]
async fn large_snapshot_with_complete_index_uses_fast_path() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store(&tmp).await;
    let cwd = tmp.path().to_string_lossy().into_owned();
    let (child, _parent) = child_thread(&store, &cwd).await;
    store
        .store_inherited_context(&child, &context(&[large_payload()]))
        .await
        .unwrap();

    let data = SqliteSessionData::new(Arc::clone(&store.database));
    reset_fallback_scan_probe();
    let mut connection = data.database.pool.acquire().await.unwrap();
    let read = data
        .read_inherited_layout_cached(&mut connection, &child)
        .await
        .unwrap();
    match read {
        InheritedFactsRead::Facts {
            snapshot: Some(snapshot),
            layout: Some(layout),
        } => {
            assert_eq!(snapshot.entry_count, 1);
            assert_eq!(layout.len(), 1);
            assert!(layout.payloads[0].indexed_logical_len.is_some());
        }
        _ => panic!("complete index must use the fast path"),
    }
    assert_eq!(fallback_scan_count(), 0, "索引完整时不得回退扫描");
}
