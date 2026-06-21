use peri_agent::tools::BaseTool;
use serde_json::Value;

use super::resolve_path;

const EDIT_FILE_DESCRIPTION: &str = r#"Performs exact string replacements in files.

Usage:
- You must use your Read tool at least once in the conversation before editing. This tool will fail if you attempt an edit without reading the file
- When editing text from Read tool output, ensure you preserve the exact indentation (tabs/spaces) as it appears AFTER the line number prefix
- ALWAYS prefer editing existing files in the codebase. DO NOT create new files unless explicitly required
- The file_path parameter must be an absolute path, not a relative path
- The old_string parameter must match exactly, including all whitespace and indentation
- The edit will FAIL if old_string is not unique in the file. Either provide a larger string with more surrounding context to make it unique or use replace_all to change every instance of old_string
- Use replace_all for replacing and renaming strings across the file

Error handling:
- old_string not found: returns an error indicating the string does not exist in the file
- old_string not unique: returns an error with the count of occurrences, suggesting more context or replace_all
- old_string is empty: returns an error rejecting the operation
- File not found: returns an error indicating the path does not exist"#;

/// Edit tool (replace) - 与 TypeScript replace_tool 对齐
pub struct EditFileTool {
    pub cwd: String,
}

impl EditFileTool {
    pub fn new(cwd: impl Into<String>) -> Self {
        Self { cwd: cwd.into() }
    }
}

/// 为 old_string not found 错误构建模糊匹配提示。
///
/// 策略 1：取 old_string 前 5 行做前缀匹配，报告匹配到的行号范围。
/// 策略 2：前缀匹配失败时，用滑动窗口找最接近的区域，报告差异行数。
/// old_string > 5000 字符时跳过，仅返回建议 Read 提示。
fn build_not_found_hint(content: &str, old_string: &str) -> String {
    const MAX_FUZZY_LEN: usize = 5000;
    if old_string.len() > MAX_FUZZY_LEN {
        return "Please Read this file to get the latest content before retrying.".to_string();
    }

    // 策略 1：前缀匹配
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

    // 策略 2：行数近似匹配（回退）
    let old_lines: Vec<&str> = old_string.lines().collect();
    let file_lines: Vec<&str> = content.lines().collect();
    let window_len = old_lines.len();

    if window_len > 0 && window_len <= file_lines.len() {
        let mut best_pos = 0;
        let mut best_common = 0;

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

#[async_trait::async_trait]
impl BaseTool for EditFileTool {
    fn name(&self) -> &str {
        "Edit"
    }

    fn description(&self) -> &str {
        EDIT_FILE_DESCRIPTION
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The absolute path to the file to modify"
                },
                "old_string": {
                    "type": "string",
                    "description": "The text to replace. Must match EXACTLY including all whitespace, indentation, and newlines. The edit will fail if old_string is not unique in the file unless replace_all is true"
                },
                "new_string": {
                    "type": "string",
                    "description": "The text to replace it with (must be different from old_string)"
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "If true, replace all occurrences of old_string. If false (default), replace only the first occurrence. Use this to rename variables or update repeated patterns across the file"
                }
            },
            "required": ["file_path", "old_string", "new_string"]
        })
    }

    async fn invoke(
        &self,
        input: Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let file_path = input["file_path"]
            .as_str()
            .ok_or("The 'file_path' parameter is required for the Edit tool.")?;
        let old_string = input["old_string"]
            .as_str()
            .ok_or("The 'old_string' parameter is required for the Edit tool.")?;
        let new_string = input["new_string"]
            .as_str()
            .ok_or("The 'new_string' parameter is required for the Edit tool.")?;
        let replace_all = input["replace_all"].as_bool().unwrap_or(false);

        if old_string.is_empty() {
            return Err("Error: old_string cannot be empty".into());
        }

        let resolved = resolve_path(&self.cwd, file_path);

        let content = match std::fs::read_to_string(&resolved) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(format!("Error: File not found at {file_path}").into());
            }
            Err(e) => return Err(e.into()),
        };

        let old_lines = old_string.lines().count();
        let new_lines = new_string.lines().count();
        let line_diff = new_lines as i64 - old_lines as i64;
        let rel = resolved
            .strip_prefix(&self.cwd)
            .unwrap_or(&resolved)
            .display()
            .to_string();

        // 构建行数变化描述
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
            std::cmp::Ordering::Equal => "Replaced text (same line count)".to_string(),
        };

        if replace_all {
            if !content.contains(old_string) {
                let hint = build_not_found_hint(&content, old_string);
                return Err(format!(
                    "Error: old_string not found in {}\n{hint}",
                    resolved.display()
                )
                .into());
            }
            let new_content = content.replace(old_string, new_string);
            let occurrences = content.matches(old_string).count();
            // 原子写入：先写临时文件再 rename
            let tmp_ext = format!("tmp.{}", uuid::Uuid::now_v7());
            let tmp_path = resolved.with_extension(tmp_ext);
            std::fs::write(&tmp_path, &new_content)?;
            match std::fs::rename(&tmp_path, &resolved) {
                Ok(_) => Ok(format!(
                    "{} to {} (replaced {} occurrence{})",
                    diff_desc,
                    rel,
                    occurrences,
                    if occurrences == 1 { "" } else { "s" }
                )),
                Err(e) => {
                    let _ = std::fs::remove_file(&tmp_path);
                    Err(format!("Error renaming temp file: {e}").into())
                }
            }
        } else {
            let occurrences = content.matches(old_string).count();
            if occurrences == 0 {
                let hint = build_not_found_hint(&content, old_string);
                return Err(format!(
                    "Error: old_string not found in {}\n{hint}",
                    resolved.display()
                )
                .into());
            }
            if occurrences > 1 {
                let locations: Vec<String> = content
                    .match_indices(old_string)
                    .take(10)
                    .map(|(offset, _)| {
                        let line = content[..offset].lines().count() + 1;
                        let end_line = line + old_string.lines().count().saturating_sub(1);
                        if end_line > line {
                            format!("lines {}-{}", line, end_line)
                        } else {
                            format!("line {}", line)
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
                return Err(format!(
                    "Error: old_string is not unique in {} (found {} occurrences).\n\
                     Match locations: {location_text}.\n\
                     Please provide more context to make old_string unique, or set replace_all=true.",
                    resolved.display(),
                    occurrences
                )
                .into());
            }
            let new_content = content.replacen(old_string, new_string, 1);
            // 原子写入：先写临时文件再 rename
            let tmp_ext = format!("tmp.{}", uuid::Uuid::now_v7());
            let tmp_path = resolved.with_extension(tmp_ext);
            std::fs::write(&tmp_path, &new_content)?;
            match std::fs::rename(&tmp_path, &resolved) {
                Ok(_) => Ok(format!("{} to {}", diff_desc, rel)),
                Err(e) => {
                    let _ = std::fs::remove_file(&tmp_path);
                    Err(format!("Error renaming temp file: {e}").into())
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("edit_test.rs");
}
