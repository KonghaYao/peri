//! 审计补充：verification 独立复核用（不是切片交付物）。

use super::*;

// ── 审计补充（verification 独立复核用；不是切片交付物） ────────────────────────

/// `EXPLAIN`（VDBE 字节码）原文：`addr opcode p1 p2 p3 p5 p4 comment`。
///
/// 与 `EXPLAIN QUERY PLAN` 的差别：字节码能回答「残差谓词在索引项上判定，还是回表之后才判定」，
/// 用来把「按索引定位」与「触达行数有界」分开。参数不绑定（`EXPLAIN` 不执行语句）。
async fn explain_bytecode(pool: &SqlitePool, sql: &str) -> Vec<String> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!("EXPLAIN {sql}")))
        .fetch_all(pool)
        .await
        .expect("EXPLAIN");
    rows.iter()
        .map(|row| {
            let addr: i64 = row.try_get(0).unwrap_or(-1);
            let opcode: String = row.try_get(1).unwrap_or_default();
            let p1: i64 = row.try_get(2).unwrap_or(0);
            let p2: i64 = row.try_get(3).unwrap_or(0);
            let p3: i64 = row.try_get(4).unwrap_or(0);
            let p4: Option<String> = row.try_get(5).unwrap_or(None);
            let p5: i64 = row.try_get(6).unwrap_or(0);
            let comment: Option<String> = row.try_get(7).unwrap_or(None);
            format!(
                "{addr:>4} {opcode:<16} {p1:>3} {p2:>4} {p3:>4} {p5:>2} {} {}",
                p4.unwrap_or_default(),
                comment.unwrap_or_default()
            )
        })
        .collect()
}

/// 审计证据（独立复核，不改动生产语义）：
///
/// ① 两条阶段一语句的 `EXPLAIN` 字节码（残差判定位置）；
/// ② 探针夹具：本段之后那一条候选行的正文不可解码——本段仍能读出（阶段二没取它），
///    而把它推进为下一段时以 `Corrupt` 失败（证明探针真的不可解码）；
/// ③ 段长与字节预算的实测：逐段打印行数与编码字节。
#[ignore = "审计补充证据；命令与输出见 .recon/S9-boundedness.md"]
#[tokio::test]
async fn audit_segment_lookahead_and_plan_shape() {
    // ── ① 字节码形状（无夹具） ──
    let directory = tempfile::tempdir().unwrap();
    let db_path = directory.path().join("audit-plan.db");
    let resources = deployment(&db_path).await;
    let pool = fixture_pool(&db_path).await;
    println!("== EXPLAIN（VDBE 字节码，生产语句原文；参数未绑定） ==");
    for (label, sql) in [
        (
            "HISTORY_PAGE_ROWS_SQL（分页阶段一，带游标）",
            HISTORY_PAGE_ROWS_SQL,
        ),
        (
            "HISTORY_SEGMENT_NEXT_SQL（回放非首段）",
            HISTORY_SEGMENT_NEXT_SQL,
        ),
        // 对照语句（**不是生产语句**，只为证据）：不带 CAST 的 length()。它的 `Column` 上应出现
        // `OPFLAG_LENGTHARG`（p5 含 64）——生产语句若没有这个标志，就说明长度是从**取回的值**
        // 上算的，而不是从记录头里的序列类型直接读出的长度。
        (
            "对照：SELECT length(content)（无 CAST）",
            "SELECT length(content) FROM messages WHERE thread_id = ?1",
        ),
    ] {
        println!("  · {label} — 语句: {sql}");
        for line in explain_bytecode(&pool, sql).await {
            println!("      {line}");
        }
    }
    drop(resources);
    pool.close().await;

    // ── ② 探针：本段之后那一条候选行的正文不可解码 ──
    let fixture = build_fixture(6, ArchivedBody::Payload, 8 * 1024).await;
    let pool = fixture_pool(&fixture.db_path).await;
    insert_rows(
        &pool,
        &fixture.thread,
        &[FixtureRow {
            message_id: MessageId::new().as_uuid().to_string(),
            content: UNDECODABLE_ARCHIVED_BODY.to_owned(),
            excluded: false,
            truncated: false,
        }],
    )
    .await;
    pool.close().await;
    let facade = deployment(&fixture.db_path).await;
    println!(
        "== 探针：6 条 8 KiB 归档行 + 3 条 64 KiB 有效行 + 1 条不可解码行（rowid 最大），limit=3 =="
    );
    let mut cursor: Option<peri_acp_types::history_page::HistoryForwardCursor> = None;
    let mut index = 0usize;
    loop {
        index += 1;
        let outcome = facade
            .load_history_segment(&fixture.thread, cursor.as_ref(), 3, usize::MAX)
            .await;
        match outcome {
            Ok(segment) => {
                let ids: Vec<MessageId> =
                    segment.payloads.iter().map(PersistedPayload::id).collect();
                println!(
                    "    第 {index} 段: payloads={} next_cursor={}",
                    segment.payloads.len(),
                    segment.next_cursor.is_some()
                );
                if index == 3 {
                    assert_eq!(
                        ids, fixture.live_ids,
                        "第 3 段是有效段；它的下一条候选行是不可解码行，本段读出即证明阶段二没取它"
                    );
                }
                match segment.next_cursor {
                    Some(next) => cursor = Some(next),
                    None => break,
                }
            }
            Err(error) => {
                println!("    第 {index} 段失败: {error}");
                assert!(
                    matches!(
                        error.kind(),
                        peri_acp_types::session_resources::SessionResourceErrorKind::Corrupt { .. }
                    ),
                    "不可解码行被推进时才应以 Corrupt 失败，实际: {error}"
                );
                assert_eq!(index, 4, "只有推进到不可解码行的那一段才允许失败");
                break;
            }
        }
    }
    assert_eq!(index, 4, "探针行必须真的被推进过一段（否则探针没响）");

    // ── ③ 段长与字节预算实测 ──
    let fixture = build_fixture(6, ArchivedBody::Payload, 8 * 1024).await;
    let facade = deployment(&fixture.db_path).await;
    let budget = 96 * 1024;
    let limit = 50;
    println!("== 段长/字节预算实测：limit={limit} 预算={budget} 字节 ==");
    let mut cursor: Option<peri_acp_types::history_page::HistoryForwardCursor> = None;
    let mut total_rows = 0usize;
    let mut total_bytes = 0usize;
    loop {
        let segment = facade
            .load_history_segment(&fixture.thread, cursor.as_ref(), limit, budget)
            .await
            .expect("段读取");
        let bytes: usize = segment
            .payloads
            .iter()
            .map(|payload| {
                serialize_persisted_payload(payload)
                    .expect("段载荷可序列化")
                    .len()
            })
            .sum();
        println!(
            "    段: payloads={} bytes={} 超预算={} next_cursor={}",
            segment.payloads.len(),
            bytes,
            bytes > budget,
            segment.next_cursor.is_some()
        );
        assert!(segment.payloads.len() <= limit, "段不得超过 limit");
        assert!(
            bytes <= budget || segment.payloads.len() == 1,
            "超过字节预算时只允许单条独占一段"
        );
        total_rows += segment.payloads.len();
        total_bytes += bytes;
        match segment.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    println!("    合计推进 {total_rows} 条自有行 / {total_bytes} 字节（夹具：6 归档 + 3 有效）");
    assert_eq!(total_rows, 6 + LIVE_ENTRIES, "回放必须不重不漏");
    assert_eq!(
        total_bytes,
        fixture.archived_bytes + fixture.live_bytes,
        "段内编码字节之和必须等于夹具写入的正文编码字节"
    );

    // ── ④ 反例：回放中途删掉 tail 那一行（精确删除/rewind 的尾部切除）再追加新行 ──
    //
    // `tail` 是首段那一刻的**数值**上界；契约只要求「追加的行 rowid > tail 时不进入本次遍历」，
    // 不检测上界行自己消失。SQLite 在删掉最大 rowid 之后会把该 rowid 复用给下一次插入，
    // 于是新行落回 (after, tail] 区间内。这一条测的是该情形下的实际交付。
    let fixture = build_fixture(0, ArchivedBody::Payload, 0).await;
    let facade = deployment(&fixture.db_path).await;
    let thread = fixture.thread.clone();
    let first = facade
        .load_history_segment(&thread, None, 2, usize::MAX)
        .await
        .expect("首段");
    let cursor = first.next_cursor.clone().expect("还有第 3 条");
    let removed = fixture.live_ids[2];
    facade
        .remove_history_entries(&thread, &[removed])
        .await
        .expect("删除 tail 行");
    let appended = PersistedPayload::Message(BaseMessage::human("回放期间新增"));
    facade
        .append_history(&thread, std::slice::from_ref(&appended))
        .await
        .expect("追加新行");
    let next = facade
        .load_history_segment(&thread, Some(&cursor), 5, usize::MAX)
        .await
        .expect("续段");
    let next_ids: Vec<MessageId> = next.payloads.iter().map(PersistedPayload::id).collect();
    println!(
        "    反例（删 tail 行 + 追加）：续段 payloads={} 含新行={} 含被删行={} next_cursor={}",
        next.payloads.len(),
        next_ids.contains(&appended.id()),
        next_ids.contains(&removed),
        next.next_cursor.is_some()
    );
}
