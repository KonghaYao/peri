//! Edit 工具：逐字复刻 `peri-middlewares/src/tools/filesystem/edit.rs`。
//!
//! 契约要点：
//! - 参数：`file_path`/`old_string`/`new_string`（必需）、`replace_all`（默认 false）；
//! - `old_string` 为空 → `Error: old_string cannot be empty`；
//! - 读前写：先读目标（不存在 → `Error: File not found`；非 UTF-8 → `Edit failed while reading the file.`）；
//! - 唯一性：`replace_all=false` 且出现多次 → 报错并列出前 10 处行号；
//! - 未命中 → `build_not_found_hint` 的模糊定位提示；
//! - 提交：与 Write 共用哨兵 + tmp + rename 事务（同一 per-target 锁）；
//! - 摘要含行数变化（`Added/Removed/Replaced N line(s)`）；行数不变时仍报被替换行数
//!   （TUI 依赖该计数展示 `· +N · -N`）。

use crate::capability::Parents;

use super::transaction::{commit, read_pre, CommitError, EDIT_IO_ERROR, SENTINEL_REJECTION};
use super::{FsContext, FsFailure, FsOutcome};

/// 为 `old_string not found` 构建模糊匹配提示（源实现 `build_not_found_hint`）。
pub(super) fn build_not_found_hint(content: &str, old_string: &str) -> String {
    const MAX_FUZZY_LEN: usize = 5000;
    if old_string.len() > MAX_FUZZY_LEN {
        return "Please Read this file to get the latest content before retrying.".to_string();
    }

    // 策略 1：前缀匹配（取 old_string 前 5 行）
    let prefix_lines: Vec<&str> = old_string.lines().take(5).collect();
    let prefix: String = prefix_lines.join("\n");
    if !prefix.is_empty() {
        if let Some(byte_offset) = content.find(&prefix) {
            let line_start = content[..byte_offset].lines().count() + 1;
            let line_end = line_start + prefix_lines.len() - 1;
            return format!(
                "old_string's first {} lines matched lines {}-{}, but the full string did not match. \
                 The file may have been modified. Please Read this file to get the latest content before retrying.",
                prefix_lines.len(),
                line_start,
                line_end
            );
        }
    }

    // 策略 2：滑窗近似（trim 后逐行相等计数）
    let old_lines: Vec<&str> = old_string.lines().collect();
    let file_lines: Vec<&str> = content.lines().collect();
    let window_len = old_lines.len();

    if window_len > 0 && window_len <= file_lines.len() {
        let mut best_pos = 0usize;
        let mut best_common = 0usize;
        for start in 0..=file_lines.len().saturating_sub(window_len) {
            let window = &file_lines[start..start + window_len];
            let common = window
                .iter()
                .zip(old_lines.iter())
                .filter(|(a, b)| a.trim() == b.trim())
                .count();
            if common > best_common {
                best_common = common;
                best_pos = start;
            }
        }
        if best_common > 0 {
            let line_start = best_pos + 1;
            let line_end = best_pos + window_len;
            let diff_count = window_len - best_common;
            return format!(
                "Closest match at lines {}-{} ({} of {} lines differ). \
                 Please Read this file to get the latest content before retrying.",
                line_start, line_end, diff_count, window_len
            );
        }
    }

    "Please Read this file to get the latest content before retrying.".to_string()
}

/// 执行 Edit。
pub(super) fn execute(ctx: &FsContext<'_>) -> Result<FsOutcome, FsFailure> {
    let _file_path = ctx.arguments["file_path"].as_str().ok_or_else(|| {
        FsFailure::text("The 'file_path' parameter is required for the Edit tool.")
    })?;
    let old_string = ctx.arguments["old_string"].as_str().ok_or_else(|| {
        FsFailure::text("The 'old_string' parameter is required for the Edit tool.")
    })?;
    let new_string = ctx.arguments["new_string"].as_str().ok_or_else(|| {
        FsFailure::text("The 'new_string' parameter is required for the Edit tool.")
    })?;
    let replace_all = ctx.arguments["replace_all"].as_bool().unwrap_or(false);

    if old_string.is_empty() {
        return Err(FsFailure::text("Error: old_string cannot be empty"));
    }
    // 编辑授权根本身：源实现在读取阶段就会因 EISDIR 失败。
    if ctx.requested().is_root() {
        return Err(FsFailure::text("Edit failed while reading the file."));
    }

    let runtime = ctx.runtime();
    let key = ctx.target_key_path();
    runtime.locks().with_lock(&key, || {
        let pre = match read_pre(runtime.root(), ctx.requested()) {
            Ok(Some(content)) => content,
            Ok(None) => return Err(FsFailure::text("Error: File not found")),
            Err(_) => return Err(FsFailure::text("Edit failed while reading the file.")),
        };
        let content = match String::from_utf8(pre.clone()) {
            Ok(content) => content,
            Err(_) => return Err(FsFailure::text("Edit failed while reading the file.")),
        };

        let old_lines = old_string.lines().count();
        let new_lines = new_string.lines().count();
        let line_diff = new_lines as i64 - old_lines as i64;
        let display = ctx.display_path();
        let relative = ctx.relative_path();

        let diff_desc = match line_diff.cmp(&0) {
            std::cmp::Ordering::Greater => format!(
                "Added {} line{}",
                line_diff,
                if line_diff == 1 { "" } else { "s" }
            ),
            std::cmp::Ordering::Less => format!(
                "Removed {} line{}",
                -line_diff,
                if -line_diff == 1 { "" } else { "s" }
            ),
            std::cmp::Ordering::Equal => format!(
                "Replaced {} line{}",
                old_lines,
                if old_lines == 1 { "" } else { "s" }
            ),
        };

        let target = runtime
            .root()
            .resolve_with(ctx.requested(), Parents::MustExist)
            .map_err(|_| FsFailure::text("Edit failed while reading the file."))?;

        if replace_all {
            if !content.contains(old_string) {
                let hint = build_not_found_hint(&content, old_string);
                return Err(FsFailure::text(format!(
                    "Error: old_string not found in {}\n{hint}",
                    display.display()
                )));
            }
            let new_content = content.replace(old_string, new_string);
            let occurrences = content.matches(old_string).count();
            match commit(&target, &pre, new_content.as_bytes()) {
                Ok(_) => Ok(FsOutcome::text(format!(
                    "{} to {} (replaced {} occurrence{})",
                    diff_desc,
                    relative,
                    occurrences,
                    if occurrences == 1 { "" } else { "s" }
                ))
                .with_extra("path", serde_json::json!(display.to_string_lossy()))
                .with_extra("occurrences", serde_json::json!(occurrences))
                .with_extra("replace_all", serde_json::json!(true))
                .with_extra("line_delta", serde_json::json!(line_diff))),
                Err(CommitError::Sentinel) => Err(FsFailure::text(SENTINEL_REJECTION)),
                Err(CommitError::Io) => Err(FsFailure::text(EDIT_IO_ERROR)),
            }
        } else {
            let occurrences = content.matches(old_string).count();
            if occurrences == 0 {
                let hint = build_not_found_hint(&content, old_string);
                return Err(FsFailure::text(format!(
                    "Error: old_string not found in {}\n{hint}",
                    display.display()
                )));
            }
            if occurrences > 1 {
                let locations: Vec<String> = content
                    .match_indices(old_string)
                    .take(10)
                    .map(|(offset, _)| {
                        let line = content[..offset].lines().count() + 1;
                        let end_line = line + old_string.lines().count().saturating_sub(1);
                        if end_line > line {
                            format!("lines {line}-{end_line}")
                        } else {
                            format!("line {line}")
                        }
                    })
                    .collect();
                let location_text = if occurrences > 10 {
                    format!(
                        "{} ({} total, showing first 10)",
                        locations.join(", "),
                        occurrences
                    )
                } else {
                    locations.join(", ")
                };
                return Err(FsFailure::text(format!(
                    "Error: old_string is not unique in {} (found {} occurrences).\nMatch locations: {location_text}.\nPlease provide more context to make old_string unique, or set replace_all=true.",
                    display.display(),
                    occurrences
                )));
            }
            let new_content = content.replacen(old_string, new_string, 1);
            match commit(&target, &pre, new_content.as_bytes()) {
                Ok(_) => Ok(FsOutcome::text(format!("{diff_desc} to {relative}"))
                    .with_extra("path", serde_json::json!(display.to_string_lossy()))
                    .with_extra("occurrences", serde_json::json!(1))
                    .with_extra("replace_all", serde_json::json!(false))
                    .with_extra("line_delta", serde_json::json!(line_diff))),
                Err(CommitError::Sentinel) => Err(FsFailure::text(SENTINEL_REJECTION)),
                Err(CommitError::Io) => Err(FsFailure::text(EDIT_IO_ERROR)),
            }
        }
    })
}
