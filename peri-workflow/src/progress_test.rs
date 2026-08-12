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
        model: None,
        model_tier: None,
        token_count: Some(100),
        tool_count: Some(2),
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
        model: None,
        model_tier: None,
        token_count: Some(200),
        tool_count: Some(5),
    });
    // agent 1 进度
    store.apply_event(&ProgressEvent::AgentProgress {
        run_id: "r1".into(),
        agent_id: 1,
        label: None,
        phase: None,
        model: None,
        model_tier: None,
        token_count: Some(50),
        tool_count: Some(1),
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

#[test]
fn test_run_done_killed_sets_killed_status() {
    let store = make_store();
    store.apply_event(&ProgressEvent::RunStarted {
        run_id: "r1".into(),
        workflow_name: "test".into(),
        meta: None,
    });
    store.apply_event(&ProgressEvent::RunDone {
        run_id: "r1".into(),
        status: "killed".into(),
        return_value: None,
        error: Some("workflow killed by user".into()),
    });
    let run = store.get_run("r1").expect("run 应存在");
    assert!(matches!(run.status, RunStatus::Killed));
    // Killed 是终态：completed_at 必须设置（否则 cleanup_completed 永不清理）
    assert!(run.completed_at.is_some());
}

/// [回归测试] msg_loop failed 收尾补发的 RunDone{failed} 必须收敛为 Failed 终态
/// （issue 2026-08-05：Node 自然崩溃时 run 永久 Running 的 reducer 层锁定）。
#[test]
fn test_run_done_failed_sets_failed_status() {
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
        status: "failed".into(),
        return_value: None,
        error: Some("workflow process exited unexpectedly".into()),
    });
    let run = store.get_run("r1").expect("run 应存在");
    assert!(matches!(run.status, RunStatus::Failed));
    // Failed 是终态：completed_at 必须设置（否则 cleanup_completed 永不清理）
    assert!(run.completed_at.is_some());
}

#[test]
fn test_cleanup_completed_keeps_recently_killed_runs() {
    let store = make_store();
    store.apply_event(&ProgressEvent::RunStarted {
        run_id: "r1".into(),
        workflow_name: "test".into(),
        meta: None,
    });
    store.apply_event(&ProgressEvent::RunDone {
        run_id: "r1".into(),
        status: "killed".into(),
        return_value: None,
        error: None,
    });
    store.cleanup_completed();
    // 刚 killed 的 run 不应被清理（completed_at 在 5 分钟保留期内）
    assert!(store.get_run("r1").is_some());
}

/// [回归测试] cleanup_completed 必须同步清理已回收 run 的执行标记
/// （P2-2026-08-11：否则 started_agents 随完成 workflow 无界增长）。
#[test]
fn test_cleanup_completed_purges_started_agents_markers() {
    let store = make_store();
    store.apply_event(&ProgressEvent::RunStarted {
        run_id: "done-run".into(),
        workflow_name: "test".into(),
        meta: None,
    });
    store.apply_event(&ProgressEvent::AgentStarted {
        run_id: "done-run".into(),
        agent_id: 7,
        label: Some("work".into()),
        phase: Some("Work".into()),
    });
    store.apply_event(&ProgressEvent::RunDone {
        run_id: "done-run".into(),
        status: "completed".into(),
        return_value: None,
        error: None,
    });

    // 运行中的 run 标记必须保留
    store.apply_event(&ProgressEvent::RunStarted {
        run_id: "running-run".into(),
        workflow_name: "other".into(),
        meta: None,
    });
    store.apply_event(&ProgressEvent::AgentStarted {
        run_id: "running-run".into(),
        agent_id: 9,
        label: Some("active".into()),
        phase: Some("Run".into()),
    });

    store.cleanup_completed();
    let markers = store.started_agents.read();
    assert!(
        !markers.contains(&("done-run".to_string(), 7)),
        "已完成 run 的标记应被清理"
    );
    assert!(
        markers.contains(&("running-run".to_string(), 9)),
        "运行中 run 的标记必须保留"
    );
}

#[test]
fn test_run_done_unknown_run_is_noop() {
    let store = make_store();
    // 对不存在的 run_id 调 RunDone：不 panic、不创建条目
    store.apply_event(&ProgressEvent::RunDone {
        run_id: "missing".into(),
        status: "killed".into(),
        return_value: None,
        error: None,
    });
    assert!(store.get_run("missing").is_none());
    assert!(store.list_runs().is_empty());
}

/// [回归测试] model 投影：AgentProgress 携带 model 时更新（运行中可见），
/// None 不覆盖已设值；AgentDone 从 AgentRunResult::Ok.model 更新保证
/// 完成快照正确。
#[test]
fn test_model_projection_via_progress_and_done() {
    let store = make_store();
    store.apply_event(&ProgressEvent::RunStarted {
        run_id: "r1".into(),
        workflow_name: "test".into(),
        meta: None,
    });
    store.apply_event(&ProgressEvent::AgentStarted {
        run_id: "r1".into(),
        agent_id: 0,
        label: Some("coder".into()),
        phase: Some("Implement".into()),
    });

    // 运行中：模型信息专用更新（agent 侧 model_name 解析后），不得修改统计。
    store.apply_event(&ProgressEvent::AgentProgress {
        run_id: "r1".into(),
        agent_id: 0,
        label: None,
        phase: None,
        model: Some("claude-sonnet-4-5".into()),
        model_tier: Some("sonnet".into()),
        token_count: None,
        tool_count: None,
    });
    let agent = store
        .get_run("r1")
        .unwrap()
        .agents
        .get(&0)
        .cloned()
        .unwrap();
    assert_eq!(agent.model.as_deref(), Some("claude-sonnet-4-5"));
    assert_eq!(agent.model_tier.as_deref(), Some("sonnet"));
    assert!(matches!(agent.status, AgentStatus::Running));
    assert_eq!(agent.token_count, None);
    assert_eq!(agent.tool_count, None);

    // 后续不带 model 的进度事件不得覆盖已设值
    store.apply_event(&ProgressEvent::AgentProgress {
        run_id: "r1".into(),
        agent_id: 0,
        label: None,
        phase: None,
        model: None,
        model_tier: None,
        token_count: Some(10),
        tool_count: Some(1),
    });
    let agent = store
        .get_run("r1")
        .unwrap()
        .agents
        .get(&0)
        .cloned()
        .unwrap();
    assert_eq!(agent.model.as_deref(), Some("claude-sonnet-4-5"));
    // 后续不带 model_tier 的进度事件不得覆盖已设档位
    assert_eq!(agent.model_tier.as_deref(), Some("sonnet"));
    assert_eq!(agent.token_count, Some(10));
    assert_eq!(agent.tool_count, Some(1));

    // 引擎重试时第二次模型专用更新不得以 0 清空第一次尝试的统计。
    store.apply_event(&ProgressEvent::AgentProgress {
        run_id: "r1".into(),
        agent_id: 0,
        label: None,
        phase: None,
        model: Some("claude-sonnet-4-5".into()),
        model_tier: None,
        token_count: None,
        tool_count: None,
    });
    let agent = store
        .get_run("r1")
        .unwrap()
        .agents
        .get(&0)
        .cloned()
        .unwrap();
    assert_eq!(agent.token_count, Some(10));
    assert_eq!(agent.tool_count, Some(1));
    // model_tier 缺失不覆盖：重试更新不携带档位时保留首次解析的档位
    assert_eq!(agent.model_tier.as_deref(), Some("sonnet"));

    // 完成快照：以 AgentRunResult::Ok.model 为准（provider 实际模型名）
    store.apply_event(&ProgressEvent::AgentDone {
        run_id: "r1".into(),
        agent_id: 0,
        label: None,
        phase: None,
        result: AgentRunResult::Ok {
            output: serde_json::json!("implemented"),
            usage: Usage { output_tokens: 10 },
            model: Some("provider-resolved-model".into()),
            tool_count: None,
            token_count: None,
            phase: None,
            duration_ms: None,
        },
    });
    let agent = store
        .get_run("r1")
        .unwrap()
        .agents
        .get(&0)
        .cloned()
        .unwrap();
    assert_eq!(agent.model.as_deref(), Some("provider-resolved-model"));
    assert!(matches!(agent.status, AgentStatus::Done));
}

/// [回归测试] serde-optional：旧版 agent_progress JSON（无 model 字段）
/// 必须能反序列化（model → None）；Some 序列化保留、None 序列化省略。
#[test]
fn test_agent_progress_model_serde_optional() {
    let old_json = serde_json::json!({
        "type": "agent_progress",
        "runId": "r1",
        "agentId": 0,
        "tokenCount": 5,
        "toolCount": 1,
    });
    let event: ProgressEvent = serde_json::from_value(old_json).unwrap();
    match event {
        ProgressEvent::AgentProgress {
            model, model_tier, ..
        } => {
            assert_eq!(model, None);
            assert_eq!(model_tier, None);
        }
        _ => panic!("expected AgentProgress"),
    }

    let with_model = ProgressEvent::AgentProgress {
        run_id: "r1".into(),
        agent_id: 0,
        label: None,
        phase: None,
        model: Some("claude-sonnet-4-5".into()),
        model_tier: None,
        token_count: None,
        tool_count: None,
    };
    let json = serde_json::to_value(&with_model).unwrap();
    assert_eq!(json["model"], "claude-sonnet-4-5");

    let without_model = ProgressEvent::AgentProgress {
        run_id: "r1".into(),
        agent_id: 0,
        label: None,
        phase: None,
        model: None,
        model_tier: None,
        token_count: Some(0),
        tool_count: Some(0),
    };
    let json = serde_json::to_value(&without_model).unwrap();
    assert!(
        json.get("model").is_none(),
        "None model 应省略，实际: {json}"
    );
}

/// [回归测试] completed run resume 的 cache-hit 只有 AgentDone，不应把历史消耗
/// 计入本次运行；历史 result 中的 phase 仍应投影为可读分组。
#[test]
fn test_resume_cache_hit_excludes_historical_usage_and_preserves_phase() {
    let store = make_store();
    store.apply_event(&ProgressEvent::RunStarted {
        run_id: "resumed".into(),
        workflow_name: "repair".into(),
        meta: None,
    });
    store.apply_event(&ProgressEvent::AgentStarted {
        run_id: "resumed".into(),
        agent_id: 1,
        label: Some("new-work".into()),
        phase: Some("Fix".into()),
    });
    store.apply_event(&ProgressEvent::AgentDone {
        run_id: "resumed".into(),
        agent_id: 1,
        label: None,
        phase: None,
        result: AgentRunResult::Ok {
            output: serde_json::json!("new"),
            usage: Usage { output_tokens: 10 },
            model: None,
            tool_count: Some(1),
            token_count: Some(10),
            phase: Some("Fix".into()),
            duration_ms: Some(20),
        },
    });
    store.apply_event(&ProgressEvent::AgentDone {
        run_id: "resumed".into(),
        agent_id: 2,
        label: Some("cached-work".into()),
        phase: None,
        result: AgentRunResult::Ok {
            output: serde_json::json!("cached"),
            usage: Usage { output_tokens: 100 },
            model: None,
            tool_count: Some(4),
            token_count: Some(100),
            phase: Some("Fix".into()),
            duration_ms: Some(200),
        },
    });
    store.apply_event(&ProgressEvent::AgentDone {
        run_id: "resumed".into(),
        agent_id: 3,
        label: Some("unphased-cache".into()),
        phase: None,
        result: AgentRunResult::Ok {
            output: serde_json::json!("cached"),
            usage: Usage { output_tokens: 50 },
            model: None,
            tool_count: None,
            token_count: Some(50),
            phase: None,
            duration_ms: Some(80),
        },
    });

    let run = store.get_run("resumed").unwrap();
    let cached = run.agents.get(&2).unwrap();
    assert_eq!(cached.phase.as_deref(), Some("Fix"));
    assert_eq!(cached.token_count, None);
    assert_eq!(cached.duration_ms, None);

    let summaries = store.get_phase_summaries("resumed");
    assert_eq!(summaries.len(), 2);
    let fix = summaries
        .iter()
        .find(|summary| summary.name == "Fix")
        .unwrap();
    assert_eq!(fix.agent_count, 2);
    assert_eq!(fix.token_count, 10);
    assert_eq!(fix.duration_ms, Some(20));
    let fallback = summaries
        .iter()
        .find(|summary| summary.name == "repair")
        .unwrap();
    assert_eq!(fallback.agent_count, 1);
    assert_eq!(fallback.token_count, 0);
    assert_eq!(fallback.duration_ms, None);
}
