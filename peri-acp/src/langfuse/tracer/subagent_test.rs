use super::*;

#[test]
fn test_empty_stack_returns_fallback_main() {
    let s = SubagentStack::new();
    assert_eq!(s.current_agent_id("main_obs"), "main_obs");
}

#[test]
fn test_begin_subagent_pushes_context() {
    let mut s = SubagentStack::new();
    s.begin_subagent(&serde_json::json!({"prompt": "go"}));
    assert_eq!(s.depth(), 1);
}

#[test]
fn test_current_agent_id_returns_top() {
    let mut s = SubagentStack::new();
    s.begin_subagent(&serde_json::json!({}));
    let top = s.current_agent_id("main");
    assert!(top.starts_with("obs_"));
    assert_ne!(top, "main");
}

#[test]
fn test_nested_subagent_stack_depth_2() {
    let mut s = SubagentStack::new();
    s.begin_subagent(&serde_json::json!({}));
    s.begin_subagent(&serde_json::json!({}));
    assert_eq!(s.depth(), 2);
}

#[test]
fn test_end_subagent_returns_context() {
    let mut s = SubagentStack::new();
    s.begin_subagent(&serde_json::json!({"prompt": "go"}));
    let end = s.end_subagent().expect("should return Some");
    assert!(end.observation_id.starts_with("obs_"));
    assert_eq!(s.depth(), 0);
}

#[test]
fn test_end_subagent_empty_returns_none() {
    let mut s = SubagentStack::new();
    assert!(s.end_subagent().is_none());
}

#[test]
fn test_is_agent_tool_anywhere_checks_main_and_stack() {
    let s = SubagentStack::new();
    let mut main_tb = ToolBatch::new();
    main_tb.on_tool_start("main_call", "Read", serde_json::json!({}), "span_stage_act");
    assert!(!s.is_agent_tool_anywhere(&main_tb, "main_call"));
    assert!(!s.is_agent_tool_anywhere(&main_tb, "nope"));
}

#[test]
fn test_current_tool_batch_mut_returns_main_when_empty() {
    let mut s = SubagentStack::new();
    let mut main_tb = ToolBatch::new();
    // 调用 current_tool_batch_mut 应该返回 main ToolBatch 引用
    let _ref = s.current_tool_batch_mut(&mut main_tb);
}

#[test]
fn test_lifo_order() {
    let mut s = SubagentStack::new();
    s.begin_subagent(&serde_json::json!({"id": 1}));
    s.begin_subagent(&serde_json::json!({"id": 2}));
    let _last_end = s.end_subagent().unwrap();
    let _first_end = s.end_subagent().unwrap();
    // 后进先出：last_end 应该是后压的（id=2）
}

// ── BUG 2: has_started 标记测试 ───────────────────────────────────────────

#[test]
fn test_has_started_flag_default_false() {
    let mut s = SubagentStack::new();
    s.begin_subagent(&serde_json::json!({"agent": "test"}));
    // begin 时 has_started 应为 false
    assert!(!s.top_has_started(), "begin 时 has_started 应为 false");
}

#[test]
fn test_mark_top_started_sets_flag() {
    let mut s = SubagentStack::new();
    s.begin_subagent(&serde_json::json!({"agent": "test"}));
    assert!(!s.top_has_started());
    s.mark_top_started();
    assert!(
        s.top_has_started(),
        "mark_top_started 后 has_started 应为 true"
    );
}

#[test]
fn test_top_has_started_returns_false_when_empty() {
    let s = SubagentStack::new();
    assert!(!s.top_has_started(), "栈空时 top_has_started 应返回 false");
}

#[test]
fn test_record_tool_output_stores_on_top() {
    let mut s = SubagentStack::new();
    s.begin_subagent(&serde_json::json!({"agent": "test"}));
    s.record_tool_output("bg agent completed successfully");
    let end = s.end_subagent().unwrap();
    assert_eq!(
        end.deferred_output.as_deref(),
        Some("bg agent completed successfully"),
        "deferred_output 应正确存储"
    );
}

#[test]
fn test_record_tool_output_noop_when_empty() {
    let mut s = SubagentStack::new();
    // 栈空时 record_tool_output 不应 panic
    s.record_tool_output("nothing");
    assert!(s.is_empty());
}

#[test]
fn test_mark_top_started_noop_when_empty() {
    let mut s = SubagentStack::new();
    // 栈空时 mark_top_started 不应 panic
    s.mark_top_started();
    assert!(s.is_empty());
}

// ── BUG 3: flush_all_subagent_tool_batches ───────────────────────────────

#[test]
fn test_flush_all_subagent_tool_batches_flushes_each_layer() {
    let mut s = SubagentStack::new();

    // 压入两层 subagent，每层各有一个工具
    s.begin_subagent(&serde_json::json!({"agent": "layer1"}));
    {
        let mut dummy = ToolBatch::new();
        let mut top_ref = s.current_tool_batch_mut(&mut dummy);
        top_ref.on_tool_start(
            "tc1",
            "Read",
            serde_json::json!({"path": "a.txt"}),
            "parent1",
        );
        top_ref.on_tool_end("tc1", "content1", false);
    }

    s.begin_subagent(&serde_json::json!({"agent": "layer2"}));
    {
        let mut dummy = ToolBatch::new();
        let mut top_ref = s.current_tool_batch_mut(&mut dummy);
        top_ref.on_tool_start(
            "tc2",
            "Write",
            serde_json::json!({"path": "b.txt"}),
            "parent2",
        );
        top_ref.on_tool_end("tc2", "ok", false);
    }

    assert_eq!(s.depth(), 2);

    // flush_all 应返回 2 个 flush 结果
    let flushes = s.flush_all_subagent_tool_batches();
    assert_eq!(flushes.len(), 2);
    assert_eq!(flushes[0].tools.len(), 1);
    assert_eq!(flushes[1].tools.len(), 1);
    assert_eq!(flushes[0].tools[0].name, "Read");
    assert_eq!(flushes[1].tools[0].name, "Write");
}

#[test]
fn test_flush_all_subagent_tool_batches_empty_stack() {
    let mut s = SubagentStack::new();
    let flushes = s.flush_all_subagent_tool_batches();
    assert!(flushes.is_empty());
}
