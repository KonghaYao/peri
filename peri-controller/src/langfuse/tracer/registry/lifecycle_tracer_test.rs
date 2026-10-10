use super::SubagentStatus;
use crate::langfuse::config::LangfuseConfig;
use crate::langfuse::fake_session::FakeLangfuseSession;
use crate::langfuse::tracer::LangfuseTracer;
use langfuse_client::types::ObservationType;
use langfuse_client::IngestionEvent;
use peri_agent::agent::events::Stage;
use peri_agent::agent::events::StageStatus;

fn make_tracer(
    rate: f64,
) -> (
    LangfuseTracer,
    std::sync::Arc<crate::langfuse::fake_session::FakeLangfuseSession>,
) {
    let session = FakeLangfuseSession::new("sess_registry");
    let config = LangfuseConfig {
        public_key: None,
        secret_key: None,
        host: "https://cloud.langfuse.com".to_string(),
        trace_sampling: rate,
        error_span_always: true,
        batch_max_events: 50,
        batch_flush_interval_secs: 10,
        user_id: None,
        ..Default::default()
    };
    let t = LangfuseTracer::new(session.clone(), "sess_registry".to_string(), config);
    (t, session)
}

fn agent_obs_creates(events: &[IngestionEvent]) -> Vec<(String, Option<String>, Option<String>)> {
    events
        .iter()
        .filter_map(|e| {
            if let IngestionEvent::ObservationCreate { body, .. } = e {
                if body.r#type == ObservationType::Agent
                    && body
                        .name
                        .as_deref()
                        .is_some_and(|name| name.starts_with("subagent-"))
                {
                    return Some((
                        body.id.clone().unwrap_or_default(),
                        body.parent_observation_id.clone(),
                        body.start_time.clone(),
                    ));
                }
            }
            None
        })
        .collect()
}

fn agent_obs_completions(
    events: &[IngestionEvent],
) -> Vec<(String, Option<String>, Option<serde_json::Value>)> {
    events
        .iter()
        .filter_map(|e| {
            if let IngestionEvent::ObservationCreate { body, .. } = e {
                if body.r#type == ObservationType::Agent
                    && body
                        .name
                        .as_deref()
                        .is_some_and(|name| name.starts_with("subagent-"))
                {
                    return Some((
                        body.id.clone().unwrap_or_default(),
                        body.end_time.clone(),
                        body.output.clone(),
                    ));
                }
            }
            None
        })
        .collect()
}

fn cached_agent(
    tracer: &LangfuseTracer,
    agent_id: &str,
) -> (String, Option<String>, Option<String>) {
    let cached = tracer.subagent.by_agent_id.get(agent_id).unwrap();
    (
        cached.observation_id.clone(),
        Some(cached.parent_observation_id.clone()),
        Some(cached.start_time.clone()),
    )
}

fn child_events(events: &[IngestionEvent]) -> Vec<(String, Option<String>, Option<String>)> {
    events
        .iter()
        .filter_map(|e| match e {
            IngestionEvent::SpanCreate { body, .. } => Some((
                body.id.clone().unwrap_or_default(),
                body.start_time.clone(),
                body.parent_observation_id.clone(),
            )),
            IngestionEvent::GenerationCreate { body, .. } => Some((
                body.id.clone().unwrap_or_default(),
                body.start_time.clone(),
                body.parent_observation_id.clone(),
            )),
            _ => None,
        })
        .collect()
}

fn parse_time(s: &str) -> chrono::DateTime<chrono::FixedOffset> {
    chrono::DateTime::parse_from_rfc3339(s).expect("rfc3339 时间")
}

// ── 8 步全生命周期 ──────────────────────────────────────────────────────────

/// ①父ToolStart(Agent) → ②SubagentStart → ③child StageStarted →
/// ④child LlmCallStart/End → ⑤child ToolStart/End → ⑥父ToolEnded →
/// ⑦SubagentStop → ⑧on_turn_end
///
/// 断言要点:
/// - ②创建 AGENT obs 且 parent = ①冻结的父 stage span id
/// - ③stage parent = ②的 obs id;④generation parent = ③的 span id
/// - ⑤工具挂 child 自己的 tool-batch(parent 链到 ③)
/// - ⑥只结束父工具记录、AGENT obs 未关闭
/// - ⑦关闭 AGENT obs,end ≥ 最晚 child 事件 start
/// - ⑧无残留;**无 17ms 空壳**(AGENT start ≤ 最早 child 事件,且有子节点)
#[tokio::test]
async fn test_full_lifecycle_eight_steps() {
    let (mut t, session) = make_tracer(1.0);
    t.set_main_agent_id("main".to_string());
    t.on_turn_start("turn_1");

    // 主 agent Act stage(父 stage span)
    let main_act = t
        .on_stage_start_gated("main", Stage::Act, "turn_1")
        .expect("主 agent stage 应创建 handle");
    let main_act_span = main_act.span_id.clone();

    // ① 父 ToolStart(Agent):invocation 登记,parent 冻结为 main_act_span
    t.on_tool_start(
        "main",
        "call_agent",
        "Agent",
        &serde_json::json!({"prompt": "review"}),
    );
    // ② SubagentStart:join 成功 → AGENT obs 开始快照缓存
    t.on_subagent_start("main", "child_1", "code-reviewer", false);

    let events = session.events_snapshot();
    let creates = agent_obs_creates(&events);
    assert_eq!(creates.len(), 0, "join 只缓存开始记录，不导出未完成观测");
    let cached = cached_agent(&t, "child_1");
    let (child_obs_id, child_parent, child_start) = &cached;
    assert_eq!(
        child_parent.as_deref(),
        Some(main_act_span.as_str()),
        "AGENT obs parent 应为 join 时冻结的父 stage span id"
    );

    // ③ child StageStarted
    let child_reason = t
        .on_stage_start_gated("child_1", Stage::Reason, "turn_1")
        .expect("child stage 应创建 handle");
    assert_eq!(
        child_reason.parent_observation_id, *child_obs_id,
        "child stage parent 应为 child AGENT obs id"
    );
    // 确保 stage duration > 0(v2 条件上报:0ms stage span 不上报)
    std::thread::sleep(std::time::Duration::from_millis(2));

    // ④ child LlmCallStart/End
    t.on_llm_start("child_1", 0, &[], &[]);
    t.on_llm_end(
        "child_1",
        0,
        "claude-4.7",
        "anthropic",
        "analysis done",
        None,
        None,
    );

    // ⑤ child ToolStart/End
    t.on_tool_start(
        "child_1",
        "call_bash",
        "Bash",
        &serde_json::json!({"cmd": "ls"}),
    );
    t.on_tool_end("child_1", "call_bash", "file list", false);
    t.on_stage_end("child_1", &child_reason, StageStatus::Done);

    // ⑥ 父 ToolEnded:只结束父工具记录,AGENT obs 未关闭
    t.on_tool_end("main", "call_agent", "subagent dispatched", false);
    assert_eq!(
        agent_obs_completions(&session.events_snapshot()).len(),
        0,
        "⑥ 父 ToolEnded 不应关闭 AGENT obs(生命周期由 Stop 驱动)"
    );
    assert_eq!(
        t.subagent.status_of("child_1"),
        Some(&SubagentStatus::Active),
        "⑥ 后 child 应仍 Active"
    );

    // ⑦ SubagentStop:关闭 AGENT obs
    t.on_subagent_stop("main", "child_1", "review complete", false);
    let completions = agent_obs_completions(&session.events_snapshot());
    assert_eq!(completions.len(), 1, "⑦ 后应有恰好 1 个 AGENT obs close");
    assert_eq!(
        completions[0].0, *child_obs_id,
        "关闭的 obs 应为 child 的 AGENT obs"
    );

    // ⑧ on_turn_end:无残留
    let _h = t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed);
    tokio::task::yield_now().await;
    let final_events = session.events_snapshot();

    // 图断言:child 内容全部挂 child AGENT obs 链,不挂 agent-run
    let all = child_events(&final_events);
    let child_spans: Vec<_> = all
        .iter()
        .filter(|(_, _, p)| *p == Some(child_obs_id.clone()))
        .collect();
    assert!(
        !child_spans.is_empty(),
        "child stage/llm 应挂 child AGENT obs"
    );

    // 无 17ms 空壳:AGENT obs 有子节点,且 start ≤ 最早 child 事件 start
    let child_event_times: Vec<_> = all
        .iter()
        .filter(|(_, _, p)| *p == Some(child_obs_id.clone()))
        .filter_map(|(_, s, _)| s.as_ref().map(|s| parse_time(s)))
        .collect();
    assert!(
        !child_event_times.is_empty(),
        "child AGENT obs 应有子节点(非空壳)"
    );
    let earliest_child = child_event_times.iter().min().unwrap();
    let agent_start = parse_time(child_start.as_ref().expect("AGENT start"));
    assert!(
        agent_start <= *earliest_child,
        "AGENT obs start(join 时刻)应 ≤ 最早 child 事件(无 17ms 空壳)"
    );

    // 关闭时间 ≥ 最晚 child 事件
    let latest_child = child_event_times.iter().max().unwrap();
    let agent_end = parse_time(completions[0].1.as_ref().expect("AGENT end"));
    assert!(
        agent_end >= *latest_child,
        "AGENT obs end(Stop 时刻)应 ≥ 最晚 child 事件"
    );

    // 每 obs 至多一个 parent + 无环(递归查 parent 不得回到自身)
    assert_eq!(
        t.subagent.status_of("child_1"),
        Some(&SubagentStatus::Closed)
    );
    assert_eq!(t.subagent.incomplete_count(), 0);
}

// ── Start 后于 ToolEnded ────────────────────────────────────────────────────

/// ①ToolStart → ②ToolEnded → ③SubagentStart → ④child events → ⑤SubagentStop
/// ②只结束父工具记录,**不关闭任何 child、不注销映射**;③join 未绑定 invocation
/// 仍成功(① 建的 invocation 在 ② 仅标 tool_ended,保留等 Stop);④ parent 正确;
/// ⑤两信号齐备(tool_ended + stop)→ 关闭 AGENT obs。
#[tokio::test]
async fn test_start_after_tool_ended() {
    let (mut t, session) = make_tracer(1.0);
    t.set_main_agent_id("main".to_string());
    t.on_turn_start("turn_1b");

    let main_act = t
        .on_stage_start_gated("main", Stage::Act, "turn_1b")
        .unwrap();
    let main_span = main_act.span_id.clone();
    // ① 父 ToolStart(Agent):invocation 登记,parent 冻结为 main_span
    t.on_tool_start("main", "call_agent", "Agent", &serde_json::json!({}));
    // ② 父 ToolEnded:只结束父工具记录,不关闭任何 child、不注销映射
    t.on_tool_end("main", "call_agent", "dispatched", false);
    assert_eq!(
        agent_obs_completions(&session.events_snapshot()).len(),
        0,
        "② 不应创建/关闭任何 AGENT obs"
    );
    assert_eq!(
        t.subagent.by_agent_id_len(),
        0,
        "② 不应有 subagent 注册(映射未注销)"
    );

    // ③ SubagentStart:invocation 仍可 join(② 仅标 tool_ended)
    t.on_subagent_start("main", "child_1", "fork", false);
    let events = session.events_snapshot();
    let creates = agent_obs_creates(&events);
    assert_eq!(creates.len(), 0, "join 不导出未完成观测");
    let cached = cached_agent(&t, "child_1");
    let (child_obs_id, child_parent, _) = &cached;
    assert_eq!(
        child_parent.as_deref(),
        Some(main_span.as_str()),
        "AGENT obs parent 应为 ① 冻结的父 stage span"
    );
    assert_eq!(
        t.subagent.status_of("child_1"),
        Some(&SubagentStatus::Active),
        "③ join 后 child 应 Active(ToolEnded 不提前关闭)"
    );

    // ④ child events:parent 正确(挂 child AGENT obs)
    let child_reason = t
        .on_stage_start_gated("child_1", Stage::Reason, "turn_1b")
        .expect("child stage 应创建 handle");
    assert_eq!(child_reason.parent_observation_id, *child_obs_id);
    // 确保 stage duration > 0(v2 条件上报:0ms stage span 不上报)
    std::thread::sleep(std::time::Duration::from_millis(2));
    t.on_llm_start("child_1", 0, &[], &[]);
    t.on_llm_end("child_1", 0, "claude-4.7", "anthropic", "out", None, None);
    t.on_stage_end("child_1", &child_reason, StageStatus::Done);

    // ⑤ SubagentStop:两信号齐备 → 关闭 AGENT obs
    t.on_subagent_stop("main", "child_1", "review done", false);
    let completions = agent_obs_completions(&session.events_snapshot());
    assert_eq!(completions.len(), 1, "⑤ Stop 后应关闭 AGENT obs");
    assert_eq!(
        completions[0].0, *child_obs_id,
        "关闭的 obs 应为 child 的 AGENT obs"
    );
    assert_eq!(
        completions[0]
            .2
            .as_ref()
            .and_then(|o| o.get("text"))
            .and_then(|t| t.as_str()),
        Some("review done"),
        "AGENT obs output 应为 Stop result text"
    );
    assert_eq!(
        t.subagent.status_of("child_1"),
        Some(&SubagentStatus::Closed)
    );

    // on_turn_end 无残留、无 incomplete
    let _h = t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed);
    tokio::task::yield_now().await;
    assert_eq!(t.subagent.incomplete_count(), 0);
}

// ── Stop 先于 ToolEnded ─────────────────────────────────────────────────────

/// ①ToolStart → ②SubagentStart → ③child events → ④SubagentStop → ⑤ToolEnded
/// ④置 StopReceived 不关闭;⑤主 batch 结束父工具 + 关闭 AGENT obs;flush 恰好一次。
#[tokio::test]
async fn test_stop_before_tool_ended() {
    let (mut t, session) = make_tracer(1.0);
    t.set_main_agent_id("main".to_string());
    t.on_turn_start("turn_2");

    let main_act = t
        .on_stage_start_gated("main", Stage::Act, "turn_2")
        .unwrap();
    t.on_tool_start("main", "call_agent", "Agent", &serde_json::json!({}));
    t.on_subagent_start("main", "child_1", "fork", false);

    let child_reason = t
        .on_stage_start_gated("child_1", Stage::Reason, "turn_2")
        .unwrap();
    // 确保 stage duration > 0(v2 条件上报)
    std::thread::sleep(std::time::Duration::from_millis(2));
    t.on_tool_start("child_1", "call_bash", "Bash", &serde_json::json!({}));
    t.on_tool_end("child_1", "call_bash", "out", false);
    t.on_stage_end("child_1", &child_reason, StageStatus::Done);

    // ④ Stop 先到:置 StopReceived,不关闭(父 ToolEnded 未到)
    t.on_subagent_stop("main", "child_1", "done", false);
    assert_eq!(
        agent_obs_completions(&session.events_snapshot()).len(),
        0,
        "Stop 先到不应立即关闭(等父 ToolEnded)"
    );
    assert_eq!(
        t.subagent.status_of("child_1"),
        Some(&SubagentStatus::StopReceived)
    );

    // ⑤ ToolEnded:两信号齐备 → 关闭
    t.on_tool_end("main", "call_agent", "subagent done", false);
    let completions = agent_obs_completions(&session.events_snapshot());
    assert_eq!(completions.len(), 1, "ToolEnded 后应关闭 AGENT obs");
    assert_eq!(
        completions[0]
            .2
            .as_ref()
            .and_then(|o| o.get("text"))
            .and_then(|t| t.as_str()),
        Some("done"),
        "AGENT obs output 应为 Stop result text"
    );

    // flush 恰好一次:child tool-batch span 恰好 1 个
    let final_events = session.events_snapshot();
    let batch_spans = final_events
        .iter()
        .filter(|e| {
            if let IngestionEvent::SpanCreate { body, .. } = e {
                body.name.as_deref() == Some("tool-batch")
            } else {
                false
            }
        })
        .count();
    assert_eq!(batch_spans, 1, "child tool-batch 应 flush 恰好一次");

    // AGENT obs end ≥ child 事件 end
    let agent_end = parse_time(completions[0].1.as_ref().unwrap());
    let _ = main_act;
    let _h = t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed);
    tokio::task::yield_now().await;
    assert!(
        agent_end >= parse_time(&child_reason.start_time),
        "AGENT obs end 应晚于 child stage start"
    );
}

// ── 注册闸门:内容先于 Start ────────────────────────────────────────────────

/// 乱序场景:Stop 先于内容事件到达(StopReceived 暂存),内容随后挂入。
/// close 时 end_time 应为 max(stop_time, 最后子内容时刻)而非 Stop 时刻,
/// 否则 AGENT obs 时长被虚标为 0(真实 trace obs_01a00d6a552175c2b5a573c10f1da5e5)。
#[tokio::test]
async fn test_stop_before_content_end_time_takes_max() {
    let (mut t, session) = make_tracer(1.0);
    t.set_main_agent_id("main".to_string());
    t.on_turn_start("turn_sb");

    let main_act = t
        .on_stage_start_gated("main", Stage::Act, "turn_sb")
        .unwrap();
    t.on_tool_start("main", "call_agent", "Agent", &serde_json::json!({}));
    t.on_subagent_start("main", "child_1", "coder", false);
    // Stop 先到:暂存 → StopReceived(父 ToolEnded 未到,不关闭)
    t.on_subagent_stop("main", "child_1", "done", false);
    assert_eq!(
        t.subagent.status_of("child_1"),
        Some(&SubagentStatus::StopReceived)
    );

    // 内容事件随后到达(晚于 Stop):stage + tool
    std::thread::sleep(std::time::Duration::from_millis(2));
    let child_reason = t
        .on_stage_start_gated("child_1", Stage::Reason, "turn_sb")
        .unwrap();
    t.on_tool_start("child_1", "call_bash", "Bash", &serde_json::json!({}));
    t.on_tool_end("child_1", "call_bash", "out", false);
    t.on_stage_end("child_1", &child_reason, StageStatus::Done);

    // 父 ToolEnded → 两信号齐备 → 关闭
    t.on_tool_end("main", "call_agent", "subagent done", false);
    let completions = agent_obs_completions(&session.events_snapshot());
    assert_eq!(completions.len(), 1, "应关闭 AGENT obs");

    // end_time ≥ 最后子内容时刻(stage start),而非早到的 Stop 时刻
    let agent_end = parse_time(completions[0].1.as_ref().unwrap());
    assert!(
        agent_end >= parse_time(&child_reason.start_time),
        "end_time 应取最后子内容时刻(≥ stage start),实际 {agent_end} < {}",
        child_reason.start_time
    );
    let _ = main_act;
    let _h = t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed);
    tokio::task::yield_now().await;
}
