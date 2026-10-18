//! S6.5：PENDING_TOOL_ENDS 预算化/引用化。

use serial_test::serial;

#[test]
#[serial]
fn pending_tool_ends_are_bounded_and_store_lightweight_summaries() {
    crate::kit::atoms::init_atoms();
    crate::kit::atoms::ACTIVE_SESSION_ID.state().write().clear();
    crate::kit::acp_events::history_window::clear_pending_tool_ends();
    let full_output = "x".repeat(4096);
    for index in 0..400 {
        crate::kit::acp_events::history_window::insert_pending_tool_end_for_tests(
            &format!("tool-{index}"),
            &full_output,
            false,
        );
    }
    let (entries, bytes) = crate::kit::acp_events::history_window::pending_tool_ends_fingerprint();
    assert!(entries <= 256, "entry cap exceeded: {entries}");
    assert!(bytes <= 1024 * 1024, "1 MiB budget exceeded: {bytes}");
    assert!(
        bytes < full_output.len() * entries,
        "must retain summaries, not full outputs"
    );
    crate::kit::acp_events::history_window::clear_pending_tool_ends();
    assert_eq!(
        crate::kit::acp_events::history_window::pending_tool_ends_fingerprint(),
        (0, 0)
    );
}

#[test]
#[serial]
fn resident_index_tool_pairing_keeps_pending_budget() {
    use crate::kit::history_window::{
        HistorySource, ResidentIndex, ResultRef, ToolPairRejected, ToolTerminalMeta,
    };
    let mut index = ResidentIndex::new(4096, 2);
    let meta = ToolTerminalMeta::new(false, 999_999, "x".repeat(2048));
    for index_id in 0..2 {
        index
            .record_tool_result(
                HistorySource::Own,
                format!("t{index_id}"),
                ResultRef::new(HistorySource::Own, format!("s1:m{index_id}"), "rev"),
                meta.clone(),
            )
            .expect("within limit");
    }
    assert!(matches!(
        index.record_tool_result(
            HistorySource::Own,
            "t3",
            ResultRef::new(HistorySource::Own, "s1:m3", "rev"),
            meta,
        ),
        Err(ToolPairRejected::TooManyPending)
    ));
    assert!(index.pending_tool_bytes() <= 4096);
}
