//! HistoryCursor：上下移动、退出与重置的状态转换。

use super::{HistoryCursor, HistoryDown};
use std::collections::VecDeque;

fn history(entries: &[&str]) -> VecDeque<String> {
    entries.iter().map(|s| (*s).to_string()).collect()
}

/// 空历史：上下移动都无效果，且不保存草稿。
#[test]
fn empty_history_is_inert() {
    let mut cursor = HistoryCursor::default();
    let empty = VecDeque::new();
    assert_eq!(cursor.up(&empty, Some("draft")), None);
    assert_eq!(cursor.index(), None);
    assert_eq!(cursor.draft(), None, "空历史不进入历史模式");
    assert_eq!(cursor.down(&empty), HistoryDown::Idle);
}

/// 从最新到最旧，端点上停住；向下逐项返回并最终恢复草稿。
#[test]
fn up_and_down_walk_the_history() {
    let mut cursor = HistoryCursor::default();
    let entries = history(&["first", "second", "third"]);
    assert_eq!(cursor.up(&entries, Some("draft")), Some("third".into()));
    assert_eq!(cursor.up(&entries, None), Some("second".into()));
    assert_eq!(cursor.up(&entries, None), Some("first".into()));
    assert_eq!(
        cursor.up(&entries, None),
        Some("first".into()),
        "最旧端点停住"
    );

    assert_eq!(cursor.down(&entries), HistoryDown::Entry("second".into()));
    assert_eq!(cursor.down(&entries), HistoryDown::Entry("third".into()));
    assert_eq!(
        cursor.down(&entries),
        HistoryDown::Draft(Some("draft".into())),
        "越过最新恢复进入前的草稿"
    );
    assert_eq!(cursor.index(), None);
    assert_eq!(cursor.down(&entries), HistoryDown::Idle, "编辑态不再下移");
}

/// 显式空串草稿与「无草稿」必须可区分。
#[test]
fn empty_draft_is_distinct_from_missing_draft() {
    let entries = history(&["old"]);

    let mut cursor = HistoryCursor::default();
    let _ = cursor.up(&entries, Some(""));
    assert_eq!(cursor.draft(), Some(""));
    assert_eq!(
        cursor.down(&entries),
        HistoryDown::Draft(Some(String::new())),
        "显式空串要交回 Some(\"\")，调用方才能清空编辑器"
    );

    let mut cursor = HistoryCursor::default();
    let _ = cursor.up(&entries, None);
    assert_eq!(cursor.draft(), None);
    assert_eq!(
        cursor.down(&entries),
        HistoryDown::Draft(None),
        "无草稿不能伪造成空串"
    );
}

/// 重复进入历史模式只保存一次草稿，不覆盖为历史项。
#[test]
fn draft_is_captured_once_per_browse_session() {
    let mut cursor = HistoryCursor::default();
    let entries = history(&["a", "b"]);
    let _ = cursor.up(&entries, Some("typed"));
    let _ = cursor.up(&entries, None);
    let _ = cursor.up(&entries, Some("ignored"));
    assert_eq!(cursor.draft(), Some("typed"));
}

/// 退出/重置：清空位置与草稿。
#[test]
fn reset_clears_position_and_draft() {
    let mut cursor = HistoryCursor::default();
    let entries = history(&["x"]);
    let _ = cursor.up(&entries, Some("draft"));
    cursor.reset();
    assert_eq!(cursor.index(), None);
    assert_eq!(cursor.draft(), None);
    assert_eq!(cursor.down(&entries), HistoryDown::Idle);
}

/// 淘汰最旧条目后重新浏览：从当前最新项开始，不沿用旧位置。
#[test]
fn eviction_then_browse_starts_at_newest() {
    let mut cursor = HistoryCursor::default();
    let mut entries = history(&["old", "new"]);
    let _ = cursor.up(&entries, Some("draft"));
    assert_eq!(cursor.draft(), Some("draft"));

    // 宿主提交新输入后重置游标并淘汰最旧条目。
    cursor.reset();
    entries.pop_front();
    entries.push_back("newest".into());
    assert_eq!(cursor.up(&entries, None), Some("newest".into()));
    assert_eq!(cursor.up(&entries, None), Some("new".into()));
}
