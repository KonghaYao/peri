use std::{
    hash::{Hash, Hasher},
    sync::{LazyLock, Mutex},
};

use lru::LruCache;
use ratatui::text::Line;
use similar::{ChangeTag, TextDiff};

use crate::theme::Theme;

pub mod renderer;

/// diff 输入超过此大小时截断，不计算
const MAX_DIFF_SIZE_BYTES: usize = 1_000_000; // 1 MB

/// LRU 缓存容量
const DIFF_CACHE_CAPACITY: usize = 64;

/// 全局 diff 计算缓存（单例）
static DIFF_CACHE: LazyLock<Mutex<LruCache<DiffCacheKey, DiffResult>>> = LazyLock::new(|| {
    Mutex::new(LruCache::new(
        std::num::NonZeroUsize::new(DIFF_CACHE_CAPACITY).unwrap(),
    ))
});

/// 缓存 key：对 DiffInput 的内容做哈希，避免存储大字符串
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct DiffCacheKey {
    old_hash: u64,
    new_hash: u64,
    flags: u8, // bit0=is_new_file, bit1=is_deleted_file, bit2=is_binary
}

impl DiffCacheKey {
    fn from_input(input: &DiffInput) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        input.old_content.hash(&mut hasher);
        let old_hash = hasher.finish();

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        input.new_content.hash(&mut hasher);
        let new_hash = hasher.finish();

        let mut flags = 0u8;
        if input.is_new_file {
            flags |= 1;
        }
        if input.is_deleted_file {
            flags |= 2;
        }
        if input.is_binary {
            flags |= 4;
        }

        DiffCacheKey {
            old_hash,
            new_hash,
            flags,
        }
    }
}

/// word diff 变更阈值：变更字符占比超过此值时跳过 word diff（避免噪声）
const WORD_DIFF_CHANGE_THRESHOLD: f64 = 0.4;

// ── 类型定义 ──────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Hash)]
pub struct DiffInput {
    pub file_path: String,
    pub old_content: String,
    pub new_content: String,
    pub is_new_file: bool,
    pub is_deleted_file: bool,
    pub is_binary: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DiffWordType {
    Unchanged,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WordDiff {
    pub segments: Vec<(String, DiffWordType)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DiffLine {
    Context {
        text: String,
        old_line_num: u32,
        new_line_num: u32,
    },
    Add {
        text: String,
        line_num: u32,
        word_diff: Option<WordDiff>,
    },
    Remove {
        text: String,
        line_num: u32,
        word_diff: Option<WordDiff>,
    },
    HunkHeader {
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiffHunk {
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiffResult {
    pub hunks: Vec<DiffHunk>,
    pub is_new_file: bool,
    pub is_deleted_file: bool,
    pub is_binary: bool,
    pub is_truncated: bool,
}

impl DiffResult {
    pub fn is_empty(&self) -> bool {
        self.hunks.is_empty()
    }
}

// ── 核心计算（带缓存）──────────────────────────────────────────

/// 计算 diff，返回结构化的 DiffResult（带 LRU 缓存）
pub fn compute_diff(input: &DiffInput) -> DiffResult {
    let key = DiffCacheKey::from_input(input);

    // 查缓存
    if let Ok(mut cache) = DIFF_CACHE.lock() {
        if let Some(cached) = cache.get(&key) {
            return cached.clone();
        }
    }

    // 未命中，执行计算
    let result = compute_diff_uncached(input);

    // 写入缓存
    if let Ok(mut cache) = DIFF_CACHE.lock() {
        cache.put(key, result.clone());
    }

    result
}

/// 实际计算逻辑（无缓存）
fn compute_diff_uncached(input: &DiffInput) -> DiffResult {
    // 截断检查
    if input.old_content.len() + input.new_content.len() > MAX_DIFF_SIZE_BYTES {
        return DiffResult {
            hunks: Vec::new(),
            is_new_file: input.is_new_file,
            is_deleted_file: input.is_deleted_file,
            is_binary: input.is_binary,
            is_truncated: true,
        };
    }
    // 二进制文件直接返回
    if input.is_binary {
        return DiffResult {
            hunks: Vec::new(),
            is_new_file: input.is_new_file,
            is_deleted_file: input.is_deleted_file,
            is_binary: true,
            is_truncated: false,
        };
    }

    let text_diff = TextDiff::from_lines(&input.old_content, &input.new_content);
    let mut hunks = Vec::new();

    for hunk in text_diff.unified_diff().iter_hunks() {
        let ops = hunk.ops();
        // 从 DiffOp 计算 old/new range（0-indexed）
        let old_start_0 = ops.first().map(|op| op.old_range().start).unwrap_or(0);
        let new_start_0 = ops.first().map(|op| op.new_range().start).unwrap_or(0);
        let old_end = ops.last().map(|op| op.old_range().end).unwrap_or(0);
        let new_end = ops.last().map(|op| op.new_range().end).unwrap_or(0);
        let old_start = old_start_0 as u32 + 1; // 转为 1-indexed
        let old_count = (old_end - old_start_0) as u32;
        let new_start = new_start_0 as u32 + 1;
        let new_count = (new_end - new_start_0) as u32;

        // 构造 hunk header 文本（与 similar 格式对齐）
        let header_text = hunk.header().to_string();

        let mut lines = vec![DiffLine::HunkHeader { text: header_text }];

        let mut old_line = old_start;
        let mut new_line = new_start;

        for change in hunk.iter_changes() {
            let text = change.value().to_string();
            match change.tag() {
                ChangeTag::Equal => {
                    lines.push(DiffLine::Context {
                        text,
                        old_line_num: old_line,
                        new_line_num: new_line,
                    });
                    old_line += 1;
                    new_line += 1;
                }
                ChangeTag::Delete => {
                    lines.push(DiffLine::Remove {
                        text,
                        line_num: old_line,
                        word_diff: None,
                    });
                    old_line += 1;
                }
                ChangeTag::Insert => {
                    lines.push(DiffLine::Add {
                        text,
                        line_num: new_line,
                        word_diff: None,
                    });
                    new_line += 1;
                }
            }
        }

        // 填充 word diff
        fill_word_diffs(&mut lines);

        hunks.push(DiffHunk {
            old_start,
            old_lines: old_count,
            new_start,
            new_lines: new_count,
            lines,
        });
    }

    DiffResult {
        hunks,
        is_new_file: input.is_new_file,
        is_deleted_file: input.is_deleted_file,
        is_binary: input.is_binary,
        is_truncated: false,
    }
}

/// 为连续的 Remove+Add 行对填充 word diff
fn fill_word_diffs(lines: &mut [DiffLine]) {
    // 收集需要做 word diff 的 (remove_idx, add_idx) 对
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if matches!(lines[i], DiffLine::Remove { .. }) {
            let remove_idx = i;
            let mut j = i + 1;
            // 收集连续的 Remove 行
            while j < lines.len() && matches!(lines[j], DiffLine::Remove { .. }) {
                j += 1;
            }
            // 检查后面是否有等量的 Add 行
            let remove_count = j - remove_idx;
            if j + remove_count <= lines.len() {
                let all_add =
                    (j..j + remove_count).all(|k| matches!(lines[k], DiffLine::Add { .. }));
                if all_add {
                    for k in 0..remove_count {
                        pairs.push((remove_idx + k, j + k));
                    }
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }

    // 计算并设置 word diff
    for (remove_idx, add_idx) in pairs {
        let (remove_text, add_text) = {
            let remove_line = &lines[remove_idx];
            let add_line = &lines[add_idx];
            let remove_text = match remove_line {
                DiffLine::Remove { text, .. } => text.clone(),
                _ => unreachable!(),
            };
            let add_text = match add_line {
                DiffLine::Add { text, .. } => text.clone(),
                _ => unreachable!(),
            };
            (remove_text, add_text)
        };

        // 计算变更比例
        let total_chars = remove_text.chars().count() + add_text.chars().count();
        if total_chars == 0 {
            continue;
        }

        // 用 word diff 计算变更量
        let wd = compute_word_diff(&remove_text, &add_text);
        let changed_chars: usize = wd
            .segments
            .iter()
            .filter(|(_, t)| !matches!(t, DiffWordType::Unchanged))
            .map(|(s, _)| s.chars().count())
            .sum();

        let change_ratio = changed_chars as f64 / total_chars as f64;
        if change_ratio > WORD_DIFF_CHANGE_THRESHOLD {
            continue; // 变更太多，跳过 word diff 避免噪声
        }

        // 设置 word diff
        if let DiffLine::Remove { word_diff, .. } = &mut lines[remove_idx] {
            *word_diff = Some(wd.clone());
        }
        if let DiffLine::Add { word_diff, .. } = &mut lines[add_idx] {
            *word_diff = Some(wd);
        }
    }
}

/// 计算行内 word diff
pub fn compute_word_diff(old_line: &str, new_line: &str) -> WordDiff {
    let diff = TextDiff::from_words(old_line, new_line);
    let mut segments = Vec::new();
    for change in diff.iter_all_changes() {
        let word_type = match change.tag() {
            ChangeTag::Equal => DiffWordType::Unchanged,
            ChangeTag::Delete => DiffWordType::Removed,
            ChangeTag::Insert => DiffWordType::Added,
        };
        segments.push((change.value().to_string(), word_type));
    }
    WordDiff { segments }
}

/// 渲染 diff 为 ratatui Line 列表
pub fn render_diff(input: &DiffInput, width: usize, theme: &dyn Theme) -> Vec<Line<'static>> {
    renderer::render_diff_impl(input, width, theme)
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
