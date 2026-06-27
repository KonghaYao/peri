//! WorkflowPanel 单元测试——聚焦 phase → agent 筛选联动。

use super::*;

fn make_agent(id: u64, phase: Option<&str>) -> WorkflowAgentSnapshot {
    WorkflowAgentSnapshot {
        agent_id: id,
        label: Some(format!("agent-{id}")),
        phase: phase.map(str::to_string),
        status: "running".to_string(),
        token_count: None,
        tool_count: None,
    }
}

fn make_phase(title: &str) -> WorkflowPhaseSnapshot {
    WorkflowPhaseSnapshot {
        title: title.to_string(),
        status: "running".to_string(),
    }
}

fn make_run(phases: Vec<&str>, agents: Vec<WorkflowAgentSnapshot>) -> WorkflowRunSnapshot {
    WorkflowRunSnapshot {
        run_id: "run-1".to_string(),
        workflow_name: "test".to_string(),
        status: "running".to_string(),
        phases: phases.into_iter().map(make_phase).collect(),
        agents,
    }
}

#[test]
fn test_filtered_agent_count_by_selected_phase() {
    let run = make_run(
        vec!["Review", "Fix"],
        vec![
            make_agent(1, Some("Review")),
            make_agent(2, Some("Review")),
            make_agent(3, Some("Fix")),
        ],
    );
    let mut panel = WorkflowPanel::new(vec![run]);
    // 默认 phase_cursor=0 → "Review"
    assert_eq!(panel.filtered_agent_count(), 2);
    // 切到 "Fix"
    panel.move_phase_cursor(1);
    assert_eq!(panel.filtered_agent_count(), 1);
}

#[test]
fn test_filtered_agent_count_no_phases_shows_all() {
    let run = make_run(
        vec![],
        vec![
            make_agent(1, None),
            make_agent(2, None),
            make_agent(3, None),
        ],
    );
    let panel = WorkflowPanel::new(vec![run]);
    // run 无 phases → 无法筛选，显示全部 agent
    assert_eq!(panel.filtered_agent_count(), 3);
}

#[test]
fn test_move_phase_cursor_resets_agent_cursor() {
    let run = make_run(
        vec!["Review", "Fix"],
        vec![
            make_agent(1, Some("Review")),
            make_agent(2, Some("Review")),
            make_agent(3, Some("Fix")),
        ],
    );
    let mut panel = WorkflowPanel::new(vec![run]);
    // 把 agent_cursor 推到 1（Review 阶段下第 2 个 agent）
    panel.move_agent_cursor(1);
    assert_eq!(panel.agent_cursor, 1);
    // 切换 phase 必须重置 agent_cursor，否则索引会落到错误 agent 上
    panel.move_phase_cursor(1);
    assert_eq!(panel.agent_cursor, 0);
    // 此时选中 Fix 阶段的唯一 agent（id=3）
    assert_eq!(panel.selected_agent(), Some(("run-1".to_string(), 3)));
}

#[test]
fn test_selected_agent_uses_filtered_index() {
    let run = make_run(
        vec!["Review", "Fix"],
        vec![
            make_agent(10, Some("Review")),
            make_agent(20, Some("Fix")),
            make_agent(30, Some("Fix")),
        ],
    );
    let mut panel = WorkflowPanel::new(vec![run]);
    // 选中 Fix 阶段（index 1），其 agents 在原始列表中是 [20, 30]
    panel.move_phase_cursor(1);
    // agent_cursor=0 → 第 1 个 Fix agent（id=20，不是原始列表的 index 0=10）
    assert_eq!(panel.selected_agent(), Some(("run-1".to_string(), 20)));
    // agent_cursor=1 → 第 2 个 Fix agent（id=30）
    panel.move_agent_cursor(1);
    assert_eq!(panel.selected_agent(), Some(("run-1".to_string(), 30)));
}

#[test]
fn test_clamp_cursors_uses_filtered_count() {
    let run = make_run(
        vec!["Review", "Fix"],
        vec![
            make_agent(1, Some("Review")),
            make_agent(2, Some("Review")),
            make_agent(3, Some("Fix")),
        ],
    );
    let mut panel = WorkflowPanel::new(vec![run]);
    // 在 Review 下把 agent_cursor 推到 1（合法）
    panel.move_agent_cursor(1);
    assert_eq!(panel.agent_cursor, 1);
    // 切到 Fix（仅 1 个 agent），不主动 reset，模拟数据刷新触发 clamp
    panel.phase_cursor = 1;
    panel.agent_cursor = 5; // 越界
    panel.clamp_cursors();
    // Fix 阶段只有 1 个 agent，agent_cursor 必须钳位到 0
    assert_eq!(panel.agent_cursor, 0);
}

#[test]
fn test_selected_agent_returns_none_when_phase_has_no_agents() {
    let run = make_run(
        vec!["Review", "Empty", "Fix"],
        vec![make_agent(1, Some("Review")), make_agent(2, Some("Fix"))],
    );
    let mut panel = WorkflowPanel::new(vec![run]);
    // 切到没有 agent 的 Empty 阶段
    panel.move_phase_cursor(1);
    assert_eq!(panel.filtered_agent_count(), 0);
    assert!(panel.selected_agent().is_none());
}
