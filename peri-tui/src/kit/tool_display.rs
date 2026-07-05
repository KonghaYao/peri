//! 工具显示名与参数摘要格式化。
//!
//! 对应 TUI-PAGE.md §2.4.2 的工具名映射表。

/// 将原始 `tool_name` 映射为用户友好的显示名。
pub fn format_tool_name(raw: &str) -> &str {
    match raw {
        "Bash" => "Shell",
        "Read" => "Read",
        "Write" => "Write",
        "Edit" => "Edit",
        "Glob" => "Glob",
        "Grep" => "Grep",
        "folder_operations" => "Folder",
        "TodoWrite" => "Todo",
        "AskUserQuestion" => "Ask",
        "Agent" => "Agent",
        "WebSearch" => "Research",
        "WebFetch" => "Browse",
        "AgentResult" => "SubAgent",
        "LSP" => "LSP",
        "artifact" => "ArtUp",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bash_maps_to_shell() {
        assert_eq!(format_tool_name("Bash"), "Shell");
    }

    #[test]
    fn test_websearch_maps_to_research() {
        assert_eq!(format_tool_name("WebSearch"), "Research");
    }

    #[test]
    fn test_folder_operations_maps_to_folder() {
        assert_eq!(format_tool_name("folder_operations"), "Folder");
    }

    #[test]
    fn test_unknown_passthrough() {
        assert_eq!(format_tool_name("CustomTool"), "CustomTool");
    }
}
