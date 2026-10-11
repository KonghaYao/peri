//! Stage 与工具批次生命周期回归。

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

// ── tool-batch 归属 stage-act ────────────────────────────────────────────────

/// ToolStart 先于 StageStarted(Act) 到达（LLM 响应产生 tool_calls 后事件链先发
/// ToolStart 再切阶段）时，batch parent 冻结在旧 stage-reason；Act 开始后必须
/// 重挂到 stage-act，否则 stage-act 变空、batch 挂错父节点。
#[tokio::test]
async fn test_tool_batch_reparents_to_stage_act() {
    let (mut t, session) = make_tracer(1.0);
    t.set_main_agent_id("main".to_string());
    t.on_turn_start("turn_act_reparent");

    // ① Reason 阶段:LLM 决定调用工具
    let reason = t
        .on_stage_start_gated("main", Stage::Reason, "turn_act_reparent")
        .unwrap();
    t.on_llm_start("main", 0, &[], &[]);
    t.on_llm_end("main", 0, "m", "p", "plan", None, None);
    // ② ToolStart 先到:batch 创建,parent 冻结为 stage-reason
    t.on_tool_start("main", "call_1", "Bash", &serde_json::json!({"cmd": "ls"}));
    std::thread::sleep(std::time::Duration::from_millis(2));
    // ③ Reason 结束(span 上报)
    t.on_stage_end("main", &reason, StageStatus::Done);
    // ④ Act 开始:batch parent 应重挂到 stage-act
    let act = t
        .on_stage_start_gated("main", Stage::Act, "turn_act_reparent")
        .unwrap();
    // ⑤ 工具结束 + Act 结束 → flush batch(此时 parent 已是 stage-act)
    t.on_tool_end("main", "call_1", "file list", false);
    std::thread::sleep(std::time::Duration::from_millis(2));
    t.on_stage_end("main", &act, StageStatus::Done);

    let events = session.events_snapshot();
    // batch span 的 parent 应为 stage-act
    let batch_parents: Vec<Option<String>> = events
        .iter()
        .filter_map(|e| {
            if let IngestionEvent::SpanCreate { body, .. } = e {
                if body.name.as_deref() == Some("tool-batch") {
                    return Some(body.parent_observation_id.clone());
                }
            }
            None
        })
        .collect();
    assert_eq!(
        batch_parents,
        vec![Some(act.span_id.clone())],
        "batch 应挂到 stage-act,而非旧 stage-reason"
    );
    // stage-act 不应为空:至少包含 batch 子节点
    let map = parent_map(&events);
    assert!(
        map.values()
            .any(|p| p.as_deref() == Some(act.span_id.as_str())),
        "stage-act 下应有 batch 子节点"
    );
    // batch span 不应挂在 stage-reason 下(其下只有 generation 等 Reason 阶段内容)
    let batch_ids: Vec<String> = events
        .iter()
        .filter_map(|e| {
            if let IngestionEvent::SpanCreate { body, .. } = e {
                if body.name.as_deref() == Some("tool-batch") {
                    return body.id.clone();
                }
            }
            None
        })
        .collect();
    assert!(
        batch_ids
            .iter()
            .all(|id| map.get(id) == Some(&Some(act.span_id.clone()))),
        "所有 batch span 应只挂 stage-act"
    );

    let _h = t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed);
    tokio::task::yield_now().await;
}

/// 回归测试：subagent 的 stage 被同一 agent 的新 stage 覆盖时，旧 stage 的
/// SpanCreate 必须立即补发，否则工具 batch 的 parent 引用一个从未创建的 span
/// （孤儿 batch，UI 中整批工具调用挂错位置）。
///
/// bug: stage span 的 SpanCreate 延迟到 on_stage_end 合并发送；subagent 的
/// reason/act 快速切换时旧 stage 的 StageEnded 可能因乱序/重放丢失
/// （bridge 无 handle 匹配 → warn 跳过），旧 stage span 永不发送 → 工具 batch
/// 成为孤儿（真实数据:batch_01a00fd3052171d2b6f0e0145dfd1cc0 的 parent
/// span_01a00fd4afa... 在 trace 中不存在）。
/// 修复:stages.on_stage_start 返回被覆盖的旧 handle，tracer 立即补发其
/// 合并 SpanCreate。
#[tokio::test]
async fn test_replaced_stage_emits_span_no_orphan_batch() {
    let (mut t, session) = make_tracer(1.0);
    t.set_main_agent_id("main".to_string());
    t.on_turn_start("turn_replaced_stage");

    // 主 agent Act stage + Agent 工具 → subagent join
    let main_act = t
        .on_stage_start_gated("main", Stage::Act, "turn_replaced_stage")
        .unwrap();
    t.on_tool_start(
        "main",
        "call_agent",
        "Agent",
        &serde_json::json!({"prompt": "review"}),
    );
    t.on_subagent_start("main", "child_1", "coder", false);
    let child_obs_id = t.subagent.observation_id_of("child_1").unwrap();

    // child Reason stage：工具开始，batch parent 冻结为 reason span
    let child_reason = t
        .on_stage_start_gated("child_1", Stage::Reason, "turn_replaced_stage")
        .unwrap();
    let reason_span_id = child_reason.span_id.clone();
    // 确保 stage duration > 0(v2 条件上报:0ms stage span 不上报)
    std::thread::sleep(std::time::Duration::from_millis(2));
    t.on_tool_start(
        "child_1",
        "call_bash",
        "Bash",
        &serde_json::json!({"cmd": "ls"}),
    );

    // child Act stage 覆盖 Reason：不调用 child_reason 的 on_stage_end，
    // 模拟真实场景中旧 stage 的 StageEnded 因乱序/重放丢失。
    // 修复前:reason span 永不发送，batch 的 parent 悬空。
    let child_act = t
        .on_stage_start_gated("child_1", Stage::Act, "turn_replaced_stage")
        .unwrap();

    // 工具结束 + Act 正常结束(span 正常上报)
    t.on_tool_end("child_1", "call_bash", "file list", false);
    std::thread::sleep(std::time::Duration::from_millis(2));
    t.on_stage_end("child_1", &child_act, StageStatus::Done);

    // 父 Agent 工具结束 + SubagentStop → flush subagent batch
    t.on_tool_end("main", "call_agent", "done", false);
    t.on_subagent_stop("main", "child_1", "done", false);
    // 主 agent Act stage 正常结束(否则其 span 未发送,主 batch 的 parent 悬空)
    std::thread::sleep(std::time::Duration::from_millis(2));
    t.on_stage_end("main", &main_act, StageStatus::Done);
    let _h = t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed);
    tokio::task::yield_now().await;

    let events = session.events_snapshot();
    let map = parent_map(&events);

    // ① 被覆盖的 reason span 必须存在(修复核心:覆盖即补发)
    assert!(
        map.contains_key(&reason_span_id),
        "被覆盖的 stage-reason span 应补发存在"
    );
    // ② reason span 的 parent 应为 child AGENT obs(归属链完整)
    assert_eq!(
        map.get(&reason_span_id),
        Some(&Some(child_obs_id.clone())),
        "被覆盖的 reason span 应挂 child AGENT obs"
    );

    // ③ batch span 存在且 parent = stage-act(重挂正常),无孤儿
    let batch_parents: Vec<Option<String>> = events
        .iter()
        .filter_map(|e| {
            if let IngestionEvent::SpanCreate { body, .. } = e {
                if body.name.as_deref() == Some("tool-batch") {
                    return Some(body.parent_observation_id.clone());
                }
            }
            None
        })
        .collect();
    assert!(!batch_parents.is_empty(), "应有 batch span");
    // ③' 每个 batch 的 parent 都必须指向已存在的 span(无孤儿)
    assert!(
        batch_parents.iter().all(|p| match p {
            Some(pid) => map.contains_key(pid),
            None => false,
        }),
        "batch 的 parent 必须存在(无孤儿),实际: {:?}",
        batch_parents
    );
    // ③'' subagent 的工具 batch(含 Bash 工具)应重挂到 stage-act
    assert!(
        batch_parents
            .iter()
            .any(|p| p.as_deref() == Some(child_act.span_id.as_str())),
        "subagent 的 batch 应挂 stage-act(重挂正常)"
    );

    // ④ 工具 obs 挂 batch 下,归属链:child obs → act → batch → tool
    let batch_ids: Vec<String> = events
        .iter()
        .filter_map(|e| {
            if let IngestionEvent::SpanCreate { body, .. } = e {
                if body.name.as_deref() == Some("tool-batch") {
                    return body.id.clone();
                }
            }
            None
        })
        .collect();
    let tool_parents: Vec<Option<String>> = events
        .iter()
        .filter_map(|e| {
            if let IngestionEvent::ObservationCreate { body, .. } = e {
                if body.r#type == ObservationType::Tool {
                    return Some(body.parent_observation_id.clone());
                }
            }
            None
        })
        .collect();
    assert!(!tool_parents.is_empty(), "应有工具 obs");
    assert!(
        tool_parents
            .iter()
            .all(|p| batch_ids.iter().any(|b| p.as_deref() == Some(b))),
        "工具 obs 应挂 batch span 下"
    );
}

/// 回归测试：subagent 工具批次必须在每个 Act 结束时独立 flush，不能跨多个
/// Act 累积并在 AGENT obs 关闭时一次性发送。跨 Act 的单 batch 会被后续 Act
/// 反复重挂；若最后一个 Act 是 0ms 并被条件过滤，整个 batch 都会成为孤儿。
///
/// 真实数据：trace 01a01001249e... 中两个 explorer 的 batch 分别跨 16/29 个
/// tools，最终 parent 指向不存在的最后 Act span。
#[tokio::test]
async fn test_subagent_flushes_tool_batch_per_act_stage() {
    let (mut t, session) = make_tracer(1.0);
    t.set_main_agent_id("main".to_string());
    t.on_turn_start("turn_subagent_batch_per_act");

    let main_act = t
        .on_stage_start_gated("main", Stage::Act, "turn_subagent_batch_per_act")
        .unwrap();
    t.on_tool_start(
        "main",
        "call_agent",
        "Agent",
        &serde_json::json!({"prompt": "inspect"}),
    );
    t.on_subagent_start("main", "child_1", "explorer", false);

    let mut act_ids = Vec::new();
    for step in 1..=2 {
        let reason = t
            .on_stage_start_gated("child_1", Stage::Reason, "turn_subagent_batch_per_act")
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        t.on_tool_start(
            "child_1",
            &format!("call_read_{step}"),
            "Read",
            &serde_json::json!({"path": format!("/tmp/{step}")}),
        );
        t.on_stage_end("child_1", &reason, StageStatus::Done);

        let act = t
            .on_stage_start_gated("child_1", Stage::Act, "turn_subagent_batch_per_act")
            .unwrap();
        act_ids.push(act.span_id.clone());
        t.on_tool_end(
            "child_1",
            &format!("call_read_{step}"),
            &format!("content-{step}"),
            false,
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        t.on_stage_end("child_1", &act, StageStatus::Done);
    }

    t.on_tool_end("main", "call_agent", "done", false);
    t.on_subagent_stop("main", "child_1", "done", false);
    std::thread::sleep(std::time::Duration::from_millis(2));
    t.on_stage_end("main", &main_act, StageStatus::Done);
    let _h = t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed);
    tokio::task::yield_now().await;

    let events = session.events_snapshot();
    let batch_parents: Vec<String> = events
        .iter()
        .filter_map(|event| {
            let IngestionEvent::SpanCreate { body, .. } = event else {
                return None;
            };
            (body.name.as_deref() == Some("tool-batch"))
                .then(|| body.parent_observation_id.clone())
                .flatten()
        })
        .filter(|parent| act_ids.contains(parent))
        .collect();

    assert_eq!(
        batch_parents, act_ids,
        "subagent 每个 Act 应各自拥有一个 tool-batch，不能跨 Act 合并或重挂"
    );
}

/// 回归测试：subagent 最后一个 stage 的 StageEnded 丢失时（事件流截断/乱序，
/// 例如 Stop 乱序早到、subagent 在 Closed 状态下仍有内容事件），其 span 必须
/// 由 turn 结束兜底补发——否则工具 batch（重挂到该 stage）的 parent 悬空，
/// 整批工具沦为孤儿挂错位置。
///
/// 复现路径（对应真实 trace 01a00fec2b... 的 explorer A/C）：
/// subagent 的 act stage 创建 → batch 重挂到它 → StageEnded 未到达 →
/// on_turn_end 兜底必须补发 act span，batch parent 因此存在。
#[tokio::test]
async fn test_turn_end_closes_stale_stage_no_orphan_batch() {
    let (mut t, session) = make_tracer(1.0);
    t.set_main_agent_id("main".to_string());
    t.on_turn_start("turn_stale_stage");

    // 主 agent Act stage + Agent 工具 → subagent join
    let main_act = t
        .on_stage_start_gated("main", Stage::Act, "turn_stale_stage")
        .unwrap();
    t.on_tool_start(
        "main",
        "call_agent",
        "Agent",
        &serde_json::json!({"prompt": "inspect"}),
    );
    t.on_subagent_start("main", "child_1", "coder", false);

    // subagent:reason stage → 工具 start(batch 创建) → act stage 重挂 batch
    t.on_stage_start_gated("child_1", Stage::Reason, "turn_stale_stage")
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    t.on_tool_start(
        "child_1",
        "call_bash",
        "Bash",
        &serde_json::json!({"cmd": "ls"}),
    );
    let child_act = t
        .on_stage_start_gated("child_1", Stage::Act, "turn_stale_stage")
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    t.on_tool_end("child_1", "call_bash", "files", false);
    // ⚠️ 故意不调用 on_stage_end(child_act) —— 模拟最后一个 stage 的
    //    StageEnded 因事件流截断/乱序丢失

    // subagent stop + 父 Agent 工具结束 + 主 turn 结束
    t.on_subagent_stop("main", "child_1", "done", false);
    t.on_tool_end("main", "call_agent", "done", false);
    std::thread::sleep(std::time::Duration::from_millis(2));
    t.on_stage_end("main", &main_act, StageStatus::Done);
    let _h = t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed);
    tokio::task::yield_now().await;

    let events = session.events_snapshot();
    let map = parent_map(&events);

    // ① 丢失 StageEnded 的 act stage 必须被 turn end 兜底补发
    assert!(
        map.contains_key(&child_act.span_id),
        "丢失 StageEnded 的 stage-act 应被 on_turn_end 兜底补发"
    );

    // ② 所有 batch 的 parent 必须存在(无孤儿)
    let batch_parents: Vec<String> = events
        .iter()
        .filter_map(|e| {
            if let IngestionEvent::SpanCreate { body, .. } = e {
                if body.name.as_deref() == Some("tool-batch") {
                    return body.parent_observation_id.clone();
                }
            }
            None
        })
        .collect();
    assert!(!batch_parents.is_empty(), "应有 batch span");
    assert!(
        batch_parents.iter().all(|pid| map.contains_key(pid)),
        "所有 batch 的 parent 必须存在(无孤儿),实际: {:?}",
        batch_parents
    );

    // ③ subagent 的 batch 挂在 act 下,工具挂 batch 下:归属链完整
    assert!(
        batch_parents.iter().any(|pid| pid == &child_act.span_id),
        "subagent 的 batch 应挂 stage-act"
    );
}
