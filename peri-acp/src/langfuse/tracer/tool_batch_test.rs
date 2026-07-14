use super::*;

#[test]
fn test_lazy_create_batch_span_on_first_start() {
    let mut tb = ToolBatch::new();
    let r = tb.on_tool_start("call_1", "Read", serde_json::json!({}));
    assert!(r.parent_span_id.starts_with("batch_") || r.parent_span_id.starts_with("agent_"));
    assert!(r.tool_span_id.starts_with("obs_"));
}

#[test]
fn test_second_start_shares_batch_span() {
    let mut tb = ToolBatch::new();
    let r1 = tb.on_tool_start("call_1", "Read", serde_json::json!({}));
    let r2 = tb.on_tool_start("call_2", "Write", serde_json::json!({}));
    assert_eq!(
        r1.parent_span_id, r2.parent_span_id,
        "同批次共享 batch span"
    );
}

#[test]
fn test_on_tool_end_returns_pending_tool() {
    let mut tb = ToolBatch::new();
    tb.on_tool_start("call_1", "Read", serde_json::json!({}));
    let pending = tb.on_tool_end("call_1").expect("should return Some");
    assert_eq!(pending.name, "Read");
}

#[test]
fn test_on_tool_end_unknown_returns_none() {
    let mut tb = ToolBatch::new();
    assert!(tb.on_tool_end("nope").is_none());
}

#[test]
fn test_flush_returns_batch_record_and_clears() {
    let mut tb = ToolBatch::new();
    tb.on_tool_start("call_1", "Read", serde_json::json!({}));
    tb.on_tool_end("call_1");
    tb.record_end_time("2026-07-14T10:00:00Z".into());
    let record = tb.flush().expect("should return Some");
    assert!(record.batch_span_id.starts_with("batch_"));
    assert!(tb.flush().is_none(), "二次 flush 应返回 None");
}

#[test]
fn test_is_agent_tool() {
    let mut tb = ToolBatch::new();
    tb.on_tool_start("call_1", "Agent", serde_json::json!({"subagent": true}));
    assert!(tb.is_agent_tool("call_1"));
    assert!(!tb.is_agent_tool("nope"));
}

#[test]
fn test_is_empty() {
    let mut tb = ToolBatch::new();
    assert!(tb.is_empty());
    tb.on_tool_start("c1", "Read", serde_json::json!({}));
    assert!(!tb.is_empty());
}
