use crate::error_suggest::context::ErrorContext;
use crate::error_suggest::format::did_you_mean_summary;
use crate::error_suggest::matcher::fuzzy_filter;
use crate::error_suggest::registry::{ErrorSuggester, Suggestion};
use std::path::{Path, PathBuf};

/// 路径类错误建议器，覆盖 A1-A4
pub struct PathSuggester;

const PATH_TOOLS: &[&str] = &[
    "Read",
    "Edit",
    "Write",
    "Glob",
    "CreateDir",
    "Move",
    "Delete",
];
const ERROR_KEYWORDS: &[&str] = &[
    "not found",
    "no such file",
    "does not exist",
    "not a directory",
    "search path does not exist",
];

impl ErrorSuggester for PathSuggester {
    fn suggest(&self, ctx: &ErrorContext) -> Option<Suggestion> {
        // 1. 工具白名单
        if !PATH_TOOLS.contains(&ctx.tool_name) {
            return None;
        }

        // 2. 关键词识别
        let lower = ctx.error_message.to_lowercase();
        if !ERROR_KEYWORDS.iter().any(|k| lower.contains(k)) {
            return None;
        }

        // 3. 从 input 提取目标路径
        let target = extract_target_path(ctx.tool_name, ctx.tool_input)?;
        let target_path = Path::new(&target);

        // 4. 找候选：同目录 fuzzy + 一层子目录
        let candidates = collect_candidates(ctx.cwd, target_path);

        // 5. fuzzy 过滤（先用完整文件名，再用 stem 提高容错）
        let target_name = target_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or(target.clone());
        let mut matched = fuzzy_filter(&candidates, &target_name);
        if matched.is_empty() {
            // 回退 1：去掉扩展名再 fuzzy
            if let Some(stem) = Path::new(&target_name).file_stem() {
                let stem_str = stem.to_string_lossy().to_string();
                if !stem_str.is_empty() {
                    matched = fuzzy_filter(&candidates, &stem_str);
                }
            }
        }
        if matched.is_empty() {
            // 回退 2：编辑距离（容错拼写错误如 maiin -> main）
            matched = edit_distance_filter(&candidates, &target_name);
        }
        let top3: Vec<String> = matched.into_iter().take(3).collect();

        if top3.is_empty() {
            return None;
        }

        let summary = did_you_mean_summary("path", &top3);
        Some(Suggestion::new(summary))
    }
}

fn extract_target_path(tool_name: &str, input: &serde_json::Value) -> Option<String> {
    let key = match tool_name {
        "Glob" => "path",
        _ => "file_path",
    };
    input
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// 收集候选：target 所在目录 + cwd 一层子目录
fn collect_candidates(cwd: &Path, target: &Path) -> Vec<String> {
    let mut candidates: Vec<String> = Vec::new();

    // 策略 1：target 所在目录的兄弟文件
    if let Some(parent) = target.parent() {
        let dir = if parent.as_os_str().is_empty() {
            // bare filename (e.g. "foo.rs") 的 parent 是空 OsStr，回退到 cwd
            cwd.to_path_buf()
        } else if parent.is_absolute() {
            parent.to_path_buf()
        } else {
            cwd.join(parent)
        };
        for entry in read_dir_names(&dir) {
            candidates.push(entry);
        }
    }

    // 策略 2：cwd 一层子目录的兄弟文件（兜底）
    if candidates.len() < 50 {
        for sub in read_subdirs(cwd) {
            for entry in read_dir_names(&sub) {
                candidates.push(entry);
            }
        }
    }

    // 去重
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
            .take(10)
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
