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

/// 后台原子写入历史到磁盘（先写 .tmp 再 rename）。
///
/// L11：磁盘 I/O（create_dir_all / write / rename）放到独立 `std::thread::spawn`
/// 执行，避免 Enter 提交后被阻塞。spawn 前先 clone history 快照，
/// 确保 worker 线程不再访问 atom（避免 `parking_lot::RwLockReadGuard` 跨线程问题）。
/// 连续多次 `push_history` 可能产生多个并发写入线程，依靠 `rename` 的原子替换保证
/// 最终一致——最坏丢一条记录，不影响内存历史正确性。
fn save_history() {
    let path = history_path();
    let history = INPUT_HISTORY.state().read().clone();
    std::thread::spawn(move || {
        persist_history(&path, &history);
    });
}

/// 实际执行磁盘持久化——错误以 `tracing::warn!` 记录，不向上传播。
fn persist_history(path: &std::path::Path, history: &VecDeque<String>) {
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(?path, error = %e, "input_history: create_dir_all failed");
        return;
    }
    let tmp = path.with_extension("tmp");
    let entries: Vec<&String> = history.iter().collect();
    let Ok(json) = serde_json::to_string(&entries) else {
        return;
    };
    if let Err(e) = std::fs::write(&tmp, &json) {
        tracing::warn!(?tmp, error = %e, "input_history: write tmp failed");
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        tracing::warn!(error = %e, "input_history: rename failed");
    }
}

#[cfg(test)]
#[path = "input_history_test.rs"]
mod tests;
