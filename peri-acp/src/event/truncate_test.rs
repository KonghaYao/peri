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
