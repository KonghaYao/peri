use super::*;

fn dummy_parent() -> String {
    "span_stage_act_123".to_string()
}

#[test]
fn test_lazy_create_batch_span_on_first_start() {
    let mut tb = ToolBatch::new();
    let r = tb.on_tool_start("call_1", "Read", serde_json::json!({}), &dummy_parent());
    assert!(r.parent_span_id.starts_with("batch_") || r.parent_span_id.starts_with("agent_"));
    assert!(r.tool_span_id.starts_with("obs_"));
}

#[test]
fn test_second_start_shares_batch_span() {
    let mut tb = ToolBatch::new();
    let r1 = tb.on_tool_start("call_1", "Read", serde_json::json!({}), &dummy_parent());
    let r2 = tb.on_tool_start("call_2", "Write", serde_json::json!({}), &dummy_parent());
    assert_eq!(
        r1.parent_span_id, r2.parent_span_id,
        "同批次共享 batch span"
    );
}

#[test]
fn test_on_tool_end_returns_completed_tool() {
    let mut tb = ToolBatch::new();
    tb.on_tool_start("call_1", "Read", serde_json::json!({}), &dummy_parent());
    let completed = tb
        .on_tool_end("call_1", "file contents", false)
        .expect("should return Some");
    assert_eq!(completed.name, "Read");
    assert!(completed.span_id.starts_with("obs_"));
    assert_eq!(completed.output, "file contents");
    assert!(!completed.is_error);
}

#[test]
fn test_on_tool_end_unknown_returns_none() {
    let mut tb = ToolBatch::new();
    assert!(tb.on_tool_end("nope", "", false).is_none());
}

#[test]
fn test_flush_returns_batch_and_tools() {
    let parent = dummy_parent();
    let mut tb = ToolBatch::new();
    tb.on_tool_start("call_1", "Read", serde_json::json!({"path": "/x"}), &parent);
    tb.on_tool_end("call_1", "file content", false);
    tb.record_end_time("2026-07-14T10:00:00Z".into());
    let flush = tb.flush();
    assert!(flush.batch.is_some(), "应有 batch record");
    let batch = flush.batch.unwrap();
    assert!(batch.batch_span_id.starts_with("batch_"));
    assert_eq!(flush.tools.len(), 1, "应有 1 个已完成工具");
    assert_eq!(flush.tools[0].name, "Read");
    assert_eq!(flush.tools[0].output, "file content");
    assert!(!flush.tools[0].is_error);
    assert_eq!(
        flush.parent_observation_id, parent,
        "parent 应在 flush 时透传"
    );
    // 二次 flush 应返回 None batch + 空 tools
    let flush2 = tb.flush();
    assert!(flush2.batch.is_none(), "二次 flush batch 应为 None");
    assert!(flush2.tools.is_empty(), "二次 flush tools 应为空");
}

#[test]
fn test_is_agent_tool() {
    let mut tb = ToolBatch::new();
    tb.on_tool_start(
        "call_1",
        "Agent",
        serde_json::json!({"subagent": true}),
        &dummy_parent(),
    );
    assert!(tb.is_agent_tool("call_1"));
    assert!(!tb.is_agent_tool("nope"));
}

#[test]
fn test_is_empty() {
    let mut tb = ToolBatch::new();
    assert!(tb.is_empty());
    tb.on_tool_start("c1", "Read", serde_json::json!({}), &dummy_parent());
    assert!(!tb.is_empty());
}
