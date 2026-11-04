//! 历史存储读取契约的行为测试：窗口页的字节预算与 oversize、标准回放的段推进与失败分类。

use super::*;
use crate::store::serialize_persisted_payload;
use crate::store::PersistedPayload;
use crate::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
};

fn human(text: &str) -> PersistedPayload {
    PersistedPayload::Message(crate::messages::BaseMessage::human(text))
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
fn client_visibility_follows_the_existing_egress_rule() {
    assert!(human("m").displayable_to_client());
    assert!(
        !PersistedPayload::Message(crate::messages::BaseMessage::system("n"))
            .displayable_to_client()
    );
    assert!(!reminder(vec![ReminderAudience::Model]).displayable_to_client());
    assert!(reminder(vec![ReminderAudience::Model, ReminderAudience::Tui]).displayable_to_client());
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
