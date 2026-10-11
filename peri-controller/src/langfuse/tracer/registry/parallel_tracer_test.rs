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
    assert_eq!(creates.len(), 2, "两个 child 各一次完整终态导出");
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
            if let IngestionEvent::ObservationCreate { body, .. } = e {
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
    assert_eq!(creates.len(), 0, "两个 child 的开始记录仅缓存");
    let obs_a_id = t.subagent.observation_id_of("child_a").unwrap();
    let obs_b_id = t.subagent.observation_id_of("child_b").unwrap();
    assert_ne!(obs_a_id, obs_b_id);

    // input 透传:child_a 的 obs input = 所绑 call_b 的工具 input(task=B)
    let create_a_input = t
        .subagent
        .by_agent_id
        .get("child_a")
        .unwrap()
        .input
        .as_ref()
        .unwrap();
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
        agent_obs_completions(&session.events_snapshot()).len(),
        0,
        "ToolEnded 不应关闭 AGENT obs(等 Stop)"
    );

    // ⑦ 各自 Stop:按各自 invocation 两信号齐备 → 正常关闭
    t.on_subagent_stop("main", "child_a", "ra", false);
    t.on_subagent_stop("main", "child_b", "rb", false);

    let final_events = session.events_snapshot();
    let completions = agent_obs_completions(&final_events);
    assert_eq!(completions.len(), 2, "两个 AGENT obs 应各自关闭");
    // 无交叉:两个 AGENT obs 各自在 Stop 后正常关闭(end_time 非空)
    let completion_ids: std::collections::HashSet<&str> =
        completions.iter().map(|(id, _, _)| id.as_str()).collect();
    assert_eq!(
        completion_ids,
        [obs_a_id.as_str(), obs_b_id.as_str()].into_iter().collect(),
        "关闭的 obs 应为两个 child 各自的 AGENT obs"
    );
    for (id, end_time, output) in &completions {
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
                if let IngestionEvent::ObservationCreate { body, .. } = e {
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
    let _h = t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed);
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
    let completions = agent_obs_completions(&final_events);
    assert_eq!(completions.len(), 1, "child_a 应正常关闭");
    let (obs_id, end_time, output) = &completions[0];
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
    let _h = t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed);
    tokio::task::yield_now().await;
    assert_eq!(t.subagent.incomplete_count(), 0);
    let _ = main_act;
}
