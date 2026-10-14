//! A04–A07 场景用例（夹具与统计口径见父模块 `history_perf_tests`）。

use super::*;

// ── 用例 ──────────────────────────────────────────────────────────────────────

/// A04 / A05 / A06：三种归档规模下的初始恢复证据 + 最大规模的 20 页遍历。
#[ignore = "性能证据夹具（1 万 / 10 万 / 100 万行）与冷热计时；运行方式见 .recon/perf-evidence.md"]
#[tokio::test]
async fn history_perf_restore_and_paging_evidence() {
    let sizes = env_rows();
    let iters = env_usize("PERI_PERF_ITERS", DEFAULT_ITERS);
    let pages = env_usize("PERI_PERF_PAGES", DEFAULT_PAGES);
    let largest = *sizes.iter().max().expect("至少一个规模");

    println!("== 环境 ==");
    for (key, value) in machine_facts() {
        println!("  {key}: {value}");
    }
    println!(
        "  夹具：有效 {LIVE_ENTRIES} 条 × {LIVE_BODY_BYTES} 字节；归档正文 {ARCHIVED_BODY_BYTES} 字节；\
         页 limit={PAGE_LIMIT} 目标={PAGE_BYTE_BUDGET} 字节；iters={iters} pages={pages}"
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
        plans.extend(initial_load_extras(&pool, &fixture.thread, None, PAGE_LIMIT).await);
        let anchor = fixture.live_ids[0].as_uuid().to_string();
        plans.extend(page_body_plan(&pool, &fixture.thread, &anchor).await);
        for evidence in &plans {
            assert_bounded_plan(evidence);
            if evidence.label.contains("active_payloads")
                || evidence.label.contains("active_flags")
                || evidence.label.contains("entry_positions")
                || evidence.label.contains("history_page")
                || evidence.label.contains("page_bodies")
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

        if size == largest {
            println!(
                "  -- 分页：{pages} 页 × limit={PAGE_LIMIT} / 目标 {PAGE_BYTE_BUDGET} 字节（同一句柄） --"
            );
            let mut cursor: Option<HistoryCursor> = None;
            let mut previous_min: Option<u64> = None;
            let mut walked = 0usize;
            let mut walk_durations = Vec::with_capacity(pages);
            for page_index in 0..pages {
                let started = Instant::now();
                let page = facade
                    .load_history_page(
                        &fixture.thread,
                        cursor.as_ref(),
                        PAGE_LIMIT,
                        PAGE_BYTE_BUDGET,
                    )
                    .await
                    .expect("读取历史页");
                let elapsed = started.elapsed();
                let bytes: usize = page.items.iter().map(|item| item.encoded_bytes).sum();
                let positions: Vec<u64> = page.items.iter().map(|item| item.position).collect();
                assert!(page.items.len() <= PAGE_LIMIT, "页不得超过 limit");
                assert!(bytes <= PAGE_BYTE_BUDGET, "普通页不得超过字节目标");
                assert!(
                    positions.windows(2).all(|pair| pair[0] < pair[1]),
                    "页内必须正序（更早 → 更晚）"
                );
                if let Some(previous_min) = previous_min {
                    assert!(positions[0] < previous_min, "下一页必须严格更旧");
                }
                let oldest = *positions.first().expect("页非空");
                previous_min = Some(oldest);
                walked += page.items.len();
                walk_durations.push(elapsed);
                println!(
                    "    第 {:>2} 页: items={} bytes={} oversize={} 最旧 position={} 语句数（代码路径）= {} p_ms={:.3}",
                    page_index + 1,
                    page.items.len(),
                    bytes,
                    page.items.iter().filter(|item| item.oversized).count(),
                    oldest,
                    3 + page.items.len(),
                    ms(elapsed),
                );
                cursor = page.next_cursor.clone();
                assert!(cursor.is_some(), "{pages} 页之内不得到头");
            }
            if fixture.messages() as usize >= pages * PAGE_LIMIT {
                assert_eq!(
                    walked,
                    pages * PAGE_LIMIT,
                    "连续翻页必须不重不漏地覆盖 limit × 页数 条"
                );
            }
            let mut first_page = Vec::with_capacity(iters);
            for _ in 0..iters {
                let started = Instant::now();
                let _ = facade
                    .load_history_page(&fixture.thread, None, PAGE_LIMIT, PAGE_BYTE_BUDGET)
                    .await
                    .expect("首屏页");
                first_page.push(started.elapsed());
            }
            print_durations("分页 hot（无游标首屏，重复读）", &first_page);
            print_durations("分页 20 页连续遍历（含游标页）", &walk_durations);
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

/// A07：单条正文超过页字节目标时独占一页、内容完整、游标前进。
#[ignore = "性能证据夹具；运行方式见 .recon/perf-evidence.md"]
#[tokio::test]
async fn history_perf_oversize_page_carries_one_body() {
    const OVERSIZE_ROWS: usize = 6;
    const OVERSIZE_BODY_BYTES: usize = 1_250_000;
    let fixture = build_fixture(OVERSIZE_ROWS, ArchivedBody::Payload, OVERSIZE_BODY_BYTES).await;
    println!(
        "== oversize 夹具：{OVERSIZE_ROWS} 条 × >1 MiB 正文（合计 {} 字节），页目标 {PAGE_BYTE_BUDGET} 字节 ==",
        fixture.archived_bytes
    );
    let facade = deployment(&fixture.db_path).await;
    // 首屏：最新的 3 条有效正文各自都放得进预算，超预算的归档正文被留到下一页。
    let first = facade
        .load_history_page(&fixture.thread, None, PAGE_LIMIT, PAGE_BYTE_BUDGET)
        .await
        .expect("读取首屏页");
    assert_eq!(first.items.len(), LIVE_ENTRIES, "首屏是有效段");
    println!(
        "    第 1 页（有效段）: items={} bytes={} oversize={}",
        first.items.len(),
        first
            .items
            .iter()
            .map(|item| item.encoded_bytes)
            .sum::<usize>(),
        first.items.iter().filter(|item| item.oversized).count(),
    );
    let mut cursor: Option<HistoryCursor> = first.next_cursor.clone();
    let mut previous_min: Option<u64> = first.items.iter().map(|item| item.position).min();
    for page_index in 0..3 {
        let started = Instant::now();
        let page = facade
            .load_history_page(
                &fixture.thread,
                cursor.as_ref(),
                PAGE_LIMIT,
                PAGE_BYTE_BUDGET,
            )
            .await
            .expect("读取 oversize 页");
        let elapsed = started.elapsed();
        assert_eq!(page.items.len(), 1, "oversized 封装必须独占一页");
        let item = &page.items[0];
        assert!(item.oversized, "超过字节目标必须显式标记 oversize");
        assert!(
            item.encoded_bytes > PAGE_BYTE_BUDGET,
            "本页正文必须确实超过预算"
        );
        assert!(
            item.payload.as_message().is_some(),
            "内容必须完整（可解码为消息）"
        );
        if let Some(previous_min) = previous_min {
            assert!(item.position < previous_min, "游标必须严格更旧");
        }
        previous_min = Some(item.position);
        println!(
            "    第 {} 页: items=1 bytes={} oversize={} position={} 语句数（代码路径）= 4 p_ms={:.3}",
            page_index + 2,
            item.encoded_bytes,
            item.oversized,
            item.position,
            ms(elapsed),
        );
        cursor = page.next_cursor.clone();
        assert!(cursor.is_some(), "oversized 页之后游标必须继续前进");
    }
}

/// A06：普通页长度由字节预算（而不是条数上限）决定时应停在预算上。
#[ignore = "性能证据夹具；运行方式见 .recon/perf-evidence.md"]
#[tokio::test]
async fn history_perf_byte_budget_bounds_page_length() {
    const ROWS: usize = 200;
    const BODY_BYTES: usize = 20 * 1024;
    let fixture = build_fixture(ROWS, ArchivedBody::Payload, BODY_BYTES).await;
    println!(
        "== 字节预算夹具：{ROWS} 条 × 约 {BODY_BYTES} 字节正文（合计 {} 字节），页目标 {PAGE_BYTE_BUDGET} 字节 ==",
        fixture.archived_bytes
    );
    let facade = deployment(&fixture.db_path).await;
    let started = Instant::now();
    let page = facade
        .load_history_page(&fixture.thread, None, PAGE_LIMIT, PAGE_BYTE_BUDGET)
        .await
        .expect("读取首屏页");
    let elapsed = started.elapsed();
    let bytes: usize = page.items.iter().map(|item| item.encoded_bytes).sum();
    assert!(
        page.items.len() < PAGE_LIMIT,
        "这一页应当由字节预算而不是条数上限截断"
    );
    assert!(bytes <= PAGE_BYTE_BUDGET, "不得超过页字节目标");
    assert!(
        bytes + BODY_BYTES > PAGE_BYTE_BUDGET,
        "预算应当被用满到只差下一条放不下"
    );
    println!(
        "    首屏页: items={} bytes={}（{:.1}% 预算）p_ms={:.3} 语句数（代码路径）= {}",
        page.items.len(),
        bytes,
        bytes as f64 / PAGE_BYTE_BUDGET as f64 * 100.0,
        ms(elapsed),
        3 + page.items.len(),
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
    let plans = segment_read_plans(&pool, PERF_SESSION, 1, 2, PAGE_LIMIT).await;
    assert_eq!(plans.len(), 4);
    for evidence in &plans {
        assert_segment_plan_bounded(evidence);
        assert_located_by_index(evidence);
        print_plan(evidence);
    }

    drop(resources);
    pool.close().await;
}
