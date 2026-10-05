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

// ── C12：乱序场景矩阵 ─────────────────────────────────────────────────────────

/// Start 后于父 ToolEnded：①ToolStart → ②ToolEnded → ③Start → ④child events → ⑤Stop。
/// ② 不关闭任何 child、不注销 invocation（join 仍成功）；④ parent 正确；⑤ 正常关闭。
#[tokio::test]
async fn test_start_after_tool_ended_order() {
    let h = harness();
    h.main_stage_start(Stage::Act);
    h.main_tool_start("call_agent", "Agent"); // ① invocation 登记（parent 冻结）
    h.main_tool_end("call_agent"); // ② ToolEnded 先到：tool_ended=true，不注销映射
    h.child_start(child_id(1), "fork", false); // ③ Start：join 未绑定 invocation 仍成功
    h.child_stage_start(child_id(1), Stage::Reason);
    std::thread::sleep(Duration::from_millis(2));
    h.child_llm_start(child_id(1), 0);
    h.child_llm_end(child_id(1), 0, "late-join-analysis");
    h.child_stage_end(child_id(1), Stage::Reason, StageStatus::Done);
    h.child_stop(child_id(1), "done"); // ⑤ Stop：两信号齐备 → 关闭
    h.main_stage_end(Stage::Act, StageStatus::Done);
    h.turn_end();
    tokio::task::yield_now().await;

    let events = h.session.events_snapshot();
    let graph = build_graph(&events);
    assert_acyclic(&graph);
    let main_obs = h.tracer.lock().agent_observation_id.clone();
    let (main_stage, _) = assert_main_structure(&graph, &events, &main_obs);

    let creates = agent_obs_creates(&events);
    assert_eq!(
        creates.len(),
        1,
        "ToolEnded 先到不注销映射，Start 仍应 join 成功"
    );
    assert_eq!(
        creates[0].1.as_deref(),
        Some(main_stage.as_str()),
        "join 时冻结的父 stage span 不变"
    );
    let (reason_span, reason_parent) =
        span_by_name(&events, "stage-reason").expect("child stage span 应上报");
    assert_eq!(
        reason_parent.as_deref(),
        Some(creates[0].0.as_str()),
        "③ 之后的内容应正常归属 child"
    );
    let (gen_id, _, _, _) =
        generation_by_output(&events, "late-join-analysis").expect("child generation");
    assert!(
        chain_reaches_before(&graph, &gen_id, &creates[0].0, &main_obs),
        "child LLM 应先归 child AGENT obs"
    );
    let completions = agent_obs_completions(&events);
    assert_eq!(completions.len(), 1, "⑤ Stop 应关闭 AGENT obs");
    assert_agent_time_contains(&events, &creates[0].0, &[reason_span.as_str()]);
}

/// Stop 先于父 ToolEnded：①ToolStart → ②Start → ③child events → ④Stop → ⑤ToolEnded。
/// ④ 置 StopReceived 不关闭；⑤ 主 batch 结束父工具 + 用 invocation 回收 → 关闭。
#[tokio::test]
async fn test_stop_before_tool_ended_order() {
    let h = harness();
    h.main_stage_start(Stage::Act);
    h.main_tool_start("call_agent", "Agent"); // ①
    h.child_start(child_id(1), "fork", false); // ②
    h.child_stage_start(child_id(1), Stage::Reason);
    std::thread::sleep(Duration::from_millis(2));
    h.child_llm_start(child_id(1), 0);
    h.child_llm_end(child_id(1), 0, "stop-first");
    h.child_tool_start(child_id(1), "call_bash", "Bash");
    h.child_tool_end(child_id(1), "call_bash");
    h.child_stage_end(child_id(1), Stage::Reason, StageStatus::Done);
    h.child_stop(child_id(1), "done"); // ④ Stop 先到：StopReceived，不关闭
    assert_eq!(
        agent_obs_completions(&h.session.events_snapshot()).len(),
        0,
        "Stop 先到不应立即关闭（等父 ToolEnded）"
    );
    h.main_tool_end("call_agent"); // ⑤ ToolEnded：两信号齐备 → 回收关闭
    h.main_stage_end(Stage::Act, StageStatus::Done);
    h.turn_end();
    tokio::task::yield_now().await;

    let events = h.session.events_snapshot();
    let graph = build_graph(&events);
    assert_acyclic(&graph);
    let main_obs = h.tracer.lock().agent_observation_id.clone();

    let completions = agent_obs_completions(&events);
    assert_eq!(
        completions.len(),
        1,
        "ToolEnded 后应关闭 AGENT obs（恰好一次）"
    );
    let creates = agent_obs_creates(&events);
    assert_eq!(
        completions[0].0, creates[0].0,
        "关闭的应为 child 的 AGENT obs"
    );
    // output 优先 Stop result（无 incomplete_reason）
    assert!(
        completions[0]
            .2
            .as_ref()
            .and_then(|m| m.get("incomplete_reason"))
            .is_none(),
        "正常关闭不应带 incomplete_reason"
    );
    // child tool-batch flush 恰好一次（child batch 挂 child stage span）
    let (reason_span, _) = span_by_name(&events, "stage-reason").expect("child stage span");
    let child_batch = events
        .iter()
        .filter(|e| {
            if let IngestionEvent::SpanCreate { body, .. } = e {
                body.name.as_deref() == Some("tool-batch")
                    && body.parent_observation_id.as_deref() == Some(reason_span.as_str())
            } else {
                false
            }
        })
        .count();
    assert_eq!(child_batch, 1, "child tool-batch 应 flush 恰好一次");
    // 无任何内容直接挂 agent-run
    assert_eq!(
        graph
            .iter()
            .filter(|(_, p)| p.as_deref() == Some(main_obs.as_str()))
            .count(),
        1,
        "除主 stage span 外无 obs 直接挂 agent-run"
    );
}

/// 子内容事件（含 Stop）全部先于主 ToolEnded 消费（主 ToolEnded 是最后的事件）：
/// 回收点 = ToolEnded，deferred_output 不丢不重，无 17ms 空壳。
#[tokio::test]
async fn test_reverse_tool_ended_first() {
    let h = harness();
    h.main_stage_start(Stage::Act);
    h.main_tool_start("call_agent", "Agent");
    h.child_start(child_id(1), "fork", false);
    h.child_stage_start(child_id(1), Stage::Reason);
    std::thread::sleep(Duration::from_millis(2));
    h.child_llm_start(child_id(1), 0);
    h.child_llm_end(child_id(1), 0, "reverse-out");
    h.child_stage_end(child_id(1), Stage::Reason, StageStatus::Done);
    h.child_stop(child_id(1), "done"); // Stop 先到
    assert_eq!(
        agent_obs_completions(&h.session.events_snapshot()).len(),
        0,
        "Stop 后仍未关闭（ToolEnded 未到）"
    );
    h.main_tool_end("call_agent"); // 主 ToolEnded 最后到达 → 回收
    h.main_stage_end(Stage::Act, StageStatus::Done);
    h.turn_end();
    tokio::task::yield_now().await;

    let events = h.session.events_snapshot();
    let graph = build_graph(&events);
    assert_acyclic(&graph);
    let main_obs = h.tracer.lock().agent_observation_id.clone();

    let completions = agent_obs_completions(&events);
    assert_eq!(completions.len(), 1, "ToolEnded 触发恰好一次关闭");
    let creates = agent_obs_creates(&events);
    let (gen_id, _, _, _) = generation_by_output(&events, "reverse-out").expect("child generation");
    assert_agent_time_contains(&events, &creates[0].0, &[gen_id.as_str()]);
    assert!(
        chain_reaches_before(&graph, &gen_id, &creates[0].0, &main_obs),
        "child 内容应先归 child AGENT obs"
    );
}
