use super::*;
use crate::protocol::{AgentRunResult, ProgressEvent, Usage};

fn make_store() -> WorkflowProgressStore {
    WorkflowProgressStore::new()
}

#[test]
fn test_run_started_creates_run() {
    let store = make_store();
    store.apply_event(&ProgressEvent::RunStarted {
        run_id: "r1".into(),
        workflow_name: "test".into(),
        meta: None,
    });
    let run = store.get_run("r1").expect("run 应存在");
    assert_eq!(run.run_id, "r1");
    assert_eq!(run.workflow_name, "test");
    assert!(matches!(run.status, RunStatus::Running));
    assert!(run.agents.is_empty());
    assert!(run.phases.is_empty());
}

#[test]
fn test_agent_lifecycle_started_progress_done() {
    let store = make_store();
    // 启动 run
    store.apply_event(&ProgressEvent::RunStarted {
        run_id: "r1".into(),
        workflow_name: "test".into(),
        meta: None,
    });
    // agent 启动
    store.apply_event(&ProgressEvent::AgentStarted {
        run_id: "r1".into(),
        agent_id: 0,
        label: Some("review".into()),
        phase: Some("Review".into()),
    });
    // agent 进度
    store.apply_event(&ProgressEvent::AgentProgress {
        run_id: "r1".into(),
        agent_id: 0,
        label: None,
        phase: None,
        token_count: 100,
        tool_count: 2,
    });
    // agent 完成
    store.apply_event(&ProgressEvent::AgentDone {
        run_id: "r1".into(),
        agent_id: 0,
        label: None,
        phase: None,
        result: AgentRunResult::Ok {
            output: serde_json::json!("done"),
            usage: Usage { output_tokens: 50 },
            model: None,
            tool_count: None,
            token_count: None,
            phase: None,
            duration_ms: None,
        },
    });
    let run = store.get_run("r1").expect("run 应存在");
    assert_eq!(run.agents.len(), 1);
    let agent = run.agents.get(&0).expect("agent 0 应存在");
    assert_eq!(agent.agent_id, 0);
    assert_eq!(agent.label.as_deref(), Some("review"));
    assert_eq!(agent.phase.as_deref(), Some("Review"));
    assert!(matches!(agent.status, AgentStatus::Done));
    assert_eq!(agent.token_count, Some(100));
    assert_eq!(agent.tool_count, Some(2));
    assert!(agent.result.is_some());
}

#[test]
fn test_concurrent_agents_no_race() {
    let store = make_store();
    // 启动 run
    store.apply_event(&ProgressEvent::RunStarted {
        run_id: "r1".into(),
        workflow_name: "test".into(),
        meta: None,
    });
    // agent 0 启动
    store.apply_event(&ProgressEvent::AgentStarted {
        run_id: "r1".into(),
        agent_id: 0,
        label: Some("coder".into()),
        phase: Some("Implement".into()),
    });
    // agent 1 启动（并发）
    store.apply_event(&ProgressEvent::AgentStarted {
        run_id: "r1".into(),
        agent_id: 1,
        label: Some("reviewer".into()),
        phase: Some("Review".into()),
    });
    // agent 0 进度（交错事件）
    store.apply_event(&ProgressEvent::AgentProgress {
        run_id: "r1".into(),
        agent_id: 0,
        label: None,
        phase: None,
        token_count: 200,
        tool_count: 5,
    });
    // agent 1 进度
    store.apply_event(&ProgressEvent::AgentProgress {
        run_id: "r1".into(),
        agent_id: 1,
        label: None,
        phase: None,
        token_count: 50,
        tool_count: 1,
    });
    // agent 1 完成（先完成）
    store.apply_event(&ProgressEvent::AgentDone {
        run_id: "r1".into(),
        agent_id: 1,
        label: None,
        phase: None,
        result: AgentRunResult::Ok {
            output: serde_json::json!("approved"),
            usage: Usage { output_tokens: 30 },
            model: None,
            tool_count: None,
            token_count: None,
            phase: None,
            duration_ms: None,
        },
    });
    // agent 0 完成
    store.apply_event(&ProgressEvent::AgentDone {
        run_id: "r1".into(),
        agent_id: 0,
        label: None,
        phase: None,
        result: AgentRunResult::Ok {
            output: serde_json::json!("implemented"),
            usage: Usage { output_tokens: 100 },
            model: None,
            tool_count: None,
            token_count: None,
            phase: None,
            duration_ms: None,
        },
    });

    let run = store.get_run("r1").expect("run 应存在");
    assert_eq!(run.agents.len(), 2);

    // agent 0：验证精确匹配，不被 LIFO 搞乱
    let agent0 = run.agents.get(&0).expect("agent 0 应存在");
    assert_eq!(agent0.label.as_deref(), Some("coder"));
    assert_eq!(agent0.token_count, Some(200));
    assert_eq!(agent0.tool_count, Some(5));
    assert!(matches!(agent0.status, AgentStatus::Done));

    // agent 1
    let agent1 = run.agents.get(&1).expect("agent 1 应存在");
    assert_eq!(agent1.label.as_deref(), Some("reviewer"));
    assert_eq!(agent1.token_count, Some(50));
    assert_eq!(agent1.tool_count, Some(1));
    assert!(matches!(agent1.status, AgentStatus::Done));
}

#[test]
fn test_run_done_updates_status() {
    let store = make_store();
    store.apply_event(&ProgressEvent::RunStarted {
        run_id: "r1".into(),
        workflow_name: "test".into(),
        meta: None,
    });
    assert!(matches!(
        store.get_run("r1").unwrap().status,
        RunStatus::Running
    ));

    store.apply_event(&ProgressEvent::RunDone {
        run_id: "r1".into(),
        status: "completed".into(),
        return_value: None,
        error: None,
    });
    assert!(matches!(
        store.get_run("r1").unwrap().status,
        RunStatus::Completed
    ));
}

#[test]
fn test_cleanup_completed_keeps_running_runs() {
    let store = make_store();
    store.apply_event(&ProgressEvent::RunStarted {
        run_id: "r1".into(),
        workflow_name: "test".into(),
        meta: None,
    });
    store.cleanup_completed();
    // Running 状态的 run 始终保留
    assert!(store.get_run("r1").is_some());
}

#[test]
fn test_cleanup_completed_keeps_recently_completed_runs() {
    let store = make_store();
    store.apply_event(&ProgressEvent::RunStarted {
        run_id: "r1".into(),
        workflow_name: "test".into(),
        meta: None,
    });
    // 刚完成 → completed_at 为当前时间，应在保留期内
    store.apply_event(&ProgressEvent::RunDone {
        run_id: "r1".into(),
        status: "completed".into(),
        return_value: None,
        error: None,
    });
    store.cleanup_completed();
    // 刚完成的 run 不应被清理（completed_at 在 5 分钟保留期内）
    assert!(store.get_run("r1").is_some());
}
