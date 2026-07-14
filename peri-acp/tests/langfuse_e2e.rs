// peri-acp/tests/langfuse_e2e.rs
// e2e mock 端到端测试：验证完整 turn 序列从 tracer → batcher → HTTP 的链路。
//
// 注意：`on_turn_end()` 内部调用 `tokio::spawn`，因此需要 `#[tokio::test]`
// 提供异步运行时。

mod tests {
    use peri_acp::langfuse::config::LangfuseConfig;
    use peri_acp::langfuse::fake_session::FakeLangfuseSession;
    use peri_acp::langfuse::LangfuseTracer;
    use peri_agent::agent::events::Stage;

    fn make_config(rate: f64) -> LangfuseConfig {
        LangfuseConfig {
            public_key: None,
            secret_key: None,
            host: "https://cloud.langfuse.com".to_string(),
            trace_sampling: rate,
            error_span_always: true,
            batch_max_events: 50,
            batch_flush_interval_secs: 10,
        }
    }

    #[tokio::test]
    async fn test_e2e_complete_turn_with_fake_session() {
        // FakeLangfuseSession::new() 已返回 Arc<Self>
        let session = FakeLangfuseSession::new("sess_e2e");
        let config = make_config(1.0);
        let mut tracer = LangfuseTracer::new(session.clone(), "sess_e2e".to_string(), config);

        tracer.on_turn_start("turn_e2e");
        tracer.on_stage_start(Stage::Receive, "turn_e2e");
        tracer.on_stage_start(Stage::Reason, "turn_e2e");
        tracer.on_llm_start(0, &[], &[]);
        tracer.on_llm_end(0, "claude-sonnet-4", "anthropic", "hello world", None);
        tracer.on_stage_start(Stage::End, "turn_e2e");
        let _handle = tracer.on_turn_end(None);

        tokio::task::yield_now().await;
        let events = session.events_snapshot();
        assert!(!events.is_empty(), "e2e: 完整 turn 应产生事件");

        // 验证包含 agent-run observation
        let has_agent_obs = events.iter().any(|e| {
            if let langfuse_client::IngestionEvent::ObservationCreate { body, .. } = e {
                body.name.as_deref() == Some("agent-run")
            } else {
                false
            }
        });
        assert!(has_agent_obs, "e2e: 应有 agent-run ObservationCreate");
    }

    #[tokio::test]
    async fn test_e2e_error_turn_with_zero_sampling() {
        // FakeLangfuseSession::new() 已返回 Arc<Self>
        let session = FakeLangfuseSession::new("sess_e2e_error");
        let config = make_config(0.0); // 采样率 0
        let mut tracer = LangfuseTracer::new(session.clone(), "sess_e2e_error".to_string(), config);

        tracer.on_turn_start("turn_err");
        let _handle = tracer.on_turn_end(Some("SomeError"));

        tokio::task::yield_now().await;
        let events = session.events_snapshot();
        // 采样率 0 但错误 turn，应有 ErrorSpan + 合成 TraceCreate
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
        assert!(has_trace, "e2e: 错误 turn 应补发 TraceCreate");
        assert!(has_error_span, "e2e: 错误 turn 应发 ErrorSpan");
    }
}
