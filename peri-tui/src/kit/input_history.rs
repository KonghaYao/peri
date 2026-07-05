//! 输入历史栈辅助——`INPUT_HISTORY` atom 的纯函数操作集。
//!
//! 历史语义（与 shell 一致）：
//! - `push_history(text)`：提交后追加。空文本/纯空白不入栈；与栈顶相同则去重。
//! - `history_up(current_text)`：从 None 或当前浏览位置向旧方向移动一步，
//!   返回该位置的历史文本。已在最旧位置则不变。
//! - `history_down()`：向新方向移动。到达最新之后清空指针（返回 None），
//!   调用方应恢复用户编辑中的草稿。
//! - `reset_history_cursor()`：清空浏览指针，回到"编辑新文本"状态。
//!
//! ## 容量限制
//!
//! `MAX_HISTORY = 1000`——超出后从头部（最旧）丢弃。

use crate::kit::atoms::{DRAFT as HISTORY_DRAFT, INPUT_HISTORY, INPUT_HISTORY_INDEX};
use std::collections::VecDeque;
use std::path::PathBuf;

/// 最大保留历史条数。
const MAX_HISTORY: usize = 1000;

/// 提交后写入历史。空白去重，与栈顶相同则不重复入栈。
pub fn push_history(text: &str) {
    let history_atom = INPUT_HISTORY.state();
    let index_atom = INPUT_HISTORY_INDEX.state();

    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }

    let mut history = history_atom.read().clone();
    // 与栈顶相同则跳过（避免连续重复）
    if history.back().map(String::as_str) == Some(trimmed) {
        // 仍然重置浏览指针
        *index_atom.write() = None;
        return;
    }
    history.push_back(trimmed.to_string());
    while history.len() > MAX_HISTORY {
        history.pop_front();
    }
    *history_atom.write() = history;
    *index_atom.write() = None;
    save_history();
}

/// 向旧方向浏览一步。返回该位置的历史文本（如有）。
///
/// 空历史直接返回 None；已在最旧位置则维持并返回该位置文本。
///
/// 首次进入历史模式时（`current_text` 非空且非纯空白），
/// 自动将 `current_text` 保存为草稿——`history_down()` 回到编辑态时会返回该草稿。
pub fn history_up(current_text: Option<&str>) -> Option<String> {
    let history_atom = INPUT_HISTORY.state();
    let index_atom = INPUT_HISTORY_INDEX.state();

    let history = history_atom.read().clone();
    if history.is_empty() {
        return None;
    }

    let new_idx = match *index_atom.read() {
        None => {
            // 进入历史模式：保存当前草稿（包括空串），history_down 回到底部时
            // 必须能明确恢复为空输入，而不是把旧历史项留在编辑器里。
            *HISTORY_DRAFT.state().write() = current_text.map(|s| s.to_string());
            history.len() - 1
        }
        Some(0) => 0,
        Some(i) => i - 1,
    };
    *index_atom.write() = Some(new_idx);
    history.get(new_idx).cloned()
}

/// 向新方向浏览一步。返回历史文本，或 None（已回到编辑状态）。
///
/// 超过最新位置时回到编辑状态，返回 `DRAFT` atom 中保存的草稿文本。
pub fn history_down() -> Option<String> {
    let history_atom = INPUT_HISTORY.state();
    let index_atom = INPUT_HISTORY_INDEX.state();

    let history = history_atom.read().clone();
    let current = *index_atom.read();
    let new_idx = match current {
        None => return None, // 已经在编辑状态，无需下移
        Some(i) => i + 1,
    };

    if new_idx >= history.len() {
        // 超过最新——回到编辑状态
        *index_atom.write() = None;
        // 返回草稿并清除
        let draft = HISTORY_DRAFT.state().read().clone();
        *HISTORY_DRAFT.state().write() = None;
        draft
    } else {
        *index_atom.write() = Some(new_idx);
        history.get(new_idx).cloned()
    }
}

/// 清空浏览指针，回到编辑新文本状态。提交成功后必须调用。
pub fn reset_history_cursor() {
    *INPUT_HISTORY_INDEX.state().write() = None;
    *HISTORY_DRAFT.state().write() = None;
}

/// 当前历史浏览位置（如有）。
#[allow(dead_code)]
pub fn current_index() -> Option<usize> {
    *INPUT_HISTORY_INDEX.state().read()
}

/// 历史条目数（测试 / 状态显示用）。
#[allow(dead_code)]
pub fn history_len() -> usize {
    INPUT_HISTORY.state().read().len()
}

/// ~/.peri/input-history.json
fn history_path() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".peri").join("input-history.json")
}

/// 启动时从磁盘加载历史到 atom。文件不存在或解析失败时静默跳过。
pub fn load_history() {
    let path = history_path();
    let Ok(data) = std::fs::read_to_string(&path) else {
        return;
    };
    let entries: Vec<String> = match serde_json::from_str(&data) {
        Ok(v) => v,
        Err(_) => return,
    };
    let mut history = entries
        .into_iter()
        .filter(|s| !s.trim().is_empty())
        .collect::<VecDeque<String>>();
    while history.len() > MAX_HISTORY {
        history.pop_front();
    }
    *INPUT_HISTORY.state().write() = history;
}

/// 原子写入历史到磁盘（先写 .tmp 再 rename）。
fn save_history() {
    let path = history_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("tmp");
    let history_handle = INPUT_HISTORY.state();
    let history = history_handle.read();
    let entries: Vec<&String> = history.iter().collect();
    if let Ok(json) = serde_json::to_string(&entries) {
        if std::fs::write(&tmp, &json).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

#[cfg(test)]
mod tests {
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
}
