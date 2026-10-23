//! A04–A05 场景用例（夹具与统计口径见父模块 `history_perf_tests`）。

use super::*;

// ── 用例 ──────────────────────────────────────────────────────────────────────

/// A04 / A05：三种归档规模下的初始恢复证据。
#[ignore = "性能证据夹具（1 万 / 10 万 / 100 万行）与冷热计时；运行方式见 .recon/perf-evidence.md"]
#[tokio::test]
async fn history_perf_restore_evidence() {
    let sizes = env_rows();
    let iters = env_usize("PERI_PERF_ITERS", DEFAULT_ITERS);

    println!("== 环境 ==");
    for (key, value) in machine_facts() {
        println!("  {key}: {value}");
    }
    println!(
        "  夹具：有效 {LIVE_ENTRIES} 条 × {LIVE_BODY_BYTES} 字节；归档正文 {ARCHIVED_BODY_BYTES} 字节；\
         iters={iters}"
    );

    for size in sizes {
        println!("\n== 归档 {size} 行 ==");
        let build_started = Instant::now();
        let fixture = build_fixture(size, ArchivedBody::Payload, ARCHIVED_BODY_BYTES).await;
        let build_cost = build_started.elapsed().as_secs_f64();
        let db_bytes = std::fs::metadata(&fixture.db_path)
            .map(|meta| meta.len())
            .unwrap_or(0);
        println!(
            "  夹具：messages={} 行（归档 {}，有效 {}），归档正文合计 {} 字节，有效正文合计 {} 字节，库文件 {db_bytes} 字节，夹具生成 {build_cost:.1}s",
            fixture.messages(),
            fixture.archived_rows,
            LIVE_ENTRIES,
            fixture.archived_bytes,
            fixture.live_bytes,
        );

        let pool = fixture_pool(&fixture.db_path).await;
        for (key, value) in storage_facts(&pool).await {
            println!("  {key}: {value}");
        }
        let (rows_in_range, excluded_rows) = row_probe(&pool, &fixture.thread).await;

        println!("  -- 计划（EXPLAIN QUERY PLAN，生产语句原文） --");
        let plans_started = Instant::now();
        let mut plans = restore_chain_plans(&pool, &fixture.thread).await;
        plans.extend(initial_load_extras(&pool, &fixture.thread).await);
        for evidence in &plans {
            assert_bounded_plan(evidence);
            if evidence.label.contains("active_payloads")
                || evidence.label.contains("active_flags")
                || evidence.label.contains("entry_positions")
            {
                assert_located_by_index(evidence);
            }
            print_plan(evidence);
        }
        println!(
            "  计划采集耗时 {:.3}s",
            plans_started.elapsed().as_secs_f64()
        );
        pool.close().await;

        // 实测：恢复只返回有效段（条数与字节都对齐夹具的有效段）。
        let facade = deployment(&fixture.db_path).await;
        let snapshot = facade
            .load_active_snapshot(&fixture.thread)
            .await
            .expect("初始恢复");
        let ids: Vec<MessageId> = snapshot
            .payloads
            .iter()
            .map(|payload| payload.id())
            .collect();
        assert_eq!(ids, fixture.live_ids, "有效集合必须与写入的有效段一致");
        let decoded = decoded_bytes(&snapshot);
        assert_eq!(
            decoded, fixture.live_bytes,
            "解码的正文必须只有有效段（字节级证据）"
        );
        assert_eq!(
            snapshot.flags.len(),
            1,
            "活跃 flags 只应返回带标记的有效条目"
        );
        println!(
            "  恢复实测：payloads={} flags={} 解码正文字节={} | 范围内行数（探针）={rows_in_range} 其中 excluded={excluded_rows}",
            snapshot.payloads.len(),
            snapshot.flags.len(),
            decoded,
        );
        println!(
            "  恢复的语句数（代码路径逐条列出，非执行期计数）：恢复链 7 条（session_data.rs 的 \
             load_active_snapshot）+ 该链不含分页/位置读取"
        );

        let cold = restore_cold(&fixture.db_path, &fixture.thread, iters).await;
        print_restore_stats("恢复 cold（每次新句柄，SQLite 页缓存为空）", &cold);
        let hot = restore_hot(&facade, &fixture.thread, iters).await;
        print_restore_stats("恢复 hot（同一句柄连续读取）", &hot);
        for sample in cold.iter().chain(hot.iter()) {
            assert_eq!(sample.payloads, LIVE_ENTRIES, "每次恢复都只返回有效段");
            assert_eq!(sample.payload_bytes, fixture.live_bytes);
        }
    }
}

/// A04：被排除的正文**一条都不解码**——归档正文写成不可解码内容，一旦读到就失败。
#[ignore = "性能证据夹具；运行方式见 .recon/perf-evidence.md"]
#[tokio::test]
async fn history_perf_excluded_bodies_are_never_decoded() {
    let rows = env_usize("PERI_PERF_TRIPWIRE_ROWS", DEFAULT_TRIPWIRE_ROWS);
    let fixture = build_fixture(rows, ArchivedBody::Undecodable, 0).await;
    println!(
        "== 探针夹具：归档 {rows} 行，正文为不可解码占位（{} 字节/行，合计 {} 字节） ==",
        UNDECODABLE_ARCHIVED_BODY.len(),
        fixture.archived_bytes
    );
    let facade = deployment(&fixture.db_path).await;

    // 对照组：完整读取会解码归档正文，必须失败——证明占位内容真的不可解码。
    let full = facade.load_session_snapshot(&fixture.thread).await;
    let full_error = full.expect_err("对照组必须失败：完整读取会解码归档正文");
    println!("  对照组 load_session_snapshot 失败（预期）：{full_error}");

    // 处理组：初始恢复只读有效范围，必须成功且只返回有效段。
    let snapshot = facade
        .load_active_snapshot(&fixture.thread)
        .await
        .expect("初始恢复不得读归档正文");
    let ids: Vec<MessageId> = snapshot
        .payloads
        .iter()
        .map(|payload| payload.id())
        .collect();
    assert_eq!(ids, fixture.live_ids, "只返回有效段");
    let decoded = decoded_bytes(&snapshot);
    assert_eq!(decoded, fixture.live_bytes, "解码字节只等于有效段");
    println!(
        "  处理组 load_active_snapshot 成功：payloads={} flags={} 解码正文字节={}（归档 {} 行正文一条未解码）",
        snapshot.payloads.len(),
        snapshot.flags.len(),
        decoded,
        fixture.archived_rows,
    );
}

// ── §5.3 / P01：手动 compact 的有效集合读取 ─────────────────────────────────

/// 固定有效上下文：条数与正文大小都不随归档规模变化。
const COMPACT_ACTIVE_ENTRIES: usize = 8;
/// 默认规模：10k 与 100k 在同一进程内对比；1M 只能用环境变量显式选择。
const DEFAULT_COMPACT_ARCHIVE_SCALES: [usize; 2] = [10_000, 100_000];

/// `PERI_PERF_ARCHIVE` 覆盖默认规模，逗号分隔（例如 `1000000` 或
/// `10000,100000,1000000`）。未设置时只跑默认的 10k/100k。
fn compact_archive_scales() -> Vec<usize> {
    match std::env::var("PERI_PERF_ARCHIVE") {
        Ok(raw) => {
            let sizes: Vec<usize> = raw
                .split(',')
                .filter_map(|value| value.trim().parse::<usize>().ok())
                .collect();
            assert!(
                !sizes.is_empty(),
                "PERI_PERF_ARCHIVE 必须包含至少一个正整数"
            );
            sizes
        }
        Err(_) => DEFAULT_COMPACT_ARCHIVE_SCALES.to_vec(),
    }
}

/// 固定有效段的确定性身份：跨夹具比较「同一有效集合」时，UUID 不能每次随机。
///
/// 仍使用 v7 布局（版本位 7、variant 位 10），走与生产相同的序列化与落库路径。
fn compact_live_message(index: usize) -> BaseMessage {
    let id = MessageId::from(uuid::Uuid::from_u128(
        0x0000_0000_0000_7000_8000_0000_0000_0000u128 + index as u128,
    ));
    BaseMessage::human(live_text(index, LIVE_BODY_BYTES)).with_message_id(id)
}

/// 与 [`build_fixture`] 同一套落库路径，只把有效段固定为 [`COMPACT_ACTIVE_ENTRIES`]。
///
/// 归档行按 [`INSERT_BATCH_ROWS`] 分批生成并等待 INSERT 落库，准备阶段不同时持有全集。
async fn build_compact_read_fixture(archived_rows: usize) -> Fixture {
    let directory = tempfile::tempdir().expect("临时目录");
    let db_path = directory.path().join("threads.db");
    let facade = deployment(&db_path).await;
    let workspace = facade
        .resolve_workspace(directory.path())
        .await
        .expect("解析工作区");
    facade
        .create_session(&fixture_session(PERF_SESSION, &workspace))
        .await
        .expect("建立会话");
    drop(facade);
    tokio::time::sleep(Duration::from_millis(200)).await;

    let pool = fixture_pool(&db_path).await;
    let mut archived_bytes = 0usize;
    for start in (0..archived_rows).step_by(INSERT_BATCH_ROWS) {
        let end = usize::min(start + INSERT_BATCH_ROWS, archived_rows);
        let rows = (start..end)
            .map(|index| {
                let payload = PersistedPayload::Message(BaseMessage::human(archived_text(
                    index,
                    ARCHIVED_BODY_BYTES,
                )));
                let message_id = payload.id().as_uuid().to_string();
                let content = serialize_persisted_payload(&payload).expect("归档载荷可序列化");
                archived_bytes += content.len();
                FixtureRow {
                    message_id,
                    content,
                    excluded: true,
                    truncated: false,
                }
            })
            .collect::<Vec<_>>();
        insert_rows(&pool, PERF_SESSION, &rows).await;
    }

    let mut live_ids = Vec::with_capacity(COMPACT_ACTIVE_ENTRIES);
    let mut live_bytes = 0usize;
    let mut live_rows = Vec::with_capacity(COMPACT_ACTIVE_ENTRIES);
    for index in 0..COMPACT_ACTIVE_ENTRIES {
        let payload = PersistedPayload::Message(compact_live_message(index));
        let id = payload.id();
        let content = serialize_persisted_payload(&payload).expect("有效载荷可序列化");
        live_bytes += content.len();
        live_ids.push(id);
        live_rows.push(FixtureRow {
            message_id: id.as_uuid().to_string(),
            content,
            excluded: false,
            // 第一条带投影标记：让活跃 flags 读取有非默认标记可返回。
            truncated: index == 0,
        });
    }
    insert_rows(&pool, PERF_SESSION, &live_rows).await;

    sqlx::query("UPDATE threads SET message_count = ?1, updated_at = ?2 WHERE id = ?3")
        .bind((archived_rows + COMPACT_ACTIVE_ENTRIES) as i64)
        .bind(peri_time::now_utc_rfc3339())
        .bind(PERF_SESSION)
        .execute(&pool)
        .await
        .expect("同步 message_count");
    pool.close().await;

    Fixture {
        _directory: directory,
        db_path,
        thread: PERF_SESSION.to_owned(),
        live_ids,
        live_bytes,
        archived_bytes,
        archived_rows,
    }
}

/// §5.3 / P01 / S3：固定有效上下文时，`load_active_snapshot` 的返回条数与
/// `serialize_persisted_payload` 总字节不随归档规模变化。
///
/// 判据只取条数、序列化字节和身份序列；墙钟时间仅打印供报告引用。解码条目数
/// 的口径是「入口返回的 `PersistedPayload` 条数」——sqlx 公开面没有底层解码
/// 尝试计数器，不把行数探针或耗时冒充解码量。
#[ignore = "性能证据夹具（默认 1 万/10 万归档行；1M 用 PERI_PERF_ARCHIVE=1000000 显式开启）"]
#[tokio::test]
async fn compact_active_snapshot_is_independent_of_archive_size() {
    let sizes = compact_archive_scales();
    println!("== 手动 compact 有效集合读取（§5.3 / P01） ==");
    println!(
        "  固定有效上下文：{COMPACT_ACTIVE_ENTRIES} 条 × {LIVE_BODY_BYTES} 字节正文；\
         归档正文 {ARCHIVED_BODY_BYTES} 字节/条；规模={sizes:?}"
    );
    println!(
        "  解码条目口径：load_active_snapshot 返回的 PersistedPayload 条数\
         （own + inherited）；API 未暴露底层解码尝试计数器。"
    );

    struct Observed {
        size: usize,
        payloads: usize,
        inherited: usize,
        encoded_bytes: usize,
        ids: Vec<MessageId>,
    }

    let mut observed: Vec<Observed> = Vec::with_capacity(sizes.len());
    for size in sizes {
        println!("\n== 归档 {size} 行 ==");
        let build_started = Instant::now();
        let fixture = build_compact_read_fixture(size).await;
        let build_cost = build_started.elapsed().as_secs_f64();
        let db_bytes = std::fs::metadata(&fixture.db_path)
            .map(|meta| meta.len())
            .unwrap_or(0);
        println!(
            "  夹具：messages={} 行（归档 {}，有效 {}），归档正文合计 {} 字节，库文件 {db_bytes} 字节，\
             生成 + 落库 {build_cost:.1}s",
            fixture.archived_rows + COMPACT_ACTIVE_ENTRIES,
            fixture.archived_rows,
            COMPACT_ACTIVE_ENTRIES,
            fixture.archived_bytes,
        );

        // 先证明归档行确实以 excluded=1 落库，避免「规模没写进去」被当成线性消失。
        let pool = fixture_pool(&fixture.db_path).await;
        let (rows_in_range, excluded_rows) = row_probe(&pool, &fixture.thread).await;
        pool.close().await;
        assert_eq!(
            excluded_rows as usize, size,
            "归档规模必须真实落库为 excluded=1"
        );
        assert_eq!(
            rows_in_range as usize,
            size + COMPACT_ACTIVE_ENTRIES,
            "消息总行数 = 归档 + 固定有效段"
        );

        let facade = deployment(&fixture.db_path).await;
        let started = Instant::now();
        let snapshot = facade
            .load_active_snapshot(&fixture.thread)
            .await
            .expect("手动 compact 的有效快照读取");
        let elapsed = started.elapsed();
        drop(facade);

        let ids: Vec<MessageId> = snapshot
            .payloads
            .iter()
            .map(|payload| payload.id())
            .collect();
        assert_eq!(
            ids, fixture.live_ids,
            "有效快照只能返回固定有效段，且身份与顺序不变"
        );
        assert_eq!(
            snapshot.payloads.len(),
            COMPACT_ACTIVE_ENTRIES,
            "payload 条数不得随归档规模变化"
        );
        assert_eq!(
            snapshot.inherited.payloads.len(),
            0,
            "本夹具没有继承区，继承 payload 必须为 0"
        );
        assert_eq!(snapshot.flags.len(), 1, "活跃 flags 只对应带标记的有效条目");

        let encoded: Vec<String> = snapshot
            .payloads
            .iter()
            .map(|payload| {
                serialize_persisted_payload(payload).expect("返回的有效载荷必须可序列化")
            })
            .collect();
        let encoded_bytes: usize = encoded.iter().map(String::len).sum();
        assert_eq!(
            encoded_bytes, fixture.live_bytes,
            "序列化总字节必须等于固定有效段"
        );
        assert!(
            encoded.iter().all(|text| !text.contains("archived #")),
            "有效快照不得包含任何归档正文"
        );

        println!(
            "  load_active_snapshot：payloads={}（解码条目） inherited={} flags={} \
             serialize 总字节={} 墙钟={:.3}ms",
            snapshot.payloads.len(),
            snapshot.inherited.payloads.len(),
            snapshot.flags.len(),
            encoded_bytes,
            elapsed.as_secs_f64() * 1000.0,
        );
        observed.push(Observed {
            size,
            payloads: snapshot.payloads.len(),
            inherited: snapshot.inherited.payloads.len(),
            encoded_bytes,
            ids,
        });

        // 释放本规模夹具（含临时库），下一个规模从空目录重新准备。
        drop(fixture);
    }

    let first = observed.first().expect("至少一个规模");
    for sample in &observed[1..] {
        assert_eq!(
            sample.payloads, first.payloads,
            "有效 payload 条数在 {} 与 {} 行归档下必须一致",
            first.size, sample.size
        );
        assert_eq!(
            sample.encoded_bytes, first.encoded_bytes,
            "序列化总字节在 {} 与 {} 行归档下必须一致",
            first.size, sample.size
        );
        assert_eq!(
            sample.inherited, first.inherited,
            "inherited payload 条数必须一致"
        );
        assert_eq!(
            sample.ids, first.ids,
            "有效身份序列在 {} 与 {} 行归档下必须一致",
            first.size, sample.size
        );
    }
    println!(
        "\n== 结论：归档 {} -> {:?} 行，有效 payloads={}、serialize 总字节={} 完全一致 ==",
        first.size,
        observed
            .iter()
            .map(|sample| sample.size)
            .collect::<Vec<_>>(),
        first.payloads,
        first.encoded_bytes,
    );
}

/// 新增的向前段读取语句的计划证据：按索引定位、无 `messages` 全表扫描。
///
/// 与其余用例不同，这一条**默认运行**（不加 `--ignored`）：`EXPLAIN QUERY PLAN` 只看 schema
/// 与索引，不需要大夹具，所以「新增语句是定界读取」这件事不依赖性能夹具是否跑过。
#[tokio::test]
async fn history_segment_statements_are_index_located() {
    let directory = tempfile::tempdir().unwrap();
    let db_path = directory.path().join("history-segment-plan.db");
    // 生产的打开路径建库（schema 与索引来自同一份 canonical DDL）。
    let resources = deployment(&db_path).await;
    let pool = fixture_pool(&db_path).await;

    println!("== 向前段读取的计划（EXPLAIN QUERY PLAN，生产语句原文；无夹具） ==");
    let plans = segment_read_plans(&pool, PERF_SESSION, 1, 2, SEGMENT_LIMIT).await;
    assert_eq!(plans.len(), 4);
    for evidence in &plans {
        assert_segment_plan_bounded(evidence);
        assert_located_by_index(evidence);
        print_plan(evidence);
    }

    drop(resources);
    pool.close().await;
}
