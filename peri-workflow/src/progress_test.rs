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
    let run = store.get_run("r1").unwrap();
    assert!(matches!(run.status, RunStatus::Completed));
    assert_eq!(
        run.post_processing_status,
        peri_acp_types::workflow::PostProcessingStatus::Unknown
    );
    assert_eq!(
        run.delivery_status,
        peri_acp_types::workflow::DeliveryStatus::Unknown
    );
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
    assert_eq!(
        run.delivery_status,
        peri_acp_types::workflow::DeliveryStatus::Blocked
    );
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

    let completed_at = store.get_run("done-run").unwrap().completed_at.unwrap();
    store.cleanup_completed_at(completed_at);
    assert!(store
        .runs
        .read()
        .get("done-run")
        .unwrap()
        .started_agents
        .contains(&7));
    store.cleanup_completed_at(completed_at + WorkflowProgressStore::COMPLETED_RETENTION);
    let runs = store.runs.read();
    assert!(
        !runs.contains_key("done-run"),
        "过期 run 与自身标记必须共同释放"
    );
    assert!(
        runs.get("running-run").unwrap().started_agents.contains(&9),
        "运行中标记保留"
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
    assert_eq!(
        fix.duration_ms, None,
        "resume 命中没有相位边界事件，墙钟未知；不得回落成 agent 时长求和"
    );
    let fallback = summaries
        .iter()
        .find(|summary| summary.name == "repair")
        .unwrap();
    assert_eq!(fallback.agent_count, 1);
    assert_eq!(fallback.token_count, 0);
    assert_eq!(fallback.duration_ms, None);
}

/// [回归测试] 保留完成 run 时必须同时保留本次执行标记，通知不能被清理变成零统计。
#[test]
fn test_completed_cleanup_preserves_phase_usage_until_run_expiration() {
    let store = make_store();
    let run_id = "retained";
    store.apply_event(&ProgressEvent::RunStarted {
        run_id: run_id.into(),
        workflow_name: "review".into(),
        meta: None,
    });
    store.apply_event(&ProgressEvent::AgentStarted {
        run_id: run_id.into(),
        agent_id: 1,
        label: None,
        phase: Some("Review".into()),
    });
    for (agent_id, tokens, duration) in [(1, 20, 40), (2, 900, 1800)] {
        store.apply_event(&ProgressEvent::AgentDone {
            run_id: run_id.into(),
            agent_id,
            label: None,
            phase: Some("Review".into()),
            result: AgentRunResult::Ok {
                output: serde_json::json!("done"),
                usage: Usage {
                    output_tokens: tokens,
                },
                model: None,
                tool_count: Some(3),
                token_count: Some(tokens),
                phase: Some("Review".into()),
                duration_ms: Some(duration),
            },
        });
    }
    store.apply_event(&ProgressEvent::RunDone {
        run_id: run_id.into(),
        status: "completed".into(),
        return_value: None,
        error: None,
    });
    let before = serde_json::to_value(store.get_run(run_id).unwrap()).unwrap();
    store.cleanup_completed();
    let summaries = store.get_phase_summaries(run_id);
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].name, "Review");
    assert_eq!(summaries[0].agent_count, 2);
    assert_eq!(
        summaries[0].token_count, 20,
        "仍保留的通知只统计本次真实执行"
    );
    assert_eq!(
        summaries[0].duration_ms, None,
        "该用例不发相位边界事件：墙钟不可得为 None，不是 agent 时长求和"
    );
    assert_eq!(
        serde_json::to_value(store.get_run(run_id).unwrap()).unwrap(),
        before
    );
    let completed_at = store.get_run(run_id).unwrap().completed_at.unwrap();
    store.cleanup_completed_at(
        completed_at + WorkflowProgressStore::COMPLETED_RETENTION
            - std::time::Duration::from_nanos(1),
    );
    assert_eq!(store.get_phase_summaries(run_id)[0].token_count, 20);
    store.cleanup_completed_at(completed_at + WorkflowProgressStore::COMPLETED_RETENTION);
    assert!(store.get_run(run_id).is_none());
    assert!(store.get_phase_summaries(run_id).is_empty());
    assert!(!store.runs.read().contains_key(run_id));
}

/// [回归测试] 相位耗时是墙钟（`PhaseStarted` → `PhaseDone`），不是相位内 agent
/// 时长求和：并行相位曾把求和读成「重叠 / 超长」（2026-10-11 issue 缺陷 3）。
#[test]
fn test_phase_summary_duration_is_wall_clock_not_agent_sum() {
    let store = make_store();
    let run_id = "wall-clock";
    store.apply_event(&ProgressEvent::RunStarted {
        run_id: run_id.into(),
        workflow_name: "review".into(),
        meta: None,
    });
    store.apply_event(&ProgressEvent::PhaseStarted {
        run_id: run_id.into(),
        phase: "Recon".into(),
    });
    // 两个并行 agent 各报 4s：求和是 8s，墙钟只有本用例的 sleep。
    for agent_id in [1, 2] {
        store.apply_event(&ProgressEvent::AgentStarted {
            run_id: run_id.into(),
            agent_id,
            label: None,
            phase: Some("Recon".into()),
        });
        store.apply_event(&ProgressEvent::AgentDone {
            run_id: run_id.into(),
            agent_id,
            label: None,
            phase: Some("Recon".into()),
            result: AgentRunResult::Ok {
                output: serde_json::json!("recon"),
                usage: Usage { output_tokens: 10 },
                model: None,
                tool_count: None,
                token_count: Some(10),
                phase: Some("Recon".into()),
                duration_ms: Some(4_000),
            },
        });
    }
    std::thread::sleep(std::time::Duration::from_millis(5));
    store.apply_event(&ProgressEvent::PhaseDone {
        run_id: run_id.into(),
        phase: "Recon".into(),
    });

    let summaries = store.get_phase_summaries(run_id);
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].agent_count, 2);
    let wall_clock = summaries[0]
        .duration_ms
        .expect("有相位边界事件时必须给出墙钟");
    assert!(
        wall_clock < 4_000,
        "墙钟不得等于/超过单个 agent 时长（求和会是 8000ms），实际 {wall_clock}ms"
    );
}

// ─── run 索引排序与观察者锁 ─────────────────────────────────

#[test]
fn run_index_is_ordered_by_start_time_not_hashmap_iteration() {
    let store = make_store();
    for (run_id, started_at_ms) in [("r_late", 200i64), ("r_early_b", 100), ("r_early_a", 100)] {
        store.apply_event(&ProgressEvent::RunStarted {
            run_id: run_id.into(),
            workflow_name: "w".into(),
            meta: None,
        });
        store
            .runs
            .write()
            .get_mut(run_id)
            .expect("run 已登记")
            .progress
            .started_at_ms = started_at_ms;
    }

    let runs = store.list_runs();
    let order: Vec<&str> = runs.iter().map(|run| run.run_id.as_str()).collect();
    assert_eq!(
        order,
        vec!["r_early_a", "r_early_b", "r_late"],
        "升序（旧→新），同毫秒按 run_id 定序，不得依赖 HashMap 迭代顺序"
    );
    assert_eq!(
        store.list_runs().last().map(|run| run.run_id.as_str()),
        Some("r_late"),
        "「最近 N 个 run」= 尾部 N（WorkflowStatus 依赖该约定）"
    );
    let snapshot: Vec<String> = store
        .get_all_runs_snapshot()
        .into_iter()
        .map(|run| run.run_id)
        .collect();
    assert_eq!(snapshot, vec!["r_early_a", "r_early_b", "r_late"]);
}

#[test]
fn run_started_records_wall_clock_start_time() {
    let store = make_store();
    let before = chrono::Utc::now().timestamp_millis();
    store.apply_event(&ProgressEvent::RunStarted {
        run_id: "r1".into(),
        workflow_name: "w".into(),
        meta: None,
    });
    let after = chrono::Utc::now().timestamp_millis();
    let started = store.get_run("r1").unwrap().started_at_ms;
    assert!(
        (before..=after).contains(&started),
        "started_at_ms 应落在本次调用窗口内，实际 {started}"
    );
}

/// 观察者回调期间不得持有 observer 读锁（否则回调内改注入会自锁）。
struct LockProbe {
    store: std::sync::Weak<WorkflowProgressStore>,
    calls: std::sync::atomic::AtomicUsize,
}

impl ProgressObserver for LockProbe {
    fn on_progress_event(&self, _event: &ProgressEvent) {
        let store = self.store.upgrade().expect("store 存活");
        assert!(
            store.observer.try_write().is_some(),
            "observer 回调不得持有读锁"
        );
        drop(store);
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[test]
fn progress_observer_is_called_without_holding_the_observer_lock() {
    let store = Arc::new(WorkflowProgressStore::new());
    let probe = Arc::new(LockProbe {
        store: Arc::downgrade(&store),
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    store.set_observer(probe.clone());
    store.apply_event(&ProgressEvent::Log {
        run_id: "r1".into(),
        message: "hello".into(),
    });
    assert_eq!(probe.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// 记录 `RunDone` 回调时刻 store 中该 run 的快照（用于断言投影已可见）。
struct ProjectionProbe {
    store: std::sync::Weak<WorkflowProgressStore>,
    seen: parking_lot::Mutex<Option<RunProgress>>,
}

impl ProgressObserver for ProjectionProbe {
    fn on_progress_event(&self, event: &ProgressEvent) {
        if matches!(event, ProgressEvent::RunDone { .. }) {
            let store = self.store.upgrade().expect("store 存活");
            *self.seen.lock() = store.get_run(event.run_id());
        }
    }
}

#[test]
fn run_done_projection_is_committed_before_the_observer_is_notified() {
    let store = Arc::new(WorkflowProgressStore::new());
    let probe = Arc::new(ProjectionProbe {
        store: Arc::downgrade(&store),
        seen: parking_lot::Mutex::new(None),
    });
    store.set_observer(probe.clone());
    store.apply_event(&ProgressEvent::RunStarted {
        run_id: "r1".into(),
        workflow_name: "w".into(),
        meta: None,
    });
    store.apply_run_done(
        "r1",
        "completed",
        None,
        None,
        peri_acp_types::workflow::PostProcessingStatus::Failed,
        peri_acp_types::workflow::DeliveryStatus::Blocked,
    );

    let seen = probe.seen.lock().clone().expect("RunDone 已回调");
    assert!(
        matches!(seen.status, RunStatus::Completed),
        "回调时刻终态已提交"
    );
    assert_eq!(
        seen.delivery_status,
        peri_acp_types::workflow::DeliveryStatus::Blocked,
        "投影与 RunDone 同一次提交，通知不得早于投影"
    );
    assert_eq!(
        seen.post_processing_status,
        peri_acp_types::workflow::PostProcessingStatus::Failed
    );
}
