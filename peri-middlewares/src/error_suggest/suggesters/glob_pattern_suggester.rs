use crate::error_suggest::context::ErrorContext;
use crate::error_suggest::registry::{ErrorSuggester, Suggestion};

/// B3：Glob pattern 语法错误建议
pub struct GlobPatternSuggester;

impl ErrorSuggester for GlobPatternSuggester {
    fn suggest(&self, ctx: &ErrorContext) -> Option<Suggestion> {
        if ctx.tool_name != "Glob" {
            return None;
        }
        if !ctx.error_message.contains("Pattern syntax error") {
            return None;
        }

        Some(Suggestion::new(
            "Glob pattern 语法有误。合法示例：\n  • *.rs —— 当前目录所有 Rust 文件\n  • **/*.rs —— 递归所有子目录\n  • src/**/*.rs —— src 下所有 Rust 文件\n  • {foo,bar}.rs —— 枚举\n注意：方括号 [ 必须闭合，例如 [abc].rs",
        ))
    }
}
