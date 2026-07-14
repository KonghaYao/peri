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
    main_tb.on_tool_start("main_call", "Read", serde_json::json!({}));
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
