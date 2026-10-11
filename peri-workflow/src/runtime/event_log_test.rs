//! `runtime/event_log.rs` 的行为测试：单调 sequence、保留边界与分页语义
//! （design §4.3 的读取契约）。

use super::*;

#[test]
fn empty_log_reports_zero_head_and_no_entries() {
    let log: BoundedEventLog<&str> = BoundedEventLog::new(4);
    assert_eq!(log.head_sequence(), 0, "无事件时已提交上界为 0");
    assert_eq!(log.truncated_before(), 1, "初始保留边界为 1（从 1 起编号）");
    assert!(log.is_empty());
    let page = log.page(None, 10);
    assert!(page.entries.is_empty());
    assert_eq!(page.head_sequence, 0);
    assert_eq!(page.truncated_before, 1);
}

#[test]
fn append_sequences_are_monotonic_from_one() {
    let log = BoundedEventLog::new(8);
    assert_eq!(log.append("a"), 1);
    assert_eq!(log.append("b"), 2);
    assert_eq!(log.append("c"), 3);
    assert_eq!(log.head_sequence(), 3);
    assert_eq!(log.len(), 3);
}

#[test]
fn append_with_receives_the_allocated_sequence() {
    let log = BoundedEventLog::new(8);
    let sequence = log.append_with(|sequence| sequence * 10);
    assert_eq!(sequence, 1);
    let page = log.page(None, 10);
    assert_eq!(page.entries, vec![10], "闭包必须收到本次分配的 sequence");
}

#[test]
fn capacity_eviction_advances_truncated_before() {
    let log = BoundedEventLog::new(3);
    for value in ["a", "b", "c", "d", "e"] {
        log.append(value);
    }
    assert_eq!(log.len(), 3, "容量即保留窗口");
    assert_eq!(log.head_sequence(), 5);
    assert_eq!(log.truncated_before(), 3, "淘汰 a/b 后边界前进到 3");
    let page = log.page(None, 10);
    assert_eq!(page.entries, vec!["c", "d", "e"]);
    assert_eq!(page.truncated_before, 3);
}

#[test]
fn page_resumes_after_cursor_and_reports_head() {
    let log = BoundedEventLog::new(8);
    for value in ["a", "b", "c", "d"] {
        log.append(value);
    }
    let page = log.page(Some(1), 2);
    assert_eq!(page.entries, vec!["b", "c"], "after 为 exclusive 游标");
    assert_eq!(page.head_sequence, 4);
    let tail = log.page(Some(3), 10);
    assert_eq!(tail.entries, vec!["d"]);
    let exhausted = log.page(Some(4), 10);
    assert!(exhausted.entries.is_empty());
}

#[test]
fn stale_cursor_reads_from_truncated_boundary_without_pretending_continuity() {
    let log = BoundedEventLog::new(2);
    for value in ["a", "b", "c", "d"] {
        log.append(value);
    }
    // 游标 1 已早于保留边界 3：从边界起读，页内 truncated_before 让读取方识别断档。
    let page = log.page(Some(1), 10);
    assert_eq!(page.entries, vec!["c", "d"]);
    assert_eq!(page.truncated_before, 3);
    assert!(
        page.entries.first().map(|v| *v != "b").unwrap_or(true),
        "不得假装连续（b 已被淘汰）"
    );
}

#[test]
fn page_where_filters_without_losing_sequence_metadata() {
    let log = BoundedEventLog::new(8);
    for value in [1, 2, 3, 4, 5] {
        log.append(value);
    }
    let page = log.page_where(None, 10, |value| value % 2 == 0);
    assert_eq!(page.entries, vec![2, 4]);
    assert_eq!(page.head_sequence, 5, "过滤不改变已提交上界");
}

#[test]
fn latest_returns_tail_in_ascending_order() {
    let log = BoundedEventLog::new(8);
    for value in [1, 2, 3, 4, 5] {
        log.append(value);
    }
    assert_eq!(log.latest(2), vec![4, 5]);
    assert_eq!(log.latest(10), vec![1, 2, 3, 4, 5], "不足时返回全部");
}
