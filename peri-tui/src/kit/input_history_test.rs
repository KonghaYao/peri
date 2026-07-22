//! Tests for input_history

use super::HISTORY_DRAFT as DRAFT;
use super::*;
use serial_test::serial;
use std::collections::VecDeque;

fn setup() {
    crate::kit::atoms::init_atoms();
    *INPUT_HISTORY.state().write() = VecDeque::new();
    *INPUT_HISTORY_INDEX.state().write() = None;
    *HISTORY_DRAFT.state().write() = None;
}

#[test]
#[serial]
fn test_push_history_grows_stack() {
    setup();
    push_history("hello");
    push_history("world");
    assert_eq!(history_len(), 2);
}

#[test]
#[serial]
fn test_push_history_ignores_empty() {
    setup();
    push_history("   ");
    push_history("");
    assert_eq!(history_len(), 0);
}

#[test]
#[serial]
fn test_push_history_dedup_consecutive() {
    setup();
    push_history("hello");
    push_history("hello"); // 重复不入栈
    assert_eq!(history_len(), 1);
}

#[test]
#[serial]
fn test_push_history_trims_whitespace() {
    setup();
    push_history("  hello  ");
    assert_eq!(history_len(), 1);
    let stored: VecDeque<String> = INPUT_HISTORY.state().read().clone();
    assert_eq!(stored[0], "hello");
}

#[test]
#[serial]
fn test_history_up_navigation() {
    setup();
    push_history("first");
    push_history("second");
    push_history("third");

    // 第一次 Up → 最新（third）
    assert_eq!(history_up(None).as_deref(), Some("third"));
    // 第二次 Up → second
    assert_eq!(history_up(None).as_deref(), Some("second"));
    // 第三次 Up → first
    assert_eq!(history_up(None).as_deref(), Some("first"));
    // 第四次 Up → 已在最旧，保持 first
    assert_eq!(history_up(None).as_deref(), Some("first"));
}

#[test]
#[serial]
fn test_history_down_restores_empty_draft_at_bottom() {
    setup();
    push_history("a");
    push_history("b");

    history_up(Some("")); // → b，并保存空草稿
    history_up(None); // → a
    assert_eq!(history_down().as_deref(), Some("b"));
    assert_eq!(history_down().as_deref(), Some(""));
    assert_eq!(current_index(), None);
}

#[test]
#[serial]
fn test_reset_history_cursor() {
    setup();
    push_history("x");
    history_up(None);
    assert!(current_index().is_some());
    reset_history_cursor();
    assert!(current_index().is_none());
}

#[test]
#[serial]
fn test_history_up_empty_stack() {
    setup();
    assert_eq!(history_up(None), None);
    assert_eq!(history_down(), None);
}

#[test]
#[serial]
fn test_max_history_capacity() {
    setup();
    for i in 0..1050 {
        push_history(&format!("cmd-{}", i));
    }
    assert_eq!(history_len(), MAX_HISTORY);
    let stored: VecDeque<String> = INPUT_HISTORY.state().read().clone();
    assert_eq!(stored.front().map(String::as_str), Some("cmd-50"));
    assert_eq!(stored.back().map(String::as_str), Some("cmd-1049"));
}

#[test]
#[serial]
fn test_draft_saved_on_history_entry_and_restored() {
    setup();
    push_history("old");
    assert_eq!(history_up(Some("current draft")).as_deref(), Some("old"));
    assert_eq!(DRAFT.state().read().as_deref(), Some("current draft"));
    assert_eq!(history_down(), Some("current draft".to_string()));
    assert!(DRAFT.state().read().is_none());
}

#[test]
#[serial]
fn test_draft_cleared_on_reset() {
    setup();
    push_history("x");
    history_up(Some("draft"));
    assert!(DRAFT.state().read().is_some());
    reset_history_cursor();
    assert!(DRAFT.state().read().is_none());
}

#[test]
#[serial]
fn test_empty_draft_is_preserved_for_restore() {
    setup();
    push_history("old");
    history_up(Some(""));
    // 空草稿也要保存，否则 Down 回到底部时无法清空旧历史项。
    assert_eq!(DRAFT.state().read().as_deref(), Some(""));
}

/// L11：`save_history` 在 `std::thread::spawn` 中异步写盘，push_history 立即返回。
///
/// 通过临时改 `HOME` 指向 tempdir，push 一条 unique marker，等后台线程 rename 完成，
/// 读 `~/.peri/input-history.json` 断言内容已落盘。`#[serial]` 防止并发污染其他测试。
///
/// 注意：edition 2024 中 `std::env::set_var`/`remove_var` 为 unsafe，
/// 需在 `unsafe` 块中调用。`#[serial]` 保证这些 mutation 与其他测试互斥。
#[test]
#[serial]
fn test_save_history_does_not_block_and_persists_async() {
    setup();

    // 临时 HOME：创建 tempdir 并 set_var。
    let tempdir = std::env::temp_dir().join(format!(
        "peri-input-history-async-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&tempdir).unwrap();
    let prev_home = std::env::var("HOME").ok();
    // SAFETY: #[serial] 保证与其他使用 HOME 的测试互斥；prev_home 用于后续恢复。
    unsafe {
        std::env::set_var("HOME", &tempdir);
    }

    // 用唯一 marker 避免与其他测试串扰。
    let marker = "async-persist-marker-7E3A";
    push_history(marker);

    // spawn 线程 rename 通常 <10ms，给一个宽松的等待窗口。
    let history_path = tempdir.join(".peri").join("input-history.json");
    let mut waited_ms = 0u64;
    let content = loop {
        if let Ok(c) = std::fs::read_to_string(&history_path) {
            break c;
        }
        if waited_ms >= 1000 {
            // 恢复 HOME 再失败。
            unsafe {
                if let Some(ref h) = prev_home {
                    std::env::set_var("HOME", h);
                } else {
                    std::env::remove_var("HOME");
                }
            }
            panic!(
                "input_history async persist 未在 1s 内落盘: {:?}",
                history_path
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        waited_ms += 10;
    };

    // 还原 HOME。
    unsafe {
        if let Some(h) = prev_home {
            std::env::set_var("HOME", h);
        } else {
            std::env::remove_var("HOME");
        }
    }

    assert!(
        content.contains(marker),
        "持久化文件应包含 marker {:?}，实际内容：{}",
        marker,
        content
    );

    // 清理 tempdir（best-effort）。
    let _ = std::fs::remove_dir_all(&tempdir);
}
