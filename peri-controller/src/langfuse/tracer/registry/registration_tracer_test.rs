use super::IncompleteReason;
use super::SubagentStatus;
use crate::langfuse::config::LangfuseConfig;
use crate::langfuse::fake_session::FakeLangfuseSession;
use crate::langfuse::tracer::LangfuseTracer;
use langfuse_client::types::ObservationType;
use langfuse_client::IngestionEvent;
use peri_agent::agent::events::Stage;
use peri_agent::agent::events::StageStatus;
use std::collections::HashMap;

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

fn parent_map(events: &[IngestionEvent]) -> HashMap<String, Option<String>> {
    assert!(!events.iter().any(|event| matches!(
        event,
        IngestionEvent::ObservationUpdate { .. }
            | IngestionEvent::SpanUpdate { .. }
            | IngestionEvent::GenerationUpdate { .. }
            | IngestionEvent::SessionCreate { .. }
    )));
    let mut map = HashMap::new();
    for e in events {
        let (id, parent) = match e {
            IngestionEvent::ObservationCreate { body, .. } => {
                (body.id.clone(), body.parent_observation_id.clone())
            }
            IngestionEvent::SpanCreate { body, .. } => {
                (body.id.clone(), body.parent_observation_id.clone())
            }
            IngestionEvent::GenerationCreate { body, .. } => {
                (body.id.clone(), body.parent_observation_id.clone())
            }
            _ => continue,
        };
        if let Some(id) = id {
            map.insert(id, parent);
        }
    }
    map
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

fn meaningful_event_count(events: &[IngestionEvent]) -> usize {
    events
        .iter()
        .filter(|e| !matches!(e, IngestionEvent::TraceCreate { .. }))
        .count()
}

/// ①child StageStarted → ②child LlmCallStart → ③SubagentStart → ④父ToolStart
/// ①②入 gate_cache 不落主 agent;③ join 后按原顺序重放;
/// 最终 ①②parent 为 child AGENT;无任何 obs 挂 agent-run。
#[tokio::test]
async fn test_content_before_start_gate() {
    let (mut t, session) = make_tracer(1.0);
    t.set_main_agent_id("main".to_string());
    t.on_turn_start("turn_3");

    // ① child StageStarted 先到:入闸门,不创建 handle
    let gate_stage = t.on_stage_start_gated("child_1", Stage::Reason, "turn_3");
    assert!(
        gate_stage.is_none(),
        "Start 未到时 StageStarted 应被闸门缓存"
    );
    // ② child LlmCallStart 先到:入闸门
    t.on_llm_start("child_1", 0, &[], &[]);
    assert_eq!(t.subagent.gated_len(), 2, "两条内容事件应被缓存");
    assert_eq!(
        meaningful_event_count(&session.events_snapshot()),
        0,
        "闸门缓存期间不应有任何 obs 落主 agent"
    );

    // ③ SubagentStart:join 失败(父 ToolStart 未到)→ Pending
    t.on_subagent_start("main", "child_1", "fork", false);
    assert_eq!(
        t.subagent.status_of("child_1"),
        Some(&SubagentStatus::PendingInvocation)
    );
    // 闸门缓存期间不应有任何 obs/span/generation 落主 agent(仅 Trace/Session 基础事件)
    assert_eq!(
        meaningful_event_count(&session.events_snapshot()),
        0,
        "闸门缓存期间不应有任何观测事件"
    );

    // ④ 父 ToolStart 晚到:register_invocation → join → 重放
    let _main_act = t
        .on_stage_start_gated("main", Stage::Act, "turn_3")
        .unwrap();
    t.on_tool_start("main", "call_agent", "Agent", &serde_json::json!({}));

    let events = session.events_snapshot();
    let creates = agent_obs_creates(&events);
    assert_eq!(creates.len(), 0, "join 不导出未完成观测");
    let cached = cached_agent(&t, "child_1");
    let (child_obs_id, _, _) = &cached;

    // 重放的 StageStarted handle 由 StageEnded 分支领取
    let replay_handle = t
        .take_replayed_stage_handle("child_1")
        .expect("重放的 stage handle 应可领取");
    assert_eq!(
        replay_handle.parent_observation_id, *child_obs_id,
        "重放的 stage parent 应为 child AGENT obs"
    );
    // 确保重放 stage duration > 0(v2 条件上报)
    std::thread::sleep(std::time::Duration::from_millis(2));
    t.on_stage_end("child_1", &replay_handle, StageStatus::Done);

    // 重放的 LlmCallEnd:generation 数据应存在(重放 LlmCallStart 已建)
    t.on_llm_end("child_1", 0, "claude-4.7", "anthropic", "out", None, None);

    let final_events = session.events_snapshot();
    let map = parent_map(&final_events);
    // 无任何 obs 挂 agent-run
    let agent_run = &t.agent_observation_id;
    assert!(
        !map.values()
            .any(|p| p.as_deref() == Some(agent_run.as_str())),
        "乱序内容事件不应挂 agent-run"
    );
    // child stage span 的 parent 为 child AGENT obs
    let all = child_events(&final_events);
    assert!(
        all.iter()
            .any(|(_, _, p)| p.as_deref() == Some(child_obs_id.as_str())),
        "重放的 stage/generation 应挂 child AGENT obs"
    );
    assert_eq!(t.subagent.gated_len(), 0, "重放后闸门缓存应清空");
}

// ── Start 先于父 ToolStart ──────────────────────────────────────────────────

/// ①SubagentStart → ②child events → ③父ToolStart
/// ①入 pending_starts;③ join → ②重放归属正确。
#[tokio::test]
async fn test_start_before_parent_tool_start() {
    let (mut t, session) = make_tracer(1.0);
    t.set_main_agent_id("main".to_string());
    t.on_turn_start("turn_4");

    // ① SubagentStart:join 失败 → PendingInvocation
    t.on_subagent_start("main", "child_1", "bg", true);
    assert_eq!(
        t.subagent.status_of("child_1"),
        Some(&SubagentStatus::PendingInvocation)
    );

    // ② child 内容事件 → 闸门缓存
    let gated = t.on_stage_start_gated("child_1", Stage::Reason, "turn_4");
    assert!(gated.is_none());
    assert_eq!(t.subagent.gated_len(), 1);

    // ③ 父 ToolStart → join → AGENT obs + 重放
    let main_act = t
        .on_stage_start_gated("main", Stage::Act, "turn_4")
        .unwrap();
    t.on_tool_start("main", "call_agent", "Agent", &serde_json::json!({}));

    let events = session.events_snapshot();
    let creates = agent_obs_creates(&events);
    assert_eq!(creates.len(), 0);
    let cached = cached_agent(&t, "child_1");
    let (child_obs_id, parent, _) = &cached;
    assert_eq!(
        parent.as_deref(),
        Some(main_act.span_id.as_str()),
        "AGENT obs parent = join 时冻结的父 stage span"
    );

    let replay_handle = t.take_replayed_stage_handle("child_1").unwrap();
    assert_eq!(replay_handle.parent_observation_id, *child_obs_id);
    assert_eq!(t.subagent.gated_len(), 0);
}

/// 无 Start 的情况下收到 agent_id="ghost" 的 StageStarted/LlmCallStart →
/// 闸门缓存,on_turn_end 清理时 Incomplete(UnknownAgent),不挂主 agent。
#[tokio::test]
async fn test_unknown_agent_id_incomplete() {
    let (mut t, session) = make_tracer(1.0);
    t.set_main_agent_id("main".to_string());
    t.on_turn_start("turn_6");

    let gated = t.on_stage_start_gated("ghost", Stage::Reason, "turn_6");
    assert!(gated.is_none(), "未知 agent 的 StageStarted 应被闸门缓存");
    t.on_llm_start("ghost", 0, &[], &[]);
    assert_eq!(t.subagent.gated_len(), 2);

    // 未 emit 任何观测事件(不挂主 agent)
    let events = session.events_snapshot();
    assert_eq!(
        meaningful_event_count(&events),
        0,
        "未知 agent 内容事件不应产生任何观测事件"
    );

    // on_turn_end:残留缓存 → UnknownAgent incomplete
    let _h = t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed);
    tokio::task::yield_now().await;
    assert_eq!(
        t.subagent.status_of("ghost"),
        Some(&SubagentStatus::Incomplete(IncompleteReason::UnknownAgent)),
        "残留缓存应标记 UnknownAgent"
    );
    assert_eq!(t.subagent.incomplete_count(), 1);
    // 无任何 obs 挂 agent-run
    let final_events = session.events_snapshot();
    let map = parent_map(&final_events);
    assert!(
        !map.values()
            .any(|p| p.as_deref() == Some(t.agent_observation_id.as_str())),
        "ghost 内容不应挂主 agent-run"
    );
}

/// ①ToolStart → ②ToolEnded → ③child events(Start 永不出现)→ on_turn_end
/// child 内容不挂主 agent;on_turn_end 清缓存;incomplete 计数 ≥ 1。
#[tokio::test]
async fn test_missing_start_incomplete() {
    let (mut t, session) = make_tracer(1.0);
    t.set_main_agent_id("main".to_string());
    t.on_turn_start("turn_7");

    let main_act = t
        .on_stage_start_gated("main", Stage::Act, "turn_7")
        .unwrap();
    // ① 父 ToolStart(Agent):invocation 登记,但 child Start 永不出现
    t.on_tool_start("main", "call_agent", "Agent", &serde_json::json!({}));
    // ② ToolEnded
    t.on_tool_end("main", "call_agent", "spawned", false);
    // ③ child 内容事件 → 闸门缓存
    let gated = t.on_stage_start_gated("child_1", Stage::Reason, "turn_7");
    assert!(gated.is_none());
    t.on_llm_start("child_1", 0, &[], &[]);
    assert_eq!(t.subagent.gated_len(), 2);

    let _h = t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed);
    tokio::task::yield_now().await;
    assert_eq!(t.subagent.gated_len(), 0, "on_turn_end 应清空闸门缓存");
    assert_eq!(
        t.subagent.incomplete_count(),
        1,
        "缺失 Start 应计数 incomplete"
    );

    let final_events = session.events_snapshot();
    let map = parent_map(&final_events);
    assert!(
        !map.values()
            .any(|p| p.as_deref() == Some(t.agent_observation_id.as_str())),
        "child 内容不应挂主 agent-run"
    );
    let _ = main_act;
}

// ── 重复 Start / Stop ───────────────────────────────────────────────────────

/// Start ×2:第二次 → Incomplete(DuplicateStart),不重复创建 obs。
#[tokio::test]
async fn test_duplicate_start() {
    let (mut t, session) = make_tracer(1.0);
    t.set_main_agent_id("main".to_string());
    t.on_turn_start("turn_8a");

    let main_act = t
        .on_stage_start_gated("main", Stage::Act, "turn_8a")
        .unwrap();
    t.on_tool_start("main", "call_agent", "Agent", &serde_json::json!({}));
    t.on_subagent_start("main", "child_1", "fork", false);
    // 重复 Start
    t.on_subagent_start("main", "child_1", "fork", false);
    assert_eq!(
        t.subagent.status_of("child_1"),
        Some(&SubagentStatus::Incomplete(
            IncompleteReason::DuplicateStart
        )),
        "重复 Start 应标记 DuplicateStart"
    );
    assert_eq!(
        agent_obs_creates(&session.events_snapshot()).len(),
        0,
        "重复 Start 不导出未完成观测"
    );
    assert_eq!(t.subagent.incomplete_count(), 1);

    t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed)
        .await
        .unwrap();
    assert_eq!(
        agent_obs_completions(&session.events_snapshot()).len(),
        1,
        "turn-end 必须关闭已打开的异常 observation，且只关闭一次"
    );
    let _ = main_act;
}

/// Stop ×2:第二次 → Incomplete(DuplicateStop),不重复关闭 obs。
#[tokio::test]
async fn test_duplicate_stop() {
    let (mut t, session) = make_tracer(1.0);
    t.set_main_agent_id("main".to_string());
    t.on_turn_start("turn_8b");

    let main_act = t
        .on_stage_start_gated("main", Stage::Act, "turn_8b")
        .unwrap();
    t.on_tool_start("main", "call_agent", "Agent", &serde_json::json!({}));
    t.on_subagent_start("main", "child_1", "fork", false);
    t.on_subagent_stop("main", "child_1", "done", false);
    assert_eq!(
        t.subagent.status_of("child_1"),
        Some(&SubagentStatus::StopReceived)
    );
    // 重复 Stop
    t.on_subagent_stop("main", "child_1", "done", false);
    assert_eq!(
        t.subagent.status_of("child_1"),
        Some(&SubagentStatus::Incomplete(IncompleteReason::DuplicateStop)),
        "重复 Stop 应标记 DuplicateStop"
    );
    assert_eq!(
        agent_obs_completions(&session.events_snapshot()).len(),
        0,
        "Incomplete 不关闭 obs"
    );
    assert_eq!(t.subagent.incomplete_count(), 1);

    t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed)
        .await
        .unwrap();
    assert_eq!(
        agent_obs_completions(&session.events_snapshot()).len(),
        1,
        "turn-end 必须关闭已打开的异常 observation，且只关闭一次"
    );
    let _ = main_act;
}

// ── 注册闸门缓存溢出 ────────────────────────────────────────────────────────

/// 灌入 >64 条未知 agent 内容事件 → 缓存有界 64,最旧被逐出(不重放);
/// 被逐出事件的 agent 若 Start 正等待 join(pending_starts)→ Incomplete(CacheOverflow)。
#[tokio::test]
async fn test_gate_cache_overflow() {
    let (mut t, session) = make_tracer(1.0);
    t.set_main_agent_id("main".to_string());
    t.on_turn_start("turn_9");

    // 先 Start(join 失败 → pending_starts)
    t.on_subagent_start("main", "child_1", "bg", true);
    assert_eq!(
        t.subagent.status_of("child_1"),
        Some(&SubagentStatus::PendingInvocation)
    );

    // 灌入 70 条内容事件:缓存上限 64,溢出逐出最旧
    for i in 0..70 {
        t.on_llm_start("child_1", i, &[], &[]);
    }
    assert_eq!(t.subagent.gated_len(), 64, "闸门缓存应有界(上限 64)");
    assert_eq!(
        t.subagent.status_of("child_1"),
        Some(&SubagentStatus::Incomplete(IncompleteReason::CacheOverflow)),
        "溢出时等待 join 的 child 应标 CacheOverflow"
    );

    // 父 ToolStart 到达:child 已 incomplete → 不 join
    let main_act = t
        .on_stage_start_gated("main", Stage::Act, "turn_9")
        .unwrap();
    t.on_tool_start("main", "call_agent", "Agent", &serde_json::json!({}));
    assert_eq!(
        agent_obs_creates(&session.events_snapshot()).len(),
        0,
        "CacheOverflow 的 child 不应创建 AGENT obs"
    );

    let _h = t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed);
    tokio::task::yield_now().await;
    assert!(t.subagent.incomplete_count() >= 1);
    let _ = main_act;
}

// ── parent 冻结(不漂移) ────────────────────────────────────────────────────

/// Start join 后父 stage 变化(父 StageStarted 另开新 stage)→ child 继续。
/// child AGENT obs parent 仍为 join 时冻结的 span_id,不随活跃 stage 漂移。
#[tokio::test]
async fn test_parent_frozen_no_drift() {
    let (mut t, session) = make_tracer(1.0);
    t.set_main_agent_id("main".to_string());
    t.on_turn_start("turn_10");

    // 父 stage A1
    let act_a1 = t
        .on_stage_start_gated("main", Stage::Act, "turn_10")
        .unwrap();
    let frozen_span = act_a1.span_id.clone();
    t.on_tool_start("main", "call_agent", "Agent", &serde_json::json!({}));
    t.on_subagent_start("main", "child_1", "fork", false);
    assert!(agent_obs_creates(&session.events_snapshot()).is_empty());
    let cached = cached_agent(&t, "child_1");
    let (child_obs_id, child_parent, _) = &cached;
    assert_eq!(
        child_parent.as_deref(),
        Some(frozen_span.as_str()),
        "join 时冻结父 stage span"
    );

    // 父另开新 stage A2
    let act_a2 = t
        .on_stage_start_gated("main", Stage::Act, "turn_10")
        .unwrap();
    assert_ne!(act_a2.span_id, frozen_span);

    // child 继续:内容归属仍为 child AGENT obs
    let child_reason = t
        .on_stage_start_gated("child_1", Stage::Reason, "turn_10")
        .unwrap();
    assert_eq!(child_reason.parent_observation_id, *child_obs_id);

    // 关闭后 AGENT obs parent 仍为冻结 span(ObservationCreate 与 create 一致)
    t.on_stage_end("child_1", &child_reason, StageStatus::Done);
    t.on_tool_end("main", "call_agent", "done", false);
    t.on_subagent_stop("main", "child_1", "done", false);
    let completions = agent_obs_completions(&session.events_snapshot());
    assert_eq!(completions.len(), 1);
    // ObservationCreate 的 parent 从 ClosedSubagent 来,断言相同 obs id
    assert_eq!(completions[0].0, *child_obs_id);
}

// ── 嵌套防环 ────────────────────────────────────────────────────────────────

/// 构造嵌套(child1 再 Agent tool → child2):
/// child1 obs id ≠ child2 obs id;child2 parent 为 child1 的 active stage
/// 且不等于 child2 自身;图无环。
#[tokio::test]
async fn test_no_cycle_parent_neq_child() {
    let (mut t, session) = make_tracer(1.0);
    t.set_main_agent_id("main".to_string());
    t.on_turn_start("turn_11");

    let main_act = t
        .on_stage_start_gated("main", Stage::Act, "turn_11")
        .unwrap();
    // child1
    t.on_tool_start("main", "call_1", "Agent", &serde_json::json!({}));
    t.on_subagent_start("main", "child_1", "c1", false);
    let obs_1 = t.subagent.observation_id_of("child_1").unwrap();

    // child1 内再调 Agent → child2
    let c1_stage = t
        .on_stage_start_gated("child_1", Stage::Act, "turn_11")
        .unwrap();
    t.on_tool_start("child_1", "call_2", "Agent", &serde_json::json!({}));
    t.on_subagent_start("child_1", "child_2", "c2", false);
    let obs_2 = t.subagent.observation_id_of("child_2").unwrap();

    assert_ne!(obs_1, obs_2, "嵌套 child 的 obs id 应不同");
    // child2 的 AGENT obs parent = child1 的 active stage(冻结)
    assert!(agent_obs_creates(&session.events_snapshot()).is_empty());
    let create_2 = cached_agent(&t, "child_2");
    assert_eq!(
        create_2.1.as_deref(),
        Some(c1_stage.span_id.as_str()),
        "child2 parent 应为 child1 的 active stage"
    );
    assert_ne!(
        create_2.1.as_deref(),
        Some(obs_2.as_str()),
        "child2 parent 不得为自身(防环)"
    );

    // 图无环:每个 obs 沿 parent 链最终到 trace(或主 agent obs),不回自身
    t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed)
        .await
        .unwrap();
    let events = session.events_snapshot();
    let map = parent_map(&events);
    for (id, parent) in &map {
        let mut cur = parent.clone();
        let mut hops = 0;
        while let Some(p) = cur {
            assert!(hops < 10, "parent 链应有界(无环)");
            if &p == id {
                panic!("检测到环:obs {id} 的 parent 链回到自身");
            }
            cur = map.get(&p).cloned().flatten();
            hops += 1;
        }
    }
    let _ = main_act;
}

// ── ToolEnded 不关 child ────────────────────────────────────────────────────

/// 全生命周期中 ToolEnded 后立即断言:child 仍 Active(不因 ToolEnded 关闭)。
#[tokio::test]
async fn test_tool_ended_never_closes_child() {
    let (mut t, _session) = make_tracer(1.0);
    t.set_main_agent_id("main".to_string());
    t.on_turn_start("turn_12");

    let main_act = t
        .on_stage_start_gated("main", Stage::Act, "turn_12")
        .unwrap();
    t.on_tool_start("main", "call_agent", "Agent", &serde_json::json!({}));
    t.on_subagent_start("main", "child_1", "fork", false);
    let child_reason = t
        .on_stage_start_gated("child_1", Stage::Reason, "turn_12")
        .unwrap();
    t.on_stage_end("child_1", &child_reason, StageStatus::Done);

    // 父 ToolEnded:child 不得关闭
    t.on_tool_end("main", "call_agent", "done", false);
    assert_eq!(
        t.subagent.status_of("child_1"),
        Some(&SubagentStatus::Active),
        "ToolEnded 绝不关闭 child(生命周期由 Stop 驱动)"
    );

    // Stop 后才关闭
    t.on_subagent_stop("main", "child_1", "done", false);
    assert_eq!(
        t.subagent.status_of("child_1"),
        Some(&SubagentStatus::Closed),
        "Stop 后 child 应关闭"
    );
    let _ = main_act;
}

// ── bg subagent:turn_end 兜底 ───────────────────────────────────────────────

/// bg:ToolStart → ToolEnded(deferred)→ child events → on_turn_end(无 Stop)。
/// 兜底关闭 AGENT obs,metadata 含 incomplete_reason;无幽灵序列挂主 agent。
#[tokio::test]
async fn test_bg_subagent_turn_end_cleanup() {
    let (mut t, session) = make_tracer(1.0);
    t.set_main_agent_id("main".to_string());
    t.on_turn_start("turn_13");

    let main_act = t
        .on_stage_start_gated("main", Stage::Act, "turn_13")
        .unwrap();
    t.on_tool_start("main", "call_agent", "Agent", &serde_json::json!({}));
    t.on_subagent_start("main", "child_1", "bg", true);
    let child_reason = t
        .on_stage_start_gated("child_1", Stage::Reason, "turn_13")
        .unwrap();
    t.on_tool_start("child_1", "call_bash", "Bash", &serde_json::json!({}));
    t.on_tool_end("child_1", "call_bash", "out", false);
    t.on_stage_end("child_1", &child_reason, StageStatus::Done);
    t.on_tool_end("main", "call_agent", "spawned", false);
    // Stop 永不到达

    let _h = t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed);
    tokio::task::yield_now().await;

    let events = session.events_snapshot();
    let completions = agent_obs_completions(&events);
    assert_eq!(completions.len(), 1, "on_turn_end 应兜底关闭 AGENT obs");
    let (obs_id, end_time, _) = &completions[0];
    assert!(end_time.is_some(), "兜底关闭应带 end_time");
    // 兜底关闭 metadata 应携带 incomplete_reason
    let meta_reason = events.iter().find_map(|e| {
        if let IngestionEvent::ObservationCreate { body, .. } = e {
            if body.r#type == ObservationType::Agent {
                return body
                    .metadata
                    .as_ref()
                    .and_then(|m| m.get("incomplete_reason"))
                    .and_then(|r| r.as_str())
                    .map(|s| s.to_string());
            }
        }
        None
    });
    assert_eq!(
        meta_reason.as_deref(),
        Some("MissingStop"),
        "兜底关闭 metadata 应携带 incomplete_reason"
    );
    // child tool-batch flush:bash 工具已上报
    assert!(
        events.iter().any(|e| {
            if let IngestionEvent::ObservationCreate { body, .. } = e {
                body.name.as_deref() == Some("Bash")
            } else {
                false
            }
        }),
        "child 工具应随兜底 flush 上报"
    );
    // 无幽灵序列挂主 agent:任何 child 事件 parent 链不指向 agent-run
    let map = parent_map(&events);
    assert!(
        !map.values()
            .any(|p| p.as_deref() == Some(t.agent_observation_id.as_str())),
        "bg child 内容不应挂主 agent-run"
    );
    let _ = (obs_id, main_act);
}
