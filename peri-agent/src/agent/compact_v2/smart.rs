//! Smart Compact 实现
//!
//! 纯规则驱动的上下文压缩策略——不调用 LLM，相比 Micro Compact 更精确地
//! 决定保留/丢弃消息。
//!
//! 策略：
//! - 保留最近 N 条 User/Assistant 对话消息（默认 5 条）
//! - 保留最近 M 个工具调用结果（默认 3 个）
//! - 保留所有错误工具结果（保留诊断信息）
//! - 保留所有 System 消息（系统提示词不可截断）
//! - 不操作 ancestor 消息（来自父 session，与 Micro 一致）
//! - 其余消息标记 truncated

use tracing::debug;

use crate::agent::compact_v2::config::CompactConfig;
use crate::messages::BaseMessage;
use crate::session::transcript::MessageTranscript;

/// Smart Compact：规则驱动，从尾部前向遍历，保留关键消息，其余标记 truncated
///
/// # 参数
/// - `transcript`: 消息记录
/// - `config`: Compact 配置，使用其中的 `smart_keep_recent_msgs` 和 `smart_keep_recent_tools`
///
/// # 返回
/// 被标记 truncated 的消息数量
pub fn smart_compact(transcript: &mut MessageTranscript, config: &CompactConfig) -> usize {
    let ancestor_len = transcript.ancestor_len();
    let entries = transcript.entries();
    if entries.len() <= ancestor_len {
        return 0;
    }

    let own_entries = &entries[ancestor_len..];
    let keep_msgs = config.smart_keep_recent_msgs;
    let keep_tools = config.smart_keep_recent_tools;

    // 从尾部反向遍历，分类计数
    let mut human_ai_count = 0usize;
    let mut tool_count = 0usize;
    let mut keep_flags = vec![false; own_entries.len()];

    for (i, entry) in own_entries.iter().enumerate().rev() {
        let msg = &entry.message;

        // 检查是否已被 truncated（避免重复标记）
        let existing_flags = transcript.flags(msg.id());
        if existing_flags.truncated {
            continue;
        }

        let should_keep = match msg {
            BaseMessage::System { .. } => {
                // System 消息永远保留
                true
            }
            BaseMessage::Tool { is_error: true, .. } => {
                // 错误输出永远保留
                true
            }
            BaseMessage::Tool {
                is_error: false, ..
            } => {
                let keep = tool_count < keep_tools;
                if keep {
                    tool_count += 1;
                }
                keep
            }
            BaseMessage::Human { .. } | BaseMessage::Ai { .. } => {
                let keep = human_ai_count < keep_msgs;
                if keep {
                    human_ai_count += 1;
                }
                keep
            }
        };

        keep_flags[i] = should_keep;
    }

    // 收集需要标记 truncated 的消息 id
    let mut ids_to_truncate: Vec<_> = Vec::new();
    for (i, entry) in own_entries.iter().enumerate() {
        if !keep_flags[i] {
            let existing_flags = transcript.flags(entry.message.id());
            if !existing_flags.truncated {
                ids_to_truncate.push(entry.message.id());
            }
        }
    }

    // 批量设置 truncated 标记
    for id in &ids_to_truncate {
        transcript.set_truncated(*id, true);
    }

    let affected = ids_to_truncate.len();
    if affected > 0 {
        debug!(
            affected,
            keep_msgs,
            keep_tools,
            human_ai_count,
            tool_count,
            "Smart Compact: 标记 truncated 消息（保留 {} Human/Ai, {} Tool）",
            human_ai_count,
            tool_count
        );
    }

    affected
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
                serde_json::json!({}),
            )],
        )
    }

    fn make_tool_result(tool_call_id: &str, text: &str) -> BaseMessage {
        BaseMessage::tool_result(
            tool_call_id.to_string(),
            MessageContent::text(text.to_string()),
        )
    }

    fn make_error_tool_result(tool_call_id: &str, text: &str) -> BaseMessage {
        BaseMessage::tool_error(
            tool_call_id.to_string(),
            MessageContent::text(text.to_string()),
        )
    }

    // ── Smart Compact 基本测试 ─────────────────────────────────────────────

    #[test]
    fn test_smart_compact_empty_transcript() {
        let mut t = MessageTranscript::new();
        let config = CompactConfig::default();
        let affected = smart_compact(&mut t, &config);
        assert_eq!(affected, 0, "空记录不应标记任何消息");
    }

    #[test]
    fn test_smart_compact_all_within_keep_window() {
        let mut t = MessageTranscript::new();
        t.append(make_human("question 1"));
        t.append(make_ai("answer 1"));
        t.append(make_human("question 2"));
        t.append(make_ai("answer 2"));

        let config = CompactConfig::default();
        let affected = smart_compact(&mut t, &config);
        // 默认保留 5 条 Human/Ai，只有 4 条，全在窗口内
        assert_eq!(affected, 0, "消息在保留窗口内，不应被标记");
    }

    #[test]
    fn test_smart_compact_truncates_old_messages() {
        let mut t = MessageTranscript::new();
        // 构造 10 轮对话（20 条消息），默认保留最近 5 条 Human/Ai
        for i in 0..10 {
            t.append(make_human(&format!("question {}", i)));
            t.append(make_ai(&format!("answer {}", i)));
        }

        let config = CompactConfig::default();
        let affected = smart_compact(&mut t, &config);

        // 20 条消息，保留最后 5 条，应标记前 15 条
        assert_eq!(affected, 15, "应标记前 15 条旧消息，实际: {}", affected);

        // 验证前 15 条被标记，后 5 条保留
        let total = t.entries().len();
        for (i, entry) in t.entries().iter().enumerate() {
            let flags = t.flags(entry.message.id());
            if i < total - 5 {
                assert!(flags.truncated, "第 {} 条消息应被 truncated", i);
            } else {
                assert!(!flags.truncated, "第 {} 条消息不应被 truncated", i);
            }
        }
    }

    #[test]
    fn test_smart_compact_keeps_recent_tool_results() {
        let mut t = MessageTranscript::new();
        // 构造多轮对话，每轮有一个工具调用
        for i in 0..8 {
            t.append(make_human(&format!("question {}", i)));
            t.append(make_ai_with_tool(
                "thinking...",
                "Bash",
                &format!("call_{}", i),
            ));
            t.append(make_tool_result(
                &format!("call_{}", i),
                &format!("output {}", i),
            ));
        }

        let config = CompactConfig::default();
        let affected = smart_compact(&mut t, &config);

        // 验证最后 3 条 Tool 结果保留
        let entries = t.entries();
        let mut tool_ids = Vec::new();
        for entry in entries.iter() {
            if matches!(
                &entry.message,
                BaseMessage::Tool {
                    is_error: false,
                    ..
                }
            ) {
                tool_ids.push(entry.message.id());
            }
        }
        assert!(
            tool_ids.len() >= 3,
            "至少应有 3 条 Tool 结果，实际: {}",
            tool_ids.len()
        );

        // 最后 3 条 Tool 不应被 truncated
        let last_3_tools = &tool_ids[tool_ids.len() - 3..];
        for id in last_3_tools {
            let flags = t.flags(*id);
            assert!(!flags.truncated, "最近 Tool 结果不应被 truncated");
        }

        assert!(affected > 0, "应有旧消息被标记");
    }

    #[test]
    fn test_smart_compact_keeps_error_tools() {
        let mut t = MessageTranscript::new();
        t.append(make_human("question"));
        t.append(make_error_tool_result("call_err", "command not found"));

        let config = CompactConfig::default();
        let affected = smart_compact(&mut t, &config);

        // 只有 2 条消息（1 Human + 1 error Tool），全在保留窗口内
        assert_eq!(affected, 0, "错误 Tool 应被保留");

        let entries = t.entries();
        let tool_flags = t.flags(entries[1].message.id());
        assert!(!tool_flags.truncated, "错误 Tool 不应被 truncated");
    }

    #[test]
    fn test_smart_compact_respects_ancestor_boundary() {
        let ancestor = make_human("ancestor message");
        let mut t = MessageTranscript::new().with_ancestor(vec![ancestor]);
        // 构造足够多的自有消息以触发截断
        for i in 0..10 {
            t.append(make_human(&format!("q {}", i)));
            t.append(make_ai(&format!("a {}", i)));
        }

        let config = CompactConfig::default();
        let affected = smart_compact(&mut t, &config);

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
        for i in 0..12 {
            t.append(make_human(&format!("q {}", i)));
            t.append(make_ai(&format!("a {}", i)));
        }

        let config = CompactConfig::default();
        let first = smart_compact(&mut t, &config);
        let second = smart_compact(&mut t, &config);
        assert!(first > 0, "首次调用应有消息被标记");
        assert_eq!(second, 0, "重复调用不应增加标记");
    }

    #[test]
    fn test_smart_compact_with_error_and_normal_tools() {
        let mut t = MessageTranscript::new();
        // 构造场景：多种消息混合
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

        let config = CompactConfig::default();
        let affected = smart_compact(&mut t, &config);

        let entries = t.entries();
        let recent_human_id = entries[entries.len() - 3].message.id();
        let recent_tool_id = entries[entries.len() - 1].message.id();
        let error_tool_id = entries[entries.len() - 5].message.id();

        // 最近消息应保留
        assert!(!t.flags(recent_human_id).truncated, "最近 Human 应保留");
        assert!(!t.flags(recent_tool_id).truncated, "最近 Tool 结果应保留");
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
        let affected = smart_compact(&mut t, &config);

        let entries = t.entries();
        let system_flags = t.flags(entries[0].message.id());
        assert!(!system_flags.truncated, "System 消息应永远保留");
        assert_eq!(affected, 0, "3 条消息全在保留窗口内");
    }
}
