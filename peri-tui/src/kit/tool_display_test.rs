//! Tests for tool_display
#[cfg(test)]
use super::*;

#[test]
fn test_bash_maps_to_shell() {
    assert_eq!(format_tool_name("Bash"), "Shell");
}

#[test]
fn test_folder_operations_maps_to_folder() {
    assert_eq!(format_tool_name("folder_operations"), "Folder");
}

#[test]
fn test_unknown_passthrough() {
    // 大部分工具名保留原样（不再映射为别名）
    assert_eq!(format_tool_name("WebSearch"), "WebSearch");
    assert_eq!(format_tool_name("WebFetch"), "WebFetch");
    assert_eq!(format_tool_name("TodoWrite"), "TodoWrite");
    assert_eq!(format_tool_name("AskUserQuestion"), "AskUserQuestion");
    assert_eq!(format_tool_name("AgentResult"), "AgentResult");
    assert_eq!(format_tool_name("artifact"), "artifact");
    assert_eq!(format_tool_name("CustomTool"), "CustomTool");
}

#[test]
fn test_format_tool_args_bash_extracts_command() {
    let args = serde_json::json!({"command": "cargo build -p peri-tui"});
    assert_eq!(format_tool_args("Bash", &args), "cargo build -p peri-tui");
}

#[test]
fn test_format_tool_args_read_extracts_file_path() {
    let args = serde_json::json!({"file_path": "src/main.rs"});
    assert_eq!(format_tool_args("Read", &args), "src/main.rs");
}

#[test]
fn test_format_tool_args_glob_truncates_pattern() {
    let long = "a".repeat(250);
    let args = serde_json::json!({"pattern": &long});
    let result = format_tool_args("Glob", &args);
    assert!(result.len() <= 203, "应为 200 字符 + '...'");
    assert!(result.ends_with("..."));
}

#[test]
fn test_format_tool_args_websearch_truncates_query() {
    let long = "q".repeat(80);
    let args = serde_json::json!({"query": &long});
    let result = format_tool_args("WebSearch", &args);
    assert!(result.len() <= 63);
}

#[test]
fn test_format_tool_args_unknown_returns_empty() {
    let args = serde_json::json!({"x": "y"});
    assert_eq!(format_tool_args("UnknownTool", &args), "");
}

#[test]
fn test_format_tool_args_folder_operations() {
    let args = serde_json::json!({"operation": "list", "folder_path": "/tmp"});
    assert_eq!(format_tool_args("folder_operations", &args), "list /tmp");
}

#[test]
fn test_format_websearch_query_truncated() {
    // WebSearch query 超过 60 字符时应截断
    let long = "q".repeat(80);
    let args = serde_json::json!({"query": &long});
    let result = format_tool_args("WebSearch", &args);
    assert!(result.len() <= 63, "应为 60 字符 + '...'");
    assert!(result.ends_with("..."));
}

#[test]
fn test_format_webfetch_url_not_truncated() {
    // WebFetch url 不截断，返回原始字符串
    let long_url =
        "https://example.com/very/long/path/that/exceeds/sixty/characters/total/here.txt";
    assert!(long_url.chars().count() > 60, "测试用 url 长度应 > 60");
    let args = serde_json::json!({"url": &long_url});
    let result = format_tool_args("WebFetch", &args);
    assert_eq!(result, long_url, "WebFetch url 不应被截断");
}
