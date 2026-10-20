//! inherited 派生索引的写时落库、命中、损坏回退与 legacy 回填回归。
//!
//! 索引是派生对象：这里的断言分两类——写路径必须与列文本同一事务落库；读取路径在索引
//! 缺失、损坏或与列总字节数不符时必须回退到全列扫描，结果与索引命中一致。

use super::inherited_blob::{InheritedFactsRead, InheritedSummaryRead};
use super::inherited_facts::PayloadSpan;
use super::inherited_index;
use super::*;
use crate::sessions::sqlite_store::SqliteThreadStore;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::store::{InheritedContext, PersistedPayload, ThreadStore};
use std::sync::Arc;
use tempfile::TempDir;

fn payloads(count: usize) -> Vec<PersistedPayload> {
    (0..count)
        .map(|index| PersistedPayload::Message(BaseMessage::human(format!("inherited-{index}"))))
        .collect()
}

fn context(payloads: &[PersistedPayload]) -> InheritedContext {
    InheritedContext {
        payloads: payloads.to_vec(),
        flags: std::collections::HashMap::new(),
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

/// 写路径：列与索引在同一事务里落库，索引事实与序列化区间逐条一致。
#[tokio::test]
async fn write_path_populates_the_index_with_serialization_spans() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store(&tmp).await;
    let payloads = payloads(3);
    let cwd = tmp.path().to_string_lossy().into_owned();
    let (child, parent) = child_thread(&store, &cwd).await;
    store
        .store_inherited_context(&child, &context(&payloads))
        .await
        .unwrap();

    let expected = context(&payloads).to_json_with_spans().unwrap();
    let database = Arc::clone(&store.database);
    let mut connection = database.pool.acquire().await.unwrap();
    let indexed = inherited_index::read(&mut connection, &child)
        .await
        .unwrap()
        .expect("写入路径必须建立派生索引");
    assert_eq!(indexed.payload_bytes, expected.json.len() as u64);
    assert_eq!(indexed.entry_count, payloads.len());
    assert_eq!(indexed.payloads.len(), payloads.len());
    for (actual, expected) in indexed.payloads.iter().zip(&expected.payloads) {
        assert_eq!(actual.id, expected.id);
        assert_eq!(actual.span.raw_start, expected.raw_start);
        assert_eq!(actual.span.raw_len, expected.raw_len);
        assert_eq!(actual.logical_len, Some(expected.logical_len));
    }
    assert!(
        indexed.validate(&child, Some(&parent), expected.json.len() as u64),
        "写出的索引必须通过同一份重算校验"
    );
    drop(connection);

    // 新句柄 = 空内存缓存：布局只能来自派生索引，逻辑长度不需要解码正文。
    let data = SqliteSessionData::new(Arc::clone(&database));
    let mut connection = data.database.pool.acquire().await.unwrap();
    let layout = match data
        .read_inherited_layout_cached(&mut connection, &child)
        .await
        .unwrap()
    {
        InheritedFactsRead::Facts {
            snapshot: Some(_),
            layout: Some(layout),
        } => layout,
        _ => panic!("索引命中必须给出完整布局"),
    };
    assert_eq!(layout.len(), payloads.len());
    for (index, payload) in layout.payloads.iter().enumerate() {
        assert_eq!(payload.id, payloads[index].id());
        assert_eq!(
            payload.indexed_logical_len,
            Some(expected.payloads[index].logical_len),
            "索引命中时逻辑长度来自写时事实"
        );
        assert!(
            payload.decoded.get().is_none(),
            "命中路径不应为正文建立解码视图"
        );
    }
}

/// legacy 行（绕过 store 的 raw SQL）第一次读布局时回填；回填没有逻辑长度，读取仍正确。
#[tokio::test]
async fn legacy_layout_is_scanned_once_then_backfilled_and_reused() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store(&tmp).await;
    let payloads = payloads(2);
    let cwd = tmp.path().to_string_lossy().into_owned();
    let (child, _parent) = child_thread(&store, &cwd).await;
    let raw = context(&payloads).to_json().unwrap();
    sqlx::query("UPDATE threads SET inherited_context = ?1 WHERE id = ?2")
        .bind(&raw)
        .bind(child.as_str())
        .execute(&store.database.pool)
        .await
        .unwrap();

    let data = SqliteSessionData::new(Arc::clone(&store.database));
    let mut connection = data.database.pool.acquire().await.unwrap();
    let (first_ids, first_spans) = match data
        .read_inherited_layout_cached(&mut connection, &child)
        .await
        .unwrap()
    {
        InheritedFactsRead::Facts {
            layout: Some(layout),
            ..
        } => (
            layout
                .payloads
                .iter()
                .map(|payload| payload.id)
                .collect::<Vec<_>>(),
            layout
                .payloads
                .iter()
                .map(|payload| payload.span)
                .collect::<Vec<_>>(),
        ),
        _ => panic!("canonical envelope must yield a layout"),
    };
    assert_eq!(
        first_ids,
        payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>()
    );

    // 回填写入的区间与序列化事实一致，逻辑长度留空（legacy 没有这一事实）。
    let serialization = context(&payloads).to_json_with_spans().unwrap();
    let indexed = inherited_index::read(&mut connection, &child)
        .await
        .unwrap()
        .expect("扫描之后必须回填索引");
    assert_eq!(indexed.entry_count, payloads.len());
    assert_eq!(
        first_spans,
        serialization
            .payloads
            .iter()
            .map(|payload| PayloadSpan {
                raw_start: payload.raw_start,
                raw_len: payload.raw_len,
            })
            .collect::<Vec<_>>(),
        "扫描区间与写时序列化同一口径"
    );
    assert!(indexed
        .payloads
        .iter()
        .all(|payload| payload.logical_len.is_none()));
    drop(connection);

    // 新句柄：索引命中，不再扫描；legacy 行没有逻辑长度，留给按需解码。
    let data = SqliteSessionData::new(Arc::clone(&store.database));
    let mut connection = data.database.pool.acquire().await.unwrap();
    let layout = match data
        .read_inherited_layout_cached(&mut connection, &child)
        .await
        .unwrap()
    {
        InheritedFactsRead::Facts {
            layout: Some(layout),
            ..
        } => layout,
        _ => panic!("回填之后必须命中索引"),
    };
    assert_eq!(
        layout
            .payloads
            .iter()
            .map(|payload| payload.id)
            .collect::<Vec<_>>(),
        payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>()
    );
    assert!(layout
        .payloads
        .iter()
        .all(|payload| payload.indexed_logical_len.is_none()));
}

/// 索引损坏或失配时 fail-open：读取回退扫描，事实与健康索引一致。
#[tokio::test]
async fn corrupt_index_rows_fall_back_to_scanning() {
    let payloads = payloads(3);
    let expected_ids = payloads
        .iter()
        .map(PersistedPayload::id)
        .collect::<Vec<_>>();
    let corruptions = [
        // 列总字节数失配。
        "UPDATE inherited_snapshot_index SET payload_bytes = payload_bytes + 1",
        // 区间越界。
        "UPDATE inherited_payload_index SET raw_len = raw_len + 4096",
        // 条数与 payload 行不符。
        "DELETE FROM inherited_payload_index WHERE ordinal = 0",
        // 持久 snapshot_id 与重算口径不符（payload 行仍指向旧 id）。
        "UPDATE inherited_snapshot_index SET snapshot_id = 'inherited:deadbeef'",
        // 身份不可解释。
        "UPDATE inherited_payload_index SET entry_id = 'not-a-uuid' WHERE ordinal = 0",
    ];
    for corruption in corruptions {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(&tmp).await;
        let cwd = tmp.path().to_string_lossy().into_owned();
        let (child, _parent) = child_thread(&store, &cwd).await;
        store
            .store_inherited_context(&child, &context(&payloads))
            .await
            .unwrap();
        sqlx::query(corruption)
            .execute(&store.database.pool)
            .await
            .unwrap();

        let data = SqliteSessionData::new(Arc::clone(&store.database));
        let mut connection = data.database.pool.acquire().await.unwrap();
        let layout = match data
            .read_inherited_layout_cached(&mut connection, &child)
            .await
            .unwrap()
        {
            InheritedFactsRead::Facts {
                layout: Some(layout),
                ..
            } => layout,
            _ => panic!("{corruption} 之后仍必须能读出布局"),
        };
        assert_eq!(
            layout
                .payloads
                .iter()
                .map(|payload| payload.id)
                .collect::<Vec<_>>(),
            expected_ids,
            "{corruption}"
        );
        assert!(
            layout
                .payloads
                .iter()
                .all(|payload| payload.indexed_logical_len.is_none()),
            "{corruption}：回退扫描没有写时逻辑长度"
        );

        let summary = match data
            .read_inherited_summary_cached(&mut connection, &child)
            .await
            .unwrap()
        {
            InheritedSummaryRead::Facts { snapshot } => snapshot,
            InheritedSummaryRead::Unsupported => panic!("{corruption}: unsupported inherited"),
        };
        let summary = summary.unwrap_or_else(|| panic!("{corruption}: summary must survive"));
        assert_eq!(summary.entry_count, payloads.len(), "{corruption}");
    }
}

/// 索引表整体缺失（旧库/手工删表）：读取回退扫描，回填失败不改变本次结果。
#[tokio::test]
async fn missing_index_tables_fall_back_and_do_not_break_reads() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store(&tmp).await;
    let payloads = payloads(2);
    let cwd = tmp.path().to_string_lossy().into_owned();
    let (child, _parent) = child_thread(&store, &cwd).await;
    store
        .store_inherited_context(&child, &context(&payloads))
        .await
        .unwrap();
    sqlx::raw_sql("DROP TABLE inherited_payload_index; DROP TABLE inherited_snapshot_index")
        .execute(&store.database.pool)
        .await
        .unwrap();

    let data = SqliteSessionData::new(Arc::clone(&store.database));
    let mut connection = data.database.pool.acquire().await.unwrap();
    let layout = match data
        .read_inherited_layout_cached(&mut connection, &child)
        .await
        .unwrap()
    {
        InheritedFactsRead::Facts {
            layout: Some(layout),
            ..
        } => layout,
        _ => panic!("缺表时必须回退扫描"),
    };
    assert_eq!(layout.len(), payloads.len());
    let summary = match data
        .read_inherited_summary_cached(&mut connection, &child)
        .await
        .unwrap()
    {
        InheritedSummaryRead::Facts { snapshot } => snapshot.expect("summary survives"),
        InheritedSummaryRead::Unsupported => panic!("inherited summary must stay supported"),
    };
    assert_eq!(summary.entry_count, payloads.len());
}

/// 只读句柄不回填：派生写入不能发生在只读 capabilities 上。
#[tokio::test]
async fn read_only_handle_scans_without_backfilling() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("threads.db");
    let store = SqliteThreadStore::new(&path).await.unwrap();
    let payloads = payloads(2);
    let cwd = tmp.path().to_string_lossy().into_owned();
    let (child, _parent) = child_thread(&store, &cwd).await;
    let raw = context(&payloads).to_json().unwrap();
    sqlx::query("UPDATE threads SET inherited_context = ?1 WHERE id = ?2")
        .bind(&raw)
        .bind(child.as_str())
        .execute(&store.database.pool)
        .await
        .unwrap();
    store.close().await;

    let store = SqliteThreadStore::open_existing_read_only(&path)
        .await
        .unwrap();
    assert!(store.database.is_read_only());
    let data = SqliteSessionData::new(Arc::clone(&store.database));
    let mut connection = data.database.pool.acquire().await.unwrap();
    let layout = match data
        .read_inherited_layout_cached(&mut connection, &child)
        .await
        .unwrap()
    {
        InheritedFactsRead::Facts {
            layout: Some(layout),
            ..
        } => layout,
        _ => panic!("只读句柄仍必须读出布局"),
    };
    assert_eq!(layout.len(), payloads.len());
    let written: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM inherited_snapshot_index")
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    assert_eq!(written, 0, "只读句柄不得回填派生索引");
}

/// 空继承区：列照写，索引不产生行；读取得到空布局而不是派生事实。
#[tokio::test]
async fn empty_context_writes_no_index_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store(&tmp).await;
    let cwd = tmp.path().to_string_lossy().into_owned();
    let (child, _parent) = child_thread(&store, &cwd).await;
    store
        .store_inherited_context(&child, &InheritedContext::default())
        .await
        .unwrap();

    let data = SqliteSessionData::new(Arc::clone(&store.database));
    let mut connection = data.database.pool.acquire().await.unwrap();
    assert!(inherited_index::read(&mut connection, &child)
        .await
        .unwrap()
        .is_none());
    match data
        .read_inherited_layout_cached(&mut connection, &child)
        .await
        .unwrap()
    {
        InheritedFactsRead::Facts {
            snapshot: None,
            layout: Some(layout),
        } => assert_eq!(layout.len(), 0),
        InheritedFactsRead::Unsupported | InheritedFactsRead::Facts { .. } => {
            panic!("empty inherited context must yield an empty layout")
        }
    }
}

/// 写路径事务性：索引写入失败时列也必须回滚，不能留下「列已发布、索引缺失」以外的
/// 半成品（此处的失败注入只针对索引表）。
#[tokio::test]
async fn index_write_failure_rolls_back_the_column() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store(&tmp).await;
    let payloads = payloads(1);
    let cwd = tmp.path().to_string_lossy().into_owned();
    let (child, _parent) = child_thread(&store, &cwd).await;
    sqlx::raw_sql(
        "CREATE TRIGGER fail_inherited_index BEFORE INSERT ON inherited_payload_index
         BEGIN SELECT RAISE(ABORT, 'index boom'); END;",
    )
    .execute(&store.database.pool)
    .await
    .unwrap();

    let error = store
        .store_inherited_context(&child, &context(&payloads))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("index boom"), "{error}");
    let stored: Option<String> =
        sqlx::query_scalar("SELECT inherited_context FROM threads WHERE id = ?1")
            .bind(child.as_str())
            .fetch_one(&store.database.pool)
            .await
            .unwrap();
    assert!(stored.is_none(), "索引失败必须连同列一起回滚");

    sqlx::raw_sql("DROP TRIGGER fail_inherited_index")
        .execute(&store.database.pool)
        .await
        .unwrap();
    store
        .store_inherited_context(&child, &context(&payloads))
        .await
        .unwrap();
}

/// write-once 与错误分类：已有继承区 / 线程缺失分别报错，且都不覆盖已有列。
#[tokio::test]
async fn write_once_and_missing_thread_are_reported_separately() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store(&tmp).await;
    let cwd = tmp.path().to_string_lossy().into_owned();
    let (child, _parent) = child_thread(&store, &cwd).await;
    store
        .store_inherited_context(&child, &InheritedContext::default())
        .await
        .unwrap();
    let before: String = sqlx::query_scalar("SELECT inherited_context FROM threads WHERE id = ?1")
        .bind(child.as_str())
        .fetch_one(&store.database.pool)
        .await
        .unwrap();
    let error = store
        .store_inherited_context(&child, &context(&payloads(1)))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("already exists"), "{error}");
    let after: String = sqlx::query_scalar("SELECT inherited_context FROM threads WHERE id = ?1")
        .bind(child.as_str())
        .fetch_one(&store.database.pool)
        .await
        .unwrap();
    assert_eq!(before, after, "已有继承区不得被覆盖");

    let missing = "missing-thread".to_owned();
    let error = store
        .store_inherited_context(&missing, &context(&payloads(1)))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("thread is missing"), "{error}");
}

/// 重复身份在写入口就被拒绝：索引的 `(snapshot_id, entry_id)` 唯一约束不应成为
/// 「半份事实」的发现点。
#[tokio::test]
async fn duplicate_payload_ids_are_rejected_before_writing() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store(&tmp).await;
    let payload = PersistedPayload::Message(BaseMessage::human("duplicate"));
    let cwd = tmp.path().to_string_lossy().into_owned();
    let (child, _parent) = child_thread(&store, &cwd).await;
    let error = store
        .store_inherited_context(&child, &context(&[payload.clone(), payload]))
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("invalid inherited context message references"),
        "{error}"
    );
    let stored: Option<String> =
        sqlx::query_scalar("SELECT inherited_context FROM threads WHERE id = ?1")
            .bind(child.as_str())
            .fetch_one(&store.database.pool)
            .await
            .unwrap();
    assert!(stored.is_none());
    let written: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM inherited_snapshot_index")
        .fetch_one(&store.database.pool)
        .await
        .unwrap();
    assert_eq!(written, 0);
}

/// [回归测试 R07] 内存缓存命中时不得再读派生索引。
///
/// 先把 layout 读进缓存，再把 payload 索引表换成坏形状：缓存命中路径若仍执行
/// `inherited_index::read`，`PAYLOADS_SQL` 会因缺列失败；正确实现应在读索引之前
/// 返回缓存，因此两条读取都必须成功。
#[tokio::test]
async fn hot_cache_hit_does_not_read_payload_index() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store(&tmp).await;
    let payloads = payloads(4);
    let cwd = tmp.path().to_string_lossy().into_owned();
    let (child, _parent) = child_thread(&store, &cwd).await;
    store
        .store_inherited_context(&child, &context(&payloads))
        .await
        .unwrap();

    let data = SqliteSessionData::new(Arc::clone(&store.database));
    let mut connection = data.database.pool.acquire().await.unwrap();
    let warm = match data
        .read_inherited_layout_cached(&mut connection, &child)
        .await
        .unwrap()
    {
        InheritedFactsRead::Facts {
            layout: Some(layout),
            ..
        } => layout,
        _ => panic!("首次读取必须建立布局缓存"),
    };

    // 只破坏 payload 索引：snapshot 行仍可读，旧的“先读索引”顺序会在 fetch_all 失败。
    sqlx::raw_sql(
        "DROP TABLE inherited_payload_index;
         CREATE TABLE inherited_payload_index (bogus INTEGER);",
    )
    .execute(&store.database.pool)
    .await
    .unwrap();

    let summary = match data
        .read_inherited_summary_cached(&mut connection, &child)
        .await
        .unwrap()
    {
        InheritedSummaryRead::Facts {
            snapshot: Some(snapshot),
        } => snapshot,
        _ => panic!("热缓存命中不得因索引表损坏而失败"),
    };
    assert_eq!(summary.entry_count, payloads.len());
    let layout = match data
        .read_inherited_layout_cached(&mut connection, &child)
        .await
        .unwrap()
    {
        InheritedFactsRead::Facts {
            layout: Some(layout),
            ..
        } => layout,
        _ => panic!("热缓存命中不得因索引表损坏而失败"),
    };
    assert_eq!(layout.len(), warm.len());
    assert_eq!(
        layout
            .payloads
            .iter()
            .map(|payload| payload.id)
            .collect::<Vec<_>>(),
        warm.payloads
            .iter()
            .map(|payload| payload.id)
            .collect::<Vec<_>>()
    );
}
