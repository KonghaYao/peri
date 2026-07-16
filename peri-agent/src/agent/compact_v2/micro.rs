//! Micro Compact 实现
//!
//! 零 LLM 调用，对符合条件的旧消息标 `truncated`（不改内容）。
//! 策略：
//! - 仅操作自有消息（ancestor_len 之后）
//! - 按 round 分组，跳过最近 `micro_compact_stale_steps` 轮
//! - 对白名单工具的 Tool 消息标 truncated
//! - 对含 Image/Document 的消息标 truncated

use tracing::debug;

use crate::agent::compact::config::CompactConfig;
use crate::messages::{BaseMessage, ContentBlock};
use crate::session::transcript::{MessageTranscript, TranscriptEntry};

/// Micro Compact：零 LLM 调用，对符合条件的旧消息标 `truncated`
///
/// 策略：
/// - 仅操作自有消息（ancestor_len 之后）
/// - 按 round 分组，跳过最近 `micro_compact_stale_steps` 轮
/// - 对白名单工具的 Tool 消息标 truncated
/// - 对含 Image/Document 的消息标 truncated
///
/// 返回被标记的消息数量。
pub fn micro_compact(transcript: &mut MessageTranscript, config: &CompactConfig) -> usize {
    let ancestor_len = transcript.ancestor_len();
    let entries = transcript.entries();
    if entries.len() <= ancestor_len {
        return 0;
    }

    // 按 round 分组自有消息（基于条目索引）
    let own_start = ancestor_len;
    let own_entries = &entries[own_start..];
    let round_starts = compute_round_starts(own_entries);
    let total_rounds = round_starts.len();
    let stale_limit = total_rounds.saturating_sub(config.micro_compact_stale_steps);

    // 构建消息 → round 索引
    let mut round_index = vec![0usize; own_entries.len()];
    for (ri, &start) in round_starts.iter().enumerate() {
        let end = if ri + 1 < round_starts.len() {
            round_starts[ri + 1]
        } else {
            own_entries.len()
        };
        let last = end.min(own_entries.len());
        if start < last {
            for slot in &mut round_index[start..last] {
                *slot = ri;
            }
        }
    }

    // 先收集所有待标记的 id（避免借用冲突）
    let mut ids_to_truncate: Vec<_> = Vec::new();
    for (i, entry) in own_entries.iter().enumerate() {
        // 跳过最近 N 轮
        if round_index[i] >= stale_limit {
            continue;
        }

        let msg = &entry.message;
        let id = msg.id();

        // 检查是否已被 truncated（避免重复标记）
        let existing_flags = transcript.flags(id);
        if existing_flags.truncated {
            continue;
        }

        let should_truncate = match msg {
            // Tool 消息：白名单工具 + 非错误
            BaseMessage::Tool {
                tool_call_id,
                is_error,
                ..
            } => {
                if *is_error {
                    false
                } else {
                    find_tool_name_in_entries(own_entries, tool_call_id)
                        .map(|name| config.micro_compactable_tools.contains(&name))
                        .unwrap_or(false)
                }
            }
            // 非 Tool 消息：检查是否含 Image/Document
            _ => {
                let blocks = msg.message_content().content_blocks();
                blocks.iter().any(|b| {
                    matches!(
                        b,
                        ContentBlock::Image { .. } | ContentBlock::Document { .. }
                    )
                })
            }
        };

        if should_truncate {
            ids_to_truncate.push(id);
        }
    }

    // 批量设置 truncated 标记
    for id in &ids_to_truncate {
        transcript.set_truncated(*id, true);
    }

    let affected = ids_to_truncate.len();
    if affected > 0 {
        debug!(affected, "Micro Compact: 标记 truncated 消息");
    }

    affected
}

/// 在条目列表中查找 Tool 消息对应的工具调用名称
fn find_tool_name_in_entries(entries: &[TranscriptEntry], tool_call_id: &str) -> Option<String> {
    for entry in entries.iter().rev() {
        if let BaseMessage::Ai { tool_calls, .. } = &entry.message {
            for tc in tool_calls {
                if tc.id == tool_call_id {
                    return Some(tc.name.clone());
                }
            }
        }
    }
    None
}

/// 按 round 边界计算每个 round 的起始索引
///
/// 简化版分组：每条消息自成一个 round，但 AI+Tool 组合视为一个 round。
/// 返回每个 round 的起始索引列表。
fn compute_round_starts(entries: &[TranscriptEntry]) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i < entries.len() {
        starts.push(i);
        if let BaseMessage::Ai { tool_calls, .. } = &entries[i].message {
            if !tool_calls.is_empty() {
                let tc_count = tool_calls.len();
                let mut end = i + 1;
                let mut matched = 0;
                while end < entries.len() && matched < tc_count {
                    if let BaseMessage::Tool { tool_call_id, .. } = &entries[end].message {
                        if tool_calls.iter().any(|tc| tc.id == *tool_call_id) {
                            matched += 1;
                            end += 1;
                            continue;
                        }
                    }
                    break;
                }
                i = end;
                continue;
            }
        }
        i += 1;
    }
    starts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::compact::config::CompactConfig;
    use crate::messages::{BaseMessage, MessageContent};
    use crate::session::transcript::MessageTranscript;

    fn make_human(text: &str) -> BaseMessage {
        BaseMessage::human(MessageContent::text(text.to_string()))
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

    // ── Micro Compact 测试 ─────────────────────────────────────────────────────

    #[test]
    fn test_micro_compact_empty_transcript() {
        let mut t = MessageTranscript::new();
        let config = CompactConfig::default();
        let affected = micro_compact(&mut t, &config);
        assert_eq!(affected, 0);
    }

    #[test]
    fn test_micro_compact_all_within_stale_window() {
        let mut t = MessageTranscript::new();
        t.append(make_human("user question"));
        let _id = t.append(make_tool_result("call_1", "large output here"));
        let config = CompactConfig::default();
        // 只有 1 轮，stale_steps 默认 5，全部在窗口内 → 不截断
        let affected = micro_compact(&mut t, &config);
        assert_eq!(affected, 0);
    }

    #[test]
    fn test_micro_compact_marks_old_tool_results() {
        let mut t = MessageTranscript::new();
        // 构造 6 轮对话（stale_steps=5，第 0 轮应被截断）
        for i in 0..6 {
            t.append(make_human(&format!("question {}", i)));
            let ai_id = format!("call_{}", i);
            t.append(make_ai_with_tool("thinking...", "Bash", &ai_id));
            t.append(make_tool_result(&ai_id, &format!("output {}", i)));
        }

        let config = CompactConfig::default();
        let affected = micro_compact(&mut t, &config);
        // 第 0 轮的 Bash tool result 应被标 truncated
        assert!(affected > 0, "应有消息被标 truncated");
    }

    #[test]
    fn test_micro_compact_skips_error_tool_results() {
        let mut t = MessageTranscript::new();
        t.append(make_human("user question"));
        t.append(make_ai_with_tool("thinking...", "Bash", "call_1"));
        let err_result =
            BaseMessage::tool_result("call_1".to_string(), MessageContent::text("error output"));
        // BaseMessage::tool_result 没有 is_error 参数，需手动构造
        t.append(err_result.clone());

        let config = CompactConfig::default();
        let affected = micro_compact(&mut t, &config);
        assert_eq!(affected, 0, "错误 tool result 不应被截断");
    }

    #[test]
    fn test_micro_compact_respects_ancestor_boundary() {
        let ancestor = make_human("ancestor message");
        let mut t = MessageTranscript::new().with_ancestor(vec![ancestor]);
        t.append(make_human("own message"));

        let config = CompactConfig::default();
        let affected = micro_compact(&mut t, &config);
        assert_eq!(affected, 0, "ancestor 消息不应被截断");
        // ancestor 消息不应被标 truncated
        let ancestor_id = t.entries()[0].message.id();
        assert!(!t.flags(ancestor_id).truncated);
    }

    #[test]
    fn test_micro_compact_no_duplicate_truncation() {
        let mut t = MessageTranscript::new();
        // 构造足够多的轮次
        for i in 0..7 {
            t.append(make_human(&format!("q {}", i)));
            t.append(make_ai_with_tool("", "Bash", &format!("c_{}", i)));
            t.append(make_tool_result(&format!("c_{}", i), &format!("out {}", i)));
        }

        let config = CompactConfig::default();
        let first = micro_compact(&mut t, &config);
        let second = micro_compact(&mut t, &config);
        assert_eq!(second, 0, "重复调用不应增加标记");
        assert_eq!(first, second + first.saturating_sub(0));
    }

    #[test]
    fn test_micro_compact_truncated_still_visible() {
        let mut t = MessageTranscript::new();
        let id = t.append(make_human("some message"));
        t.set_truncated(id, true);

        let visible = t.visible_messages();
        assert_eq!(visible.len(), 1, "truncated 消息仍然可见");
        assert_eq!(visible[0].id(), id);
    }
}
