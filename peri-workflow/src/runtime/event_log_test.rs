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
fn page_where_resumes_from_middle_tail_and_future_cursor() {
    let log = BoundedEventLog::new(8);
    for value in [1, 2, 3, 4, 5] {
        log.append(value);
    }
    let middle = log.page_where(Some(2), 2, |_| true);
    assert_eq!(middle.entries, vec![3, 4], "中间游标从 cursor+1 起读");
    assert_eq!(middle.head_sequence, 5);
    assert_eq!(middle.truncated_before, 1);

    let tail = log.page_where(Some(4), 10, |_| true);
    assert_eq!(tail.entries, vec![5], "尾游标读到 head 为止");

    let exhausted = log.page_where(Some(5), 10, |_| true);
    assert!(exhausted.entries.is_empty(), "游标等于 head：空页");
    assert_eq!(exhausted.head_sequence, 5);

    let future = log.page_where(Some(99), 10, |_| true);
    assert!(future.entries.is_empty(), "未来游标（> head）返回空页");
    assert_eq!(future.head_sequence, 5, "未来游标不回退已提交上界");
    assert_eq!(future.truncated_before, 1);
}

#[test]
fn page_where_on_empty_log_and_zero_limit_are_empty() {
    let empty_log: BoundedEventLog<u32> = BoundedEventLog::new(4);
    let page = empty_log.page_where(None, 10, |_| true);
    assert!(page.entries.is_empty());
    assert_eq!(page.head_sequence, 0);
    assert_eq!(page.truncated_before, 1);

    let log = BoundedEventLog::new(8);
    for value in [1, 2, 3] {
        log.append(value);
    }
    let zero_limit = log.page_where(None, 0, |_| true);
    assert!(zero_limit.entries.is_empty(), "limit = 0 → 空页");
    assert_eq!(zero_limit.head_sequence, 3, "空页仍携带水位元数据");
}

#[test]
fn page_where_stale_cursor_starts_at_retention_boundary() {
    let log = BoundedEventLog::new(2);
    for value in [1, 2, 3, 4] {
        log.append(value);
    }
    // 保留边界 3：旧游标与 None 都从边界起读，页内 truncated_before 表达断档。
    let stale = log.page_where(Some(1), 10, |_| true);
    assert_eq!(stale.entries, vec![3, 4], "旧游标不得重读已淘汰条目");
    assert_eq!(stale.truncated_before, 3);

    let from_none = log.page_where(None, 10, |_| true);
    assert_eq!(from_none.entries, vec![3, 4]);
}

#[test]
fn page_where_applies_predicate_after_start_and_keeps_order() {
    let log = BoundedEventLog::new(8);
    for value in [1, 2, 3, 4, 5, 6] {
        log.append(value);
    }
    let filtered = log.page_where(Some(1), 2, |value| value % 2 == 0);
    assert_eq!(
        filtered.entries,
        vec![2, 4],
        "谓词在游标定位之后生效，按 sequence 升序取前 2 条"
    );

    let none_matched = log.page_where(Some(3), 10, |value| *value > 100);
    assert!(none_matched.entries.is_empty(), "谓词全不命中：空页");
    assert_eq!(none_matched.head_sequence, 6);
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
