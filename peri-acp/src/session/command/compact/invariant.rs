//! Compact 输出结构不变量。
//!
//! 封装 CLAUDE.md 第一优先级 [TRAP]：
//! compact 后消息必须以 `BaseMessage::human(summary + continuation)` 开头，
//! 完整结构为 `[Human(摘要+续接指令), System(文件)..., System(Skills)...]`。
//! 禁止将摘要放入 `BaseMessage::system()`（会导致 anthropic/invoke.rs 把它 hoist 到
//! 顶层 system prompt，污染 frozen_system_prompt + 破坏 Prompt Cache）。
//!
// [TRAP] compact 后消息结构必须以 `BaseMessage::human(summary + continuation)` 开头。
// 禁止将摘要放在 `BaseMessage::system()` 中。compact 后的完整结构：
//   [Human(摘要+续接指令), System(文件)..., System(Skills)...]。
// （详见 spec/global/domains/compact.md#issue_2026-05-20-auto-compact-empty-messages-400）

use peri_agent::messages::BaseMessage;

/// 构造 compact 输出的首条 Human 消息（摘要 + 续接指令）。
///
/// 集中 [TRAP] "摘要必须作为 Human 消息" 约束，供 pipeline 与 auto-compact 路径共享。
/// 任何把摘要放入 System 的尝试都会破坏 Prompt Cache 前缀稳定性 + frozen_system_prompt
/// 不变量——见 CLAUDE.md `system-prompt.md#issue_2026-06-17-mid-conversation-system-message-breaks-frozen-prompt`。
///
/// `<system-reminder>` 包裹与 auto-compact 路径（`peri-middlewares/src/compact_middleware.rs`）
/// 完全对齐，让 TUI 能折叠显示为 `📋 Context compacted`。
pub fn build_summary_human_message(summary: &str) -> BaseMessage {
    let summary_content = format!(
        "<system-reminder>\n{}\n\n{}\n</system-reminder>",
        summary,
        peri_agent::agent::compact::CONTINUATION_HINT
    );
    BaseMessage::human(summary_content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_summary_human_message_returns_human_variant() {
        let msg = build_summary_human_message("## Summary\nCompleted main.rs review");
        assert!(
            matches!(msg, BaseMessage::Human { .. }),
            "摘要必须封装为 Human 消息，实际: {:?}",
            msg
        );
    }

    #[test]
    fn test_build_summary_human_message_includes_summary_and_continuation() {
        let msg = build_summary_human_message("UNIQUE_SUMMARY_BODY");
        let text = msg.content();
        assert!(
            text.contains("UNIQUE_SUMMARY_BODY"),
            "首条 Human 必须包含摘要内容"
        );
        assert!(
            text.contains(peri_agent::agent::compact::CONTINUATION_HINT),
            "首条 Human 必须包含续接指令标记"
        );
        assert!(
            text.contains("<system-reminder>"),
            "首条 Human 必须包裹 <system-reminder> 标签以触发 TUI 折叠"
        );
    }
}
