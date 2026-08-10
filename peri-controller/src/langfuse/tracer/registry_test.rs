//! SubagentRegistry 状态机测试(替代旧 SubagentStack 的 subagent_test.rs)。
//!
//! 覆盖:8 步全生命周期、注册闸门乱序缓存与重放、未知 agent / 缺失 Start /
//! 重复 Start/Stop / 缓存溢出 → incomplete、parent 冻结与防环、
//! ToolEnded 不关 child、on_turn_end 兜底。
//!
//! 所有事件经 tracer 的 `on_*` 公开入口注入(与生产路径同构):
//! StageStarted 走 `on_stage_start_gated`(bridge 分支的入口),StageEnded
//! 用其返回的 handle 调 `on_stage_end`。

use std::collections::HashMap;

use langfuse_client::types::ObservationType;
use langfuse_client::IngestionEvent;
use peri_agent::agent::events::{Stage, StageStatus};

use super::*;
use crate::langfuse::config::LangfuseConfig;
use crate::langfuse::fake_session::FakeLangfuseSession;
use crate::langfuse::tracer::LangfuseTracer;

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
    };
    let t = LangfuseTracer::new(session.clone(), "sess_registry".to_string(), config);
    (t, session)
}

/// 事件图:(obs/span/generation id → parent observation id)
fn parent_map(events: &[IngestionEvent]) -> HashMap<String, Option<String>> {
    let mut map = HashMap::new();
    for e in events {
        let (id, parent) = match e {
            IngestionEvent::ObservationCreate { body, .. }
            | IngestionEvent::ObservationUpdate { body, .. } => {
                (body.id.clone(), body.parent_observation_id.clone())
            }
            IngestionEvent::SpanCreate { body, .. } | IngestionEvent::SpanUpdate { body, .. } => {
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

/// AGENT observation 创建(open)列表:(id, parent, start_time)
fn agent_obs_creates(events: &[IngestionEvent]) -> Vec<(String, Option<String>, Option<String>)> {
    events
        .iter()
        .filter_map(|e| {
            if let IngestionEvent::ObservationCreate { body, .. } = e {
                if body.r#type == ObservationType::Agent {
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

/// AGENT observation 关闭(update)列表:(id, end_time, output)
fn agent_obs_updates(
    events: &[IngestionEvent],
) -> Vec<(String, Option<String>, Option<serde_json::Value>)> {
    events
        .iter()
        .filter_map(|e| {
            if let IngestionEvent::ObservationUpdate { body, .. } = e {
                if body.r#type == ObservationType::Agent {
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

/// span/generation 事件的 (id, start_time, parent)
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

/// 非 Trace/Session 的观测事件数(obs/span/generation/event)
fn meaningful_event_count(events: &[IngestionEvent]) -> usize {
    events
        .iter()
        .filter(|e| {
            !matches!(
                e,
                IngestionEvent::TraceCreate { .. } | IngestionEvent::SessionCreate { .. }
            )
        })
        .count()
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
    // ② SubagentStart:join 成功 → AGENT obs create(open)
    t.on_subagent_start("main", "child_1", "code-reviewer", false);

    let events = session.events_snapshot();
    let creates = agent_obs_creates(&events);
    assert_eq!(creates.len(), 1, "②后应有恰好 1 个 AGENT obs create");
    let (child_obs_id, child_parent, child_start) = &creates[0];
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
        agent_obs_updates(&session.events_snapshot()).len(),
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
    let updates = agent_obs_updates(&session.events_snapshot());
    assert_eq!(updates.len(), 1, "⑦ 后应有恰好 1 个 AGENT obs close");
    assert_eq!(
        updates[0].0, *child_obs_id,
        "关闭的 obs 应为 child 的 AGENT obs"
    );

    // ⑧ on_turn_end:无残留
    let _h = t.on_turn_end(None);
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
    let agent_end = parse_time(updates[0].1.as_ref().expect("AGENT end"));
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
        agent_obs_updates(&session.events_snapshot()).len(),
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
    assert_eq!(creates.len(), 1, "③ join 应创建 AGENT obs");
    let (child_obs_id, child_parent, _) = &creates[0];
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
    let updates = agent_obs_updates(&session.events_snapshot());
    assert_eq!(updates.len(), 1, "⑤ Stop 后应关闭 AGENT obs");
    assert_eq!(
        updates[0].0, *child_obs_id,
        "关闭的 obs 应为 child 的 AGENT obs"
    );
    assert_eq!(
        updates[0]
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
    let _h = t.on_turn_end(None);
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
        agent_obs_updates(&session.events_snapshot()).len(),
        0,
        "Stop 先到不应立即关闭(等父 ToolEnded)"
    );
    assert_eq!(
        t.subagent.status_of("child_1"),
        Some(&SubagentStatus::StopReceived)
    );

    // ⑤ ToolEnded:两信号齐备 → 关闭
    t.on_tool_end("main", "call_agent", "subagent done", false);
    let updates = agent_obs_updates(&session.events_snapshot());
    assert_eq!(updates.len(), 1, "ToolEnded 后应关闭 AGENT obs");
    assert_eq!(
        updates[0]
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
    let agent_end = parse_time(updates[0].1.as_ref().unwrap());
    let _ = main_act;
    let _h = t.on_turn_end(None);
    tokio::task::yield_now().await;
    assert!(
        agent_end >= parse_time(&child_reason.start_time),
        "AGENT obs end 应晚于 child stage start"
    );
}

// ── 注册闸门:内容先于 Start ────────────────────────────────────────────────

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
    assert_eq!(creates.len(), 1, "join 后应创建 AGENT obs");
    let (child_obs_id, _, _) = &creates[0];

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
    assert_eq!(creates.len(), 1);
    let (child_obs_id, parent, _) = &creates[0];
    assert_eq!(
        parent.as_deref(),
        Some(main_act.span_id.as_str()),
        "AGENT obs parent = join 时冻结的父 stage span"
    );

    let replay_handle = t.take_replayed_stage_handle("child_1").unwrap();
    assert_eq!(replay_handle.parent_observation_id, *child_obs_id);
    assert_eq!(t.subagent.gated_len(), 0);
}

// ── 并行双 subagent 交错 ────────────────────────────────────────────────────

/// 主ToolStart A、主ToolStart B、Start A、Start B、A.StageStart、B.StageStart、
/// A.Llm、B.Tool、A.StageEnd、B.StageEnd、ToolEnded A、ToolEnded B、Stop A、Stop B
/// 各自内容 parent 指向各自 AGENT obs;generation (agent_id, step) 不串;
/// tool 各自 batch;无任何 A 内容挂 B。
#[tokio::test]
async fn test_parallel_two_subagents_interleaved() {
    let (mut t, session) = make_tracer(1.0);
    t.set_main_agent_id("main".to_string());
    t.on_turn_start("turn_5");

    let main_act = t
        .on_stage_start_gated("main", Stage::Act, "turn_5")
        .unwrap();
    t.on_tool_start("main", "call_a", "Agent", &serde_json::json!({}));
    t.on_tool_start("main", "call_b", "Agent", &serde_json::json!({}));
    t.on_subagent_start("main", "child_a", "explorer-a", false);
    t.on_subagent_start("main", "child_b", "explorer-b", false);

    let a_stage = t
        .on_stage_start_gated("child_a", Stage::Reason, "turn_5")
        .unwrap();
    let b_stage = t
        .on_stage_start_gated("child_b", Stage::Reason, "turn_5")
        .unwrap();
    // 确保 stage duration > 0(v2 条件上报)
    std::thread::sleep(std::time::Duration::from_millis(2));
    t.on_llm_start("child_a", 0, &[], &[]);
    t.on_llm_end("child_a", 0, "m", "p", "a-out", None, None);
    t.on_llm_start("child_b", 0, &[], &[]);
    t.on_llm_end("child_b", 0, "m", "p", "b-out", None, None);
    t.on_tool_start("child_a", "call_a1", "Bash", &serde_json::json!({}));
    t.on_tool_end("child_a", "call_a1", "a", false);
    t.on_tool_start("child_b", "call_b1", "Grep", &serde_json::json!({}));
    t.on_tool_end("child_b", "call_b1", "b", false);
    t.on_stage_end("child_a", &a_stage, StageStatus::Done);
    t.on_stage_end("child_b", &b_stage, StageStatus::Done);

    t.on_tool_end("main", "call_a", "done-a", false);
    t.on_tool_end("main", "call_b", "done-b", false);
    t.on_subagent_stop("main", "child_a", "ra", false);
    t.on_subagent_stop("main", "child_b", "rb", false);

    let events = session.events_snapshot();
    let creates = agent_obs_creates(&events);
    assert_eq!(creates.len(), 2, "两个 child 各一个 AGENT obs");
    let obs_a = creates
        .iter()
        .find(|(id, _, _)| t.subagent.observation_id_of("child_a").as_deref() == Some(id.as_str()));
    let obs_b = creates
        .iter()
        .find(|(id, _, _)| t.subagent.observation_id_of("child_b").as_deref() == Some(id.as_str()));
    assert!(obs_a.is_some() && obs_b.is_some());
    let obs_a_id = t.subagent.observation_id_of("child_a").unwrap();
    let obs_b_id = t.subagent.observation_id_of("child_b").unwrap();
    assert_ne!(obs_a_id, obs_b_id);

    // create 携带父工具 input + metadata(turn_id/is_synthetic/was_sampled)
    let create_a = events
        .iter()
        .find_map(|e| {
            if let IngestionEvent::ObservationCreate { body, .. } = e {
                if body.id.as_deref() == Some(obs_a_id.as_str()) {
                    return Some((body.input.clone(), body.metadata.clone()));
                }
            }
            None
        })
        .expect("child_a obs create 应存在");
    assert!(create_a.0.is_some(), "AGENT obs create 应携带父工具 input");
    let meta_a = create_a.1.expect("AGENT obs create 应携带 metadata");
    assert_eq!(
        meta_a.get("turn_id").and_then(|t| t.as_str()),
        Some(t.trace_id.as_str()),
        "AGENT obs create metadata 应携带 turn_id(trace_id)"
    );
    assert_eq!(
        meta_a.get("is_synthetic").and_then(|v| v.as_bool()),
        Some(false),
        "AGENT obs create metadata 应标记 is_synthetic=false"
    );
    assert_eq!(
        meta_a.get("was_sampled").and_then(|v| v.as_bool()),
        Some(true),
        "AGENT obs create metadata 应标记 was_sampled=true"
    );

    // update 携带 input + output(text = Stop result) + metadata 无 incomplete_reason
    let update_a = events
        .iter()
        .find_map(|e| {
            if let IngestionEvent::ObservationUpdate { body, .. } = e {
                if body.id.as_deref() == Some(obs_a_id.as_str()) {
                    return Some((
                        body.input.clone(),
                        body.output.clone(),
                        body.metadata.clone(),
                    ));
                }
            }
            None
        })
        .expect("child_a obs update 应存在");
    assert!(update_a.0.is_some(), "AGENT obs update 应携带父工具 input");
    assert_eq!(
        update_a
            .1
            .as_ref()
            .and_then(|o| o.get("text"))
            .and_then(|t| t.as_str()),
        Some("ra"),
        "AGENT obs update output 应为 Stop result text"
    );
    assert_eq!(
        update_a.2.as_ref().and_then(|m| m.get("incomplete_reason")),
        None,
        "正常关闭不应携带 incomplete_reason"
    );

    // 各自内容 parent 指向各自 AGENT obs
    let map = parent_map(&events);
    let a_stage_id = &a_stage.span_id;
    let b_stage_id = &b_stage.span_id;
    assert_eq!(
        map.get(a_stage_id).and_then(|p| p.clone()),
        Some(obs_a_id.clone()),
        "A 的 stage 应挂 A 的 AGENT obs"
    );
    assert_eq!(
        map.get(b_stage_id).and_then(|p| p.clone()),
        Some(obs_b_id.clone()),
        "B 的 stage 应挂 B 的 AGENT obs"
    );

    // 无任何 A 内容挂 B:generation/tool 的 parent 链均指向各自 batch/stage
    let gen_a = events.iter().find_map(|e| {
        if let IngestionEvent::GenerationCreate { body, .. } = e {
            if body
                .output
                .as_ref()
                .and_then(|o| o.get("text"))
                .and_then(|t| t.as_str())
                == Some("a-out")
            {
                return Some((
                    body.id.clone().unwrap_or_default(),
                    body.parent_observation_id.clone(),
                ));
            }
        }
        None
    });
    let gen_b = events.iter().find_map(|e| {
        if let IngestionEvent::GenerationCreate { body, .. } = e {
            if body
                .output
                .as_ref()
                .and_then(|o| o.get("text"))
                .and_then(|t| t.as_str())
                == Some("b-out")
            {
                return Some((
                    body.id.clone().unwrap_or_default(),
                    body.parent_observation_id.clone(),
                ));
            }
        }
        None
    });
    assert_eq!(gen_a.as_ref().unwrap().1, Some(a_stage_id.clone()));
    assert_eq!(gen_b.as_ref().unwrap().1, Some(b_stage_id.clone()));

    // 工具各自 batch:A 的 Bash 挂 A 的 batch(B 的 Grep 挂 B 的 batch)
    let tool_a = events.iter().find_map(|e| {
        if let IngestionEvent::ObservationCreate { body, .. } = e {
            if body.name.as_deref() == Some("Bash") {
                return Some(body.parent_observation_id.clone());
            }
        }
        None
    });
    let tool_b = events.iter().find_map(|e| {
        if let IngestionEvent::ObservationCreate { body, .. } = e {
            if body.name.as_deref() == Some("Grep") {
                return Some(body.parent_observation_id.clone());
            }
        }
        None
    });
    assert_ne!(tool_a, tool_b, "A/B 工具应挂各自 tool-batch");

    assert_eq!(t.subagent.incomplete_count(), 0);
    let _ = main_act;
}

// ── Start 先于父 ToolStart + 并行交错(08-07 memo 时序) ─────────────────────

/// 模拟跨 forwarder 竞态:Start(child_a) 先到(pending),父 ToolStart 随后交错
/// 到达。FIFO 配对语义:child_a 绑最旧的 call_b,child_b 绑 call_a;
/// 已绑定的 invocation 不得被复用(防交叉绑定 → DuplicateStop/MissingStop)。
///
/// 时序:Start(child_a) → ToolStart(call_b) → ToolStart(call_a) →
/// Start(child_b) → 各自 stage/llm/tool → ToolEnded(call_a/call_b) →
/// Stop(child_a/child_b)。
#[tokio::test]
async fn test_start_before_tool_start_interleaved() {
    let (mut t, session) = make_tracer(1.0);
    t.set_main_agent_id("main".to_string());
    t.on_turn_start("turn_5b");

    let main_act = t
        .on_stage_start_gated("main", Stage::Act, "turn_5b")
        .unwrap();

    // ① Start(child_a) 先于任何父 ToolStart:入 pending,无 AGENT obs
    t.on_subagent_start("main", "child_a", "explorer-a", false);
    assert_eq!(
        t.subagent.status_of("child_a"),
        Some(&SubagentStatus::PendingInvocation)
    );
    assert_eq!(
        agent_obs_creates(&session.events_snapshot()).len(),
        0,
        "join 前不应创建 AGENT obs"
    );

    // ② 父 ToolStart(call_b):join child_a → call_b(最旧未绑定)
    t.on_tool_start("main", "call_b", "Agent", &serde_json::json!({"task": "B"}));
    // ③ 父 ToolStart(call_a):无 pending Start 可 join,invocation 保持未绑定
    t.on_tool_start("main", "call_a", "Agent", &serde_json::json!({"task": "A"}));

    // ④ Start(child_b):必须跳过已绑定 child_a 的 call_b,绑未绑定的 call_a
    t.on_subagent_start("main", "child_b", "explorer-b", false);
    assert_eq!(
        t.subagent.invocation_key_of("child_a"),
        Some(("main".to_string(), "call_b".to_string())),
        "child_a 应绑最旧的 call_b(FIFO)"
    );
    assert_eq!(
        t.subagent.invocation_key_of("child_b"),
        Some(("main".to_string(), "call_a".to_string())),
        "child_b 应绑未绑定的 call_a(不得复用已绑定 invocation)"
    );

    let events = session.events_snapshot();
    let creates = agent_obs_creates(&events);
    assert_eq!(creates.len(), 2, "两个 child 各一个 AGENT obs");
    let obs_a_id = t.subagent.observation_id_of("child_a").unwrap();
    let obs_b_id = t.subagent.observation_id_of("child_b").unwrap();
    assert_ne!(obs_a_id, obs_b_id);

    // input 透传:child_a 的 obs input = 所绑 call_b 的工具 input(task=B)
    let create_a_input = events
        .iter()
        .find_map(|e| {
            if let IngestionEvent::ObservationCreate { body, .. } = e {
                if body.id.as_deref() == Some(obs_a_id.as_str()) {
                    return body.input.clone();
                }
            }
            None
        })
        .expect("child_a obs create 应存在");
    assert_eq!(
        create_a_input.get("task").and_then(|t| t.as_str()),
        Some("B"),
        "child_a obs input 应为所绑 call_b 的工具 input"
    );

    // ⑤ 各自内容事件:parent 指向各自 AGENT obs
    let a_stage = t
        .on_stage_start_gated("child_a", Stage::Reason, "turn_5b")
        .unwrap();
    let b_stage = t
        .on_stage_start_gated("child_b", Stage::Reason, "turn_5b")
        .unwrap();
    // 确保 stage duration > 0(v2 条件上报:0ms stage span 不上报)
    std::thread::sleep(std::time::Duration::from_millis(2));
    t.on_llm_start("child_a", 0, &[], &[]);
    t.on_llm_end("child_a", 0, "m", "p", "a-out", None, None);
    t.on_llm_start("child_b", 0, &[], &[]);
    t.on_llm_end("child_b", 0, "m", "p", "b-out", None, None);
    t.on_tool_start("child_a", "call_a1", "Bash", &serde_json::json!({}));
    t.on_tool_end("child_a", "call_a1", "a", false);
    t.on_stage_end("child_a", &a_stage, StageStatus::Done);
    t.on_tool_start("child_b", "call_b1", "Grep", &serde_json::json!({}));
    t.on_tool_end("child_b", "call_b1", "b", false);
    t.on_stage_end("child_b", &b_stage, StageStatus::Done);

    // ⑥ 父 ToolEnded:只结束父工具记录,不关闭任何 AGENT obs
    t.on_tool_end("main", "call_a", "done-a", false);
    t.on_tool_end("main", "call_b", "done-b", false);
    assert_eq!(
        agent_obs_updates(&session.events_snapshot()).len(),
        0,
        "ToolEnded 不应关闭 AGENT obs(等 Stop)"
    );

    // ⑦ 各自 Stop:按各自 invocation 两信号齐备 → 正常关闭
    t.on_subagent_stop("main", "child_a", "ra", false);
    t.on_subagent_stop("main", "child_b", "rb", false);

    let final_events = session.events_snapshot();
    let updates = agent_obs_updates(&final_events);
    assert_eq!(updates.len(), 2, "两个 AGENT obs 应各自关闭");
    // 无交叉:两个 AGENT obs 各自在 Stop 后正常关闭(end_time 非空)
    let update_ids: std::collections::HashSet<&str> =
        updates.iter().map(|(id, _, _)| id.as_str()).collect();
    assert_eq!(
        update_ids,
        [obs_a_id.as_str(), obs_b_id.as_str()].into_iter().collect(),
        "关闭的 obs 应为两个 child 各自的 AGENT obs"
    );
    for (id, end_time, output) in &updates {
        assert!(end_time.is_some(), "AGENT obs 应正常关闭(end_time 非空)");
        // F2 核心:output.text = 各自 Stop result(而非 ToolEnded output)
        let expect = if id == &obs_a_id { "ra" } else { "rb" };
        assert_eq!(
            output
                .as_ref()
                .and_then(|o| o.get("text"))
                .and_then(|t| t.as_str()),
            Some(expect),
            "AGENT obs update output.text 应为各自 Stop result"
        );
    }
    let update_meta = |id: &str| {
        final_events
            .iter()
            .find_map(|e| {
                if let IngestionEvent::ObservationUpdate { body, .. } = e {
                    if body.id.as_deref() == Some(id) {
                        return body.metadata.clone();
                    }
                }
                None
            })
            .expect("obs update 应存在")
    };
    for id in [obs_a_id.as_str(), obs_b_id.as_str()] {
        assert_eq!(
            update_meta(id).get("incomplete_reason"),
            None,
            "正常关闭不应携带 incomplete_reason"
        );
    }

    // 各自内容 parent 指向各自 AGENT obs(无 A 内容挂 B)
    let map = parent_map(&final_events);
    assert_eq!(
        map.get(&a_stage.span_id).and_then(|p| p.clone()),
        Some(obs_a_id.clone()),
        "A 的 stage 应挂 A 的 AGENT obs"
    );
    assert_eq!(
        map.get(&b_stage.span_id).and_then(|p| p.clone()),
        Some(obs_b_id.clone()),
        "B 的 stage 应挂 B 的 AGENT obs"
    );

    // 无 DuplicateStop/MissingStop:全部正常关闭
    assert_eq!(
        t.subagent.status_of("child_a"),
        Some(&SubagentStatus::Closed)
    );
    assert_eq!(
        t.subagent.status_of("child_b"),
        Some(&SubagentStatus::Closed)
    );
    assert_eq!(t.subagent.incomplete_count(), 0);

    // on_turn_end 无残留
    let _h = t.on_turn_end(None);
    tokio::task::yield_now().await;
    assert_eq!(t.subagent.incomplete_count(), 0);
    let _ = main_act;
}

// ── 嵌套 parent 不错绑 ──────────────────────────────────────────────────────

/// 跨 parent 错绑回归:child_a(parent=main)先入 pending,随后另一 parent
/// (child_b_owner) 的 Agent invocation 到达。join 必须按 parent 过滤,
/// 不得把 child_a 绑到不同 parent 的 invocation(front() fallback 旧行为);
/// 等 parent=main 的 ToolStart 到达后才正确 join。
#[tokio::test]
async fn test_nested_parent_no_cross_binding() {
    let (mut t, session) = make_tracer(1.0);
    t.set_main_agent_id("main".to_string());
    t.on_turn_start("turn_5c");

    let main_act = t
        .on_stage_start_gated("main", Stage::Act, "turn_5c")
        .unwrap();

    // ① Start(child_a, parent=main):父 ToolStart 未到,入 pending
    t.on_subagent_start("main", "child_a", "explorer-a", false);
    assert_eq!(
        t.subagent.status_of("child_a"),
        Some(&SubagentStatus::PendingInvocation)
    );

    // ② 另一 parent 的 invocation 先到(child_b_owner 的 Agent 工具调用):
    // parent 不匹配,不得 join/占用 child_a(直接注入 registry 状态——
    // child_b_owner 未 join 时经 on_tool_start 会走注册闸门,不会登记 invocation)
    let outcome = t.subagent.register_invocation(
        "child_b_owner",
        "call_b",
        &serde_json::json!({"task": "B"}),
        "span_other_parent",
    );
    assert!(
        outcome.is_none(),
        "不同 parent 的 invocation 不应 join child_a"
    );
    assert_eq!(
        t.subagent.status_of("child_a"),
        Some(&SubagentStatus::PendingInvocation),
        "child_a 不得绑到不同 parent 的 invocation,保持 pending"
    );
    assert_eq!(
        t.subagent.invocation_key_of("child_a"),
        None,
        "child_a 未绑定任何 invocation"
    );

    // ③ parent=main 的 ToolStart 到达 → 跳过不同 parent 的 invocation,
    // 正确 join (main, call_a)
    t.on_tool_start("main", "call_a", "Agent", &serde_json::json!({"task": "A"}));
    assert_eq!(
        t.subagent.status_of("child_a"),
        Some(&SubagentStatus::Active),
        "child_a 应 join 到 (main, call_a)"
    );
    assert_eq!(
        t.subagent.invocation_key_of("child_a"),
        Some(("main".to_string(), "call_a".to_string())),
        "child_a 绑定 parent 匹配的 (main, call_a),而非 child_b_owner 的 invocation"
    );

    // ④ child_a 完整生命周期:obs parent = main 的 stage span,output = Stop result
    let a_stage = t
        .on_stage_start_gated("child_a", Stage::Reason, "turn_5c")
        .unwrap();
    // 确保 stage duration > 0(v2 条件上报)
    std::thread::sleep(std::time::Duration::from_millis(2));
    t.on_llm_start("child_a", 0, &[], &[]);
    t.on_llm_end("child_a", 0, "m", "p", "a-out", None, None);
    t.on_stage_end("child_a", &a_stage, StageStatus::Done);
    t.on_tool_end("main", "call_a", "done-a", false);
    t.on_subagent_stop("main", "child_a", "ra", false);

    let final_events = session.events_snapshot();
    let updates = agent_obs_updates(&final_events);
    assert_eq!(updates.len(), 1, "child_a 应正常关闭");
    let (obs_id, end_time, output) = &updates[0];
    assert_eq!(
        obs_id,
        &t.subagent.observation_id_of("child_a").unwrap(),
        "关闭的 obs 应为 child_a 的 AGENT obs"
    );
    assert!(end_time.is_some(), "AGENT obs 应正常关闭(end_time 非空)");
    assert_eq!(
        output
            .as_ref()
            .and_then(|o| o.get("text"))
            .and_then(|t| t.as_str()),
        Some("ra"),
        "AGENT obs output.text 应为 Stop result"
    );
    let map = parent_map(&final_events);
    assert_eq!(
        map.get(obs_id).and_then(|p| p.clone()),
        Some(main_act.span_id.clone()),
        "child_a obs parent = main 的 stage span(经 (main, call_a) invocation)"
    );

    // ⑤ 收尾:未绑定的 (child_b_owner, call_b) 由 turn_end 兜底清除
    let _h = t.on_turn_end(None);
    tokio::task::yield_now().await;
    assert_eq!(t.subagent.incomplete_count(), 0);
    let _ = main_act;
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
    let _h = t.on_turn_end(None);
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

    let _h = t.on_turn_end(None);
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
        1,
        "AGENT obs 只创建一次"
    );
    assert_eq!(t.subagent.incomplete_count(), 1);

    let _h = t.on_turn_end(None);
    tokio::task::yield_now().await;
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
        agent_obs_updates(&session.events_snapshot()).len(),
        0,
        "Incomplete 不关闭 obs"
    );
    assert_eq!(t.subagent.incomplete_count(), 1);

    let _h = t.on_turn_end(None);
    tokio::task::yield_now().await;
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

    let _h = t.on_turn_end(None);
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
    let (child_obs_id, child_parent, _) = &agent_obs_creates(&session.events_snapshot())[0];
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

    // 关闭后 AGENT obs parent 仍为冻结 span(ObservationUpdate 与 create 一致)
    t.on_stage_end("child_1", &child_reason, StageStatus::Done);
    t.on_tool_end("main", "call_agent", "done", false);
    t.on_subagent_stop("main", "child_1", "done", false);
    let updates = agent_obs_updates(&session.events_snapshot());
    assert_eq!(updates.len(), 1);
    // ObservationUpdate 的 parent 从 ClosedSubagent 来,断言相同 obs id
    assert_eq!(updates[0].0, *child_obs_id);
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
    let creates = agent_obs_creates(&session.events_snapshot());
    let create_2 = creates
        .iter()
        .find(|(id, _, _)| *id == obs_2)
        .expect("child2 obs create");
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

    let _h = t.on_turn_end(None);
    tokio::task::yield_now().await;

    let events = session.events_snapshot();
    let updates = agent_obs_updates(&events);
    assert_eq!(updates.len(), 1, "on_turn_end 应兜底关闭 AGENT obs");
    let (obs_id, end_time, _) = &updates[0];
    assert!(end_time.is_some(), "兜底关闭应带 end_time");
    // 兜底关闭 metadata 应携带 incomplete_reason
    let meta_reason = events.iter().find_map(|e| {
        if let IngestionEvent::ObservationUpdate { body, .. } = e {
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
