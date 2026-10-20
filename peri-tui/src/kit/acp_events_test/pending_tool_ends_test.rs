//! S6.5：PENDING_TOOL_ENDS 预算化/引用化；F07-b 按条目精准清理。

use std::collections::HashSet;

use crate::kit::history_window::{
    PENDING_TOOL_ENDS_BYTES_MAX, PENDING_TOOL_ENDS_LIMIT, PersistentEntryKey,
};
use serial_test::serial;

fn entry(tool: &str) -> PersistentEntryKey {
    PersistentEntryKey::own(format!("s1:{tool}"))
}

#[test]
#[serial]
fn pending_tool_ends_are_bounded_and_keep_full_outputs() {
    crate::kit::atoms::init_atoms();
    crate::kit::atoms::ACTIVE_SESSION_ID.state().write().clear();
    crate::kit::acp_events::history_window::clear_pending_tool_ends();
    let full_output = "x".repeat(4096);
    for index in 0..400 {
        crate::kit::acp_events::history_window::insert_pending_tool_end_for_tests(
            &format!("tool-{index}"),
            entry(&format!("tool-{index}")),
            &full_output,
            false,
        );
    }
    let (entries, bytes) = crate::kit::acp_events::history_window::pending_tool_ends_fingerprint();
    assert!(
        entries <= PENDING_TOOL_ENDS_LIMIT,
        "entry cap exceeded: {entries}"
    );
    assert!(
        bytes <= PENDING_TOOL_ENDS_BYTES_MAX,
        "8 MiB budget exceeded: {bytes}"
    );
    assert!(
        bytes >= entries * full_output.len(),
        "must retain complete outputs, not 1 KiB summaries"
    );
    crate::kit::acp_events::history_window::clear_pending_tool_ends();
    assert_eq!(
        crate::kit::acp_events::history_window::pending_tool_ends_fingerprint(),
        (0, 0)
    );
}

/// F07-a：跨页暂存只有一份预算权威（`history_window::budget`），
/// `WindowLimits` 不再维护第二套 pending 值。
#[test]
#[serial]
fn pending_tool_ends_budget_has_single_authority() {
    crate::kit::atoms::init_atoms();
    crate::kit::atoms::ACTIVE_SESSION_ID.state().write().clear();
    assert_eq!(PENDING_TOOL_ENDS_LIMIT, 256);
    assert_eq!(PENDING_TOOL_ENDS_BYTES_MAX, 8 * 1024 * 1024);
    assert_eq!(
        crate::kit::acp_events::history_window::pending_tool_ends_limits_for_tests(),
        (PENDING_TOOL_ENDS_LIMIT, PENDING_TOOL_ENDS_BYTES_MAX)
    );
}

/// F07-b：按结果条目身份删除；其他段等待配对的终点必须保留。
#[test]
#[serial]
fn remove_entries_only_drops_matching_result_entries() {
    crate::kit::atoms::init_atoms();
    crate::kit::atoms::ACTIVE_SESSION_ID.state().write().clear();
    crate::kit::acp_events::history_window::clear_pending_tool_ends();
    let released = entry("released");
    let kept = entry("kept");
    crate::kit::acp_events::history_window::insert_pending_tool_end_for_tests(
        "tool-released",
        released.clone(),
        &"x".repeat(64),
        false,
    );
    crate::kit::acp_events::history_window::insert_pending_tool_end_for_tests(
        "tool-kept",
        kept.clone(),
        &"y".repeat(32),
        false,
    );

    let removed = crate::kit::acp_events::history_window::remove_pending_tool_ends_for_entries(
        &HashSet::from([released]),
    );
    assert_eq!(removed, 1, "只有释放集合覆盖的条目被删除");
    assert!(
        crate::kit::acp_events::history_window::pending_tool_end_entry_for_tests("tool-released")
            .is_none()
    );
    assert_eq!(
        crate::kit::acp_events::history_window::pending_tool_end_entry_for_tests("tool-kept"),
        Some(kept),
        "别的段的未配对终点不能被误删"
    );
    crate::kit::acp_events::history_window::clear_pending_tool_ends();
}
