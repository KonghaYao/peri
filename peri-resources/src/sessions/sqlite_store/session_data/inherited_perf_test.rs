//! A42 性能证据：冷进程巨大 inherited、小/空 own、多会话与坏 legacy（默认 `#[ignore]`）。
//!
//! 运行命令与原始输出见 `.recon/perf-evidence.md`：
//!
//! ```text
//! ./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib \
//!     -- inherited_perf --ignored --nocapture
//! ```
//!
//! 环境变量 `PERI_PERF_ITERS`（默认 20）控制冷命中循环次数。
//! 口径：
//!
//! 1. 夹具正文由生产写入口 `store_inherited_context` 落库，派生索引与列文本同一事务；
//!    不经 SQL 直接造索引行。
//! 2. 冷 = 每次测量新建部署句柄（新连接池 ⇒ SQLite 页缓存为空）。同一进程无法清 OS
//!    页缓存，因此数字是**进程内冷**，不冒充整机冷启动。
//! 3. 命中路径不把正文读进内存：布局只保留区间/身份/逻辑长度，`decoded` 保持空；
//!    正文可读性用 `read_history_body_chunk` 按块实测，并与索引给出的逻辑长度核对。
//! 4. legacy = 删除派生索引行后首次读取：一次全列扫描 + 回填；随后新句柄直接命中。
//! 5. 坏索引 = `payload_bytes` 与列总字节数不符：小快照必须回退扫描，结果与命中一致；
//!    大快照（> 8 MiB）必须显式 `index_not_ready`，不得全列扫描。
//! 6. 内存只报告可核对的事实：索引行数、`dbstat` 页字节、布局结构体估算、缓存容量；
//!    不伪造 RSS。磁盘占用单列，不计入内存结论。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use peri_acp_types::history_common::{HistoryReadTicket, HistorySource};
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::store::{InheritedContext, PersistedPayload, ThreadStore};
use sqlx::{AssertSqlSafe, Row};
use tempfile::TempDir;

use super::inherited_blob::{InheritedFactsRead, InheritedSummaryRead};
use super::inherited_facts::{LayoutPayload, INHERITED_FACTS_CACHE_CAPACITY};
use super::inherited_index::{PAYLOADS_SQL, SNAPSHOT_SQL};
use super::*;
use crate::sessions::sqlite_store::SqliteThreadStore;

const LARGE_ENTRIES: usize = 64;
const LARGE_BODY_BYTES: usize = 256 * 1024;
const MEDIUM_ENTRIES: usize = 16;
const MEDIUM_BODY_BYTES: usize = 64 * 1024;
const SMALL_ENTRIES: usize = 4;
const SMALL_BODY_BYTES: usize = 8 * 1024;
const LEGACY_ENTRIES: usize = 16;
const LEGACY_BODY_BYTES: usize = 64 * 1024;

struct Fixture {
    _tmp: TempDir,
    path: PathBuf,
    parent: ThreadId,
    large: Session,
    medium: Session,
    small: Session,
    legacy: Session,
}

struct Session {
    thread: ThreadId,
    ids: Vec<MessageId>,
}

async fn child(
    store: &SqliteThreadStore,
    parent: &ThreadId,
    cwd: &str,
    entries: usize,
    body_bytes: usize,
) -> Session {
    let mut meta = ThreadMeta::new_at(cwd.to_owned(), peri_time::now_wall());
    meta.parent_thread_id = Some(parent.clone());
    let thread = store.create_thread(meta).await.unwrap();
    let payloads: Vec<PersistedPayload> = (0..entries)
        .map(|index| {
            let mut body = format!("{index:08}:");
            body.push_str(&"x".repeat(body_bytes.saturating_sub(body.len())));
            PersistedPayload::Message(BaseMessage::human(body))
        })
        .collect();
    let ids = payloads.iter().map(PersistedPayload::id).collect();
    let context = InheritedContext {
        payloads,
        flags: HashMap::new(),
    };
    store
        .store_inherited_context(&thread, &context)
        .await
        .unwrap();
    Session { thread, ids }
}

async fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("threads.db");
    let store = SqliteThreadStore::new(&path).await.unwrap();
    let cwd = tmp.path().to_string_lossy().into_owned();
    let parent = store
        .create_thread(ThreadMeta::new_at(cwd.clone(), peri_time::now_wall()))
        .await
        .unwrap();
    let large = child(&store, &parent, &cwd, LARGE_ENTRIES, LARGE_BODY_BYTES).await;
    let medium = child(&store, &parent, &cwd, MEDIUM_ENTRIES, MEDIUM_BODY_BYTES).await;
    let small = child(&store, &parent, &cwd, SMALL_ENTRIES, SMALL_BODY_BYTES).await;
    let legacy = child(&store, &parent, &cwd, LEGACY_ENTRIES, LEGACY_BODY_BYTES).await;
    // legacy：删掉派生索引行，模拟 F12b 之前的库；首次读取应扫描并回填。
    delete_index(&store, &legacy.thread).await;
    Fixture {
        _tmp: tmp,
        path,
        parent,
        large,
        medium,
        small,
        legacy,
    }
}

async fn cold(path: &Path) -> (SqliteThreadStore, SqliteSessionData) {
    let store = SqliteThreadStore::new(path.to_path_buf()).await.unwrap();
    let data = SqliteSessionData::new(Arc::clone(&store.database));
    (store, data)
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

async fn index_rows(store: &SqliteThreadStore, thread: &ThreadId) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM inherited_payload_index WHERE snapshot_id = \
         (SELECT snapshot_id FROM inherited_snapshot_index WHERE thread_id = ?1 \
          ORDER BY rowid DESC LIMIT 1)",
    )
    .bind(thread.as_str())
    .fetch_one(&store.database.pool)
    .await
    .unwrap()
}

async fn index_pages(store: &SqliteThreadStore) -> i64 {
    sqlx::query_scalar(
        "SELECT COALESCE(SUM(pgsize), 0) FROM dbstat \
         WHERE name IN ('inherited_snapshot_index', 'inherited_payload_index')",
    )
    .fetch_one(&store.database.pool)
    .await
    .unwrap_or(0)
}

async fn plans(store: &SqliteThreadStore, thread: &ThreadId) -> Vec<String> {
    let Some(snapshot_id): Option<String> = sqlx::query_scalar(
        "SELECT snapshot_id FROM inherited_snapshot_index WHERE thread_id = ?1 \
         ORDER BY rowid DESC LIMIT 1",
    )
    .bind(thread.as_str())
    .fetch_optional(&store.database.pool)
    .await
    .unwrap() else {
        return Vec::new();
    };
    let mut details = Vec::new();
    for (label, sql, value) in [
        ("snapshot", SNAPSHOT_SQL, thread.as_str()),
        ("payloads", PAYLOADS_SQL, snapshot_id.as_str()),
    ] {
        let query = format!("EXPLAIN QUERY PLAN {sql}");
        let rows = sqlx::query(AssertSqlSafe(query))
            .bind(value)
            .fetch_all(&store.database.pool)
            .await
            .unwrap();
        for row in rows {
            let detail: String = row.get("detail");
            details.push(format!("{label}: {detail}"));
        }
    }
    details
}

fn percentile(samples: &mut [Duration], fraction: f64) -> f64 {
    samples.sort();
    let index = (((samples.len() - 1) as f64) * fraction).round() as usize;
    samples[index].as_secs_f64() * 1000.0
}

fn report(label: &str, samples: &mut [Duration]) {
    if samples.is_empty() {
        println!("  {label}: 无可测样本");
        return;
    }
    println!(
        "  {label}: n={} min={:.3}ms p50={:.3}ms p95={:.3}ms max={:.3}ms",
        samples.len(),
        percentile(samples, 0.0),
        percentile(samples, 0.5),
        percentile(samples, 0.95),
        percentile(samples, 1.0)
    );
}

fn summary_entries(read: &InheritedSummaryRead) -> usize {
    match read {
        InheritedSummaryRead::Facts {
            snapshot: Some(snapshot),
        } => snapshot.entry_count,
        InheritedSummaryRead::Facts { snapshot: None } => {
            panic!("预期命中 inherited 摘要：未找到 inherited 前缀")
        }
        InheritedSummaryRead::Unsupported => {
            panic!("预期命中 inherited 摘要：不支持的列类型")
        }
    }
}

async fn read_body(
    data: &SqliteSessionData,
    thread: &ThreadId,
    source: &HistorySource,
    entry: MessageId,
) -> Vec<u8> {
    let ticket = HistoryReadTicket::new("a42-evidence");
    let mut bytes = Vec::new();
    let mut offset = 0u64;
    loop {
        let chunk = data
            .read_history_body_chunk(
                thread,
                source.clone(),
                entry,
                offset,
                peri_acp_types::history_common::HISTORY_WINDOW_MAX_CHUNK_SIZE,
                None,
                &ticket,
            )
            .await
            .unwrap();
        assert_eq!(chunk.offset, offset, "块起点必须与请求一致");
        bytes.extend_from_slice(&chunk.bytes);
        if chunk.complete {
            break;
        }
        let next = chunk.next_offset.expect("未完成的块必须给出下一偏移");
        assert!(next > offset, "块读取必须前进");
        offset = next;
    }
    bytes
}

#[ignore = "A42 性能证据（16 MiB inherited 夹具）；手动运行，见 .recon/perf-evidence.md"]
#[tokio::test]
async fn inherited_index_cold_paths_evidence() {
    let iters = std::env::var("PERI_PERF_ITERS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(20usize)
        .max(1);
    let fixture = fixture().await;
    let db_bytes = std::fs::metadata(&fixture.path)
        .map(|meta| meta.len())
        .unwrap_or(0);

    println!("== 环境 ==");
    println!(
        "  fixture: large={LARGE_ENTRIES}x{LARGE_BODY_BYTES}B medium={MEDIUM_ENTRIES}x\
         {MEDIUM_BODY_BYTES}B small={SMALL_ENTRIES}x{SMALL_BODY_BYTES}B legacy=\
         {LEGACY_ENTRIES}x{LEGACY_BODY_BYTES}B；所有 child 的 own 历史为空"
    );
    println!("  db file: {db_bytes} 字节（磁盘占用单列，不计入内存）");

    // 写路径索引形状与查询计划。
    let (warm_store, _warm_data) = cold(&fixture.path).await;
    for (label, session, expected) in [
        ("large", &fixture.large, LARGE_ENTRIES),
        ("medium", &fixture.medium, MEDIUM_ENTRIES),
        ("small", &fixture.small, SMALL_ENTRIES),
    ] {
        let rows = index_rows(&warm_store, &session.thread).await;
        println!("  索引行 {label}: {rows}（预期 {expected}）");
        assert_eq!(rows, expected as i64);
    }
    println!("  index pages: {} 字节", index_pages(&warm_store).await);
    for detail in plans(&warm_store, &fixture.large.thread).await {
        println!("  plan {detail}");
        assert!(
            !detail.contains("SCAN inherited_"),
            "索引读取不得退化为派生表全扫：{detail}"
        );
    }
    println!(
        "  内存上限: cache={INHERITED_FACTS_CACHE_CAPACITY} 线程条目；LayoutPayload={} 字节/条；\
         正文不驻留（decoded 按块构建）",
        std::mem::size_of::<LayoutPayload>()
    );

    // 冷命中：每次新句柄 + 新内存缓存。
    let mut open_samples = Vec::with_capacity(iters);
    let mut summary_samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let started = Instant::now();
        let (store, data) = cold(&fixture.path).await;
        open_samples.push(started.elapsed());
        let mut connection = data.database.pool.acquire().await.unwrap();
        let started = Instant::now();
        let read = data
            .read_inherited_summary_cached(&mut connection, &fixture.large.thread)
            .await
            .unwrap();
        summary_samples.push(started.elapsed());
        assert_eq!(summary_entries(&read), LARGE_ENTRIES);
        drop(connection);
        drop(store);
    }
    println!("  -- large 冷命中（{iters} 次新句柄） --");
    report("open", &mut open_samples);
    report("summary", &mut summary_samples);

    // 冷布局：只取区间/身份/逻辑长度，不建正文解码视图。
    let (layout_store, layout_data) = cold(&fixture.path).await;
    let mut connection = layout_data.database.pool.acquire().await.unwrap();
    let started = Instant::now();
    let read = layout_data
        .read_inherited_layout_cached(&mut connection, &fixture.large.thread)
        .await
        .unwrap();
    let layout_time = started.elapsed();
    drop(connection);
    let (layout, snapshot_id) = match read {
        InheritedFactsRead::Facts {
            snapshot: Some(snapshot),
            layout: Some(layout),
        } => (layout, snapshot.snapshot_id),
        _ => panic!("派生索引命中必须给出布局"),
    };
    assert_eq!(layout.len(), LARGE_ENTRIES);
    assert!(
        layout
            .payloads
            .iter()
            .all(|payload| payload.indexed_logical_len.is_some()),
        "新写入的派生索引必须带有逻辑长度"
    );
    assert!(
        layout
            .payloads
            .iter()
            .all(|payload| payload.decoded.get().is_none()),
        "布局命中不得为正文建立解码视图"
    );
    println!(
        "  large 冷布局: {:.3}ms（{} 条，全部带逻辑长度，解码视图 0）",
        layout_time.as_secs_f64() * 1000.0,
        layout.len()
    );

    // 完成后全历史可读：首尾条目按块读满，字节数与索引逻辑长度一致。
    let source = HistorySource::Inherited {
        frozen_snapshot_id: snapshot_id,
        ancestor_thread_id: Some(fixture.parent.clone()),
    };
    for (label, id) in [
        ("first", fixture.large.ids[0]),
        ("last", *fixture.large.ids.last().unwrap()),
    ] {
        let started = Instant::now();
        let body = read_body(&layout_data, &fixture.large.thread, &source, id).await;
        let elapsed = started.elapsed();
        let index = layout.position_of(id).expect("布局必须包含该条目");
        let logical = layout.payloads[index]
            .indexed_logical_len
            .expect("索引逻辑长度");
        assert_eq!(body.len() as u64, logical, "{label} 条目必须完整可读");
        println!(
            "  large {label} 正文: {} 字节，{:.3}ms",
            body.len(),
            elapsed.as_secs_f64() * 1000.0
        );
    }
    drop(layout_store);

    // 多会话冷摘要：同一新句柄上交错读取，互不串数据；缓存 LRU 容量见上。
    println!("  -- 多会话冷摘要（同一新句柄，交错读取） --");
    let (multi_store, multi_data) = cold(&fixture.path).await;
    for (label, session, expected, first) in [
        ("large", &fixture.large, LARGE_ENTRIES, fixture.large.ids[0]),
        (
            "medium",
            &fixture.medium,
            MEDIUM_ENTRIES,
            fixture.medium.ids[0],
        ),
        ("small", &fixture.small, SMALL_ENTRIES, fixture.small.ids[0]),
    ] {
        let mut connection = multi_data.database.pool.acquire().await.unwrap();
        let started = Instant::now();
        let read = multi_data
            .read_inherited_summary_cached(&mut connection, &session.thread)
            .await
            .unwrap();
        let elapsed = started.elapsed();
        match &read {
            InheritedSummaryRead::Facts {
                snapshot: Some(snapshot),
            } => {
                assert_eq!(snapshot.entry_count, expected);
                assert_eq!(snapshot.first_entry, Some(first));
            }
            _ => panic!("{label} 冷摘要必须命中"),
        }
        println!(
            "  {label}: {:.3}ms（{expected} 条，首条 {first:?}）",
            elapsed.as_secs_f64() * 1000.0
        );
        drop(connection);
    }
    drop(multi_store);
    // legacy：无索引行 → 一次全列扫描并回填；新句柄随后命中。
    println!("  -- legacy 回填 --");
    let (legacy_store, legacy_data) = cold(&fixture.path).await;
    let mut connection = legacy_data.database.pool.acquire().await.unwrap();
    let started = Instant::now();
    let read = legacy_data
        .read_inherited_layout_cached(&mut connection, &fixture.legacy.thread)
        .await
        .unwrap();
    let scan_time = started.elapsed();
    drop(connection);
    match read {
        InheritedFactsRead::Facts {
            snapshot: Some(snapshot),
            layout: Some(layout),
        } => {
            assert_eq!(snapshot.entry_count, LEGACY_ENTRIES);
            assert_eq!(layout.len(), LEGACY_ENTRIES);
            assert!(
                layout
                    .payloads
                    .iter()
                    .all(|payload| payload.indexed_logical_len.is_none()),
                "legacy 扫描不伪造索引逻辑长度"
            );
        }
        _ => panic!("legacy 布局必须可读"),
    }
    let backfilled = index_rows(&legacy_store, &fixture.legacy.thread).await;
    assert_eq!(backfilled, LEGACY_ENTRIES as i64);
    println!(
        "  legacy 首次布局: {:.3}ms，回填 {backfilled} 行",
        scan_time.as_secs_f64() * 1000.0
    );
    drop(legacy_store);

    let (legacy_hit_store, legacy_hit_data) = cold(&fixture.path).await;
    let mut connection = legacy_hit_data.database.pool.acquire().await.unwrap();
    let started = Instant::now();
    let read = legacy_hit_data
        .read_inherited_summary_cached(&mut connection, &fixture.legacy.thread)
        .await
        .unwrap();
    let hit_time = started.elapsed();
    drop(connection);
    assert_eq!(summary_entries(&read), LEGACY_ENTRIES);
    println!(
        "  legacy 回填后冷摘要: {:.3}ms",
        hit_time.as_secs_f64() * 1000.0
    );
    drop(legacy_hit_store);

    // 坏 legacy：payload_bytes 与列总字节数不符 → 小快照回退扫描且结果一致；
    // 大快照超过有界回退上界，必须显式 index_not_ready。
    println!("  -- 坏索引回退 --");
    for (label, session, expected) in [("medium", &fixture.medium, MEDIUM_ENTRIES)] {
        let (corrupt_store, _corrupt_data) = cold(&fixture.path).await;
        sqlx::query(
            "UPDATE inherited_snapshot_index SET payload_bytes = payload_bytes + 1 \
             WHERE thread_id = ?1",
        )
        .bind(session.thread.as_str())
        .execute(&corrupt_store.database.pool)
        .await
        .unwrap();
        drop(corrupt_store);
        let (fallback_store, fallback_data) = cold(&fixture.path).await;
        let mut connection = fallback_data.database.pool.acquire().await.unwrap();
        let started = Instant::now();
        let read = fallback_data
            .read_inherited_summary_cached(&mut connection, &session.thread)
            .await
            .unwrap();
        let fallback_time = started.elapsed();
        drop(connection);
        match &read {
            InheritedSummaryRead::Facts {
                snapshot: Some(snapshot),
            } => {
                assert_eq!(snapshot.entry_count, expected);
                assert_eq!(snapshot.first_entry, Some(session.ids[0]));
            }
            _ => panic!("{label} 坏索引必须回退扫描并给出摘要"),
        }
        println!(
            "  {label} 坏索引回退: {:.3}ms（{expected} 条，结果与命中一致）",
            fallback_time.as_secs_f64() * 1000.0
        );
        drop(fallback_store);
    }

    let (corrupt_store, _corrupt_data) = cold(&fixture.path).await;
    sqlx::query(
        "UPDATE inherited_snapshot_index SET payload_bytes = payload_bytes + 1 \
         WHERE thread_id = ?1",
    )
    .bind(fixture.large.thread.as_str())
    .execute(&corrupt_store.database.pool)
    .await
    .unwrap();
    drop(corrupt_store);
    let (gate_store, gate_data) = cold(&fixture.path).await;
    let mut connection = gate_data.database.pool.acquire().await.unwrap();
    let error = match gate_data
        .read_inherited_summary_cached(&mut connection, &fixture.large.thread)
        .await
    {
        Err(error) => error,
        Ok(_) => panic!("large bad index must fail closed"),
    };
    assert!(
        matches!(
            error.kind(),
            peri_acp_types::session_resources::SessionResourceErrorKind::IndexNotReady { .. }
        ),
        "large bad index must fail closed, got {error:?}"
    );
    drop(connection);
    drop(gate_store);
    println!("  large 坏索引: index_not_ready（未回退全列扫描）");
}
