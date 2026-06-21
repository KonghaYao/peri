use crate::error_suggest::context::ErrorContext;
use crate::error_suggest::registry::{ErrorSuggester, Suggestion};

/// B4：Grep 工具 regex 语法错误建议
pub struct RegexSuggester;

impl ErrorSuggester for RegexSuggester {
    fn suggest(&self, ctx: &ErrorContext) -> Option<Suggestion> {
        if ctx.tool_name != "Grep" {
            return None;
        }
        let lower = ctx.error_message.to_lowercase();
        if !lower.contains("regex parse error") && !lower.contains("regex") {
            return None;
        }
        if !lower.contains("parse error")
            && !lower.contains("unclosed")
            && !lower.contains("unbalanced")
        {
            return None;
        }

        Some(Suggestion::new(
            "Invalid regex syntax. Common issues:\n  • Brackets must be closed: () [] {}\n  • Special characters need escaping: \\ . \\* \\+\n  • For literal matching, use fixed_strings: true to disable regex mode\n  • For complex patterns, validate with a tool like regex101 first",
        ))
    }
}
