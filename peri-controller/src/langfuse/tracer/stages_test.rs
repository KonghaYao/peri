use super::*;
use peri_agent::agent::events::{Stage, StageStatus};

#[test]
fn test_on_stage_start_returns_handle() {
    let mut s = StageSpans::new();
    let h = s.on_stage_start("main", Stage::Reason, "turn_1", "trace_1", "agent_obs");
    assert!(h.span_id.starts_with("span_"));
    assert_eq!(s.active_stage("main"), Some(Stage::Reason));
}

#[test]
fn test_on_stage_end_clears_active() {
    let mut s = StageSpans::new();
    let h = s.on_stage_start("main", Stage::Reason, "turn_1", "trace_1", "agent_obs");
    s.on_stage_end("main", &h, StageStatus::Done);
    assert_eq!(s.active_stage("main"), None);
}

#[test]
fn test_nested_stages_auto_finish_previous() {
    let mut s = StageSpans::new();
    let _h1 = s.on_stage_start("main", Stage::Receive, "turn_1", "trace_1", "agent_obs");
    let _h2 = s.on_stage_start("main", Stage::Reason, "turn_1", "trace_1", "agent_obs");
    assert_eq!(s.active_stage("main"), Some(Stage::Reason));
}

#[test]
fn test_double_end_early_return() {
    let mut s = StageSpans::new();
    let h = s.on_stage_start("main", Stage::Reason, "turn_1", "trace_1", "agent_obs");
    s.on_stage_end("main", &h, StageStatus::Done);
    s.on_stage_end("main", &h, StageStatus::Done); // 二次 end 不应 panic
}

#[test]
fn test_on_mq_drained_writes_to_receive() {
    let mut s = StageSpans::new();
    let _h = s.on_stage_start("main", Stage::Receive, "turn_1", "trace_1", "agent_obs");
    s.on_mq_drained("main", 2, 1, 0);
    assert_eq!(s.mq_counts("main"), Some((2, 1, 0)));
}

#[test]
fn test_on_mq_drained_outside_receive_no_op() {
    let mut s = StageSpans::new();
    let _h = s.on_stage_start("main", Stage::Reason, "turn_1", "trace_1", "agent_obs");
    s.on_mq_drained("main", 2, 1, 0);
    assert_eq!(s.mq_counts("main"), None);
}

#[test]
fn test_on_workflow_start_creates_child_span() {
    let mut s = StageSpans::new();
    let _h = s.on_stage_start("main", Stage::Act, "turn_1", "trace_1", "agent_obs");
    let w = s.on_workflow_start("wf_1", "plan summary");
    assert!(w.span_id.starts_with("span_"));
}

#[test]
fn test_on_workflow_start_outside_act_no_op() {
    let mut s = StageSpans::new();
    let _h = s.on_stage_start("main", Stage::Reason, "turn_1", "trace_1", "agent_obs");
    let w = s.on_workflow_start("wf_1", "plan");
    assert!(w.span_id.is_empty(), "Reason 阶段不应创建 workflow span");
}

#[test]
fn test_on_workflow_end_returns_stats() {
    let mut s = StageSpans::new();
    let _h = s.on_stage_start("main", Stage::Act, "turn_1", "trace_1", "agent_obs");
    s.on_workflow_start("wf_1", "plan");
    let end = s
        .on_workflow_end("wf_1", 3, 10)
        .expect("should return Some");
    assert_eq!(end.agents_spawned, 3);
    assert_eq!(end.tool_calls, 10);
}
