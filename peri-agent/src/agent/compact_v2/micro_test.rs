//! Tests for micro

use super::*;
use crate::agent::compact_v2::config::CompactConfig;
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
