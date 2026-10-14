//! 历史分页契约的行为测试：游标的三种失败、页的正序与推进规则、字节预算与 oversize。

use super::*;
use crate::store::serialize_persisted_payload;
use crate::store::PersistedPayload;
use crate::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
};

fn thread(name: &str) -> ThreadId {
    name.to_owned()
}

fn human(text: &str) -> PersistedPayload {
    PersistedPayload::Message(crate::messages::BaseMessage::human(text))
}

/// 与远端 `(message_id, content)` 两列同形的一行。
fn row(payload: &PersistedPayload) -> HistoryPageRow {
    HistoryPageRow::new(
        &payload.id().as_uuid().to_string(),
        serialize_persisted_payload(payload).unwrap(),
    )
    .unwrap()
}

/// 阶段一的候选行：id 取自 payload，正文只用来算长度（正文本身不进入候选行）。
fn candidate(payload: &PersistedPayload, position: i64, content: &str) -> HistoryPageCandidate {
    HistoryPageCandidate::new(
        &payload.id().as_uuid().to_string(),
        position,
        content.len() as i64,
    )
    .unwrap()
}

/// 一次分页读取：两个存储 adapter 的同一流程——阶段一定页，阶段二**只取本页那些行**的正文。
///
/// `candidates` 按存储序降序给入，元素是（payload，该行的信封原文）。阶段二按
/// `body_ids()` 取正文，因此「不该被读的行」可以把正文做成不可解码的垃圾来证明它没被读。
fn read_page(
    candidates: &[(PersistedPayload, String)],
    limit: usize,
    byte_budget: usize,
) -> SessionResourceResult<HistoryPage> {
    let planned: Vec<HistoryPageCandidate> = candidates
        .iter()
        .enumerate()
        .map(|(index, (payload, content))| candidate(payload, 1_000 - index as i64, content))
        .collect();
    let plan = HistoryPagePlan::plan(&thread("s-1"), "head", planned, limit, byte_budget);
    let rows = plan
        .body_ids()
        .into_iter()
        .map(|id| {
            let (_, content) = candidates
                .iter()
                .find(|(payload, _)| payload.id() == id)
                .expect("planned id comes from the candidates");
            HistoryPageRow::new(&id.as_uuid().to_string(), content.clone()).unwrap()
        })
        .collect();
    plan.materialize(rows)
}

/// 可选：给一组 payload 配上它们的信封原文（顺序与入参一致）。
fn bodies(payloads: &[PersistedPayload]) -> Vec<(PersistedPayload, String)> {
    payloads
        .iter()
        .map(|payload| {
            (
                payload.clone(),
                serialize_persisted_payload(payload).unwrap(),
            )
        })
        .collect()
}

fn reminder(audiences: Vec<ReminderAudience>) -> PersistedPayload {
    let reminder = SystemReminder {
        version: SYSTEM_REMINDER_VERSION,
        category: ReminderCategory::Security,
        source: ReminderSource("history_page_test".into()),
        kind: "notice".into(),
        severity: ReminderSeverity::Warning,
        delivery: ReminderDelivery::Configurable,
        audiences: ReminderAudiences(audiences),
        body: "body".into(),
        summary: None,
        metadata: serde_json::json!({}),
    };
    PersistedPayload::SystemReminder {
        id: crate::messages::MessageId::new(),
        reminder: TrustedSystemReminderFactory::for_producer()
            .construct(reminder)
            .unwrap(),
    }
}

#[test]
fn cursor_round_trips_and_distinguishes_its_failure_modes() {
    let cursor = HistoryCursor::new(thread("s-1"), crate::messages::MessageId::new(), "head-1");
    let raw = cursor.encode();
    assert_eq!(HistoryCursor::decode(&raw, &thread("s-1")).unwrap(), cursor);

    // 形式错误：不是本实现的产物、版本不认识、缺字段。
    for malformed in [
        "",
        "not json",
        r#"{"v":1,"thread":"s-1"}"#,
        r#"{"v":9,"thread":"s-1","anchor":"00000000-0000-0000-0000-000000000000","range":"h"}"#,
    ] {
        assert!(
            matches!(
                HistoryCursor::decode(malformed, &thread("s-1")),
                Err(HistoryCursorError::Malformed { .. })
            ),
            "{malformed} must be classified as malformed"
        );
    }

    // 跨会话：会话身份不匹配是分类，不是「回退到首屏」。
    assert_eq!(
        HistoryCursor::decode(&raw, &thread("s-2")).unwrap_err(),
        HistoryCursorError::CrossSession {
            expected: thread("s-2"),
            cursor: thread("s-1"),
        }
    );
}

#[test]
fn pages_are_forward_ordered_and_advance_without_gap_or_duplicate() {
    let payloads: Vec<PersistedPayload> = (0..5).map(|i| human(&format!("m{i}"))).collect();
    // 候选行按存储序降序：最新的一条在最前。
    let all: Vec<(PersistedPayload, String)> = bodies(&payloads).into_iter().rev().collect();

    // 第一次读取：limit 2 + 一条候选行 → 页内正序，游标停在最旧一条已推进的行上。
    let page = read_page(&all[..3], 2, usize::MAX).unwrap();
    assert_eq!(
        page.items.iter().map(|item| item.id).collect::<Vec<_>>(),
        vec![payloads[3].id(), payloads[4].id()]
    );
    let cursor = page.next_cursor.expect("older rows remain");
    assert_eq!(cursor.anchor, payloads[3].id());
    assert_eq!(cursor.range_token, "head");

    // 续读：锚点定位重读，多出的那一条候选行（m2）会被重新取回 → 无缺口、无重复。
    let page = read_page(&all[2..], 2, usize::MAX).unwrap();
    assert_eq!(
        page.items.iter().map(|item| item.id).collect::<Vec<_>>(),
        vec![payloads[1].id(), payloads[2].id()]
    );
    assert_eq!(page.next_cursor.unwrap().anchor, payloads[1].id());

    // 最后一页：候选行全部推进完且没有更多 → 不再给游标。
    let page = read_page(&all[4..], 2, usize::MAX).unwrap();
    assert_eq!(page.items[0].id, payloads[0].id());
    assert!(page.next_cursor.is_none());
}

/// REQ-H01-3 / 合并契约：页内 `position` 是存储序键原样给出的不透明整数——历史正序时严格递增，
/// 且与「更早方向」同向（页内最小值就是本页最旧一条）。
#[test]
fn page_positions_are_the_storage_order_key() {
    let payloads: Vec<PersistedPayload> = (0..4).map(|i| human(&format!("m{i}"))).collect();
    let all: Vec<(PersistedPayload, String)> = bodies(&payloads).into_iter().rev().collect();

    let page = read_page(&all[..2], 2, usize::MAX).unwrap();
    let positions: Vec<u64> = page.items.iter().map(|item| item.position).collect();
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "页内按历史正序，存储序键必须递增：{positions:?}"
    );
    assert_eq!(positions, vec![999, 1_000]);
}

/// PARTIAL A 的证据：阶段二**只**被问本页那些行的正文。
///
/// 第二条候选行的正文是不可解码的垃圾：任何「先把候选行的正文都取回来再裁页」的实现都会在
/// 阶段二读到它并失败（`Corrupt`）。它超预算，因此不在本页——本页必须原样读出。
#[test]
fn the_second_phase_is_asked_for_the_page_rows_only() {
    let visible = human("small");
    let unreadable_beyond_the_page = (
        human("older"),
        String::from("{\"this is not\": a payload envelope"),
    );
    let candidates = vec![
        (
            visible.clone(),
            serialize_persisted_payload(&visible).unwrap(),
        ),
        unreadable_beyond_the_page,
    ];

    let plan = HistoryPagePlan::plan(
        &thread("s-1"),
        "head",
        vec![
            candidate(&candidates[0].0, 9, &candidates[0].1),
            candidate(&candidates[1].0, 8, &candidates[1].1),
        ],
        1,
        usize::MAX,
    );
    assert_eq!(
        plan.body_ids(),
        vec![visible.id()],
        "阶段二只问本页的行，多读的那一条候选行不在其中"
    );

    let page = read_page(&candidates, 1, usize::MAX).unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].id, visible.id());
    assert!(
        page.next_cursor.is_some(),
        "多出的候选行说明更早方向还有内容"
    );
}

/// 阶段二的回复必须**恰好**是本页的行：少一行、多一行、换了顺序都是损坏，
/// 不能静默装配成一页（那会把一次不完整读取当成正常页）。
#[test]
fn the_second_phase_must_return_exactly_the_planned_rows() {
    let first = human("m0");
    let second = human("m1");
    let plan = || {
        HistoryPagePlan::plan(
            &thread("s-1"),
            "head",
            vec![
                candidate(&first, 2, &serialize_persisted_payload(&first).unwrap()),
                candidate(&second, 1, &serialize_persisted_payload(&second).unwrap()),
            ],
            5,
            usize::MAX,
        )
    };

    // 少一行。
    let error = plan()
        .materialize(vec![row(&first)])
        .expect_err("failing closed on a partial body reply");
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::Corrupt { .. }
    ));

    // 顺序不符（拿另一条顶替本页的行）。
    let error = plan()
        .materialize(vec![row(&second), row(&first)])
        .expect_err("failing closed when the bodies are not the planned rows");
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::Corrupt { .. }
    ));
}

#[test]
fn non_displayable_rows_advance_the_position_without_faking_the_end() {
    let visible = human("m0");
    let system = PersistedPayload::Message(crate::messages::BaseMessage::system("tool notice"));
    let model_only = reminder(vec![ReminderAudience::Model]);
    let rows = bodies(&[
        visible.clone(),
        system.clone(),
        model_only.clone(),
        visible.clone(),
    ]);

    // 一整段不可展示条目：页内没有它们，但游标照常前进，不能报「到头」。
    let page = read_page(&rows, 2, usize::MAX).unwrap();
    assert_eq!(
        page.items.iter().map(|item| item.id).collect::<Vec<_>>(),
        vec![visible.id()]
    );
    assert!(page.next_cursor.is_some(), "hidden rows are not the end");

    // 整页都是不可展示条目：空页 + 前进的游标（客户端据此继续读，而不是当成到头）。
    let rows = bodies(&[system, model_only.clone(), visible.clone()]);
    let page = read_page(&rows, 2, usize::MAX).unwrap();
    assert!(page.items.is_empty());
    assert_eq!(page.next_cursor.unwrap().anchor, model_only.id());
    // 续读拿回被跳过的可见条目，最后一段读空才收尾。
    let page = read_page(&bodies(&[visible]), 2, usize::MAX).unwrap();
    assert_eq!(page.items.len(), 1);
    assert!(page.next_cursor.is_none());
}

#[test]
fn a_body_over_the_remaining_budget_stops_the_page_before_decoding() {
    let first = human("small");
    let first_size = serialize_persisted_payload(&first).unwrap().len();
    // 第二条不是可解码的信封：它超预算时必须留在页外（连正文都不取），否则这里会变成 `Corrupt`。
    let undecodable = (human("never read"), "x".repeat(4096));
    let candidates = vec![
        (first.clone(), serialize_persisted_payload(&first).unwrap()),
        undecodable,
        (human("older"), "y".repeat(8)),
    ];
    let page = read_page(&candidates, 3, first_size + 8).unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].id, first.id());
    assert_eq!(page.total_bytes(), first_size);
    assert_eq!(page.next_cursor.unwrap().anchor, first.id());
}

#[test]
fn an_oversized_first_row_occupies_a_page_alone_and_still_advances() {
    let big = human(&"y".repeat(4096));
    let big_row = row(&big);
    let size = big_row.content.len();
    let candidates = vec![
        (big.clone(), big_row.content.clone()),
        (human("older"), "z".repeat(8)),
    ];
    let page = read_page(&candidates, 10, size / 4).unwrap();
    assert_eq!(page.items.len(), 1);
    assert!(page.items[0].oversized);
    assert!(page.total_bytes() > size / 4);
    assert_eq!(
        page.next_cursor.expect("older rows remain").anchor,
        big.id(),
        "oversize must not stop the cursor"
    );
}

#[test]
fn a_row_whose_id_does_not_match_its_payload_is_corrupt() {
    let payload = human("m");
    let mismatched = HistoryPageRow {
        message_id: crate::messages::MessageId::new(),
        content: serialize_persisted_payload(&payload).unwrap(),
    };
    let planned = HistoryPageCandidate::new(
        &mismatched.message_id.as_uuid().to_string(),
        1,
        mismatched.content.len() as i64,
    )
    .unwrap();
    let plan = HistoryPagePlan::plan(&thread("s-1"), "head", vec![planned], 1, usize::MAX);
    let error = plan.materialize(vec![mismatched]).unwrap_err();
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::Corrupt { .. }
    ));
}

#[test]
fn a_cursor_is_only_usable_while_its_anchor_and_range_head_survive() {
    let cursor = HistoryCursor::new(thread("s-1"), crate::messages::MessageId::new(), "head");
    assert_eq!(
        resolve_browsing_range(Some(&cursor), Some("head".to_owned()), true).unwrap(),
        "head"
    );
    // 锚点行被删：失败关闭，不回退到最新一页。
    for (head, anchor_present) in [
        (Some("head".to_owned()), false),
        (Some("other".to_owned()), true),
        (None, true),
        (None, false),
    ] {
        assert!(
            matches!(
                resolve_browsing_range(Some(&cursor), head, anchor_present),
                Err(HistoryCursorError::Expired { .. })
            ),
            "cursor must expire when head/anchor move"
        );
    }
    // 首屏：空会话没有浏览起点，身份是空串；有内容时起点即范围身份。
    assert_eq!(resolve_browsing_range(None, None, false).unwrap(), "");
    assert_eq!(
        resolve_browsing_range(None, Some("head".to_owned()), false).unwrap(),
        "head"
    );
}

#[test]
fn client_visibility_follows_the_existing_egress_rule() {
    assert!(human("m").displayable_to_client());
    assert!(
        !PersistedPayload::Message(crate::messages::BaseMessage::system("n"))
            .displayable_to_client()
    );
    assert!(!reminder(vec![ReminderAudience::Model]).displayable_to_client());
    assert!(reminder(vec![ReminderAudience::Model, ReminderAudience::Tui]).displayable_to_client());
}

#[test]
fn page_bounds_keep_the_stored_id_text_and_reject_non_uuids() {
    let head = "6f9619ff-8b86-d011-b42d-00c04fc964ff";
    let bounds = HistoryPageBounds::new(Some(head)).unwrap();
    assert_eq!(
        bounds.range_head,
        Some(MessageId::from(uuid::Uuid::parse_str(head).unwrap()))
    );
    // 范围身份就是持久化那列的文本：分页读取拿它和游标比较，两边不能各转一次。
    assert_eq!(bounds.range_token, head);

    // 空会话：没有起点，也没有范围身份（不是「起点为空串」）。
    let empty = HistoryPageBounds::new(None).unwrap();
    assert_eq!(empty.range_head, None);
    assert!(empty.range_token.is_empty());

    // id 不可解释即损坏：不降级成空范围。
    let error = HistoryPageBounds::new(Some("not-a-uuid")).unwrap_err();
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::Corrupt { .. }
    ));
}

// ─── canonical 完整回放（向前段） ──────────────────────────────────────────────

fn segment_ids(segment: &HistorySegment) -> Vec<MessageId> {
    segment
        .payloads
        .iter()
        .map(|payload| payload.id())
        .collect()
}

/// 一次段读取：阶段一按长度定段，阶段二**只取本段那些行**的正文。
///
/// 候选行按存储序**升序**给入（`positions_from` 起连续给位置，等价于 `rowid > after` 的
/// 那段区间）；继承区只在首段出现，`tail` 是本次回放的固定上界。
fn read_segment(
    candidates: &[(PersistedPayload, String)],
    positions_from: i64,
    limit: usize,
    byte_budget: usize,
    inherited: Vec<PersistedPayload>,
    tail: Option<&str>,
) -> SessionResourceResult<HistorySegment> {
    let planned: Vec<HistoryPageCandidate> = candidates
        .iter()
        .enumerate()
        .map(|(index, (payload, content))| {
            candidate(payload, positions_from + index as i64, content)
        })
        .collect();
    let plan = HistorySegmentPlan::plan(planned, limit, byte_budget);
    let rows = plan
        .body_ids()
        .into_iter()
        .map(|id| {
            let (_, content) = candidates
                .iter()
                .find(|(payload, _)| payload.id() == id)
                .expect("planned id comes from the candidates");
            HistoryPageRow::new(&id.as_uuid().to_string(), content.clone()).unwrap()
        })
        .collect();
    plan.materialize(inherited, tail, rows)
}

/// 向前游标只认自己的线上形式；形式错误一律是 `Malformed`，不回退成首段、也不读成「已到上界」。
#[test]
fn a_history_segment_forward_cursor_only_parses_its_own_wire_form() {
    let cursor = HistoryForwardCursor::new("7", "42");
    let raw = cursor.encode();
    assert_eq!(HistoryForwardCursor::decode(&raw).unwrap(), cursor);
    assert_eq!(cursor.rowid_bounds().unwrap(), (7, 42));

    for malformed in [
        "",
        "not json",
        r#"{"v":1,"after":"7"}"#,
        r#"{"v":9,"after":"7","tail":"42"}"#,
        r#"{"v":1,"after":"42","tail":"42"}"#,
        r#"{"v":1,"after":"43","tail":"42"}"#,
        r#"{"v":1,"after":"-1","tail":"42"}"#,
        r#"{"v":1,"after":" 7","tail":"42"}"#,
        r#"{"v":1,"after":"","tail":"42"}"#,
        r#"{"v":1,"after":"1.5","tail":"42"}"#,
    ] {
        assert!(
            matches!(
                HistoryForwardCursor::decode(malformed),
                Err(HistoryCursorError::Malformed { .. })
            ),
            "{malformed} must be classified as malformed"
        );
    }

    // 两个游标各有结构版本，线上形式互不解析：缺字段的对方产物不能顶替成本方向的游标。
    let browsing = HistoryCursor::new(thread("s-1"), MessageId::new(), "head");
    assert!(matches!(
        HistoryCursor::decode(&raw, &thread("s-1")),
        Err(HistoryCursorError::Malformed { .. })
    ));
    assert!(matches!(
        HistoryForwardCursor::decode(&browsing.encode()),
        Err(HistoryCursorError::Malformed { .. })
    ));
}

/// 一次完整回放：段按 canonical 顺序推进，继承区只在首段，无缺口、无重复，读尽上界才收尾。
#[test]
fn a_history_segment_replay_advances_without_gap_or_duplicate_until_the_tail() {
    let payloads: Vec<PersistedPayload> = (0..5).map(|i| human(&format!("m{i}"))).collect();
    let all = bodies(&payloads);
    let inherited = human("frozen");

    // 首段：limit 2，候选多给一条 → 还有更晚内容，游标停在最后一条已推进的行上。
    let segment = read_segment(
        &all[..3],
        1,
        2,
        usize::MAX,
        vec![inherited.clone()],
        Some("5"),
    )
    .unwrap();
    assert_eq!(
        segment_ids(&segment),
        vec![inherited.id(), payloads[0].id(), payloads[1].id()],
        "继承区整份跟在首段最前面"
    );
    let cursor = segment.next_cursor.expect("later rows remain");
    assert_eq!(cursor.after, "2");
    assert_eq!(cursor.tail, "5", "上界首段固定，此后随游标透传");

    // 续段：从 `after` 之后继续，继承区不再出现（它只在首段）。
    let segment =
        read_segment(&all[2..], 3, 2, usize::MAX, Vec::new(), Some(&cursor.tail)).unwrap();
    assert_eq!(
        segment_ids(&segment),
        vec![payloads[2].id(), payloads[3].id()]
    );
    let cursor = segment.next_cursor.unwrap();
    assert_eq!(cursor.after, "4");

    // 最后一段：候选行读完且没有多出来的那条 → 不再给游标（回放确实到头）。
    let segment =
        read_segment(&all[4..], 5, 2, usize::MAX, Vec::new(), Some(&cursor.tail)).unwrap();
    assert_eq!(segment_ids(&segment), vec![payloads[4].id()]);
    assert!(segment.next_cursor.is_none());
}

/// 段长由字节预算决定，不是被 `limit` 决定：预算装不下的那条留在下一段。
#[test]
fn a_history_segment_length_is_decided_by_bytes_not_by_limit() {
    let payloads: Vec<PersistedPayload> = (0..3).map(|i| human(&format!("m{i}"))).collect();
    let rows = bodies(&payloads);
    let budget: usize = rows[..2].iter().map(|(_, content)| content.len()).sum();

    let segment = read_segment(&rows, 1, 10, budget, Vec::new(), Some("3")).unwrap();
    assert_eq!(
        segment_ids(&segment),
        vec![payloads[0].id(), payloads[1].id()],
        "预算刚好装下两条"
    );
    assert_eq!(segment.next_cursor.expect("one row remains").after, "2");
}

/// 第一条就超预算时独占一段：完整回放不丢内容，且仍然推进——否则它之后的历史永远读不到。
#[test]
fn an_oversized_entry_takes_a_history_segment_alone_and_still_advances() {
    let big = human(&"y".repeat(4096));
    let segment = read_segment(
        &bodies(&[big.clone(), human("older")]),
        1,
        10,
        8,
        Vec::new(),
        Some("2"),
    )
    .unwrap();
    assert_eq!(segment_ids(&segment), vec![big.id()]);
    assert_eq!(segment.next_cursor.expect("older rows remain").after, "1");
}

/// 阶段二的回复必须**恰好**是本段的条目；上界丢失也是损坏，不当成「没有更多」。
#[test]
fn a_history_segment_fails_closed_on_a_partial_reply_or_a_missing_tail() {
    let payloads: Vec<PersistedPayload> = (0..3).map(|i| human(&format!("m{i}"))).collect();
    let rows = bodies(&payloads);
    let plan = || {
        HistorySegmentPlan::plan(
            rows.iter()
                .enumerate()
                .map(|(index, (payload, content))| candidate(payload, index as i64 + 1, content))
                .collect(),
            2,
            usize::MAX,
        )
    };
    let body_row = |index: usize| {
        let (payload, content) = &rows[index];
        HistoryPageRow::new(&payload.id().as_uuid().to_string(), content.clone()).unwrap()
    };

    // 少一行。
    let error = plan()
        .materialize(Vec::new(), Some("3"), vec![body_row(0)])
        .expect_err("failing closed on a partial body reply");
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::Corrupt { .. }
    ));

    // 顺序不符（拿本段之外的一条顶替）。
    let error = plan()
        .materialize(Vec::new(), Some("3"), vec![body_row(2), body_row(1)])
        .expect_err("failing closed when the bodies are not the planned entries");
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::Corrupt { .. }
    ));

    // 还有更晚内容却说不出上界：读取不完整，不给一段「看起来正常」的回放。
    let error = plan()
        .materialize(Vec::new(), None, vec![body_row(0), body_row(1)])
        .expect_err("failing closed when the fixed tail is missing");
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::Corrupt { .. }
    ));
}

/// `limit = 0` 的段既不推进任何行也铸不出游标，而「没有游标」等于「回放结束」——静默截断，
/// 因此失败关闭而不是返回空段。
#[test]
fn a_history_segment_with_a_zero_limit_fails_closed_instead_of_ending_the_replay() {
    let payloads: Vec<PersistedPayload> = (0..2).map(|i| human(&format!("m{i}"))).collect();
    let error = read_segment(&bodies(&payloads), 1, 0, usize::MAX, Vec::new(), Some("2"))
        .expect_err("a zero limit must not end the replay silently");
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::InvalidInput { .. }
    ));
}

/// 完整回放**不过滤**任何条目（已排除的归档原文、System、model-only reminder 都在其中），
/// 且阶段二只被问本段那些行：多取回来的那一条候选行不在其中。
#[test]
fn a_history_segment_delivers_every_entry_and_never_asks_for_the_lookahead_row() {
    let visible = human("m0");
    let system = PersistedPayload::Message(crate::messages::BaseMessage::system("tool notice"));
    let model_only = reminder(vec![ReminderAudience::Model]);
    let rows = bodies(&[visible.clone(), system.clone(), model_only.clone()]);

    let segment = read_segment(&rows, 1, 10, usize::MAX, Vec::new(), None).unwrap();
    assert_eq!(
        segment_ids(&segment),
        vec![visible.id(), system.id(), model_only.id()],
        "完整历史不做可展示性过滤"
    );
    assert!(segment.next_cursor.is_none());

    let plan = HistorySegmentPlan::plan(
        rows.iter()
            .enumerate()
            .map(|(index, (payload, content))| candidate(payload, index as i64 + 1, content))
            .collect(),
        1,
        usize::MAX,
    );
    assert_eq!(
        plan.body_ids(),
        vec![visible.id()],
        "阶段二只问本段的行，多读的那一条候选行不在其中"
    );
}
