use std::sync::Arc;

use super::test_support::*;
use super::*;

#[test]
fn resident_index_keeps_slot_prefix_sums_in_layout_order() {
    let mut window = enabled_window(WindowLimits::default());
    let a = window
        .register_segment(
            segment_with_units("a", 10, 1, 10, 20),
            SegmentPlacement::Tail,
        )
        .expect("a");
    let b = window
        .register_segment(
            segment_with_units("b", 20, 2, 20, 30),
            SegmentPlacement::Tail,
        )
        .expect("b");
    assert_eq!(window.index().slot_range(a), Some(0..1));
    assert_eq!(window.index().slot_range(b), Some(1..3));
    assert_eq!(window.unit_count(), 3);

    let c = window
        .register_segment(
            segment_with_units("c", 5, 3, 30, 40),
            SegmentPlacement::BeforeSegment(a),
        )
        .expect("c");
    assert_eq!(window.index().slot_range(c), Some(0..3));
    assert_eq!(window.index().slot_range(a), Some(3..4));
    assert_eq!(window.index().slot_range(b), Some(4..6));
    assert_eq!(window.unit_count(), 6);
}

#[test]
fn resident_index_rejects_duplicate_identity_until_segment_is_released() {
    let mut window = enabled_window(WindowLimits::default());
    let first = window
        .register_segment(segment("a", 10, 10, 20), SegmentPlacement::Tail)
        .expect("first");
    assert!(matches!(
        window.register_segment(segment("a", 10, 10, 20), SegmentPlacement::Tail),
        Err(WindowModelError::DuplicateEntry(_))
    ));

    window.detach_segment(first).expect("detach");
    assert!(
        !window
            .index()
            .contains_entry(&PersistentEntryKey::own("s1:a"))
    );
    assert!(
        window
            .register_segment(segment("a", 10, 10, 20), SegmentPlacement::Tail)
            .is_ok(),
        "after eviction the same identity may be materialized again (A25)"
    );
}

#[test]
fn shared_arc_allocation_is_counted_once_but_distinct_allocations_are_not() {
    let mut index = ResidentIndex::default();
    let shared = Arc::new(vec![0u8; 64]);
    assert_eq!(index.account_shared_arc(&shared, 64), 64);
    assert_eq!(index.account_shared_arc(&shared.clone(), 64), 0);
    let other = Arc::new(vec![0u8; 32]);
    assert_eq!(index.account_shared_arc(&other, 32), 32);
    assert_eq!(index.allocations().bytes(), 96);
    assert_eq!(index.allocations().deduplicated_calls(), 1);
    assert_eq!(index.allocations().len(), 2);
}

#[test]
fn tool_pairing_crosses_segments_and_keeps_terminal_meta() {
    let mut window = enabled_window(WindowLimits::default());
    let call_segment = window
        .register_segment(segment("call", 10, 10, 20), SegmentPlacement::Tail)
        .expect("call segment");
    let result_segment = window
        .register_segment(segment("result", 20, 10, 20), SegmentPlacement::Tail)
        .expect("result segment");
    let call_entry = window.segment(call_segment).unwrap().entries[0].key.clone();
    let result_entry = window.segment(result_segment).unwrap().entries[0]
        .key
        .clone();

    assert_eq!(
        window
            .index
            .record_tool_started(HistorySource::Own, "tool-1", call_entry.clone(), 0),
        ToolPairOutcome::CallOnly
    );
    assert_eq!(
        window
            .index
            .record_tool_result(
                HistorySource::Own,
                "tool-1",
                ResultRef::new(HistorySource::Own, "s1:result", "rev-1"),
                ToolTerminalMeta::new(false, 9_000, "ok"),
            )
            .expect("pair result"),
        ToolPairOutcome::Paired
    );
    assert!(matches!(
        window
            .index
            .tool_call(&ToolCallKey::new(HistorySource::Own, "tool-1")),
        Some(ToolCallState::Terminal {
            call_ref,
            result_ref: Some(_),
            ..
        }) if call_ref == &call_entry
    ));

    // 结果段被淘汰：调用仍在，终态元数据保留，不退回 running。
    window
        .detach_segment(result_segment)
        .expect("detach result");
    assert!(matches!(
        window
            .index
            .tool_call(&ToolCallKey::new(HistorySource::Own, "tool-1")),
        Some(ToolCallState::Terminal {
            result_ref: Some(_),
            ..
        })
    ));

    // 两侧都不驻留：轻量配对状态可丢弃。
    window.detach_segment(call_segment).expect("detach call");
    assert!(
        window
            .index
            .tool_call(&ToolCallKey::new(HistorySource::Own, "tool-1"))
            .is_none()
    );
    let _ = result_entry;
}

#[test]
fn unmatched_tool_result_is_reference_only_and_pairs_when_call_arrives() {
    let mut index = ResidentIndex::new(8 * 1024, 4);
    let meta = ToolTerminalMeta::new(false, 500_000, "x".repeat(2048));
    assert_eq!(meta.output().len(), 2048, "full output is retained");
    assert!(meta.summary().len() <= ToolTerminalMeta::MAX_SUMMARY_BYTES);
    assert_eq!(
        index
            .record_tool_result(
                HistorySource::Own,
                "tool-2",
                ResultRef::new(HistorySource::Own, "s1:late", "rev-2"),
                meta.clone(),
            )
            .expect("result only"),
        ToolPairOutcome::ResultOnly
    );
    assert_eq!(index.pending_tool_bytes(), meta.retained_bytes());
    assert!(
        index.pending_tool_bytes() >= 2048,
        "full output, not a 1 KiB summary"
    );

    assert_eq!(
        index.record_tool_started(
            HistorySource::Own,
            "tool-2",
            PersistentEntryKey::own("s1:call"),
            1
        ),
        ToolPairOutcome::Paired
    );
    assert_eq!(index.pending_tool_bytes(), 0);
}
