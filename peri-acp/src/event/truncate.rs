//! 共享的工具调用摘要 / 截断 helper。
//!
//! `router.rs`（streaming 通道：`tool-started` / `tool-ended` JSON 事件）与
//! `view_mapper.rs`（view-commit 通道：`ToolCard` ViewModel 的 input_summary /
//! output_summary）原本各自维护一份相似但**格式分歧**的实现（同一工具调用在两个
//! 通道显示不同格式，最典型的是 `pattern: "TODO"` vs `pattern: TODO`）。
//!
//! 本模块提供共享 helper，统一两通道的显示格式：
//!
//! - `pattern` / `query` / `cmd` / `path` 等键名前缀统一为**带引号** repr（JSON value
//!   的标准 repr，与 streaming 通道历史行为一致），便于人眼快速定位 value 边界
//! - 其余字段（`file_path`、`command`、`operation` 等）按既有约定：无引号裸值
//!
//! 统一后，同一工具调用在 streaming 与 view-commit 通道显示**相同**格式，避免 TUI
//! 渲染层对两通道做差异化处理。

/// Truncate text to `max_chars` Unicode code points (CJK-safe).
///
/// 等价于历史上的 `truncate_text` / `truncate_chars`（两份实现完全一致，仅命名不同）。
pub fn truncate_text(s: &str, max_chars: usize) -> String {
    let len = s.chars().count();
    if len <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{}...", truncated)
    }
}

/// Produce a one-line summary of a tool's JSON input.
///
/// 合并 router.rs 与 view_mapper.rs 的实现：
/// - 兼具 view_mapper 的 Read `path` 兜底、folder_operations 完整字段
/// - 兼具 router 的 `_` 分支多级兜底（path/file_path → query/pattern → command → 首个 KV）
/// - `pattern` / `query` 统一为**带引号**格式
pub fn summarize_input(name: &str, input: &serde_json::Value) -> String {
    // 优先按 Object 提取，非 Object 走 truncate 兜底（与 view_mapper 一致）
    let obj = match input {
        serde_json::Value::Object(map) => map,
        other => return truncate_text(&other.to_string(), 120),
    };

    // 字符串字段读取 helper（view_mapper 风格，空值返回 ""）
    let str_val = |key: &str| -> String {
        obj.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    // 旧 router 风格的 Option<&str> 读取（用于 "有值才格式化带引号" 分支）
    let field = |key: &str| -> Option<&str> { obj.get(key).and_then(|v| v.as_str()) };

    match name {
        // ── 无前缀，文件路径不截断（view_mapper：file_path 为空时回退 path；
        //    若仍为空返回 "(empty input)" 占位，避免渲染层显示空白卡片）──
        "Read" | "Write" | "Edit" => {
            let p = str_val("file_path");
            if p.is_empty() {
                let fallback = str_val("path");
                if fallback.is_empty() {
                    "(empty input)".to_string()
                } else {
                    fallback
                }
            } else {
                p
            }
        }
        // ── 无前缀，命令截断 400 ──
        "Bash" => truncate_text(&str_val("command"), 400),
        // ── 有前缀 pattern:，pattern 截断 200，带引号（统一格式）──
        "Glob" | "Grep" => {
            let p = str_val("pattern");
            let p = if p.is_empty() { str_val("query") } else { p };
            format!(r#"pattern: "{}""#, truncate_text(&p, 200))
        }
        // ── "operation folder_path"，不截断 ──
        "folder_operations" => {
            let op = str_val("operation");
            let fp = str_val("folder_path");
            format!("{} {}", op, fp)
        }
        // ── query: 截断 60，带引号（统一格式）──
        "WebSearch" => field("query")
            .map(|s| format!(r#"query: "{}""#, truncate_text(s, 60)))
            .unwrap_or_else(|| "(empty input)".to_string()),
        // ── url: 不截断 ──
        "WebFetch" => field("url")
            .map(|s| format!("url: {}", s))
            .unwrap_or_else(|| "(empty input)".to_string()),
        // ── 空字符串（文档无参数）──
        "TodoWrite" => String::new(),
        // ── task_id 截断 12 ──
        "AgentResult" => truncate_text(&str_val("task_id"), 12),
        // ── file_path 不截断 ──
        "artifact" => str_val("file_path"),
        // ── operation 截断 40 ──
        "LSP" => truncate_text(&str_val("operation"), 40),
        // ── tool_name 截断 40 ──
        "ExecuteExtraTool" => truncate_text(&str_val("tool_name"), 40),
        // ── query 截断 40 ──
        "SearchExtraTools" => truncate_text(&str_val("query"), 40),
        // ── 兜底：router 风格的多级探测（path/file_path → query/pattern → command → 首个 KV）──
        _ => {
            if let Some(path) = obj.get("path").or_else(|| obj.get("file_path")) {
                return format!("path: {}", truncate_text(&path.to_string(), 120));
            }
            if let Some(query) = obj.get("query").or_else(|| obj.get("pattern")) {
                return format!("query: {}", truncate_text(&query.to_string(), 120));
            }
            if let Some(cmd) = obj.get("command") {
                return format!("cmd: {}", truncate_text(&cmd.to_string(), 120));
            }
            if let Some((k, v)) = obj.iter().next() {
                let raw = v.as_str().unwrap_or("");
                return format!("{}: {}", k, truncate_text(raw, 100));
            }
            "(empty input)".to_string()
        }
    }
}

/// Produce a one-line summary of a tool's output.
///
/// 合并 router.rs 与 view_mapper.rs 的实现，保留 view_mapper 更丰富的特殊分支
/// （WebFetch / TodoWrite / Read / Glob / Grep 折叠态行数），streaming 与 view-commit
/// 共享同一展示语义。
pub fn summarize_output(name: &str, output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    match name {
        "Edit" | "Write" => {
            let lines = trimmed.lines().count();
            if lines <= 3 {
                return truncate_text(trimmed, 200);
            }
            format!("{} lines changed", lines)
        }
        "WebFetch" => {
            let lines = trimmed.lines().count();
            let bytes = output.len();
            format!(
                "{} lines · {} bytes\n{}",
                lines,
                bytes,
                truncate_text(trimmed, 400)
            )
        }
        // TodoWrite 返回全量内容（显示完整 todo 列表）
        "TodoWrite" => trimmed.to_string(),
        // Read / Glob / Grep — 折叠态显示行数
        "Read" | "Glob" | "Grep" => {
            let lines = trimmed.lines().count();
            format!("{} lines", lines)
        }
        _ => truncate_text(trimmed, 200),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_text_short() {
        assert_eq!(truncate_text("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_text_exact() {
        assert_eq!(truncate_text("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_text_long() {
        assert_eq!(truncate_text("abcdefghij", 5), "abcde...");
    }

    #[test]
    fn test_truncate_text_cjk() {
        assert_eq!(truncate_text("你好世界", 2), "你好...");
    }

    #[test]
    fn test_summarize_input_grep_unified_quoted_format() {
        // 关键不变量：streaming 与 view-commit 通道共享此 helper，
        // 同一工具调用必须显示相同格式（带引号）
        let input = serde_json::json!({ "pattern": "TODO" });
        assert_eq!(summarize_input("Grep", &input), r#"pattern: "TODO""#);
        assert_eq!(summarize_input("Glob", &input), r#"pattern: "TODO""#);
    }

    #[test]
    fn test_summarize_input_web_search_quoted_format() {
        let input = serde_json::json!({ "query": "rust async" });
        assert_eq!(
            summarize_input("WebSearch", &input),
            r#"query: "rust async""#
        );
    }

    #[test]
    fn test_summarize_input_read_fallback_path() {
        let input = serde_json::json!({ "path": "/tmp/bar.rs" });
        assert_eq!(summarize_input("Read", &input), "/tmp/bar.rs");
    }

    #[test]
    fn test_summarize_input_empty_object() {
        let input = serde_json::json!({});
        assert_eq!(summarize_input("Read", &input), "(empty input)");
    }

    #[test]
    fn test_summarize_input_non_object_fallback() {
        // 非 Object 的 JSON value 走 `to_string()` 兜底（JSON 字符串带引号）
        let input = serde_json::json!("raw string");
        assert_eq!(summarize_input("Read", &input), "\"raw string\"");
    }

    #[test]
    fn test_summarize_output_empty() {
        assert_eq!(summarize_output("Bash", ""), "");
        assert_eq!(summarize_output("Bash", "   "), "");
    }

    #[test]
    fn test_summarize_output_edit_long_collapses_to_line_count() {
        let output = "line1\nline2\nline3\nline4\nline5";
        assert_eq!(summarize_output("Edit", output), "5 lines changed");
    }
}
