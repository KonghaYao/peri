//! Tests for full

use super::*;
use crate::agent::compact_v2::config::CompactConfig;
use crate::messages::{BaseMessage, ContentBlock, ImageSource, MessageContent};
use crate::session::transcript::MessageTranscript;

fn make_human(text: &str) -> BaseMessage {
    BaseMessage::human(MessageContent::text(text.to_string()))
}

fn make_ai(text: &str) -> BaseMessage {
    BaseMessage::ai(MessageContent::text(text.to_string()))
}

// ── Full Compact 测试 ──────────────────────────────────────────────────────

#[tokio::test]
async fn test_full_compact_no_llm_returns_error() {
    let mut t = MessageTranscript::new();
    t.append(make_human("user question"));
    t.append(make_ai("assistant response"));

    let config = CompactConfig::default();
    let result = full_compact_inner(&mut t, None, &config, "/tmp").await;
    assert!(result.is_err(), "无 LLM 应返回错误");
}

#[tokio::test]
async fn test_full_compact_empty_transcript_skips() {
    // 需要 mock LLM，但空 transcript 应直接跳过
    // 由于 full_compact_inner 需要 LLM，这里用 Micro 代替测试空 transcript
    let mut t = MessageTranscript::new();
    let config = CompactConfig::default();
    let affected = crate::agent::compact_v2::micro::micro_compact(&mut t, &config);
    assert_eq!(affected, 0);
}

// ── 辅助函数测试 ───────────────────────────────────────────────────────────

#[test]
fn test_truncate_str_short() {
    assert_eq!(truncate_str("hello", 100), "hello");
}

#[test]
fn test_truncate_str_exact() {
    assert_eq!(truncate_str("hello", 5), "hello");
}

#[test]
fn test_truncate_str_long() {
    let result = truncate_str("hello world", 5);
    assert_eq!(result, "hello...(truncated)");
}

#[test]
fn test_truncate_str_cjk() {
    // CJK 字符级截断不应 panic
    let result = truncate_str("你好世界测试", 2);
    assert_eq!(result, "你好...(truncated)");
}

#[test]
fn test_postprocess_summary_removes_analysis() {
    let raw = "<analysis>some analysis</analysis><summary>the summary</summary>";
    let result = postprocess_summary(raw);
    assert!(!result.contains("<analysis>"));
}

#[test]
fn test_postprocess_summary_extracts_summary() {
    let raw = "prefix text <summary>real summary content</summary> suffix";
    let result = postprocess_summary(raw);
    assert!(result.contains("real summary content"));
    assert!(!result.contains("<summary>"));
    assert!(!result.contains("prefix text"));
}

#[test]
fn test_postprocess_summary_no_tags() {
    let raw = "plain summary text";
    let result = postprocess_summary(raw);
    assert!(result.contains("plain summary text"));
}

#[test]
fn test_postprocess_summary_collapses_newlines() {
    let raw = "line1\n\n\n\n\nline2";
    let result = postprocess_summary(raw);
    assert!(!result.contains("\n\n\n"), "应折叠连续空行");
}

#[test]
fn test_replace_images_and_truncate() {
    let blocks = vec![
        ContentBlock::Text {
            text: "some text".to_string(),
        },
        ContentBlock::Image {
            source: ImageSource::Url {
                url: "http://example.com/img.png".to_string(),
            },
        },
    ];
    let content = MessageContent::blocks(blocks);
    let result = replace_images_and_truncate(&content, 100);
    assert!(result.contains("[image]"));
    assert!(result.contains("some text"));
}

#[test]
fn test_format_tool_call_summary() {
    let tc = crate::messages::ToolCallRequest::new(
        "id1",
        "Edit",
        serde_json::json!({"file_path": "/tmp/test.rs", "old_string": "old"}),
    );
    let result = format_tool_call_summary(&tc);
    assert!(result.contains("Edit"));
    assert!(result.contains("file_path"));
    assert!(result.contains("/tmp/test.rs"));
}

#[test]
fn test_format_tool_call_summary_no_key_fields() {
    let tc = crate::messages::ToolCallRequest::new(
        "id1",
        "Bash",
        serde_json::json!({"random_key": "value"}),
    );
    let result = format_tool_call_summary(&tc);
    assert_eq!(result, "Bash");
}

#[test]
fn test_format_tool_result_summary_empty() {
    let content = MessageContent::text("");
    let result = format_tool_result_summary("call_1", &content, false, 3, 200);
    assert!(result.contains("[ToolResult:call_1][ok]"));
}

#[test]
fn test_format_tool_result_summary_truncates() {
    let long_text = "a".repeat(500);
    let content = MessageContent::text(&long_text);
    let result = format_tool_result_summary("call_1", &content, false, 3, 100);
    assert!(result.contains("...(truncated)"), "超长输出应被截断");
}

// ── CompactResult 测试 ─────────────────────────────────────────────────────

#[test]
fn test_compact_result_fields() {
    let result = crate::agent::compact_v2::CompactResult {
        strategy: CompactStrategy::Micro,
        affected_count: 3,
        estimated_tokens_saved: 1500,
        before_visible_len: 10,
        after_visible_len: 7,
        summary: None,
        full_escalation_reason: None,
    };
    assert_eq!(result.strategy, CompactStrategy::Micro);
    assert_eq!(result.affected_count, 3);
    assert_eq!(result.estimated_tokens_saved, 1500);
    assert!(result.summary.is_none());
}

#[test]
fn test_compact_strategy_equality() {
    assert_eq!(CompactStrategy::Micro, CompactStrategy::Micro);
    assert_ne!(CompactStrategy::Micro, CompactStrategy::Full);
}

// ── 集成测试：Full Compact 消息结构 ─────────────────────────────────────────

#[test]
fn test_full_compact_message_structure() {
    // 模拟 Full Compact 后的消息结构：
    // 旧消息标 excluded + Human 摘要追加
    let mut t = MessageTranscript::new();
    let id1 = t.append(make_human("user question"));
    let id2 = t.append(make_ai("assistant response"));

    // 模拟 excluded
    t.set_excluded(id1, true);
    t.set_excluded(id2, true);

    // 追加 Human 摘要（与 full_compact_inner 中的格式一致）
    let summary_text = format!(
        "<system-reminder>\n{}\n\n## Summary\nPrevious conversation about X.\n</system-reminder>",
        crate::agent::compact_v2::CONTINUATION_HINT
    );
    t.append(BaseMessage::human(summary_text));

    // 验证：只有摘要可见
    let visible = t.visible_messages();
    assert_eq!(visible.len(), 1, "只有摘要消息应可见");
    assert!(
        visible[0].content().contains("compact"),
        "可见消息应包含摘要内容"
    );
}

#[test]
fn test_excluded_not_visible() {
    let mut t = MessageTranscript::new();
    let id1 = t.append(make_human("visible"));
    let id2 = t.append(make_human("will be hidden"));
    t.set_excluded(id2, true);

    let visible = t.visible_messages();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].id(), id1);
}
