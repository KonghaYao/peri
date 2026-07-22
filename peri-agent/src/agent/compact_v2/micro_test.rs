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
    for i in 0..6 {
        t.append(make_human(&format!("question {}", i)));
        let ai_id = format!("call_{}", i);
        t.append(make_ai_with_tool("thinking...", "Bash", &ai_id));
        t.append(make_tool_result(&ai_id, &format!("output {}", i)));
    }

    let config = CompactConfig::default();
    let affected = micro_compact(&mut t, &config);
    // 第 0 轮的 tool_use + tool_result 都应被截断
    assert!(
        affected >= 2,
        "tool_use + tool_result 应被截断，实际: {}",
        affected
    );
}

#[test]
fn test_micro_compact_skips_error_tool_results() {
    let mut t = MessageTranscript::new();
    t.append(make_human("user question"));
    t.append(make_ai_with_tool("thinking...", "Bash", "call_1"));
    t.append(make_tool_result("call_1", "error output"));

    // 只有 1 轮，stale_steps=5 → 所有消息在窗口内，affected=0
    let config = CompactConfig::default();
    let affected = micro_compact(&mut t, &config);
    assert_eq!(affected, 0, "只有 1 轮，全在 stale 窗口内");
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
    for i in 0..7 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool("", "Bash", &format!("c_{}", i)));
        t.append(make_tool_result(&format!("c_{}", i), &format!("out {}", i)));
    }

    let config = CompactConfig::default();
    let first = micro_compact(&mut t, &config);
    let second = micro_compact(&mut t, &config);
    assert_eq!(second, 0, "重复调用不应增加标记");
    assert!(first > 0, "首次调用应有消息被标记");
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

#[test]
fn test_micro_compact_truncates_tool_use_arguments() {
    let mut t = MessageTranscript::new();
    // 构造足够多轮次，使第 0 轮的 Ai 消息被截断
    for i in 0..7 {
        t.append(make_human(&format!("q {}", i)));
        // Write 工具的 tool_use 有大量 arguments（如 file content）
        t.append(BaseMessage::ai_with_tool_calls(
            MessageContent::text("I'll write the file"),
            vec![crate::messages::ToolCallRequest::new(
                format!("call_{}", i),
                "Write",
                serde_json::json!({"file_path": "/tmp/test.txt", "content": "very long content here"}),
            )],
        ));
        t.append(make_tool_result(&format!("call_{}", i), "Wrote file"));
    }

    let config = CompactConfig::default();
    let affected = micro_compact(&mut t, &config);
    // 第 0-1 轮的 Ai (tool_use) + Tool (tool_result) 都应被截断
    assert!(
        affected >= 2,
        "tool_use + tool_result 应被截断，实际: {}",
        affected
    );

    // 确认第 0 条 Ai 消息被标 truncated（tool_use input）
    let ai_id = t.entries()[1].message.id();
    assert!(
        t.flags(ai_id).truncated,
        "Ai 消息（含 Write tool_use arguments）应被 truncated"
    );
}

#[test]
fn test_micro_compact_respects_blacklist() {
    // 将 Bash 加入黑名单——Bash tool_use 和 tool_result 都不应截断
    let config = CompactConfig {
        micro_excluded_tools: vec!["Bash".to_string()],
        ..Default::default()
    };

    let mut t = MessageTranscript::new();
    for i in 0..7 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool("", "Bash", &format!("bash_{}", i)));
        t.append(make_tool_result(
            &format!("bash_{}", i),
            &format!("bash output {}", i),
        ));
    }

    let affected = micro_compact(&mut t, &config);
    assert_eq!(affected, 0, "Bash 在黑名单中，不应被截断");
}

#[test]
fn test_micro_compact_blacklist_case_insensitive() {
    let config = CompactConfig {
        micro_excluded_tools: vec!["bash".to_string()],
        ..Default::default()
    };

    let mut t = MessageTranscript::new();
    for i in 0..7 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool("", "Bash", &format!("c_{}", i)));
        t.append(make_tool_result(&format!("c_{}", i), &format!("out {}", i)));
    }

    let affected = micro_compact(&mut t, &config);
    assert_eq!(affected, 0, "黑名单应大小写无关");
}

#[test]
fn test_micro_compact_low_affected_does_not_break_transcript() {
    let mut t = MessageTranscript::new();
    // 只有 3 轮，第 0 轮在 stale 窗口外，但只有 Human+Ai+Tool 共 3 条 → affected=3 < 5
    for i in 0..3 {
        t.append(make_human(&format!("q {}", i)));
        t.append(make_ai_with_tool("", "Bash", &format!("c_{}", i)));
        t.append(make_tool_result(&format!("c_{}", i), &format!("out {}", i)));
    }

    let config = CompactConfig {
        micro_compact_stale_steps: 1,
        ..Default::default()
    };
    let affected = micro_compact(&mut t, &config);
    assert!(affected > 0, "应有消息被截断");
    assert!(affected < 5, "affected 应 < 5（模拟 Micro 无效场景）");

    // 验证 truncated 消息仍然可见（后续 Full 可读完整内容生成摘要）
    let visible = t.visible_messages();
    assert_eq!(visible.len(), 9, "truncated 消息仍应全部可见");
}
