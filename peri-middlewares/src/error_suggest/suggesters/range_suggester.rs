use crate::error_suggest::context::ErrorContext;
use crate::error_suggest::registry::{ErrorSuggester, Suggestion};
use regex::Regex;
use std::sync::OnceLock;

/// B2：Read 工具 offset/limit 越界建议
pub struct RangeSuggester;

impl ErrorSuggester for RangeSuggester {
    fn suggest(&self, ctx: &ErrorContext) -> Option<Suggestion> {
        if ctx.tool_name != "Read" {
            return None;
        }

        // 识别 "offset X exceeds file length (Y lines)" 错误
        static RE: OnceLock<Regex> = OnceLock::new();
        let re = RE.get_or_init(|| {
            Regex::new(r"offset\s+(\d+)\s+exceeds file length\s+\((\d+)\s+lines\)").unwrap()
        });
        let caps = re.captures(ctx.error_message)?;

        let total: u64 = caps[2].parse().ok()?;

        // 错误正文已含 "offset X exceeds file length (Y lines)"，此处只给修正方向
        Some(Suggestion::new(format!(
            "Use offset 1 to read from the start, or any offset below {total}."
        )))
    }
}
