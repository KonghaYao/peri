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
    let recv_handle = t.stages.on_stage_start(
        Stage::Receive,
        &t.trace_id,
        "turn_1",
        &t.agent_observation_id,
    );
    t.on_stage_end(&recv_handle, StageStatus::Done);

    // Stage: Reason + LLM
    t.on_stage_start(Stage::Reason, "turn_1");
    t.on_llm_start(0, &[], &[]);
    t.on_llm_end(0, "claude-4.7", "anthropic", "hello", None);
    let reason_handle = t.stages.on_stage_start(
        Stage::Reason,
        &t.trace_id,
        "turn_1",
        &t.agent_observation_id,
    );
    t.on_stage_end(&reason_handle, StageStatus::Done);

    // Stage: End
    t.on_stage_start(Stage::End, "turn_1");
    let end_handle =
        t.stages
            .on_stage_start(Stage::End, &t.trace_id, "turn_1", &t.agent_observation_id);
    t.on_stage_end(&end_handle, StageStatus::Done);

    let _handle = t.on_turn_end(None);
    // 等待 flush async 任务完成（FakeLangfuseSession 的 flush 是同步的，但 spawn 需要运行）
    tokio::task::yield_now().await;
    let events = session.events_snapshot();
    assert!(!events.is_empty(), "应有至少一个事件");
}

// ── 采样率测试 ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_sampling_rate_0_emits_nothing() {
    let (mut t, session) = make_tracer(0.0);
    t.on_turn_start("turn_1");
    t.on_stage_start(Stage::Reason, "turn_1");
    t.on_llm_start(0, &[], &[]);
    t.on_llm_end(0, "m", "p", "o", None);
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
        if let langfuse_client::IngestionEvent::SpanCreate { body, .. } = e {
            body.name.as_deref() == Some("ErrorTurn")
        } else {
            false
        }
    });
    assert!(has_trace, "错误 turn 应补发 TraceCreate");
    assert!(has_error_span, "错误 turn 应发 ErrorSpan");
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
    t.on_llm_start(0, &[], &[]);
    t.on_llm_end(0, "gpt-4", "openai", "response", None);
    let _handle = t.on_turn_end(None);
    tokio::task::yield_now().await;
    let events = session.events_snapshot();

    let gen_count = events
        .iter()
        .filter(|e| matches!(e, langfuse_client::IngestionEvent::GenerationCreate { .. }))
        .count();
    assert!(gen_count > 0, "应有至少一个 GenerationCreate 事件");
}

#[tokio::test]
async fn test_llm_retry_accumulates_metadata() {
    let (mut t, session) = make_tracer(1.0);
    t.on_turn_start("turn_1");
    t.on_llm_start(0, &[], &[]);
    t.on_llm_retrying(1, 3, 500, "timeout");
    t.on_llm_retrying(2, 3, 1000, "timeout");
    t.on_llm_end(0, "gpt-4", "openai", "response", None);
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
    t.on_compact_end("summary text", 3, 2, 5, false, "");
    let _handle = t.on_turn_end(None);
    tokio::task::yield_now().await;
    let events = session.events_snapshot();

    let has_compact_span = events.iter().any(|e| {
        if let langfuse_client::IngestionEvent::SpanCreate { body, .. } = e {
            body.name.as_deref() == Some("compact")
        } else {
            false
        }
    });
    assert!(has_compact_span, "应有 compact SpanCreate 事件");

    // v2 条件上报：compact 改为延迟创建，不再发 SpanUpdate
    let has_compact_update = events.iter().any(|e| {
        if let langfuse_client::IngestionEvent::SpanUpdate { body, .. } = e {
            body.name.as_deref() == Some("compact")
        } else {
            false
        }
    });
    assert!(!has_compact_update, "v2 条件上报不应发 compact SpanUpdate");
}
