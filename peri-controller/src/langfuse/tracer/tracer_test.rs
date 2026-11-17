//! LangfuseTracer 集成烟雾测试。
//!
//! 覆盖完整的 turn 生命周期、采样率控制、ErrorSpan 机制、
//! 文本增量无副作用和 LLM generation 事件流。
//!
//! 注意：`on_turn_end()` 内部调用 `tokio::spawn`，因此需要 `#[tokio::test]`
//! 提供异步运行时。

use crate::langfuse::langfuse_session_like::LangfuseSessionLike as _;

use crate::langfuse::tracer::input_snapshot::GenerationInput;
use crate::langfuse::tracer::LangfuseTracer;
use langfuse_client::types::ObservationLevel;
use langfuse_client::IngestionEvent;
use langfuse_client::ObservationType;
use peri_acp_types::command::PromptStopReason;
use peri_acp_types::session::{ExecutionFailure, TurnTelemetryOutcome};
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
        ..Default::default()
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

    let _handle = t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed);
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
    let _handle = t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed);
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
    let _handle = t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed);
    tokio::task::yield_now().await;
    let events = session.events_snapshot();
    assert!(!events.is_empty(), "采样率 1.0 应有事件");
}

// ── ErrorSpan 测试 ──────────────────────────────────────────────────────────

#[tokio::test]
async fn test_error_span_emitted_for_error_turn() {
    let (mut t, session) = make_tracer(0.0);
    t.on_turn_start("turn_1");
    t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Failed {
        failure: Box::new(peri_acp_types::session::ExecutionFailure::internal(
            "Turn failed",
        )),
    })
    .await
    .expect("flush task should finish");
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
    assert_eq!(session.flush_count(), 1, "未采样 fatal turn 必须立即 flush");
}

#[tokio::test]
async fn test_turn_end_failed_closes_active_generation_with_canonical_failure() {
    let (mut t, session) = make_tracer(1.0);
    t.on_turn_start("turn_1");
    let stage = t
        .on_stage_start_gated("main", Stage::Reason, "turn_1")
        .expect("sampled stage");
    std::thread::sleep(std::time::Duration::from_millis(2));
    t.on_llm_start("main", 0, &[], &[]);
    let failure =
        ExecutionFailure::from_agent_error(&peri_acp_types::error::AgentError::LlmHttpError {
            status: 429,
            message: "retry exhausted token=secret".to_string(),
        });

    t.on_turn_end(TurnTelemetryOutcome::Failed {
        failure: Box::new(failure),
    })
    .await
    .expect("flush task should finish");

    let events = session.events_snapshot();
    let stage_index = events
        .iter()
        .position(|event| matches!(event, langfuse_client::IngestionEvent::SpanCreate { body, .. } if body.id.as_deref() == Some(stage.span_id.as_str())))
        .expect("active stage parent must be closed");
    let generation_index = events
        .iter()
        .position(|event| {
            matches!(
                event,
                langfuse_client::IngestionEvent::GenerationCreate { .. }
            )
        })
        .expect("active generation must be closed");
    assert!(
        stage_index < generation_index,
        "stage parent 必须先于 generation child 入队"
    );

    let generation = events.into_iter().find_map(|event| {
        if let langfuse_client::IngestionEvent::GenerationCreate { body, .. } = event {
            Some(body)
        } else {
            None
        }
    });
    let generation = generation.expect("active generation must be closed at turn end");
    assert_eq!(generation.level, Some(ObservationLevel::Error));
    assert_eq!(generation.status_message.as_deref(), Some("rate_limit"));
    assert_eq!(
        generation
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("http_status")),
        Some(&serde_json::json!(429))
    );
    assert!(
        generation
            .output
            .as_ref()
            .expect("failure output")
            .to_string()
            .contains("retry exhausted token=secret"),
        "generation must retain the provider diagnostic"
    );
}

#[tokio::test]
async fn test_cancelled_turn_closes_active_generation_as_warning() {
    let (mut t, session) = make_tracer(1.0);
    t.on_turn_start("turn_1");
    t.on_llm_start("main", 0, &[], &[]);

    t.on_turn_end(TurnTelemetryOutcome::Stopped {
        reason: PromptStopReason::Cancelled,
    })
    .await
    .expect("flush task should finish");

    let generation = session.events_snapshot().into_iter().find_map(|event| {
        if let langfuse_client::IngestionEvent::GenerationCreate { body, .. } = event {
            Some(body)
        } else {
            None
        }
    });
    let generation = generation.expect("cancelled active generation must be closed");
    assert_eq!(generation.level, Some(ObservationLevel::Warning));
    assert_eq!(generation.status_message.as_deref(), Some("cancelled"));
}

#[tokio::test]
async fn test_unresolved_llm_parent_is_preserved_until_turn_end_fallback() {
    let (mut t, session) = make_tracer(1.0);
    t.set_main_agent_id("main".to_string());
    t.on_turn_start("turn_1");
    // 追踪器只搬运不透明来源；这里直接登记一个"已 start 但归属不可解析"的 generation。
    t.generation
        .on_llm_start("unregistered-child", 0, GenerationInput::Rejected);
    t.on_ai_reasoning_chunk("unregistered-child", "think");

    t.on_llm_end(
        "unregistered-child",
        0,
        "test-model",
        "test-provider",
        "completed",
        None,
        None,
    );
    assert!(
        t.generation.contains("unregistered-child", 0),
        "unresolved parent must not consume generation state"
    );

    t.on_turn_end(TurnTelemetryOutcome::Failed {
        failure: Box::new(ExecutionFailure::internal("safe internal failure")),
    })
    .await
    .expect("flush task should finish");

    let generation = session.events_snapshot().into_iter().find_map(|event| {
        if let langfuse_client::IngestionEvent::GenerationCreate { body, .. } = event {
            Some(body)
        } else {
            None
        }
    });
    let generation = generation.expect("unresolved generation must use turn-end fallback");
    assert_eq!(
        generation.parent_observation_id.as_deref(),
        Some(t.agent_observation_id.as_str())
    );
    assert_eq!(
        generation
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("ownership_unresolved")),
        Some(&serde_json::json!(true))
    );
    assert_eq!(
        generation.output,
        Some(serde_json::json!({"text": "completed"}))
    );
    assert_eq!(generation.model.as_deref(), Some("test-model"));
    assert_eq!(
        generation.status_message.as_deref(),
        Some("ownership_unresolved")
    );
    assert_eq!(
        generation
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("incomplete")),
        Some(&serde_json::json!(false))
    );
    assert!(
        generation.completion_start_time.is_some(),
        "兜底路径不得丢失首个 reasoning 建立起的 completion 起点"
    );
    let meta = generation.metadata.as_ref().expect("应携带 metadata");
    assert_eq!(meta["completion_start_source"], "reasoning_chunk");
    assert!(meta["first_reasoning_ms"].as_u64().is_some());
    assert_eq!(meta["first_visible_text_ms"], serde_json::Value::Null);
}

#[tokio::test]
async fn test_llm_end_without_start_emits_synthetic_generation() {
    let (mut t, session) = make_tracer(1.0);
    t.on_turn_start("turn_1");

    t.on_llm_end(
        "main",
        7,
        "test-model",
        "test-provider",
        "completed",
        None,
        Some("req-7"),
    );

    let generation = session.events_snapshot().into_iter().find_map(|event| {
        if let langfuse_client::IngestionEvent::GenerationCreate { body, .. } = event {
            Some(body)
        } else {
            None
        }
    });
    let generation = generation.expect("missing start must produce synthetic generation");
    assert_eq!(
        generation.output,
        Some(serde_json::json!({"text": "completed"}))
    );
    assert_eq!(generation.model.as_deref(), Some("test-model"));
    assert_eq!(
        generation
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("request_id")),
        Some(&serde_json::json!("req-7"))
    );
    assert_eq!(
        generation
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("start_missing")),
        Some(&serde_json::json!(true))
    );
}

#[tokio::test]
async fn test_unsampled_failure_emits_parent_before_error_and_preserves_message() {
    let (mut t, session) = make_tracer(0.0);
    let parent_id = t.agent_observation_id.clone();
    let secrets = "sk-live-raw eyJhbGciOiJIUzI1NiJ9.payload.signature -----BEGIN PRIVATE KEY----- postgres://user:password@host/db";

    t.on_turn_end(TurnTelemetryOutcome::Failed {
        failure: Box::new(ExecutionFailure::internal(secrets)),
    })
    .await
    .expect("flush task should finish");

    let events = session.events_snapshot();
    let parent_index = events.iter().position(|event| {
        matches!(
            event,
            langfuse_client::IngestionEvent::ObservationCreate { body, .. }
                if body.id.as_deref() == Some(parent_id.as_str())
        )
    });
    let error_index = events.iter().position(|event| {
        matches!(
            event,
            langfuse_client::IngestionEvent::EventCreate { body, .. }
                if body.name.as_deref() == Some("ErrorTurn")
                    && body.parent_observation_id.as_deref() == Some(parent_id.as_str())
        )
    });
    assert!(
        parent_index.is_some(),
        "synthetic ErrorTurn parent must exist"
    );
    assert!(
        parent_index < error_index,
        "synthetic parent must be queued before ErrorTurn"
    );
    let serialized = serde_json::to_string(&events).expect("events should serialize");
    for secret in [
        "sk-live-raw",
        "eyJhbGci",
        "BEGIN PRIVATE KEY",
        "postgres://",
    ] {
        assert!(
            serialized.contains(secret),
            "Langfuse payload lost diagnostic marker: {secret}"
        );
    }
}
include!("span_payload_test.rs");

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
    t.on_mq_drained(
        "main",
        1,
        2,
        0,
        Some("2020-01-01T00:00:00.000000Z".to_string()),
    );

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

    // 排队等待以 Receive 起点减去最早入队时间；两端都在 span 上，可复算。
    let enqueued = chrono::DateTime::parse_from_rfc3339("2020-01-01T00:00:00.000000Z")
        .expect("fixture 时间应可解析");
    let started = chrono::DateTime::parse_from_rfc3339(
        span.start_time.as_deref().expect("span start_time 应存在"),
    )
    .expect("span start_time 应可解析");
    let expected_ms = started
        .signed_duration_since(enqueued)
        .num_milliseconds()
        .max(0) as u64;
    assert_eq!(
        input["queue_wait_ms"],
        serde_json::json!(expected_ms),
        "queue_wait_ms 应等于 Receive 起点与最早入队时间的差"
    );
}

/// 没有可用入队时间时，queue_wait_ms 必须是 null（保持未知），不能补零。
#[test]
fn test_receive_stage_queue_wait_is_null_without_enqueued_at() {
    let (mut t, session) = make_tracer(1.0);
    t.on_turn_start("turn_mq_null");
    t.on_stage_start(Stage::Receive, "turn_mq_null");
    t.on_mq_drained("main", 1, 0, 0, None);
    std::thread::sleep(std::time::Duration::from_millis(2));
    let handle = t
        .stages
        .active_handle("main")
        .expect("Receive stage 应为 active")
        .clone();
    t.on_stage_end("main", &handle, StageStatus::Done);

    let events = session.events_snapshot();
    let span = events
        .iter()
        .find_map(|e| {
            if let IngestionEvent::SpanCreate { body, .. } = e {
                (body.name.as_deref() == Some("stage-receive")).then_some(body)
            } else {
                None
            }
        })
        .expect("应有 stage-receive SpanCreate");
    let input = span.input.as_ref().expect("Receive span input 应存在");
    assert_eq!(
        input.get("queue_wait_ms"),
        Some(&serde_json::Value::Null),
        "无入队时间时 queue_wait_ms 应为 null"
    );
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

    let _handle = t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed);
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
    assert_eq!(
        obs.metadata
            .as_ref()
            .and_then(|meta| meta.get("tool_call_id"))
            .and_then(|id| id.as_str()),
        Some("tc_read"),
        "TOOL observation metadata 应携带 tool_call_id，用于与 Generation tool_calls 对齐"
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

    let _handle = t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed);
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

// ── 活跃 turn trace 登记（指标出口归属） ──────────────────────────────────────

#[tokio::test]
async fn turn_lifecycle_registers_and_clears_active_trace_for_metrics() {
    let (mut t, session) = make_tracer(1.0);
    let trace_id = t.trace_id.clone();

    t.on_turn_start("turn_metrics");
    let active = session
        .turn_traces()
        .resolve("sess_smoke")
        .expect("turn 开始后指标出口应能取到本 turn 的 trace");
    assert_eq!(active.trace_id, trace_id);
    assert_eq!(
        active.parent_observation_id.as_deref(),
        Some(trace_id.as_str()),
        "采样 turn 的指标应挂到 turn 根 span（id = trace id）下"
    );

    let _flush = t.on_turn_end(TurnTelemetryOutcome::Completed);
    assert!(
        session.turn_traces().resolve("sess_smoke").is_none(),
        "turn 结束后应清理活跃登记，后续指标回退独立 root trace"
    );
}

#[tokio::test]
async fn background_subagent_metrics_survive_turn_end_until_stop() {
    let (mut t, session) = make_tracer(1.0);
    let trace_id = t.trace_id.clone();

    t.on_turn_start("turn_background_subagent");
    t.on_subagent_start("main", "child-bg", "coder", true);

    let _flush = t.on_turn_end(TurnTelemetryOutcome::Completed);
    let active = session
        .turn_traces()
        .resolve("child-bg")
        .expect("后台子 agent 活过父 turn，其指标登记不能被 turn 结束清掉");
    assert_eq!(active.trace_id, trace_id);
    assert_eq!(
        active.parent_observation_id.as_deref(),
        Some(trace_id.as_str()),
        "turn 结束后产生的指标仍归原 trace 根 span"
    );

    t.on_subagent_stop("main", "child-bg", "done", false);
    assert!(
        session.turn_traces().resolve("child-bg").is_none(),
        "后台子 agent 结束后清理其指标归属"
    );
}

#[tokio::test]
async fn unsampled_turn_registers_without_parent_observation() {
    let (mut t, session) = make_tracer(0.0);
    let trace_id = t.trace_id.clone();

    t.on_turn_start("turn_unsampled");
    let active = session
        .turn_traces()
        .resolve("sess_smoke")
        .expect("登记不受采样影响");
    assert_eq!(active.trace_id, trace_id);
    assert!(
        active.parent_observation_id.is_none(),
        "未采样 turn 没有根 span，指标只共享 trace 不挂父观测"
    );

    let _flush = t.on_turn_end(TurnTelemetryOutcome::Completed);
    assert!(session.turn_traces().resolve("sess_smoke").is_none());
}

#[tokio::test]
async fn subagent_sid_follows_turn_trace_and_is_cleared() {
    let (mut t, session) = make_tracer(1.0);
    let trace_id = t.trace_id.clone();

    t.on_turn_start("turn_subagent_metrics");
    t.on_subagent_start("main", "child-thread-1", "worker", false);
    let child = session
        .turn_traces()
        .resolve("child-thread-1")
        .expect("子 agent 的 thread id 应在本 turn 登记");
    assert_eq!(child.trace_id, trace_id, "子 agent 指标归同一 turn trace");
    assert_eq!(
        child.parent_observation_id.as_deref(),
        Some(trace_id.as_str()),
        "子 agent 指标挂到同一 turn 根 span 下"
    );

    t.on_subagent_stop("main", "child-thread-1", "done", false);
    assert!(
        session.turn_traces().resolve("child-thread-1").is_none(),
        "子 agent 结束后应清理其指标归属"
    );

    // 未正常收 Stop 的子 agent：turn 结束时由 clear_trace 兜底清理
    t.on_subagent_start("main", "child-thread-2", "worker", false);
    assert!(session.turn_traces().resolve("child-thread-2").is_some());

    let _flush = t.on_turn_end(TurnTelemetryOutcome::Completed);
    assert!(
        session.turn_traces().resolve("child-thread-2").is_none(),
        "turn 结束应清理残留的子 agent sid"
    );
    assert!(session.turn_traces().resolve("sess_smoke").is_none());
}
