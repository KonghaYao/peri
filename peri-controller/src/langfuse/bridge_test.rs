//! LangfuseBridge 双 producer 集成测试（C11/C12）。
//!
//! 框架：共享 `Arc<Mutex<LangfuseTracer>>` + 两个 `LangfuseBridge`（bridge1 带
//! `main_agent_id`、bridge2 不带），模拟主/subagent 两个独立 forwarder。
//! 两个 producer 直接调 `process_render_event`/`process_observe_event`
//! （不经 tokio channel、不依赖 yield_now，顺序完全可控），经
//! `FakeLangfuseSession` 收集全部 `IngestionEvent`，构造观测图（obs id → parent id）
//! 后断言：
//!
//! - child stage 的 parent 链为该 child AGENT obs；child LLM/tool 不指向主 agent-run
//! - child AGENT `start ≤ 最早 child 事件` 且 `end ≥ 最晚 child 事件`（无 17ms 空壳）
//! - 每 obs 至多一个 parent（create/update 的 parent 一致，parent 冻结不漂移）
//! - 图无环；主 agent 既有结构（agent-run → stage → tool-batch）不变
//! - C12 乱序矩阵：Start 先/后于父 ToolStart、Stop 先/后于 ToolEnded、
//!   Start 丢失、Stop 丢失、缓存溢出 → 对应降级断言

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use langfuse_client::types::ObservationType;
use langfuse_client::IngestionEvent;
use parking_lot::Mutex;
use peri_acp_types::identity::AgentId;
use peri_agent::agent::events::{Stage, StageStatus};
use peri_agent::agent::events_v2::{ObserveEvent, RenderEvent};
use peri_agent::agent::LangfuseBridgeLike;
use peri_agent::session::turn::TurnId;

use super::*;
use crate::langfuse::config::LangfuseConfig;
use crate::langfuse::fake_session::FakeLangfuseSession;
use crate::langfuse::tracer::LangfuseTracer;

/// 固定可预测的 agent id（child_thread_id 收敛后的同一值语义）
fn child_id(n: u128) -> AgentId {
    AgentId::from_uuid(uuid::Uuid::from_u128(
        0x2222_2222_2222_2222_2222_2222_2222_2222 + n,
    ))
}

/// 双 producer 测试框架。
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

impl Harness {
    // ── 主 agent 事件（bridge1：render + observe 双通道） ──────────────────────

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

    fn main_llm_start(&self, step: usize) {
        self.b1.process_observe_event(&ObserveEvent::LlmCallStart {
            turn_id: self.turn,
            agent_id: self.main,
            step,
            messages: Arc::new(Vec::new()),
            tools: Vec::new(),
        });
    }

    fn main_llm_end(&self, step: usize, output: &str) {
        self.b1.process_observe_event(&ObserveEvent::LlmCallEnd {
            turn_id: self.turn,
            agent_id: self.main,
            step,
            model: "claude-4.7".to_string(),
            output: output.to_string(),
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            request_id: None,
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
        });
    }

    // ── subagent 事件（bridge2：child 自己的 render + observe 通道） ───────────

    fn child_start(&self, child: AgentId, name: &str, is_background: bool) {
        self.b2.process_observe_event(&ObserveEvent::SubagentStart {
            turn_id: self.turn,
            agent_id: self.main,
            child_agent_id: child,
            agent_name: name.to_string(),
            is_background,
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
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
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
        });
    }

    /// on_turn_end（主 turn 结束；内部 spawn，调用方需在 tokio runtime 内）
    fn turn_end(&self) {
        drop(self.tracer.lock().on_turn_end(None));
    }
}

// ── 观测图断言工具 ─────────────────────────────────────────────────────────────

/// 观测图：(obs/span/generation id → parent id)。
///
/// 同一 id 出现多次（ObservationCreate+ObservationUpdate 配对）时 parent 必须一致，
/// 不一致即"parent 漂移"，直接 fail——即"每 obs 至多一个 parent"。
fn build_graph(events: &[IngestionEvent]) -> HashMap<String, Option<String>> {
    let mut map: HashMap<String, Option<String>> = HashMap::new();
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

/// 图无环：每个 obs 沿 parent 链必须有界且不回自身。
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

/// 沿 parent 链从 `node` 出发：在遇到 `forbidden` 之前到达 `target` → true。
///
/// 用于"child 内容必须先归 child AGENT obs，不得先指向主 agent-run"。
/// 链途经 forbidden（如主 Act span → agent-run）不算失败——只要求先到 target。
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

/// 提取 obs/span/generation 的 (start_time, end_time)（按 id 合并 create/update）。
fn obs_time_range(events: &[IngestionEvent], id: &str) -> (Option<String>, Option<String>) {
    let mut start = None;
    let mut end = None;
    for e in events {
        let (oid, st, et) = match e {
            IngestionEvent::ObservationCreate { body, .. }
            | IngestionEvent::ObservationUpdate { body, .. } => (
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

/// subagent AGENT obs 创建（open）列表：(id, parent, start_time)。
/// 过滤主 agent-run（name == "agent-run"），只统计 subagent 的 AGENT obs。
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

/// subagent AGENT obs 关闭（update）列表：(id, end_time, metadata)。
/// 主 agent-run 只有 create 无 update，无需过滤，但保持一致。
fn agent_obs_updates(
    events: &[IngestionEvent],
) -> Vec<(String, Option<String>, Option<serde_json::Value>)> {
    events
        .iter()
        .filter_map(|e| {
            if let IngestionEvent::ObservationUpdate { body, .. } = e {
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

/// 按 name 找 span 事件（stage/batch 等）：(id, parent)
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

/// 观测事件引用：(id, parent, start_time, end_time)
type ObsRef = (String, Option<String>, Option<String>, Option<String>);

/// 按 name 找工具 observation：(id, parent, start, end)
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

/// 按 output 文本找 generation：(id, parent, start, end)
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

/// 断言 child AGENT obs 时间窗包含全部 child 内容事件：
/// `start ≤ 最早 child 事件 start` 且 `end ≥ 最晚 child 事件 end`（无 17ms 空壳）。
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

/// 断言主 agent 既有结构不变：agent-run → 主 stage span → 主 tool-batch → 工具 obs。
/// 返回 (主 stage span id, 主 tool-batch span id)。
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

// ── C11：正常顺序全链路（图断言全通过） ────────────────────────────────────────

/// 主 Act → 主 ToolStart(Agent) → child Start → child stage/llm/tool →
/// 主 ToolEnded → child Stop → 主 Act end → turn end。
///
/// 断言：child stage parent 链为该 child AGENT obs；child LLM/tool 不指向
/// 主 agent-run；AGENT start ≤ 最早 child 事件 且 end ≥ 最晚 child 事件；
/// 每 obs 至多一个 parent；图无环；主 agent 既有结构不变。
#[tokio::test]
async fn test_ordered_flow_graph() {
    let h = harness();
    h.main_stage_start(Stage::Act); // 主 Act stage（AGENT obs 的冻结父）
    h.main_tool_start("call_agent", "Agent"); // ① 父 ToolStart：invocation 登记
    h.child_start(child_id(1), "fork", false); // ② Start：join → AGENT obs create
    h.child_stage_start(child_id(1), Stage::Reason); // ③ child stage
    std::thread::sleep(Duration::from_millis(2));
    h.child_llm_start(child_id(1), 0);
    h.child_llm_end(child_id(1), 0, "analysis");
    h.child_tool_start(child_id(1), "call_bash", "Bash");
    h.child_tool_end(child_id(1), "call_bash");
    h.child_stage_end(child_id(1), Stage::Reason, StageStatus::Done);
    h.main_tool_end("call_agent"); // ⑥ 父 ToolEnded：只结束记录，不关 child
    h.child_stop(child_id(1), "done"); // ⑦ Stop：关闭 AGENT obs
    h.main_stage_end(Stage::Act, StageStatus::Done);
    h.turn_end();
    tokio::task::yield_now().await;

    let events = h.session.events_snapshot();
    let graph = build_graph(&events);
    assert_acyclic(&graph);
    let main_obs = h.tracer.lock().agent_observation_id.clone();

    // child AGENT obs：恰好一个，parent = 主 Act stage span
    let creates = agent_obs_creates(&events);
    assert_eq!(creates.len(), 1, "应恰好一个 child AGENT obs");
    let (child_obs, child_parent, _) = creates[0].clone();
    let (main_stage, _) = assert_main_structure(&graph, &events, &main_obs);
    assert_eq!(
        child_parent.as_deref(),
        Some(main_stage.as_str()),
        "AGENT obs parent 应为 join 时冻结的主 Act stage span"
    );

    // child stage：parent = child AGENT obs
    let (reason_span, reason_parent) =
        span_by_name(&events, "stage-reason").expect("child stage span 应上报");
    assert_eq!(
        reason_parent.as_deref(),
        Some(child_obs.as_str()),
        "child stage 应挂 child AGENT obs"
    );
    // child LLM：parent = child stage（不指向主 agent-run）
    let (gen_id, gen_parent, _, _) =
        generation_by_output(&events, "analysis").expect("child generation 应上报");
    assert_eq!(
        gen_parent.as_deref(),
        Some(reason_span.as_str()),
        "child LLM 应挂 child stage"
    );
    // child 工具：挂 child 自己的 tool-batch（parent 链到 child stage）
    let (bash_id, bash_parent, _, _) =
        tool_obs_by_name(&events, "Bash").expect("child 工具 obs 应上报");
    let (child_batch, child_batch_parent) = events
        .iter()
        .filter_map(|e| {
            if let IngestionEvent::SpanCreate { body, .. } = e {
                if body.name.as_deref() == Some("tool-batch")
                    && body.parent_observation_id.as_deref() == Some(reason_span.as_str())
                {
                    return Some((
                        body.id.clone().unwrap_or_default(),
                        body.parent_observation_id.clone(),
                    ));
                }
            }
            None
        })
        .next()
        .expect("child 应有自己的 tool-batch");
    assert_eq!(
        bash_parent.as_deref(),
        Some(child_batch.as_str()),
        "child 工具应挂 child 的 tool-batch"
    );
    assert_eq!(
        child_batch_parent.as_deref(),
        Some(reason_span.as_str()),
        "child tool-batch 应挂 child stage"
    );

    // child LLM/tool 不指向主 agent-run：parent 链先到 child AGENT obs
    assert!(
        chain_reaches_before(&graph, &gen_id, &child_obs, &main_obs),
        "child LLM 应先归 child AGENT obs"
    );
    assert!(
        chain_reaches_before(&graph, &bash_id, &child_obs, &main_obs),
        "child 工具应先归 child AGENT obs"
    );
    // 直接挂 agent-run 的只有主 stage span
    assert_eq!(
        graph
            .iter()
            .filter(|(_, p)| p.as_deref() == Some(main_obs.as_str()))
            .count(),
        1,
        "除主 stage span 外无任何 obs 直接挂 agent-run"
    );

    // 时间窗：AGENT start ≤ 最早 child 事件，end ≥ 最晚 child 事件（无 17ms 空壳）
    assert_agent_time_contains(
        &events,
        &child_obs,
        &[reason_span.as_str(), gen_id.as_str(), bash_id.as_str()],
    );

    // AGENT obs 关闭且 end ≥ 最晚 child 事件
    let updates = agent_obs_updates(&events);
    assert_eq!(updates.len(), 1, "Stop 后 AGENT obs 应恰好关闭一次");
    assert_eq!(updates[0].0, child_obs);
}

// ── C11：纯主 agent 事件流 → 主结构不变 ───────────────────────────────────────

/// 无任何 subagent 事件：主 agent 既有图结构（agent-run → stage → tool-batch）
/// 与重构前一致，且不产生任何 AGENT obs。
#[tokio::test]
async fn test_main_structure_unchanged() {
    let h = harness();
    h.main_stage_start(Stage::Reason);
    h.main_llm_start(0);
    h.main_llm_end(0, "hello");
    std::thread::sleep(Duration::from_millis(2)); // stage duration > 0（v2 条件上报）
    h.main_stage_end(Stage::Reason, StageStatus::Done);
    h.main_stage_start(Stage::Act);
    h.main_tool_start("call_bash", "Bash");
    h.main_tool_end("call_bash");
    std::thread::sleep(Duration::from_millis(2));
    h.main_stage_end(Stage::Act, StageStatus::Done);
    h.turn_end();
    tokio::task::yield_now().await;

    let events = h.session.events_snapshot();
    let graph = build_graph(&events);
    assert_acyclic(&graph);
    let main_obs = h.tracer.lock().agent_observation_id.clone();

    assert!(
        agent_obs_creates(&events).is_empty(),
        "无 subagent 场景不应产生 AGENT obs"
    );
    let (main_stage, batch_id) = assert_main_structure(&graph, &events, &main_obs);
    // 主 LLM generation：parent = 主 Reason stage span（LLM 发生在 Reason 阶段）
    let main_reason = events
        .iter()
        .filter_map(|e| {
            if let IngestionEvent::SpanCreate { body, .. } = e {
                if body.name.as_deref() == Some("stage-reason")
                    && body.parent_observation_id.as_deref() == Some(main_obs.as_str())
                {
                    return body.id.clone();
                }
            }
            None
        })
        .next()
        .expect("主 stage-reason span 应上报");
    let (gen_id, gen_parent, _, _) =
        generation_by_output(&events, "hello").expect("主 generation 应上报");
    assert_eq!(
        gen_parent.as_deref(),
        Some(main_reason.as_str()),
        "主 LLM 应挂主 stage span"
    );
    assert!(
        chain_reaches_before(&graph, &gen_id, &main_obs, &batch_id),
        "主 LLM 链应最终归 agent-run"
    );
    let _ = main_stage;
    assert_eq!(
        graph.len(),
        6,
        "主结构：agent-run + 2 stage + batch + 工具 + gen"
    );
}

// ── C11：并行双 subagent 交错（双 producer 交替注入） ──────────────────────────

/// 主ToolStart A、主ToolStart B、Start A、Start B、A.StageStart、B.StageStart、
/// A.Llm、B.Tool、A.StageEnd、B.StageEnd、ToolEnded A、ToolEnded B、Stop A、Stop B。
/// 各自内容 parent 指向各自 AGENT obs；无任何 A 内容挂 B；generation 不串。
#[tokio::test]
async fn test_parallel_interleaved_producers() {
    let h = harness();
    let a = child_id(1);
    let b = child_id(2);
    h.main_stage_start(Stage::Act);
    h.main_tool_start("call_a", "Agent");
    h.main_tool_start("call_b", "Agent");
    h.child_start(a, "explorer-a", false);
    h.child_start(b, "explorer-b", false);
    h.child_stage_start(a, Stage::Reason);
    h.child_stage_start(b, Stage::Reason);
    std::thread::sleep(Duration::from_millis(2));
    h.child_llm_start(a, 0);
    h.child_llm_end(a, 0, "a-out");
    h.child_llm_start(b, 0);
    h.child_llm_end(b, 0, "b-out");
    h.child_tool_start(a, "call_a1", "Bash");
    h.child_tool_end(a, "call_a1");
    h.child_tool_start(b, "call_b1", "Grep");
    h.child_tool_end(b, "call_b1");
    h.child_stage_end(a, Stage::Reason, StageStatus::Done);
    h.child_stage_end(b, Stage::Reason, StageStatus::Done);
    h.main_tool_end("call_a");
    h.main_tool_end("call_b");
    h.child_stop(a, "ra");
    h.child_stop(b, "rb");
    h.main_stage_end(Stage::Act, StageStatus::Done);
    h.turn_end();
    tokio::task::yield_now().await;

    let events = h.session.events_snapshot();
    let graph = build_graph(&events);
    assert_acyclic(&graph);
    let main_obs = h.tracer.lock().agent_observation_id.clone();
    let (main_stage, _) = assert_main_structure(&graph, &events, &main_obs);

    // 两个 child 各一个 AGENT obs，parent 均为主 Act stage，id 不同
    let creates = agent_obs_creates(&events);
    assert_eq!(creates.len(), 2, "两个 child 各一个 AGENT obs");
    let obs_a = creates
        .iter()
        .find(|(id, _, _)| {
            h.tracer
                .lock()
                .subagent
                .observation_id_of(&a.to_string())
                .as_deref()
                == Some(id.as_str())
        })
        .expect("A 的 AGENT obs");
    let obs_b = creates
        .iter()
        .find(|(id, _, _)| {
            h.tracer
                .lock()
                .subagent
                .observation_id_of(&b.to_string())
                .as_deref()
                == Some(id.as_str())
        })
        .expect("B 的 AGENT obs");
    assert_ne!(obs_a.0, obs_b.0, "A/B 的 AGENT obs id 应不同");
    for (_, parent, _) in &creates {
        assert_eq!(
            parent.as_deref(),
            Some(main_stage.as_str()),
            "AGENT obs parent 应为主 Act stage（冻结）"
        );
    }

    // 各自 stage 挂各自 AGENT obs（两个 stage-reason span 按 parent 区分）
    let a_stage = events
        .iter()
        .filter_map(|e| {
            if let IngestionEvent::SpanCreate { body, .. } = e {
                if body.name.as_deref() == Some("stage-reason")
                    && body.parent_observation_id.as_deref() == Some(obs_a.0.as_str())
                {
                    return body.id.clone();
                }
            }
            None
        })
        .next()
        .expect("A 的 stage span");
    let b_stage = events
        .iter()
        .filter_map(|e| {
            if let IngestionEvent::SpanCreate { body, .. } = e {
                if body.name.as_deref() == Some("stage-reason")
                    && body.parent_observation_id.as_deref() == Some(obs_b.0.as_str())
                {
                    return body.id.clone();
                }
            }
            None
        })
        .next()
        .expect("B 的 stage span");

    // A/B 的 generation 各自挂各自的 stage
    let (gen_a, gen_a_parent, _, _) = generation_by_output(&events, "a-out").expect("A generation");
    let (gen_b, gen_b_parent, _, _) = generation_by_output(&events, "b-out").expect("B generation");
    assert_eq!(gen_a_parent.as_deref(), Some(a_stage.as_str()));
    assert_eq!(gen_b_parent.as_deref(), Some(b_stage.as_str()));
    assert_ne!(gen_a_parent, gen_b_parent, "A/B generation parent 不得相同");

    // 工具各自 batch：A 的 Bash 与 B 的 Grep 挂不同 batch
    let (bash_id, bash_parent, _, _) = tool_obs_by_name(&events, "Bash").expect("A 的 Bash");
    let (grep_id, grep_parent, _, _) = tool_obs_by_name(&events, "Grep").expect("B 的 Grep");
    let a_batch = events
        .iter()
        .filter_map(|e| {
            if let IngestionEvent::SpanCreate { body, .. } = e {
                if body.name.as_deref() == Some("tool-batch")
                    && body.parent_observation_id.as_deref() == Some(a_stage.as_str())
                {
                    return body.id.clone();
                }
            }
            None
        })
        .next()
        .expect("A 的 tool-batch 应挂 A 的 stage");
    let b_batch = events
        .iter()
        .filter_map(|e| {
            if let IngestionEvent::SpanCreate { body, .. } = e {
                if body.name.as_deref() == Some("tool-batch")
                    && body.parent_observation_id.as_deref() == Some(b_stage.as_str())
                {
                    return body.id.clone();
                }
            }
            None
        })
        .next()
        .expect("B 的 tool-batch 应挂 B 的 stage");
    assert_ne!(bash_parent, grep_parent, "A/B 工具应挂各自 tool-batch");
    assert_eq!(bash_parent.as_deref(), Some(a_batch.as_str()));
    assert_eq!(grep_parent.as_deref(), Some(b_batch.as_str()));

    // 无任何 A 内容挂 B：链检查
    assert!(chain_reaches_before(&graph, &gen_a, &obs_a.0, &obs_b.0));
    assert!(chain_reaches_before(&graph, &bash_id, &obs_a.0, &obs_b.0));
    assert!(chain_reaches_before(&graph, &grep_id, &obs_b.0, &obs_a.0));

    // 各自时间窗包含各自内容
    assert_agent_time_contains(
        &events,
        &obs_a.0,
        &[a_stage.as_str(), gen_a.as_str(), bash_id.as_str()],
    );
    assert_agent_time_contains(&events, &obs_b.0, &[gen_b.as_str(), grep_id.as_str()]);
    // 无任何内容直接挂 agent-run（仅主 stage span）
    assert_eq!(
        graph
            .iter()
            .filter(|(_, p)| p.as_deref() == Some(main_obs.as_str()))
            .count(),
        1
    );
    // 两信号齐备后全部关闭
    assert_eq!(agent_obs_updates(&events).len(), 2);
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
    let updates = agent_obs_updates(&events);
    assert_eq!(updates.len(), 1, "⑤ Stop 应关闭 AGENT obs");
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
        agent_obs_updates(&h.session.events_snapshot()).len(),
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

    let updates = agent_obs_updates(&events);
    assert_eq!(updates.len(), 1, "ToolEnded 后应关闭 AGENT obs（恰好一次）");
    let creates = agent_obs_creates(&events);
    assert_eq!(updates[0].0, creates[0].0, "关闭的应为 child 的 AGENT obs");
    // output 优先 Stop result（无 incomplete_reason）
    assert!(
        updates[0]
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
        agent_obs_updates(&h.session.events_snapshot()).len(),
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

    let updates = agent_obs_updates(&events);
    assert_eq!(updates.len(), 1, "ToolEnded 触发恰好一次关闭");
    let creates = agent_obs_creates(&events);
    let (gen_id, _, _, _) = generation_by_output(&events, "reverse-out").expect("child generation");
    assert_agent_time_contains(&events, &creates[0].0, &[gen_id.as_str()]);
    assert!(
        chain_reaches_before(&graph, &gen_id, &creates[0].0, &main_obs),
        "child 内容应先归 child AGENT obs"
    );
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
    assert_eq!(agent_obs_updates(&events).len(), 1);
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
    assert_eq!(agent_obs_updates(&events).len(), 1);
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
    let updates = agent_obs_updates(&events);
    assert_eq!(updates.len(), 1, "on_turn_end 应兜底关闭 AGENT obs");
    assert_eq!(updates[0].0, creates[0].0);
    assert!(updates[0].1.is_some(), "兜底关闭应带 end_time");
    // metadata 携带 incomplete_reason（MissingStop）
    let meta_reason = updates[0]
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
