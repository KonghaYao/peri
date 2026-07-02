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
//! `MAX_HISTORY = 100`——超出后从头部（最旧）丢弃。

use crate::kit::atoms::{INPUT_HISTORY, INPUT_HISTORY_INDEX};

/// 最大保留历史条数。
const MAX_HISTORY: usize = 100;

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
}

/// 向旧方向浏览一步。返回该位置的历史文本（如有）。
///
/// 空历史直接返回 None；已在最旧位置则维持并返回该位置文本。
pub fn history_up() -> Option<String> {
    let history_atom = INPUT_HISTORY.state();
    let index_atom = INPUT_HISTORY_INDEX.state();

    let history = history_atom.read().clone();
    if history.is_empty() {
        return None;
    }

    let new_idx = match *index_atom.read() {
        None => history.len() - 1, // 从栈顶（最新）开始
        Some(0) => 0,              // 已在最旧
        Some(i) => i - 1,
    };
    *index_atom.write() = Some(new_idx);
    history.get(new_idx).cloned()
}

/// 向新方向浏览一步。返回历史文本，或 None（已回到编辑状态）。
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
        None
    } else {
        *index_atom.write() = Some(new_idx);
        history.get(new_idx).cloned()
    }
}

/// 清空浏览指针，回到编辑新文本状态。提交成功后必须调用。
pub fn reset_history_cursor() {
    *INPUT_HISTORY_INDEX.state().write() = None;
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

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::collections::VecDeque;

    fn setup() {
        crate::kit::atoms::init_atoms();
        *INPUT_HISTORY.state().write() = VecDeque::new();
        *INPUT_HISTORY_INDEX.state().write() = None;
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
        assert_eq!(history_up().as_deref(), Some("third"));
        // 第二次 Up → second
        assert_eq!(history_up().as_deref(), Some("second"));
        // 第三次 Up → first
        assert_eq!(history_up().as_deref(), Some("first"));
        // 第四次 Up → 已在最旧，保持 first
        assert_eq!(history_up().as_deref(), Some("first"));
    }

    #[test]
    #[serial]
    fn test_history_down_returns_none_at_bottom() {
        setup();
        push_history("a");
        push_history("b");

        history_up(); // → b
        history_up(); // → a
        assert_eq!(history_down().as_deref(), Some("b"));
        assert_eq!(history_down(), None); // 超过最新 → None
        assert_eq!(current_index(), None);
    }

    #[test]
    #[serial]
    fn test_reset_history_cursor() {
        setup();
        push_history("x");
        history_up();
        assert!(current_index().is_some());
        reset_history_cursor();
        assert!(current_index().is_none());
    }

    #[test]
    #[serial]
    fn test_history_up_empty_stack() {
        setup();
        assert_eq!(history_up(), None);
        assert_eq!(history_down(), None);
    }

    #[test]
    #[serial]
    fn test_max_history_capacity() {
        setup();
        for i in 0..150 {
            push_history(&format!("cmd-{}", i));
        }
        assert_eq!(history_len(), MAX_HISTORY);
        // 最旧的 cmd-0~49 应被丢弃，cmd-50 是栈底
        let stored: VecDeque<String> = INPUT_HISTORY.state().read().clone();
        assert_eq!(stored.front().map(String::as_str), Some("cmd-50"));
        assert_eq!(stored.back().map(String::as_str), Some("cmd-149"));
    }
}
