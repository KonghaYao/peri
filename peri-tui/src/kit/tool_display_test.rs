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

// ── A4 / A8 匹配型归一：builtin 一等工具的 effective name 走同一分支 ────────────

#[test]
fn tui_web_tools_still_summarize_after_migration() {
    // effective name（模型面名字）与迁移前的裸名必须产出**同一**参数摘要：
    // 归一走 IF-D15 的 original_tool_name_of_effective，本模块不得硬编码名字字面量。
    let long_query = "q".repeat(80);
    let args = serde_json::json!({ "query": long_query });
    let effective = format_tool_args("mcp__web__WebSearch", &args);
    assert_eq!(effective, format_tool_args("WebSearch", &args));
    assert!(effective.len() <= 63, "仍按 60 字符截断: {effective:?}");

    let url = "https://example.com/very/long/path/that/exceeds/sixty/characters/total/here.txt";
    let args = serde_json::json!({ "url": url });
    assert_eq!(format_tool_args("mcp__web__WebFetch", &args), url);

    // artifact 走 file_path 专用分支（未命中会返回空串，对比 test_format_tool_args_unknown_returns_empty）
    let args = serde_json::json!({ "file_path": "peri-tui/src/lib.rs" });
    assert_eq!(
        format_tool_args("mcp__artifact__artifact", &args),
        "peri-tui/src/lib.rs"
    );
}

#[test]
fn tui_unknown_mcp_names_fall_back_to_generic() {
    // 反证：未知 / 外部 `mcp__*` 两次都不命中 ⇒ 与迁移前逐位一致（无摘要）。
    let args = serde_json::json!({ "file_path": "peri-tui/src/lib.rs" });
    assert_eq!(format_tool_args("mcp__foo__bar", &args), "");
    // 归一表是冻结字面量的精确匹配：不做前缀、大小写或 sanitize 反拆
    assert_eq!(format_tool_args("mcp__web__WebSearchExtra", &args), "");
    assert_eq!(format_tool_args("mcp__web__websearch", &args), "");
}

#[test]
fn tui_has_no_hardcoded_effective_name() {
    // A4 / A8：名字字面量只在 peri-acp-types 的 builtin 声明表存一份——按名分支
    // 必须经 IF-D15 归一 helper 进入 builtin 名字空间，不得自建第二张反查表。
    let src = include_str!("tool_display.rs");
    assert!(
        src.contains("original_tool_name_of_effective"),
        "tool_display.rs 必须经 IF-D15 归一 helper"
    );
    for forbidden in ["mcp__web__", "mcp__artifact__"] {
        assert!(
            !src.contains(forbidden),
            "tool_display.rs 不得硬编码 effective name: {forbidden}"
        );
    }
}
