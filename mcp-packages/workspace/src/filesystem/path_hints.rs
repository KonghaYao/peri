//! 目标路径不存在时的模型诊断：收集同目录与 cwd 一层子目录的条目，
//! 模糊匹配给出 did-you-mean 候选；无候选时保持原文本（保守语义，不产生噪声建议）。

use crate::fuzzy::fuzzy_filter;
use std::path::{Path, PathBuf};

/// 同目录候选少于该值时，追加 cwd 一层子目录的条目作为兜底候选。
const FALLBACK_CANDIDATE_THRESHOLD: usize = 50;
/// 兜底扫描的 cwd 子目录数量上限。
const MAX_SUBDIRS: usize = 10;

/// 为"目标路径不存在"的 recovery 追加 did-you-mean 候选；无候选时原样返回。
pub(crate) fn with_path_hint(base: impl Into<String>, cwd: &str, target: &Path) -> String {
    let base = base.into();
    match did_you_mean(cwd, target) {
        Some(hint) => format!("{base} {hint}"),
        None => base,
    }
}

fn did_you_mean(cwd: &str, target: &Path) -> Option<String> {
    let target = absolute(cwd, target);
    let target_name = target.file_name()?.to_string_lossy().to_string();
    let candidates = collect_candidates(Path::new(cwd), &target);
    if candidates.is_empty() {
        return None;
    }

    // 先用完整文件名 fuzzy；不命中时回退扩展名 stem，再回退编辑距离
    // （容错拼错如 maiin -> main：Skim 子序列匹配对重复字符不命中，
    // 需要 stem + 编辑距离两级回退）。
    let mut matched = fuzzy_filter(&candidates, &target_name);
    if matched.is_empty() {
        if let Some(stem) = Path::new(&target_name).file_stem() {
            let stem = stem.to_string_lossy();
            if !stem.is_empty() {
                matched = fuzzy_filter(&candidates, &stem);
            }
        }
    }
    if matched.is_empty() {
        matched = edit_distance_filter(&candidates, &target_name);
    }

    let top3: Vec<String> = matched.into_iter().take(3).collect();
    if top3.is_empty() {
        return None;
    }
    Some(format!(
        "Did you mean one of these paths: {}?",
        top3.join(", ")
    ))
}

fn absolute(cwd: &str, target: &Path) -> PathBuf {
    if target.is_absolute() {
        target.to_path_buf()
    } else {
        Path::new(cwd).join(target)
    }
}

/// 收集候选：target 所在目录的兄弟条目 + cwd 一层子目录的条目（兜底）。
fn collect_candidates(cwd: &Path, target: &Path) -> Vec<String> {
    let mut candidates: Vec<String> = Vec::new();

    if let Some(parent) = target.parent() {
        candidates.extend(read_dir_names(parent));
    }

    if candidates.len() < FALLBACK_CANDIDATE_THRESHOLD {
        for sub in read_subdirs(cwd) {
            candidates.extend(read_dir_names(&sub));
        }
    }

    candidates.sort();
    candidates.dedup();
    candidates
}

fn read_dir_names(dir: &Path) -> Vec<String> {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn read_subdirs(dir: &Path) -> Vec<PathBuf> {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|e| e.path())
            .take(MAX_SUBDIRS)
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// 基于归一化编辑距离的候选过滤，阈值 0.3（允许 ~30% 的字符差异）
fn edit_distance_filter(candidates: &[String], query: &str) -> Vec<String> {
    let query_len = query.len().max(1);
    let threshold = 0.3f64;
    let mut scored: Vec<(String, f64)> = candidates
        .iter()
        .filter_map(|c| {
            let dist = levenshtein(c, query) as f64;
            let norm = dist / query_len.max(c.len()) as f64;
            if norm <= threshold {
                Some((c.clone(), norm))
            } else {
                None
            }
        })
        .collect();
    scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().map(|(c, _)| c).collect()
}

/// 标准 Levenshtein 编辑距离
fn levenshtein(a: &str, b: &str) -> usize {
    let a_len = a.chars().count();
    let b_len = b.chars().count();
    if a_len == 0 {
        return b_len;
    }
    if b_len == 0 {
        return a_len;
    }
    let mut prev = (0..=b_len).collect::<Vec<_>>();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.chars().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            cur.push((prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost));
        }
        prev = cur;
    }
    prev[b_len]
}

#[cfg(test)]
#[path = "path_hints_test.rs"]
mod tests;
