//! Smart Compact 实现
//!
//! 纯规则驱动的上下文压缩策略——不调用 LLM。
//! 现已收缩为 planner 兼容入口：通过 plan_micro 生成计划再应用。
//! 未来可在 CompactPolicy 中扩展排序模式以恢复独立策略。

use tracing::debug;

use crate::agent::compact_v2::config::CompactConfig;
use crate::session::transcript::MessageTranscript;

use super::projection::ProjectionTarget;

/// Smart Compact：规则驱动，从尾部前向遍历，保留关键消息，其余标记 truncated
///
/// 现通过 plan_micro 的兼容入口生成计划再应用。
/// 未来可在 CompactPolicy 中扩展排序模式。
///
/// # 返回
/// (被标记消息数量, 估算 token 节省量)
pub fn smart_compact(transcript: &mut MessageTranscript, config: &CompactConfig) -> (usize, u64) {
    tracing::warn!("Smart Compact is deprecated and will be removed. Converging to Micro Compact.");
    let plan = super::planner::plan_micro(transcript, config, true);
    let affected = plan.actions.len();
    let saved = plan.estimated_tokens_saved;

    for action in &plan.actions {
        match &action.target {
            ProjectionTarget::Message
            | ProjectionTarget::ContentBlock { .. }
            | ProjectionTarget::ToolCall { .. } => {
                // 统一使用 set_flags_projection 持久化 directive（与 micro_compact 一致）
                // set_flags_projection 同时设置 truncated=true + projection directive
                let directive = crate::agent::compact_v2::projection::MessageProjectionDirective {
                    policy_version: crate::agent::compact_v2::projection::PROJECTION_POLICY_VERSION,
                    entries: vec![action.clone()],
                };
                transcript.set_flags_projection(action.message_id, directive);
            }
        }
    }

    if affected > 0 {
        debug!(
            affected,
            "Smart Compact (via plan_micro): 标记 truncated 消息"
        );
    }

    (affected, saved)
}

// ─── 测试 ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::compact_v2::config::CompactConfig;
    use crate::messages::{BaseMessage, MessageContent};

    fn make_human(text: &str) -> BaseMessage {
        BaseMessage::human(MessageContent::text(text.to_string()))
    }

    fn make_ai(text: &str) -> BaseMessage {
        BaseMessage::ai(MessageContent::text(text.to_string()))
    }

    fn make_ai_with_tool(text: &str, tool_name: &str, tool_id: &str) -> BaseMessage {
        BaseMessage::ai_with_tool_calls(
            MessageContent::text(text.to_string()),
            vec![crate::messages::ToolCallRequest::new(
                tool_id,
                tool_name,
                serde_json::json!({"content": "x".repeat(501)}),
            )],
        )
    }

    fn make_tool_result(tool_call_id: &str, _text: &str) -> BaseMessage {
        BaseMessage::tool_result(
            tool_call_id.to_string(),
            MessageContent::text("x".repeat(501)),
        )
    }

    fn make_error_tool_result(tool_call_id: &str, text: &str) -> BaseMessage {
        BaseMessage::tool_error(
            tool_call_id.to_string(),
            MessageContent::text(text.to_string()),
        )
    }

    // ── Smart Compact 基本测试（plan_micro 包装） ──────────────────────────

    #[test]
    fn test_smart_compact_empty_transcript() {
        let mut t = MessageTranscript::new();
        let config = CompactConfig::default();
        let (affected, _saved) = smart_compact(&mut t, &config);
        assert_eq!(affected, 0, "空记录不应标记任何消息");
    }

    #[test]
    fn test_smart_compact_all_within_stale_window() {
        let mut t = MessageTranscript::new();
        t.append(make_human("question 1"));
        t.append(make_ai_with_tool("think", "Bash", "call_1"));
        t.append(make_tool_result("call_1", "output"));

        let config = CompactConfig::default();
        // 只有 1 轮，stale_steps=3 → 全在窗口内
        let (affected, _saved) = smart_compact(&mut t, &config);
        assert_eq!(affected, 0, "消息在 stale 窗口内，不应被标记");
    }

    #[test]
    fn test_smart_compact_truncates_old_tool_exchanges() {
        let mut t = MessageTranscript::new();
        for i in 0..7 {
            t.append(make_human(&format!("question {}", i)));
            t.append(make_ai_with_tool("think", "Bash", &format!("call_{}", i)));
            t.append(make_tool_result(
                &format!("call_{}", i),
                &format!("output {}", i),
            ));
        }

        let config = CompactConfig::default();
        let (affected, _saved) = smart_compact(&mut t, &config);
        let marked_messages = t
            .entries()
            .iter()
            .filter(|entry| t.flags(entry.message.id()).truncated)
            .count();
        assert_eq!(
            affected, marked_messages,
            "affected 应等于实际被标记的消息数，实际 affected={}, marked={}",
            affected, marked_messages
        );
        assert!(marked_messages > 0, "过期 tool exchange 应产生消息级标记");
    }

    #[test]
    fn test_smart_compact_keeps_error_tool_result() {
        let mut t = MessageTranscript::new();
        // 构造足够多轮次，让第 0 轮进入 stale 窗口
        for i in 0..7 {
            t.append(make_human(&format!("q {}", i)));
            t.append(make_ai_with_tool(
                "run command",
                "Bash",
                &format!("bash_{}", i),
            ));
            t.append(make_error_tool_result(
                &format!("bash_{}", i),
                &format!("error output {}", i),
            ));
        }

        let config = CompactConfig::default();
        let (affected, _saved) = smart_compact(&mut t, &config);
        // 4 stale rounds: tool_use 被压缩（CompactToolInput），error tool_result 不被压缩
        assert_eq!(
            affected, 4,
            "只有 tool_use 被标记，错误 tool_result 保留，实际: {}",
            affected
        );

        // 验证所有错误 tool_result 未被 truncated
        for entry in t.entries() {
            if let BaseMessage::Tool { is_error, .. } = &entry.message {
                if *is_error {
                    assert!(
                        !t.flags(entry.message.id()).truncated,
                        "错误 ToolResult 不应被 truncated"
                    );
                }
            }
        }
    }

    #[test]
    fn test_smart_compact_respects_ancestor_boundary() {
        let ancestor = make_human("ancestor message");
        let mut t = MessageTranscript::new().with_ancestor(vec![ancestor]);
        // 构造足够多的自有消息以触发截断
        for i in 0..7 {
            t.append(make_human(&format!("q {}", i)));
            t.append(make_ai_with_tool("", "Bash", &format!("c_{}", i)));
            t.append(make_tool_result(&format!("c_{}", i), &format!("out {}", i)));
        }

        let config = CompactConfig::default();
        let (affected, _saved) = smart_compact(&mut t, &config);

        // ancestor 消息不应被标记
        let entries = t.entries();
        assert!(entries.len() > 1);
        let ancestor_flags = t.flags(entries[0].message.id());
        assert!(!ancestor_flags.truncated, "ancestor 消息不应被 truncated");

        // 自有消息有被标记的
        assert!(affected > 0, "自有消息应有被标记的");
    }

    #[test]
    fn test_smart_compact_no_duplicate_truncation() {
        let mut t = MessageTranscript::new();
        for i in 0..7 {
            t.append(make_human(&format!("q {}", i)));
            t.append(make_ai_with_tool("", "Bash", &format!("c_{}", i)));
            t.append(make_tool_result(&format!("c_{}", i), &format!("out {}", i)));
        }

        let config = CompactConfig::default();
        let (first, _) = smart_compact(&mut t, &config);
        let (second, _) = smart_compact(&mut t, &config);
        assert!(first > 0, "首次调用应有消息被标记");
        assert_eq!(second, 0, "重复调用不应增加标记");
    }

    #[test]
    fn test_smart_compact_with_error_and_normal_tools() {
        let mut t = MessageTranscript::new();
        for i in 0..5 {
            t.append(make_human(&format!("old q {}", i)));
            t.append(make_ai_with_tool(
                "old thinking",
                "Read",
                &format!("old_call_{}", i),
            ));
            t.append(make_tool_result(
                &format!("old_call_{}", i),
                &format!("old result {}", i),
            ));
        }
        // 中间有错误
        t.append(make_human("error query"));
        t.append(make_ai_with_tool("error thinking", "Bash", "error_call"));
        t.append(make_error_tool_result("error_call", "permission denied"));
        // 最近消息
        t.append(make_human("recent question"));
        t.append(make_ai_with_tool("recent thinking", "Read", "recent_call"));
        t.append(make_tool_result("recent_call", "recent result"));

        let config = CompactConfig {
            micro_compact_stale_steps: 3,
            ..Default::default()
        };
        let (affected, _saved) = smart_compact(&mut t, &config);

        let entries = t.entries();
        let recent_human_id = entries[entries.len() - 3].message.id();
        let recent_tool_id = entries[entries.len() - 1].message.id();
        let error_tool_id = entries[entries.len() - 5].message.id();

        // 最近消息（在 stale 窗口内）应保留
        assert!(
            !t.flags(recent_human_id).truncated,
            "最近 Human 应保留（不是工具消息）"
        );
        assert!(
            !t.flags(recent_tool_id).truncated,
            "最近 Tool 结果应保留（在窗口内）"
        );
        // 错误消息应保留
        assert!(!t.flags(error_tool_id).truncated, "错误 Tool 应保留");
        // 旧消息应被标记
        assert!(affected > 0, "旧消息应被标记");
    }

    #[test]
    fn test_smart_compact_keeps_system_messages() {
        let mut t = MessageTranscript::new();
        t.append(BaseMessage::system(MessageContent::text(
            "system instruction".to_string(),
        )));
        t.append(make_human("question"));
        t.append(make_ai("answer"));

        let config = CompactConfig::default();
        let (affected, _saved) = smart_compact(&mut t, &config);

        let entries = t.entries();
        let system_flags = t.flags(entries[0].message.id());
        assert!(
            !system_flags.truncated,
            "System 消息应保留（非工具，不被选中）"
        );
        assert_eq!(affected, 0, "无工具调用，plan_micro 不产生 action");
    }
}
