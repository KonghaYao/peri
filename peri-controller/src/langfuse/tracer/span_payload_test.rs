// Responsibility-specific cases share the original test namespace and fixtures.

// ── TextChunk 无副作用回归测试 ─────────────────────────────────────────────

/// [回归] TextChunk 曾无条件累积到 `LangfuseTracer::final_answer`（无读取者、
/// 无上限、无清理）。文本增量不得进入 Langfuse 事件模型。
#[test]
fn test_text_chunks_produce_no_langfuse_events() {
    use crate::langfuse::bridge::UnifiedLangfuseEvent;
    use peri_agent::agent::events::ExecutorEvent;
    use peri_agent::agent::events_v2::RenderEvent;

    for chunk in ["Hello ", "World"] {
        assert!(
            UnifiedLangfuseEvent::from_executor_event(ExecutorEvent::TextChunk {
                message_id: peri_acp_types::messages::MessageId::new(),
                chunk: chunk.to_string(),
                source_agent_id: None,
            })
            .is_none(),
            "v1 TextChunk 不应产生 Langfuse 事件"
        );
        assert!(
            UnifiedLangfuseEvent::from_render_event(RenderEvent::TextChunk {
                turn_id: peri_acp_types::session::TurnId::new(),
                agent_id: peri_acp_types::identity::AgentId::new(),
                message_id: peri_acp_types::messages::MessageId::new(),
                chunk: chunk.to_string(),
            })
            .is_none(),
            "v2 TextChunk 不应产生 Langfuse 事件"
        );
    }
}

/// 文本增量不进入 tracer 后，完整文本仍由 `on_llm_end` 的 Generation output 承载。
#[tokio::test]
async fn test_generation_output_remains_complete_after_text_chunks() {
    let (mut t, session) = make_tracer(1.0);
    t.on_turn_start("turn_1");
    t.on_llm_start("main", 0, &[], &[]);
    t.on_llm_end("main", 0, "gpt-4", "openai", "Hello World", None, None);
    let _handle = t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed);
    tokio::task::yield_now().await;
    let events = session.events_snapshot();

    let generation_output = events.iter().find_map(|e| {
        if let langfuse_client::IngestionEvent::GenerationCreate { body, .. } = e {
            body.output.clone()
        } else {
            None
        }
    });
    assert_eq!(
        generation_output,
        Some(serde_json::json!({"text": "Hello World"})),
        "文本增量不进入 tracer 后，Generation output 仍须是完整文本"
    );
}

// ── LLM Generation 事件测试 ─────────────────────────────────────────────────

#[tokio::test]
async fn test_llm_generation_emits_events() {
    let (mut t, session) = make_tracer(1.0);
    t.on_turn_start("turn_1");
    t.on_llm_start("main", 0, &[], &[]);
    t.on_llm_end("main", 0, "gpt-4", "openai", "response", None, None);
    let _handle = t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed);
    tokio::task::yield_now().await;
    let events = session.events_snapshot();

    let gen_count = events
        .iter()
        .filter(|e| matches!(e, langfuse_client::IngestionEvent::GenerationCreate { .. }))
        .count();
    assert!(gen_count > 0, "应有至少一个 GenerationCreate 事件");

    // 回归防护：input 不再进 body（由部署侧冻结来源承载，见 `input_snapshot_test` 的逐字节
    // 用例），因此这里的假会话（无冻结能力）必须留下稳定拒收标记，不得静默丢掉 generation input
    // （历史上曾因重构误写成硬编码 None，导致所有 generation 的 input 丢失，见 commit 0c0a3313）。
    let rejected = events.iter().any(|e| {
        matches!(e, langfuse_client::IngestionEvent::GenerationCreate { body, .. }
            if body.metadata.as_ref().and_then(|meta| meta.get("input_snapshot_rejected"))
                == Some(&serde_json::json!(true)))
    });
    assert!(rejected, "冻结被拒必须显式标记 input_snapshot_rejected");
}

/// 首个 reasoning 分片即 completion 起点；可见文本没有事件，必须显式标 unavailable。
#[tokio::test]
async fn test_llm_generation_completion_start_from_reasoning() {
    let (mut t, session) = make_tracer(1.0);
    t.on_turn_start("turn_1");
    t.on_llm_start("main", 0, &[], &[]);
    t.on_ai_reasoning_chunk("main", "think");
    std::thread::sleep(std::time::Duration::from_millis(2));
    t.on_llm_end("main", 0, "gpt-4", "openai", "answer", None, None);
    let _handle = t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed);
    tokio::task::yield_now().await;
    let events = session.events_snapshot();

    let gen = events
        .iter()
        .find_map(|e| {
            if let langfuse_client::IngestionEvent::GenerationCreate { body, .. } = e {
                Some(body)
            } else {
                None
            }
        })
        .expect("应有 GenerationCreate 事件");
    assert!(
        gen.completion_start_time.is_some(),
        "首个 reasoning 分片应写入 completion_start_time"
    );
    let meta = gen.metadata.as_ref().expect("generation 应有 metadata");
    assert_eq!(meta["completion_start_source"], "reasoning_chunk");
    assert!(
        meta["first_reasoning_ms"].as_u64().is_some(),
        "first_reasoning_ms 应为毫秒数"
    );
    assert_eq!(
        meta["first_visible_text_ms"],
        serde_json::Value::Null,
        "首个可见文本无法测量，必须显式 null"
    );
}

/// 无 reasoning 分片时 completion_start_time 保持 null，来源标 unavailable。
#[tokio::test]
async fn test_llm_generation_completion_start_unavailable_without_reasoning() {
    let (mut t, session) = make_tracer(1.0);
    t.on_turn_start("turn_1");
    t.on_llm_start("main", 0, &[], &[]);
    t.on_llm_end("main", 0, "gpt-4", "openai", "answer", None, None);
    let _handle = t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed);
    tokio::task::yield_now().await;
    let events = session.events_snapshot();

    let gen = events
        .iter()
        .find_map(|e| {
            if let langfuse_client::IngestionEvent::GenerationCreate { body, .. } = e {
                Some(body)
            } else {
                None
            }
        })
        .expect("应有 GenerationCreate 事件");
    assert!(
        gen.completion_start_time.is_none(),
        "没有 reasoning 事件时不得伪造 completion 起点"
    );
    let meta = gen.metadata.as_ref().expect("generation 应有 metadata");
    assert_eq!(meta["completion_start_source"], "unavailable");
    assert!(
        meta.get("first_reasoning_ms").is_none(),
        "不可用时不应写入毫秒值"
    );
    assert_eq!(meta["first_visible_text_ms"], serde_json::Value::Null);
}

#[tokio::test]
async fn test_llm_error_preserves_status_message() {
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
    let _handle = t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed);
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
        Some("ERROR: sentinel-secret"),
        "generation statusMessage 应保留实际错误"
    );
    assert!(format!("{gen:?}").contains("sentinel-secret"));
}

#[tokio::test]
async fn test_turn_error_reason_is_safe_in_error_span() {
    let (mut t, session) = make_tracer(0.0);
    t.on_turn_start("turn_1");
    t.on_turn_error(peri_agent::agent::events_v2::TurnErrorReason::LlmFailure);
    let _handle = t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Failed {
        failure: Box::new(peri_acp_types::session::ExecutionFailure {
            kind: peri_acp_types::session::ExecutionFailureKind::Llm,
            public_message: "LLM failure".to_string(),
            error_category: None,
            causes: Vec::new(),
            http_status: None,
            diagnostic: None,
        }),
    });
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
        Some(&serde_json::json!(3))
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
    let _handle = t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed);
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
    let _handle = t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed);
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
    let _handle = t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed);
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
    let _handle = t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed);
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
    let _handle = t.on_turn_end(peri_acp_types::session::TurnTelemetryOutcome::Completed);
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
