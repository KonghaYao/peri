use super::LangfuseBridge;
use crate::langfuse::config::LangfuseConfig;
use crate::langfuse::fake_session::FakeLangfuseSession;
use crate::langfuse::tracer::LangfuseTracer;
use langfuse_client::types::ObservationType;
use langfuse_client::IngestionEvent;
use parking_lot::Mutex;
use peri_acp_types::identity::AgentId;
use peri_agent::agent::events::Stage;
use peri_agent::agent::events::StageStatus;
use peri_agent::agent::events_v2::ObserveEvent;
use peri_agent::agent::events_v2::RenderEvent;
use peri_agent::session::turn::TurnId;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use peri_agent::agent::LangfuseBridgeLike;

type ObsRef = (String, Option<String>, Option<String>, Option<String>);

struct Harness {
    /// 主 agent forwarder 的 bridge（带 main_agent_id）
    b1: LangfuseBridge,
    /// subagent forwarder 的 bridge（不带 main_agent_id，与生产 bridge2 一致）
    b2: LangfuseBridge,
    session: Arc<FakeLangfuseSession>,
    tracer: Arc<Mutex<LangfuseTracer>>,
    turn: TurnId,
    main: AgentId,
}

impl Harness {
    fn main_stage_start(&self, stage: Stage) {
        self.b1.process_observe_event(&ObserveEvent::StageStarted {
            turn_id: self.turn,
            agent_id: self.main,
            stage,
        });
    }

    fn main_stage_end(&self, stage: Stage, status: StageStatus) {
        self.b1.process_observe_event(&ObserveEvent::StageEnded {
            turn_id: self.turn,
            agent_id: self.main,
            stage,
            status,
            duration_ms: 1,
        });
    }

    fn main_tool_start(&self, tool_call_id: &str, name: &str) {
        self.b1.process_render_event(&RenderEvent::ToolStarted {
            turn_id: self.turn,
            agent_id: self.main,
            tool_call_id: tool_call_id.to_string(),
            name: name.to_string(),
            input: serde_json::json!({}),
        });
    }

    fn main_tool_end(&self, tool_call_id: &str) {
        self.b1.process_render_event(&RenderEvent::ToolEnded {
            turn_id: self.turn,
            agent_id: self.main,
            tool_call_id: tool_call_id.to_string(),
            name: "Agent".to_string(),
            output: "dispatched".to_string(),
            is_error: false,
            subagent_failure: None,
        });
    }

    fn child_start(&self, child: AgentId, name: &str, is_background: bool) {
        self.b2.process_observe_event(&ObserveEvent::SubagentStart {
            turn_id: self.turn,
            agent_id: self.main,
            child_agent_id: child,
            agent_name: name.to_string(),
            is_background,
            parent_tool_call_id: None,
        });
    }

    fn child_stop(&self, child: AgentId, result: &str) {
        self.b2.process_observe_event(&ObserveEvent::SubagentStop {
            turn_id: self.turn,
            agent_id: self.main,
            child_agent_id: child,
            agent_name: "fork".to_string(),
            result: result.to_string(),
            is_error: false,
            subagent_failure: None,
        });
    }

    fn child_stage_start(&self, child: AgentId, stage: Stage) {
        self.b2.process_observe_event(&ObserveEvent::StageStarted {
            turn_id: self.turn,
            agent_id: child,
            stage,
        });
    }

    fn child_stage_end(&self, child: AgentId, stage: Stage, status: StageStatus) {
        self.b2.process_observe_event(&ObserveEvent::StageEnded {
            turn_id: self.turn,
            agent_id: child,
            stage,
            status,
            duration_ms: 1,
        });
    }

    fn child_llm_start(&self, child: AgentId, step: usize) {
        self.b2.process_observe_event(&ObserveEvent::LlmCallStart {
            turn_id: self.turn,
            agent_id: child,
            step,
            messages: Arc::new(Vec::new()),
            tools: Vec::new(),
        });
    }

    fn child_llm_end(&self, child: AgentId, step: usize, output: &str) {
        self.b2.process_observe_event(&ObserveEvent::LlmCallEnd {
            turn_id: self.turn,
            agent_id: child,
            step,
            model: "claude-4.7".to_string(),
            output: output.to_string(),
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
            request_id: None,
        });
    }

    fn child_tool_start(&self, child: AgentId, tool_call_id: &str, name: &str) {
        self.b2.process_render_event(&RenderEvent::ToolStarted {
            turn_id: self.turn,
            agent_id: child,
            tool_call_id: tool_call_id.to_string(),
            name: name.to_string(),
            input: serde_json::json!({}),
        });
    }

    fn child_tool_end(&self, child: AgentId, tool_call_id: &str) {
        self.b2.process_render_event(&RenderEvent::ToolEnded {
            turn_id: self.turn,
            agent_id: child,
            tool_call_id: tool_call_id.to_string(),
            name: "Bash".to_string(),
            output: "ok".to_string(),
            is_error: false,
            subagent_failure: None,
        });
    }

    fn turn_end(&self) {
        drop(
            self.tracer
                .lock()
                .on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed),
        );
    }
}

fn child_id(n: u128) -> AgentId {
    AgentId::from_uuid(uuid::Uuid::from_u128(
        0x2222_2222_2222_2222_2222_2222_2222_2222 + n,
    ))
}

fn harness() -> Harness {
    let session = FakeLangfuseSession::new("sess_bridge");
    let config = LangfuseConfig {
        public_key: None,
        secret_key: None,
        host: "https://cloud.langfuse.com".to_string(),
        trace_sampling: 1.0,
        error_span_always: true,
        batch_max_events: 50,
        batch_flush_interval_secs: 10,
        user_id: None,
        ..Default::default()
    };
    let tracer = Arc::new(Mutex::new(LangfuseTracer::new(
        session.clone(),
        "sess_bridge".to_string(),
        config,
    )));
    let main = AgentId::from_uuid(uuid::Uuid::from_u128(
        0x1111_1111_1111_1111_1111_1111_1111_1111,
    ));
    let b1 = LangfuseBridge::new(
        tracer.clone(),
        "main-provider".to_string(),
        Some(main.to_string()),
    );
    let b2 = LangfuseBridge::new(tracer.clone(), "sub-provider".to_string(), None);
    Harness {
        b1,
        b2,
        session,
        tracer,
        turn: TurnId::new(),
        main,
    }
}

fn build_graph(events: &[IngestionEvent]) -> HashMap<String, Option<String>> {
    assert!(!events.iter().any(|event| matches!(
        event,
        IngestionEvent::ObservationUpdate { .. }
            | IngestionEvent::SpanUpdate { .. }
            | IngestionEvent::GenerationUpdate { .. }
            | IngestionEvent::SessionCreate { .. }
    )));
    let mut map: HashMap<String, Option<String>> = HashMap::new();
    for e in events {
        let (id, parent) = match e {
            IngestionEvent::ObservationCreate { body, .. } => {
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
            if let Some(existing) = map.get(&id) {
                assert_eq!(
                    existing, &parent,
                    "obs {id} 出现多个 parent（create/update 漂移）"
                );
            }
            map.insert(id, parent);
        }
    }
    map
}

fn assert_acyclic(graph: &HashMap<String, Option<String>>) {
    for (id, parent) in graph {
        let mut cur = parent.clone();
        let mut hops = 0;
        while let Some(p) = cur {
            assert!(hops < 32, "obs {id} 的 parent 链过长（疑似环）");
            assert_ne!(&p, id, "检测到环：obs {id} 的 parent 链回到自身");
            cur = graph.get(&p).cloned().flatten();
            hops += 1;
        }
    }
}

fn chain_reaches_before(
    graph: &HashMap<String, Option<String>>,
    node: &str,
    target: &str,
    forbidden: &str,
) -> bool {
    let mut cur = graph.get(node).cloned().flatten();
    let mut hops = 0;
    while let Some(p) = cur {
        assert!(hops < 32, "obs {node} 的 parent 链过长（疑似环）");
        if p == target {
            return true;
        }
        if p == forbidden {
            return false;
        }
        cur = graph.get(&p).cloned().flatten();
        hops += 1;
    }
    false
}

fn agent_obs_creates(events: &[IngestionEvent]) -> Vec<(String, Option<String>, Option<String>)> {
    events
        .iter()
        .filter_map(|e| {
            if let IngestionEvent::ObservationCreate { body, .. } = e {
                if body.r#type == ObservationType::Agent
                    && body.name.as_deref() != Some("agent-run")
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
                    && body.name.as_deref() != Some("agent-run")
                {
                    return Some((
                        body.id.clone().unwrap_or_default(),
                        body.end_time.clone(),
                        body.metadata.clone(),
                    ));
                }
            }
            None
        })
        .collect()
}

fn span_by_name(events: &[IngestionEvent], name: &str) -> Option<(String, Option<String>)> {
    events.iter().find_map(|e| {
        if let IngestionEvent::SpanCreate { body, .. } = e {
            if body.name.as_deref() == Some(name) {
                return Some((
                    body.id.clone().unwrap_or_default(),
                    body.parent_observation_id.clone(),
                ));
            }
        }
        None
    })
}

fn tool_obs_by_name(events: &[IngestionEvent], name: &str) -> Option<ObsRef> {
    events.iter().find_map(|e| {
        if let IngestionEvent::ObservationCreate { body, .. } = e {
            if body.r#type == ObservationType::Tool && body.name.as_deref() == Some(name) {
                return Some((
                    body.id.clone().unwrap_or_default(),
                    body.parent_observation_id.clone(),
                    body.start_time.clone(),
                    body.end_time.clone(),
                ));
            }
        }
        None
    })
}

fn generation_by_output(events: &[IngestionEvent], output: &str) -> Option<ObsRef> {
    events.iter().find_map(|e| {
        if let IngestionEvent::GenerationCreate { body, .. } = e {
            let text = body
                .output
                .as_ref()
                .and_then(|o| o.get("text"))
                .and_then(|t| t.as_str());
            if text == Some(output) {
                return Some((
                    body.id.clone().unwrap_or_default(),
                    body.parent_observation_id.clone(),
                    body.start_time.clone(),
                    body.end_time.clone(),
                ));
            }
        }
        None
    })
}

fn assert_agent_time_contains(
    events: &[IngestionEvent],
    agent_obs_id: &str,
    child_content_ids: &[&str],
) {
    let (a_start, a_end) = obs_time_range(events, agent_obs_id);
    let a_start = a_start.expect("AGENT obs 应有 start_time");
    let a_end = a_end.expect("AGENT obs 应有 end_time（已关闭）");
    let mut earliest: Option<String> = None;
    let mut latest: Option<String> = None;
    for id in child_content_ids {
        let (s, e) = obs_time_range(events, id);
        if let Some(s) = s {
            if earliest
                .as_deref()
                .is_none_or(|v| parse_time(&s) < parse_time(v))
            {
                earliest = Some(s);
            }
        }
        if let Some(e) = e {
            if latest
                .as_deref()
                .is_none_or(|v| parse_time(&e) > parse_time(v))
            {
                latest = Some(e);
            }
        }
    }
    if let Some(earliest) = earliest {
        assert!(
            parse_time(&a_start) <= parse_time(&earliest),
            "AGENT obs start({a_start}) 应 ≤ 最早 child 事件 start({earliest})（无空壳）"
        );
    }
    if let Some(latest) = latest {
        assert!(
            parse_time(&a_end) >= parse_time(&latest),
            "AGENT obs end({a_end}) 应 ≥ 最晚 child 事件 end({latest})"
        );
    }
}

fn assert_main_structure(
    graph: &HashMap<String, Option<String>>,
    events: &[IngestionEvent],
    main_obs: &str,
) -> (String, String) {
    // 主 Act stage span：parent 直接为 agent-run 且 name 为 stage-act
    // （subagent 的 stage 挂各自 AGENT obs，不直接挂 agent-run）
    let main_stage = events
        .iter()
        .filter_map(|e| {
            if let IngestionEvent::SpanCreate { body, .. } = e {
                if body.name.as_deref() == Some("stage-act")
                    && body.parent_observation_id.as_deref() == Some(main_obs)
                {
                    return body.id.clone();
                }
            }
            None
        })
        .next()
        .expect("主 stage-act span 应上报且 parent=agent-run");
    assert_eq!(
        graph.get(&main_stage).and_then(|p| p.clone()),
        Some(main_obs.to_string()),
        "主 stage-act 应直接挂 agent-run"
    );

    // 主 tool-batch：name == "tool-batch" 且 parent = 主 stage span
    let batch_id = events
        .iter()
        .filter_map(|e| {
            if let IngestionEvent::SpanCreate { body, .. } = e {
                if body.name.as_deref() == Some("tool-batch")
                    && body.parent_observation_id.as_deref() == Some(main_stage.as_str())
                {
                    return body.id.clone();
                }
            }
            None
        })
        .next()
        .expect("主 tool-batch 应挂主 stage span");

    // 主工具 obs：ObservationCreate(Tool) 且 parent = 主 tool-batch
    let has_main_tool = events.iter().any(|e| {
        if let IngestionEvent::ObservationCreate { body, .. } = e {
            body.r#type == ObservationType::Tool
                && body.parent_observation_id.as_deref() == Some(batch_id.as_str())
        } else {
            false
        }
    });
    assert!(has_main_tool, "主工具 obs 应挂主 tool-batch");
    (main_stage, batch_id)
}

fn obs_time_range(events: &[IngestionEvent], id: &str) -> (Option<String>, Option<String>) {
    let mut start = None;
    let mut end = None;
    for e in events {
        let (oid, st, et) = match e {
            IngestionEvent::ObservationCreate { body, .. } => (
                body.id.clone(),
                body.start_time.clone(),
                body.end_time.clone(),
            ),
            IngestionEvent::SpanCreate { body, .. } | IngestionEvent::SpanUpdate { body, .. } => (
                body.id.clone(),
                body.start_time.clone(),
                body.end_time.clone(),
            ),
            IngestionEvent::GenerationCreate { body, .. } => (
                body.id.clone(),
                body.start_time.clone(),
                body.end_time.clone(),
            ),
            _ => continue,
        };
        if oid.as_deref() == Some(id) {
            if let Some(s) = st {
                start = Some(s);
            }
            if let Some(e) = et {
                end = Some(e);
            }
        }
    }
    (start, end)
}

fn parse_time(s: &str) -> chrono::DateTime<chrono::FixedOffset> {
    chrono::DateTime::parse_from_rfc3339(s).expect("rfc3339 时间")
}

/// 内容先于 Start 与父 ToolStart（注册闸门 + parent-first 重放）：
/// child StageStarted/LlmCallStart/ToolStart/ToolEnd → Start(pending) → 父 ToolStart(join+重放)。
/// 重放后 parent 正确；无任何 obs 挂 agent-run。
#[tokio::test]
async fn test_content_before_start_replay() {
    let h = harness();
    h.main_stage_start(Stage::Act); // 父 stage 先建（ToolStart 的 parent 冻结来源）
    let c1 = child_id(1);
    h.child_stage_start(c1, Stage::Reason); // ① gate
    h.child_llm_start(c1, 0); // ② gate
    h.child_tool_start(c1, "call_bash", "Bash"); // gate
    h.child_tool_end(c1, "call_bash"); // gate
    h.child_start(c1, "fork", false); // ③ Start：join 失败 → PendingInvocation
    h.main_tool_start("call_agent", "Agent"); // ④ 父 ToolStart：join + 按原顺序重放
    std::thread::sleep(Duration::from_millis(2));
    h.child_stage_end(c1, Stage::Reason, StageStatus::Done); // 重放 handle 由 StageEnded 领取
    h.child_llm_end(c1, 0, "replayed-out");
    h.main_tool_end("call_agent");
    h.child_stop(c1, "done");
    h.main_stage_end(Stage::Act, StageStatus::Done);
    h.turn_end();
    tokio::task::yield_now().await;

    let events = h.session.events_snapshot();
    let graph = build_graph(&events);
    assert_acyclic(&graph);
    let main_obs = h.tracer.lock().agent_observation_id.clone();
    let (main_stage, _) = assert_main_structure(&graph, &events, &main_obs);

    let creates = agent_obs_creates(&events);
    assert_eq!(creates.len(), 1, "join 后应创建 AGENT obs");
    assert_eq!(
        creates[0].1.as_deref(),
        Some(main_stage.as_str()),
        "AGENT obs parent = join 时冻结的父 stage span"
    );
    // 重放的 stage/generation 归属 child AGENT obs
    let (reason_span, reason_parent) =
        span_by_name(&events, "stage-reason").expect("重放的 stage span 应上报");
    assert_eq!(
        reason_parent.as_deref(),
        Some(creates[0].0.as_str()),
        "重放的 stage 应挂 child AGENT obs"
    );
    let (gen_id, gen_parent, _, _) =
        generation_by_output(&events, "replayed-out").expect("重放的 generation 应上报");
    assert!(
        chain_reaches_before(&graph, &gen_id, &creates[0].0, &main_obs),
        "重放的 LLM 应先归 child AGENT obs"
    );
    assert_ne!(
        gen_parent.as_deref(),
        Some(main_obs.as_str()),
        "重放内容不得挂 agent-run"
    );
    // 重放的 Bash 工具：挂 child 自己的 batch
    let (bash_id, _, _, _) = tool_obs_by_name(&events, "Bash").expect("重放的 Bash 应上报");
    assert!(
        chain_reaches_before(&graph, &bash_id, &creates[0].0, &main_obs),
        "重放的工具应先归 child AGENT obs"
    );
    // 无任何 obs 直接挂 agent-run（仅主 stage span）
    assert_eq!(
        graph
            .iter()
            .filter(|(_, p)| p.as_deref() == Some(main_obs.as_str()))
            .count(),
        1
    );
    assert_agent_time_contains(
        &events,
        &creates[0].0,
        &[reason_span.as_str(), gen_id.as_str()],
    );
    assert_eq!(agent_obs_completions(&events).len(), 1);
}

/// Start 先于父 ToolStart：child Start(pending) → child 内容(gate) → 父 ToolStart(join+重放)。
#[tokio::test]
async fn test_start_before_parent_tool_start_order() {
    let h = harness();
    h.main_stage_start(Stage::Act);
    let c1 = child_id(1);
    h.child_start(c1, "bg", true); // ① Start：join 失败 → pending_starts
    h.child_stage_start(c1, Stage::Reason); // ② gate
    h.main_tool_start("call_agent", "Agent"); // ③ 父 ToolStart：join + 重放
    std::thread::sleep(Duration::from_millis(2));
    h.child_stage_end(c1, Stage::Reason, StageStatus::Done);
    h.main_tool_end("call_agent");
    h.child_stop(c1, "done");
    h.main_stage_end(Stage::Act, StageStatus::Done);
    h.turn_end();
    tokio::task::yield_now().await;

    let events = h.session.events_snapshot();
    let graph = build_graph(&events);
    assert_acyclic(&graph);
    let main_obs = h.tracer.lock().agent_observation_id.clone();
    let (main_stage, _) = assert_main_structure(&graph, &events, &main_obs);

    let creates = agent_obs_creates(&events);
    assert_eq!(creates.len(), 1, "父 ToolStart 应 join 成功");
    assert_eq!(
        creates[0].1.as_deref(),
        Some(main_stage.as_str()),
        "AGENT obs parent = 冻结的父 stage span"
    );
    let (reason_span, reason_parent) =
        span_by_name(&events, "stage-reason").expect("重放的 stage span");
    assert_eq!(
        reason_parent.as_deref(),
        Some(creates[0].0.as_str()),
        "重放的 stage 应挂 child AGENT obs"
    );
    assert_eq!(agent_obs_completions(&events).len(), 1);
    assert!(
        graph
            .iter()
            .filter(|(_, p)| p.as_deref() == Some(main_obs.as_str()))
            .count()
            == 1,
        "无任何 obs 直接挂 agent-run"
    );
    let _ = reason_span;
}

/// Start 丢失：父 ToolStart → ToolEnded → child 内容（Start 永不出现）→ turn end。
/// child 内容不挂主 agent；on_turn_end 清缓存 + incomplete；无幽灵序列。
#[tokio::test]
async fn test_missing_start_drops_to_incomplete() {
    let h = harness();
    h.main_stage_start(Stage::Act);
    h.main_tool_start("call_agent", "Agent");
    h.main_tool_end("call_agent"); // ToolEnded 先到（invocation 残留）
    let c1 = child_id(1);
    h.child_stage_start(c1, Stage::Reason); // → 注册闸门缓存
    h.child_llm_start(c1, 0); // → 注册闸门缓存
                              // Start 永不出现
    std::thread::sleep(Duration::from_millis(2)); // 主 Act duration > 0
    h.main_stage_end(Stage::Act, StageStatus::Done);
    h.turn_end();
    tokio::task::yield_now().await;

    let events = h.session.events_snapshot();
    // 无 child AGENT obs、无 child stage/LLM 上报（gate 丢弃，不挂主 agent）
    assert!(
        agent_obs_creates(&events).is_empty(),
        "Start 丢失不应创建 AGENT obs"
    );
    assert!(
        !events.iter().any(|e| {
            if let IngestionEvent::SpanCreate { body, .. } = e {
                body.name.as_deref() == Some("stage-reason")
            } else {
                false
            }
        }),
        "child stage 不应上报"
    );
    let graph = build_graph(&events);
    let main_obs = h.tracer.lock().agent_observation_id.clone();
    assert_eq!(
        graph
            .iter()
            .filter(|(_, p)| p.as_deref() == Some(main_obs.as_str()))
            .count(),
        1,
        "child 内容不得挂 agent-run（仅主 stage span）"
    );
    assert_acyclic(&graph);
    // tracer 侧：gate 清空 + incomplete 计数
    assert_eq!(
        h.tracer.lock().subagent.gated_len(),
        0,
        "turn_end 应清空闸门缓存"
    );
    assert!(
        h.tracer.lock().subagent.incomplete_count() >= 1,
        "缺失 Start 应计数 incomplete"
    );
}

/// Stop 丢失：父 ToolStart → Start → child 内容 → 父 ToolEnded(deferred) → turn end（无 Stop）。
/// 兜底关闭 AGENT obs（metadata 含 incomplete_reason）；无幽灵序列挂主 agent。
#[tokio::test]
async fn test_missing_stop_turn_end_cleanup() {
    let h = harness();
    h.main_stage_start(Stage::Act);
    h.main_tool_start("call_agent", "Agent");
    let c1 = child_id(1);
    h.child_start(c1, "fork", false);
    h.child_stage_start(c1, Stage::Reason);
    std::thread::sleep(Duration::from_millis(2));
    h.child_tool_start(c1, "call_bash", "Bash");
    h.child_tool_end(c1, "call_bash");
    h.child_stage_end(c1, Stage::Reason, StageStatus::Done);
    h.main_tool_end("call_agent"); // deferred_output 已存（Stop 永不出现）
    h.main_stage_end(Stage::Act, StageStatus::Done);
    h.turn_end(); // 兜底
    tokio::task::yield_now().await;

    let events = h.session.events_snapshot();
    let graph = build_graph(&events);
    assert_acyclic(&graph);
    let main_obs = h.tracer.lock().agent_observation_id.clone();

    let creates = agent_obs_creates(&events);
    assert_eq!(creates.len(), 1);
    let completions = agent_obs_completions(&events);
    assert_eq!(completions.len(), 1, "on_turn_end 应兜底关闭 AGENT obs");
    assert_eq!(completions[0].0, creates[0].0);
    assert!(completions[0].1.is_some(), "兜底关闭应带 end_time");
    // metadata 携带 incomplete_reason（MissingStop）
    let meta_reason = completions[0]
        .2
        .as_ref()
        .and_then(|m| m.get("incomplete_reason"))
        .and_then(|r| r.as_str());
    assert_eq!(meta_reason, Some("MissingStop"), "兜底关闭应标 MissingStop");
    // child 工具随兜底 flush 上报，且先归 child AGENT obs
    let (reason_span, reason_parent) =
        span_by_name(&events, "stage-reason").expect("child stage span 应上报");
    let (bash_id, bash_parent, _, _) =
        tool_obs_by_name(&events, "Bash").expect("child Bash 应随兜底 flush 上报");
    assert_eq!(
        reason_parent.as_deref(),
        Some(creates[0].0.as_str()),
        "child stage 应挂 child AGENT obs"
    );
    // child 工具 obs 挂 child 自己的 tool-batch（batch 挂 child stage）
    let child_batch = events
        .iter()
        .filter_map(|e| {
            if let IngestionEvent::SpanCreate { body, .. } = e {
                if body.name.as_deref() == Some("tool-batch")
                    && body.parent_observation_id.as_deref() == Some(reason_span.as_str())
                {
                    return body.id.clone();
                }
            }
            None
        })
        .next()
        .expect("child tool-batch 应挂 child stage");
    assert_eq!(
        bash_parent.as_deref(),
        Some(child_batch.as_str()),
        "child 工具应挂 child 的 tool-batch"
    );
    assert!(
        chain_reaches_before(&graph, &bash_id, &creates[0].0, &main_obs),
        "child 内容应先归 child AGENT obs"
    );
    // 无幽灵序列挂主 agent：除主 stage span 外无 obs 直接挂 agent-run
    assert_eq!(
        graph
            .iter()
            .filter(|(_, p)| p.as_deref() == Some(main_obs.as_str()))
            .count(),
        1
    );
}

/// 注册闸门缓存溢出（有界）：Start 先到（pending）→ 灌 70 条内容事件 →
/// 缓存上限 64、最旧被逐出、等待 join 的 child 标 CacheOverflow →
/// 父 ToolStart 到达不再 join（无 AGENT obs 空壳）；无内容挂 agent-run。
#[tokio::test]
async fn test_gate_cache_overflow_bounded() {
    let h = harness();
    h.main_stage_start(Stage::Act);
    let c1 = child_id(1);
    h.child_start(c1, "bg", true); // Start → pending_starts（父 ToolStart 未到）
    for i in 0..70 {
        h.child_llm_start(c1, i); // 灌内容事件：溢出逐出最旧
    }
    assert!(
        h.tracer.lock().subagent.gated_len() <= 64,
        "注册闸门缓存应有界（≤64）"
    );
    h.main_tool_start("call_agent", "Agent"); // child 已 Incomplete → 不 join
    h.main_tool_end("call_agent");
    std::thread::sleep(Duration::from_millis(2)); // 主 Act duration > 0
    h.main_stage_end(Stage::Act, StageStatus::Done);
    h.turn_end();
    tokio::task::yield_now().await;

    let events = h.session.events_snapshot();
    // 无 AGENT obs 空壳；无任何 child generation 上报
    assert!(
        agent_obs_creates(&events).is_empty(),
        "CacheOverflow 的 child 不应创建 AGENT obs（无空壳）"
    );
    assert!(
        !events
            .iter()
            .any(|e| { matches!(e, IngestionEvent::GenerationCreate { .. }) }),
        "gate 丢弃的 LLM 不应产生 generation"
    );
    let graph = build_graph(&events);
    let main_obs = h.tracer.lock().agent_observation_id.clone();
    assert_eq!(
        graph
            .iter()
            .filter(|(_, p)| p.as_deref() == Some(main_obs.as_str()))
            .count(),
        1,
        "无任何内容挂 agent-run"
    );
    assert_acyclic(&graph);
    assert!(
        h.tracer.lock().subagent.incomplete_count() >= 1,
        "缓存溢出应计数 incomplete"
    );
}
