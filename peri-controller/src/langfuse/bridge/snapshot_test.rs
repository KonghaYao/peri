use super::*;
use crate::langfuse::config::LangfuseConfig;
use crate::langfuse::fake_session::FakeLangfuseSession;
use peri_acp_types::identity::AgentId;
use peri_acp_types::session::TurnTelemetryOutcome;
use peri_agent::agent::LangfuseBridgeLike;
use peri_agent::messages::BaseMessage;
use peri_agent::session::turn::TurnId;
use peri_agent::tools::ToolDefinition;

#[tokio::test]
async fn observe_start_reuses_snapshot_and_fallback_keeps_messages_and_tools() {
    let session = FakeLangfuseSession::new("snapshot-session");
    let tracer = Arc::new(Mutex::new(LangfuseTracer::new(
        session.clone(),
        "snapshot-session".to_string(),
        LangfuseConfig::default(),
    )));
    let agent_id = AgentId::new();
    let bridge = LangfuseBridge::new(
        tracer.clone(),
        "provider".to_string(),
        Some(agent_id.to_string()),
    );
    let messages = Arc::new(vec![BaseMessage::human("complete-context🙂".repeat(4096))]);
    let tools = vec![ToolDefinition {
        name: "lookup".to_string(),
        description: "lookup description".to_string(),
        parameters: serde_json::json!({"type": "object"}),
    }];
    let expected = serde_json::json!({"messages": messages.as_ref(), "tools": &tools});
    let event = ObserveEvent::LlmCallStart {
        turn_id: TurnId::new(),
        agent_id,
        step: 0,
        messages: Arc::clone(&messages),
        tools,
    };
    tracer.lock().on_turn_start("input");
    assert_eq!(Arc::strong_count(&messages), 2);
    bridge.process_observe_event(&event);
    assert_eq!(Arc::strong_count(&messages), 3);
    let end = tracer.lock().on_turn_end(TurnTelemetryOutcome::Completed);
    end.await.unwrap();
    assert_eq!(Arc::strong_count(&messages), 2);
    let events = session.events_snapshot();
    let bodies: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            langfuse_client::IngestionEvent::GenerationCreate { body, .. } => Some(body),
            _ => None,
        })
        .collect();
    assert_eq!(bodies.len(), 1);
    assert_eq!(bodies[0].input.as_ref(), Some(&expected));
}

#[tokio::test]
async fn observe_raw_request_supersedes_snapshot_for_completed_generation() {
    let session = FakeLangfuseSession::new("snapshot-session");
    let tracer = Arc::new(Mutex::new(LangfuseTracer::new(
        session.clone(),
        "snapshot-session".to_string(),
        LangfuseConfig::default(),
    )));
    let agent_id = AgentId::new();
    let bridge = LangfuseBridge::new(
        tracer.clone(),
        "provider".to_string(),
        Some(agent_id.to_string()),
    );
    let turn_id = TurnId::new();
    let messages = Arc::new(vec![BaseMessage::human("fallback")]);
    let start = ObserveEvent::LlmCallStart {
        turn_id,
        agent_id,
        step: 0,
        messages: Arc::clone(&messages),
        tools: vec![ToolDefinition {
            name: "fallback-tool".to_string(),
            description: "fallback description".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }],
    };
    let raw = serde_json::json!({"messages": [{"content": "native🙂".repeat(4096)}], "tools": [{"name": "native-tool"}], "provider_extra": {"enabled": true}});
    tracer.lock().on_turn_start("input");
    bridge.process_observe_event(&start);
    assert_eq!(Arc::strong_count(&messages), 3);
    bridge.process_observe_event(&ObserveEvent::LlmRequestPayload {
        turn_id,
        agent_id,
        step: 0,
        body: Arc::new(raw.clone()),
    });
    assert_eq!(Arc::strong_count(&messages), 2);
    bridge.process_observe_event(&ObserveEvent::LlmCallEnd {
        turn_id,
        agent_id,
        step: 0,
        model: "provider-model".to_string(),
        output: "answer".to_string(),
        input_tokens: 0,
        output_tokens: 0,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
        request_id: None,
    });
    let end = tracer.lock().on_turn_end(TurnTelemetryOutcome::Completed);
    end.await.unwrap();
    let events = session.events_snapshot();
    let bodies: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            langfuse_client::IngestionEvent::GenerationCreate { body, .. } => Some(body),
            _ => None,
        })
        .collect();
    assert_eq!(bodies.len(), 1);
    assert_eq!(bodies[0].input.as_ref(), Some(&raw));
}
