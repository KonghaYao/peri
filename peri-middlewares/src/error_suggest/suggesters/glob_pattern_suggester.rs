use crate::error_suggest::context::ErrorContext;
use crate::error_suggest::normalized_tool_name;
use crate::error_suggest::registry::{ErrorSuggester, Suggestion};

/// B3：Glob pattern 语法错误建议
pub struct GlobPatternSuggester;

impl ErrorSuggester for GlobPatternSuggester {
    fn suggest(&self, ctx: &ErrorContext) -> Option<Suggestion> {
        // 归一：`mcp__workspace__Glob` 与 `Glob` 同门槛
        if normalized_tool_name(ctx.tool_name) != "Glob" {
            return None;
        }
        if !ctx.error_message.contains("Pattern syntax error") {
            return None;
        }

        Some(Suggestion::new(
            "Invalid glob syntax. Examples: *.rs (current dir), **/*.rs (recursive), src/**/*.rs, {foo,bar}.rs (enum). Note: brackets like [abc] must be closed.",
        ))
    }
}
