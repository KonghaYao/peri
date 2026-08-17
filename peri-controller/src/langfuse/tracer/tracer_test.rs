//! LangfuseTracer 集成烟雾测试。
//!
//! 覆盖完整的 turn 生命周期、采样率控制、ErrorSpan 机制、
//! text chunk 累积和 LLM generation 事件流。
//!
//! 注意：`on_turn_end()` 内部调用 `tokio::spawn`，因此需要 `#[tokio::test]`
//! 提供异步运行时。

use super::*;
use peri_agent::agent::events::{Stage, StageStatus};

fn make_tracer(
    rate: f64,
) -> (
    LangfuseTracer,
    std::sync::Arc<crate::langfuse::fake_session::FakeLangfuseSession>,
) {
    // FakeLangfuseSession::new() 已返回 Arc<Self>，无需再包一层
    let session = crate::langfuse::fake_session::FakeLangfuseSession::new("sess_smoke");
    let config = crate::langfuse::config::LangfuseConfig {
        public_key: None,
        secret_key: None,
        host: "https://cloud.langfuse.com".to_string(),
        trace_sampling: rate,
        error_span_always: true,
        batch_max_events: 50,
        batch_flush_interval_secs: 10,
        user_id: None,
    };
    let t = LangfuseTracer::new(session.clone(), "sess_smoke".to_string(), config);
    (t, session)
}

// ── 烟雾测试：完整 turn 序列 ─────────────────────────────────────────────────

#[tokio::test]
async fn test_smoke_complete_turn_sequence() {
    let (mut t, session) = make_tracer(1.0);
    t.on_turn_start("turn_1");

    // Stage: Receive
    t.on_stage_start(Stage::Receive, "turn_1");
    let (recv_handle, _replaced) = t.stages.on_stage_start(
        "main",
        Stage::Receive,
        &t.trace_id,
        "turn_1",
        &t.agent_observation_id,
    );
    t.on_stage_end("main", &recv_handle, StageStatus::Done);

    // Stage: Reason + LLM
    t.on_stage_start(Stage::Reason, "turn_1");
    t.on_llm_start("main", 0, &[], &[]);
    t.on_llm_end("main", 0, "claude-4.7", "anthropic", "hello", None, None);
    let (reason_handle, _replaced) = t.stages.on_stage_start(
        "main",
        Stage::Reason,
        &t.trace_id,
        "turn_1",
        &t.agent_observation_id,
    );
    t.on_stage_end("main", &reason_handle, StageStatus::Done);

    let _handle = t.on_turn_end(None);
    // 等待 flush async 任务完成（FakeLangfuseSession 的 flush 是同步的，但 spawn 需要运行）
    tokio::task::yield_now().await;
    let events = session.events_snapshot();
    assert!(!events.is_empty(), "应有至少一个事件");

    // 回归防护：agent-run（AGENT observation）必须携带 on_turn_start 的 input
    // （历史上曾因重构删除 agent_input 存储逻辑，导致 AGENT 类型 input 全部丢失）
    let agent_run_input = events.iter().find_map(|e| {
        if let langfuse_client::IngestionEvent::ObservationCreate { body, .. } = e {
            if body.r#type == langfuse_client::ObservationType::Agent {
                body.input.as_ref()
            } else {
                None
            }
        } else {
            None
        }
    });
    assert_eq!(agent_run_input, Some(&serde_json::json!("turn_1")));
}

// ── 采样率测试 ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_sampling_rate_0_emits_nothing() {
    let (mut t, session) = make_tracer(0.0);
    t.on_turn_start("turn_1");
    t.on_stage_start(Stage::Reason, "turn_1");
    t.on_llm_start("main", 0, &[], &[]);
    t.on_llm_end("main", 0, "m", "p", "o", None, None);
    let _handle = t.on_turn_end(None);
    tokio::task::yield_now().await;
    let events = session.events_snapshot();
    assert!(
        events.is_empty(),
        "采样率 0 应不上报任何事件，实际有 {} 个",
        events.len()
    );
}

#[tokio::test]
async fn test_sampling_rate_1_emits_events() {
    let (mut t, session) = make_tracer(1.0);
    t.on_turn_start("turn_1");
    t.on_stage_start(Stage::Reason, "turn_1");
    let _handle = t.on_turn_end(None);
    tokio::task::yield_now().await;
    let events = session.events_snapshot();
    assert!(!events.is_empty(), "采样率 1.0 应有事件");
}

// ── ErrorSpan 测试 ──────────────────────────────────────────────────────────

#[tokio::test]
async fn test_error_span_emitted_for_error_turn() {
    let (mut t, session) = make_tracer(0.0);
    t.on_turn_start("turn_1");
    let _handle = t.on_turn_end(Some("TurnError"));
    tokio::task::yield_now().await;
    let events = session.events_snapshot();

    let has_trace = events
        .iter()
        .any(|e| matches!(e, langfuse_client::IngestionEvent::TraceCreate { .. }));
    let has_error_span = events.iter().any(|e| {
        if let langfuse_client::IngestionEvent::EventCreate { body, .. } = e {
            body.name.as_deref() == Some("ErrorTurn")
        } else {
            false
        }
    });
    assert!(has_trace, "错误 turn 应补发 TraceCreate");
    assert!(has_error_span, "错误 turn 应发 ErrorTurn event");
}

// ── TextChunk 累积测试 ─────────────────────────────────────────────────────

#[test]
fn test_on_text_chunk_accumulates() {
    let (mut t, _session) = make_tracer(1.0);
    t.on_text_chunk("Hello ");
    t.on_text_chunk("World");
    assert_eq!(t.final_answer, "Hello World");
}

// ── LLM Generation 事件测试 ─────────────────────────────────────────────────

#[tokio::test]
async fn test_llm_generation_emits_events() {
    let (mut t, session) = make_tracer(1.0);
    t.on_turn_start("turn_1");
    t.on_llm_start("main", 0, &[], &[]);
    t.on_llm_end("main", 0, "gpt-4", "openai", "response", None, None);
    let _handle = t.on_turn_end(None);
    tokio::task::yield_now().await;
    let events = session.events_snapshot();

    let gen_count = events
        .iter()
        .filter(|e| matches!(e, langfuse_client::IngestionEvent::GenerationCreate { .. }))
        .count();
    assert!(gen_count > 0, "应有至少一个 GenerationCreate 事件");

    // 回归防护：generation 必须携带 input（历史上曾因重构误写成硬编码 None，
    // 导致所有 generation 的 input 丢失，见 commit 0c0a3313）
    let gen_input = events.iter().find_map(|e| {
        if let langfuse_client::IngestionEvent::GenerationCreate { body, .. } = e {
            body.input.as_ref()
        } else {
            None
        }
    });
    assert!(gen_input.is_some(), "GenerationCreate 应携带 input");
}

#[tokio::test]
async fn test_llm_error_uses_safe_status_message() {
    let (mut t, session) = make_tracer(1.0);
    t.on_turn_start("turn_1");
    t.on_llm_start("main", 0, &[], &[]);
    t.on_llm_end(
        "main",
        0,
        "gpt-4",
        "openai",
        "ERROR: sentinel-secret",
        None,
        None,
    );
    let _handle = t.on_turn_end(None);
    tokio::task::yield_now().await;
    let events = session.events_snapshot();

    let gen = events.iter().find_map(|e| {
        if let langfuse_client::IngestionEvent::GenerationCreate { body, .. } = e {
            Some(body)
        } else {
            None
        }
    });
    let gen = gen.expect("应有 GenerationCreate 事件");
    assert_eq!(
        gen.level,
        Some(langfuse_client::types::ObservationLevel::Error),
        "LLM 失败时 generation 应标记 Error 级"
    );
    assert_eq!(
        gen.status_message.as_deref(),
        Some("provider_or_stream_failure"),
        "generation statusMessage 应为稳定分类"
    );
    assert!(!format!("{gen:?}").contains("sentinel-secret"));
}

#[tokio::test]
async fn test_turn_error_reason_is_safe_in_error_span() {
    let (mut t, session) = make_tracer(0.0);
    t.on_turn_start("turn_1");
    t.on_turn_error(peri_agent::agent::events_v2::TurnErrorReason::LlmFailure);
    let _handle = t.on_turn_end(Some("sentinel-secret"));
    tokio::task::yield_now().await;
    let events = session.events_snapshot();

    let error_span = events
        .iter()
        .find_map(|e| {
            if let langfuse_client::IngestionEvent::EventCreate { body, .. } = e {
                (body.name.as_deref() == Some("ErrorTurn")).then_some(body)
            } else {
                None
            }
        })
        .expect("应有 ErrorTurn event");
    assert_eq!(
        error_span
            .output
            .as_ref()
            .and_then(|output| output.get("error_class")),
        Some(&serde_json::json!("llm_failure"))
    );
    assert_eq!(
        error_span
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("error_schema_version")),
        Some(&serde_json::json!(2))
    );
    assert!(!format!("{error_span:?}").contains("sentinel-secret"));
}

#[tokio::test]
async fn test_llm_retry_accumulates_metadata() {
    let (mut t, session) = make_tracer(1.0);
    t.on_turn_start("turn_1");
    t.on_llm_start("main", 0, &[], &[]);
    t.on_llm_retrying("main", 0, 1, 3, 500, "timeout");
    t.on_llm_retrying("main", 0, 2, 3, 1000, "timeout");
    t.on_llm_end("main", 0, "gpt-4", "openai", "response", None, None);
    let _handle = t.on_turn_end(None);
    tokio::task::yield_now().await;
    let events = session.events_snapshot();

    // 验证 GenerationCreate 包含重试 metadata（字段名为 retry_count）
    let has_retry_meta = events.iter().any(|e| {
        if let langfuse_client::IngestionEvent::GenerationCreate { body, .. } = e {
            body.metadata
                .as_ref()
                .map(|m| m.get("retry_count").is_some())
                .unwrap_or(false)
        } else {
            false
        }
    });
    assert!(
        has_retry_meta,
        "GenerationCreate 应包含重试 metadata (retry_count)"
    );
}

// ── Middleware 事件测试 ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_middleware_start_and_end() {
    let (mut t, session) = make_tracer(1.0);
    t.on_turn_start("turn_1");
    t.on_middleware_start(
        "auth",
        peri_agent::agent::events::MiddlewareHook::BeforeAgent,
    );
    let mw_handle = t.middleware.on_start(
        "auth",
        peri_agent::agent::events::MiddlewareHook::BeforeAgent,
    );
    // 微小延迟确保 duration > 0（MiddlewareSpan 条件上报）
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    t.on_middleware_end(&mw_handle, StageStatus::Done, None);
    let _handle = t.on_turn_end(None);
    tokio::task::yield_now().await;
    let events = session.events_snapshot();

    let has_mw_span = events.iter().any(|e| {
        if let langfuse_client::IngestionEvent::SpanCreate { body, .. } = e {
            body.name.as_deref() == Some("mw-auth")
        } else {
            false
        }
    });
    assert!(has_mw_span, "应有 mw-auth SpanCreate 事件");
}

// ── Compact 事件测试 ────────────────────────────────────────────────────────

/// Micro compact 应记录为独立类型 `micro-compact`（name 区分），
/// 且 input/output 结构完整（执行前状态 / 执行结果含 token 指标）。
#[tokio::test]
async fn test_compact_lifecycle() {
    let (mut t, session) = make_tracer(1.0);
    t.on_turn_start("turn_1");
    t.on_compact_start(
        peri_agent::agent::events::CompactStrategy::Micro,
        peri_agent::agent::events::CompactTrigger::Auto,
    );
    // 微小延迟确保 duration > 0（Compact 条件上报）
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    t.on_compact_end(crate::langfuse::tracer::compact::CompactEndInfo {
        summary: "summary text".to_string(),
        files_count: 3,
        skills_count: 2,
        micro_cleared: 5,
        is_error: false,
        error_message: String::new(),
        estimated_tokens_saved: 12000,
        estimated_tokens_before: 45000,
        estimated_tokens_after: 33000,
        cache_hit_rate_before: 0.8,
        full_escalation_reason: None,
        outcome: Some("done".to_string()),
    });
    let _handle = t.on_turn_end(None);
    tokio::task::yield_now().await;
    let events = session.events_snapshot();

    let compact = events.iter().find_map(|e| {
        if let langfuse_client::IngestionEvent::SpanCreate { body, .. } = e {
            if body.name.as_deref() == Some("micro-compact") {
                Some(body)
            } else {
                None
            }
        } else {
            None
        }
    });
    let body = compact.expect("Micro 策略应有 micro-compact SpanCreate 事件");

    // input = 执行前状态
    let input = body.input.as_ref().expect("compact 应有 input");
    assert_eq!(input["strategy"], "Micro");
    assert_eq!(input["estimated_tokens_before"], 45000);
    assert_eq!(input["cache_hit_rate_before"], 0.8);

    // output = 执行结果（含 token 指标）
    let output = body.output.as_ref().expect("compact 应有 output");
    assert_eq!(output["summary"], "summary text");
    assert_eq!(output["files_count"], 3);
    assert_eq!(output["skills_count"], 2);
    assert_eq!(output["micro_cleared"], 5);
    assert_eq!(output["estimated_tokens_saved"], 12000);
    assert_eq!(output["estimated_tokens_after"], 33000);
    assert_eq!(output["outcome"], "done");

    // v2 条件上报：compact 改为延迟创建，不再发 SpanUpdate
    let has_compact_update = events.iter().any(|e| {
        if let langfuse_client::IngestionEvent::SpanUpdate { body, .. } = e {
            body.name.as_deref() == Some("micro-compact") || body.name.as_deref() == Some("compact")
        } else {
            false
        }
    });
    assert!(!has_compact_update, "v2 条件上报不应发 compact SpanUpdate");
}

/// Full 策略应记录为 `compact`（非 micro-compact）。
#[tokio::test]
async fn test_full_compact_span_name() {
    let (mut t, session) = make_tracer(1.0);
    t.on_turn_start("turn_1");
    t.on_compact_start(
        peri_agent::agent::events::CompactStrategy::Full,
        peri_agent::agent::events::CompactTrigger::Auto,
    );
    // 微小延迟确保 duration > 0（Compact 条件上报）
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    t.on_compact_end(crate::langfuse::tracer::compact::CompactEndInfo {
        summary: "summary text".to_string(),
        files_count: 0,
        skills_count: 0,
        micro_cleared: 0,
        is_error: false,
        error_message: String::new(),
        estimated_tokens_saved: 0,
        estimated_tokens_before: 0,
        estimated_tokens_after: 0,
        cache_hit_rate_before: 0.0,
        full_escalation_reason: None,
        outcome: None,
    });
    let _handle = t.on_turn_end(None);
    tokio::task::yield_now().await;
    let events = session.events_snapshot();

    let has_full = events.iter().any(|e| {
        if let langfuse_client::IngestionEvent::SpanCreate { body, .. } = e {
            body.name.as_deref() == Some("compact")
        } else {
            false
        }
    });
    assert!(has_full, "Full 策略应有 compact SpanCreate 事件");

    let has_micro = events.iter().any(|e| {
        if let langfuse_client::IngestionEvent::SpanCreate { body, .. } = e {
            body.name.as_deref() == Some("micro-compact")
        } else {
            false
        }
    });
    assert!(!has_micro, "Full 策略不应有 micro-compact 事件");
}

// ── Workflow 事件测试 ───────────────────────────────────────────────────────

#[tokio::test]
async fn test_workflow_span_carries_plan_input() {
    let (mut t, session) = make_tracer(1.0);
    t.on_turn_start("turn_1");
    t.on_stage_start(Stage::Act, "turn_1");
    t.on_workflow_start("wf-1", "my test plan");
    let _handle = t.on_turn_end(None);
    tokio::task::yield_now().await;
    let events = session.events_snapshot();

    // 回归防护：workflow span 必须携带 plan input
    // （历史上曾因重构把 {"plan": plan} 误改为硬编码 None）
    let wf_input = events.iter().find_map(|e| {
        if let langfuse_client::IngestionEvent::SpanCreate { body, .. } = e {
            if body.name.as_deref() == Some("workflow-wf-1") {
                body.input.as_ref()
            } else {
                None
            }
        } else {
            None
        }
    });
    assert_eq!(
        wf_input,
        Some(&serde_json::json!({"plan": "my test plan"})),
        "workflow span 应携带 plan input"
    );
}

/// 回归测试：当 `stages.active_handle()` 返回 None 但 subagent 已注册时，
/// `on_tool_start` 的 parent_id 应 fallback 到该 subagent 的 AGENT obs id，
/// 而非主 agent 的 agent_observation_id。
///
/// 新语义(registry):内容归属按事件 agent_id 查注册表;未知 agent 走注册闸门
/// 或 incomplete,禁止静默挂主 agent。本场景由 registry_test 的
/// `test_content_before_start_gate` 等用例覆盖,此处不再模拟旧栈。
#[test]
fn test_main_agent_tool_not_routed_to_subagent_batch() {
    let (mut t, _session) = make_tracer(1.0);
    t.on_turn_start("turn_main_only");

    // 无 subagent 时,工具应写入主 agent 的 tool_batch
    t.on_tool_start(
        "main",
        "tc_read",
        "Read",
        &serde_json::json!({"path": "test.txt"}),
    );
    t.on_tool_end("main", "tc_read", "file content", false);

    // 主 agent 的 tool_batch 应有该工具
    let main_flush = t.tool_batch.flush();
    assert_eq!(
        main_flush.tools.len(),
        1,
        "无 subagent 时,工具应写入主 agent 的 tool_batch"
    );
    assert_eq!(main_flush.tools[0].name, "Read");
}

/// 回归测试：Receive stage span 的 input 应包含 mq_counts 排空数据。
/// bug: stages.on_stage_end() 在 span body 构造前清空 active → mq_counts 丢失，
/// 导致 Receive span 的 input 始终为 None。
/// 修复：在 stages.on_stage_end() 之前捕获 mq_counts，填入 span body 的 input 字段。
#[test]
fn test_receive_stage_span_includes_mq_counts_in_input() {
    let (mut t, session) = make_tracer(1.0);
    t.on_turn_start("turn_mq");

    // 1. 开始 Receive 阶段 → mq_counts 初始化为 (0,0,0)
    t.on_stage_start(Stage::Receive, "turn_mq");

    // 2. 模拟 MQ 排空：1 条 prompt + 2 条 defer + 0 条 info = 共 3 条
    t.on_mq_drained("main", 1, 2, 0);

    // 3. 确保 stage 持续 ≥ 2ms，避免 duration_ms==0 导致 span 被条件跳过
    std::thread::sleep(std::time::Duration::from_millis(2));

    // 4. 获取 active handle（on_stage_start 返回的是忽略的，从 stages 拿实际 handle）
    let handle = t
        .stages
        .active_handle("main")
        .expect("Receive stage 应为 active")
        .clone();

    // 5. 结束 Receive 阶段
    t.on_stage_end("main", &handle, StageStatus::Done);

    // 6. 验证：发出的 SpanCreate 事件中 input 应包含 mq_counts
    let events = session.events_snapshot();
    let span_creates: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let IngestionEvent::SpanCreate { body, .. } = e {
                if body.name.as_deref() == Some("stage-receive") {
                    Some(body)
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    assert_eq!(
        span_creates.len(),
        1,
        "应有恰好 1 个 stage-receive SpanCreate 事件"
    );

    let span = &span_creates[0];
    let input = span
        .input
        .as_ref()
        .expect("Receive span 的 input 不应为 None");
    let mq = input
        .get("messages_drained")
        .expect("input 应包含 messages_drained 字段");

    assert_eq!(mq["prompt"], serde_json::json!(1), "prompt 计数应为 1");
    assert_eq!(mq["defer"], serde_json::json!(2), "defer 计数应为 2");
    assert_eq!(mq["info"], serde_json::json!(0), "info 计数应为 0");
    assert_eq!(mq["total"], serde_json::json!(3), "total 应为 1+2+0 = 3");
}

/// 非 Receive 阶段的 span input 应保持 None，不应误填充。
#[test]
fn test_non_receive_stage_span_input_is_none() {
    let (mut t, session) = make_tracer(1.0);
    t.on_turn_start("turn_reason");

    // Reason 阶段
    t.on_stage_start(Stage::Reason, "turn_reason");
    // 确保 stage 持续 ≥ 2ms，避免 duration_ms==0 导致 span 被条件跳过
    std::thread::sleep(std::time::Duration::from_millis(2));
    let handle = t
        .stages
        .active_handle("main")
        .expect("Reason stage 应为 active")
        .clone();
    t.on_stage_end("main", &handle, StageStatus::Done);

    let events = session.events_snapshot();
    let reason_spans: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let IngestionEvent::SpanCreate { body, .. } = e {
                if body.name.as_deref() == Some("stage-reason") {
                    Some(body)
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    assert_eq!(
        reason_spans.len(),
        1,
        "应有恰好 1 个 stage-reason SpanCreate"
    );
    assert!(
        reason_spans[0].input.is_none(),
        "非 Receive 阶段的 input 应为 None，不应误填入 mq_counts"
    );
}

// ── 工具 TOOL observation 出入参完整性（回归：0c0a3313e 误删 input/output）──────

#[tokio::test]
async fn test_tool_observation_carries_input_and_output() {
    let (mut t, session) = make_tracer(1.0);
    t.on_turn_start("turn_1");
    t.on_stage_start(Stage::Reason, "turn_1");

    t.on_tool_start(
        "main",
        "tc_read",
        "Read",
        &serde_json::json!({"path": "/tmp/a.txt"}),
    );
    t.on_tool_end("main", "tc_read", "file content: hello", false);

    let _handle = t.on_turn_end(None);
    tokio::task::yield_now().await;
    let events = session.events_snapshot();

    let tool_obs = events
        .iter()
        .filter_map(|e| {
            if let IngestionEvent::ObservationCreate { body, .. } = e {
                (body.r#type == ObservationType::Tool && body.name.as_deref() == Some("Read"))
                    .then_some(body)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(tool_obs.len(), 1, "应有恰好 1 个 Read TOOL observation");

    let obs = &tool_obs[0];
    assert_eq!(
        obs.input,
        Some(serde_json::json!({"path": "/tmp/a.txt"})),
        "TOOL observation 应携带工具输入参数"
    );
    assert_eq!(
        obs.output,
        Some(serde_json::json!("file content: hello")),
        "TOOL observation 应携带工具执行结果"
    );
}

#[tokio::test]
async fn test_tool_observation_error_marks_error_class() {
    let (mut t, session) = make_tracer(1.0);
    t.on_turn_start("turn_1");
    t.on_stage_start(Stage::Reason, "turn_1");

    t.on_tool_start(
        "main",
        "tc_fail",
        "Bash",
        &serde_json::json!({"command": "exit 1"}),
    );
    t.on_tool_end("main", "tc_fail", "command failed", true);

    let _handle = t.on_turn_end(None);
    tokio::task::yield_now().await;
    let events = session.events_snapshot();

    let tool_obs = events
        .iter()
        .filter_map(|e| {
            if let IngestionEvent::ObservationCreate { body, .. } = e {
                (body.r#type == ObservationType::Tool && body.name.as_deref() == Some("Bash"))
                    .then_some(body)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(tool_obs.len(), 1, "应有恰好 1 个 Bash TOOL observation");
    assert_eq!(
        tool_obs[0].input,
        Some(serde_json::json!({"command": "exit 1"})),
        "错误工具也应携带输入参数"
    );
    assert_eq!(
        tool_obs[0].output,
        Some(serde_json::json!({
            "error_class": "tool_failure",
            "error_message": "command failed",
            "error_schema_version": 2,
        })),
        "错误工具 output 应保留 error_class 与 error_message 标记"
    );
}
