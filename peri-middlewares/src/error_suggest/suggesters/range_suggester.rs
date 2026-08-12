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

        // 新版 Read 错误已给出实际范围与确定恢复动作，不重复附加建议；
        // 重复的 "try another offset" 会重新诱导模型猜测大值。
        if ctx.error_message.contains("Do not guess") {
            return None;
        }

        // 兼容识别旧版 "offset X exceeds file length (Y lines)" 错误
        static RE: OnceLock<Regex> = OnceLock::new();
        let re = RE.get_or_init(|| {
            Regex::new(r"offset\s+(\d+)\s+exceeds file length\s+\((\d+)\s+lines\)").unwrap()
        });
        let caps = re.captures(ctx.error_message)?;

        let total: u64 = caps[2].parse().ok()?;

        // 旧版错误正文只有长度信息；给出确定恢复动作，不鼓励再猜一个数字。
        Some(Suggestion::new(format!(
            "Omit offset to read from the beginning. If targeting a known location, use only an observed line number in 1..={total}; do not guess."
        )))
    }
}
