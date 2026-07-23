//! 工具显示名与参数摘要格式化。
//!
//! 对应 spec/global/domains/tui/tui-rendering.md §2.4.2 的工具名映射表和参数摘要规则。

use crate::i18n;

/// 将原始 `tool_name` 映射为用户友好的显示名。
pub fn format_tool_name(raw: &str) -> String {
    match raw {
        "Bash" => i18n::tr("tool-name-shell"),
        "folder_operations" => i18n::tr("tool-name-folder"),
        other => other.to_string(),
    }
}

/// 从工具参数 JSON 值中提取显示摘要（按 tool_name 选择关键字段）。
///
/// 对应 spec/global/domains/tui/tui-rendering.md §2.4.2 的 `format_tool_args` 规则。
/// 当 ACP view_mapper 已将 args 预摘要为 `input_summary` 字符串时，
/// 优先使用预摘要；本函数用于需要从原始 args 提取的场合。
pub fn format_tool_args(tool_name: &str, args: &serde_json::Value) -> String {
    let truncate = |s: &str, max: usize| -> String {
        if s.chars().count() > max {
            format!("{}...", s.chars().take(max).collect::<String>())
        } else {
            s.to_string()
        }
    };
    match tool_name {
        "Bash" => args
            .get("command")
            .and_then(|v| v.as_str())
            .map(|s| truncate(s, 400))
            .unwrap_or_default(),
        "Read" | "Write" | "Edit" => args
            .get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "Glob" | "Grep" => args
            .get("pattern")
            .and_then(|v| v.as_str())
            .map(|s| truncate(s, 200))
            .unwrap_or_default(),
        "folder_operations" => {
            let op = args.get("operation").and_then(|v| v.as_str()).unwrap_or("");
            let path = args
                .get("folder_path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("{} {}", op, path)
        }
        "WebSearch" => args
            .get("query")
            .and_then(|v| v.as_str())
            .map(|s| truncate(s, 60))
            .unwrap_or_default(),
        "WebFetch" => args
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        "ExecuteExtraTool" | "SearchExtraTools" => {
            let key = if tool_name == "ExecuteExtraTool" {
                "tool_name"
            } else {
                "query"
            };
            args.get(key)
                .and_then(|v| v.as_str())
                .map(|s| truncate(s, 40))
                .unwrap_or_default()
        }
        "AgentResult" => args
            .get("task_id")
            .and_then(|v| v.as_str())
            .map(|s| truncate(s, 12))
            .unwrap_or_default(),
        "artifact" => args
            .get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "LSP" => args
            .get("operation")
            .and_then(|v| v.as_str())
            .map(|s| truncate(s, 40))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

#[cfg(test)]
#[path = "tool_display_test.rs"]
mod tests;
