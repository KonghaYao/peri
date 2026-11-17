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
//! 双预算：条数 `MAX_HISTORY` 与 UTF-8 载荷 `MAX_HISTORY_BYTES`，任一超限都从
//! 头部（最旧）淘汰；单条超过 `MAX_ENTRY_BYTES` 不入栈（输入照常提交）。
//! 浏览路径只复制选中项；持久化由单个 worker 合并写，提交路径不复制整栈。

use crate::kit::atoms::DRAFT as HISTORY_DRAFT;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::SyncSender;
use std::time::Duration;

/// 最大保留历史条数。
const MAX_HISTORY: usize = 1000;
/// 历史总字节预算（UTF-8 载荷，不含容器/分配器开销）。
const MAX_HISTORY_BYTES: usize = 4 * 1024 * 1024;
/// 单条历史字节上限；更大的输入仍会提交，只是不进入回放历史。
const MAX_ENTRY_BYTES: usize = 64 * 1024;
/// serde_json 的最坏转义倍率：控制字符（含 U+0000）写作 `\u00XX`，1 字节变 6 字节。
const JSON_WORST_CASE_ESCAPE: u64 = 6;
/// 顶层数组的容器开销上界：`[`/`]`，每条两个引号，条目间至多一个逗号。
const JSON_ARRAY_OVERHEAD: u64 = 2 + (MAX_HISTORY as u64) * 3;
/// 启动加载文件的字节上限——超过则整份忽略，避免对遗留超大文件全量 parse。
/// 由 writer 最坏输出推导（载荷 × 6 + 容器开销），保证 `load_history` 一定能
/// 读回 writer 写出的文件；`persist_history` 以同一门限拒绝超限输出兜底。
const MAX_HISTORY_FILE_BYTES: u64 =
    (MAX_HISTORY_BYTES as u64) * JSON_WORST_CASE_ESCAPE + JSON_ARRAY_OVERHEAD;
/// 合并写静默窗口：窗口内到达的提交合并为一次落盘。
const SAVE_DEBOUNCE: Duration = Duration::from_millis(25);

/// 提交后写入历史。空白去重，与栈顶相同则不重复入栈。
pub fn push_history(text: &str) {
    let history_atom = INPUT_HISTORY.state();
    let index_atom = INPUT_HISTORY_INDEX.state();

    let trimmed = text.trim();
    if trimmed.is_empty() {
        *index_atom.write() = None;
        return;
    }
    if trimmed.len() > MAX_ENTRY_BYTES {
        tracing::debug!(
            bytes = trimmed.len(),
            limit = MAX_ENTRY_BYTES,
            "input_history: entry exceeds per-entry budget, not retained"
        );
        *index_atom.write() = None;
        return;
    }

    let changed = {
        let mut history = history_atom.write();
        // 与栈顶相同则跳过（避免连续重复）
        if history.back().map(String::as_str) == Some(trimmed) {
            false
        } else {
            let mut total_bytes = history.iter().map(|entry| entry.len()).sum::<usize>();
            total_bytes += trimmed.len();
            history.push_back(trimmed.to_string());
            while history.len() > MAX_HISTORY || total_bytes > MAX_HISTORY_BYTES {
                let Some(evicted) = history.pop_front() else {
                    break;
                };
                total_bytes = total_bytes.saturating_sub(evicted.len());
            }
            true
        }
    };
    *index_atom.write() = None;
    if changed {
        request_history_save(history_path());
    }
}

/// 向旧方向浏览一步。返回该位置的历史文本（如有）。
///
/// 空历史直接返回 None；已在最旧位置则维持并返回该位置文本。
///
/// 首次进入历史模式时（`current_text` 非空且非纯空白），
/// 自动将 `current_text` 保存为草稿——`history_down()` 回到编辑态时会返回该草稿。
pub fn history_up(current_text: Option<&str>) -> Option<String> {
    // 先取 index 再取 history，保持与 history_down 相同的加锁顺序。
    let current = *INPUT_HISTORY_INDEX.state().read();
    let history_atom = INPUT_HISTORY.state();
    let history = history_atom.read();
    if history.is_empty() {
        return None;
    }

    let new_idx = match current {
        None => {
            // 进入历史模式：保存当前草稿（包括空串），history_down 回到底部时
            // 必须能明确恢复为空输入，而不是把旧历史项留在编辑器里。
            *HISTORY_DRAFT.state().write() = current_text.map(str::to_string);
            history.len() - 1
        }
        Some(0) => 0,
        Some(i) => i - 1,
    };
    *INPUT_HISTORY_INDEX.state().write() = Some(new_idx);
    // 只复制选中项，不复制整栈。
    history.get(new_idx).cloned()
}

/// 向新方向浏览一步。返回历史文本，或 None（已回到编辑状态）。
///
/// 超过最新位置时回到编辑状态，返回 `DRAFT` atom 中保存的草稿文本。
pub fn history_down() -> Option<String> {
    let current = *INPUT_HISTORY_INDEX.state().read();
    let Some(index) = current else {
        return None; // 已经在编辑状态，无需下移
    };

    let history_atom = INPUT_HISTORY.state();
    let history = history_atom.read();
    let new_idx = index + 1;
    if new_idx >= history.len() {
        // 超过最新——回到编辑状态
        drop(history);
        *INPUT_HISTORY_INDEX.state().write() = None;
        // 返回草稿并清除（take 避免复制）
        HISTORY_DRAFT.state().write().take()
    } else {
        let selected = history.get(new_idx).cloned();
        drop(history);
        *INPUT_HISTORY_INDEX.state().write() = Some(new_idx);
        selected
    }
}

/// 清空浏览指针，回到编辑新文本状态。提交成功后必须调用。
pub fn reset_history_cursor() {
    *INPUT_HISTORY_INDEX.state().write() = None;
    let _ = HISTORY_DRAFT.state().write().take();
}

/// ~/.peri/input-history.json
fn history_path() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".peri").join("input-history.json")
}

/// 启动时从磁盘加载历史到 atom。文件不存在、超限或解析失败时静默跳过。
///
/// 先检查文件大小再 parse：遗留的超大文件（旧版本无字节预算）直接忽略，
/// 避免启动时为了裁剪而全量反序列化。
pub fn load_history() {
    let path = history_path();
    let Ok(meta) = std::fs::metadata(&path) else {
        return;
    };
    if meta.len() > MAX_HISTORY_FILE_BYTES {
        tracing::warn!(
            ?path,
            bytes = meta.len(),
            limit = MAX_HISTORY_FILE_BYTES,
            "input_history: file exceeds load budget, ignoring"
        );
        return;
    }
    let Ok(data) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(entries) = serde_json::from_str::<Vec<String>>(&data) else {
        return;
    };
    *INPUT_HISTORY.state().write() = bounded_history(entries);
}

/// 按双预算裁剪一组历史条目；保留顺序，淘汰最旧。
fn bounded_history(entries: impl IntoIterator<Item = String>) -> VecDeque<String> {
    let mut history = VecDeque::new();
    let mut total_bytes = 0usize;
    for entry in entries {
        if entry.trim().is_empty() || entry.len() > MAX_ENTRY_BYTES {
            continue;
        }
        total_bytes += entry.len();
        history.push_back(entry);
        while history.len() > MAX_HISTORY || total_bytes > MAX_HISTORY_BYTES {
            let Some(evicted) = history.pop_front() else {
                break;
            };
            total_bytes = total_bytes.saturating_sub(evicted.len());
        }
    }
    history
}

/// 单 worker 合并写：提交路径只投递目标路径，不复制历史、不新建线程。
static WRITER: OnceLock<Option<SyncSender<PathBuf>>> = OnceLock::new();

/// 请求持久化当前历史。
///
/// 有界通道容量 1：已有待处理请求时本次投递被丢弃——worker 落盘前读取的是
/// atom 的最新值，丢弃的请求内容不会丢失。
fn request_history_save(path: PathBuf) {
    let Some(tx) = WRITER.get_or_init(spawn_writer) else {
        return;
    };
    let _ = tx.try_send(path);
}

fn spawn_writer() -> Option<SyncSender<PathBuf>> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let spawned = std::thread::Builder::new()
        .name("input-history-writer".to_string())
        .spawn(move || run_writer(rx, persist_current_history));
    match spawned {
        Ok(_) => Some(tx),
        Err(error) => {
            tracing::warn!(error = %error, "input_history: writer spawn failed");
            None
        }
    }
}

/// 合并写循环：收到首个请求后等待静默窗口，期间到达的请求合并为一次落盘。
///
/// 路径取批次里最新的一条（生产环境只有测试会切换 HOME）；实际内容在落盘时
/// 从 atom 读取，因此连续提交只会触发受控次数的磁盘写入。
fn run_writer(rx: Receiver<PathBuf>, mut persist: impl FnMut(&Path)) {
    while let Ok(mut path) = rx.recv() {
        // 静默窗口内到达的请求并入同一批；超时或通道关闭即落盘。
        while let Ok(next) = rx.recv_timeout(SAVE_DEBOUNCE) {
            path = next;
        }
        persist(&path);
    }
}

/// 读取当前历史并原子落盘。序列化在锁内完成（不复制整栈），磁盘 I/O 在锁外。
fn persist_current_history(path: &Path) {
    let json = {
        let history_atom = INPUT_HISTORY.state();
        let history = history_atom.read();
        match serde_json::to_string(&*history) {
            Ok(json) => json,
            Err(error) => {
                tracing::warn!(error = %error, "input_history: serialize failed");
                return;
            }
        }
    };
    persist_history(path, &json);
}

/// 实际执行磁盘持久化——错误以 `tracing::warn!` 记录，不向上传播。
///
/// 序列化输出超过加载门限时拒绝写入（保留旧文件）：否则会留下 `load_history`
/// 整份忽略的文件，形成「保存成功但重启丢失」的中间态。
fn persist_history(path: &Path, json: &str) {
    if json.len() as u64 > MAX_HISTORY_FILE_BYTES {
        tracing::error!(
            ?path,
            bytes = json.len(),
            limit = MAX_HISTORY_FILE_BYTES,
            "input_history: serialized history exceeds file budget, refusing to write"
        );
        return;
    }
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(?path, error = %e, "input_history: create_dir_all failed");
        return;
    }
    let tmp = path.with_extension("tmp");
    if let Err(e) = std::fs::write(&tmp, json) {
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

use crate::kit::atoms::INPUT_HISTORY;

use crate::kit::atoms::INPUT_HISTORY_INDEX;
