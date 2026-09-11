use super::*;
use peri_acp_types::messages::MessageId;
use serde_json::json;

fn params() -> AgentRunParams {
    serde_json::from_value(json!({"runId": "run", "agentId": 7, "prompt": "test"})).unwrap()
}

fn llm_end(model: &str, output_tokens: Option<u32>) -> ExecutorEvent {
    ExecutorEvent::LlmCallEnd {
        step: 1,
        model: model.into(),
        output: String::new(),
        usage: output_tokens.map(|tokens| peri_model::TokenUsage::new(10, tokens)),
        stop_reason: None,
        request_id: None,
        source_agent_id: None,
    }
}

#[test]
fn model_progress_leaves_counts_unspecified_and_preserves_collected_usage() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let observation = WorkflowObservation::new(&params(), Some(tx), None);
    observation
        .handler()
        .on_event(llm_end("provider", Some(12)));
    rx.try_recv().unwrap();
    observation.report_model("effective", Some("haiku".into()));
    let ProgressEvent::AgentProgress {
        run_id,
        agent_id,
        model,
        model_tier,
        token_count,
        tool_count,
        ..
    } = rx.try_recv().unwrap()
    else {
        panic!("expected agent progress")
    };
    assert_eq!((run_id.as_str(), agent_id), ("run", 7));
    assert_eq!(model.as_deref(), Some("effective"));
    assert_eq!(model_tier.as_deref(), Some("haiku"));
    assert_eq!((token_count, tool_count), (None, None));
    assert_eq!(observation.snapshot().output_tokens, 12);
    assert_eq!(
        observation.snapshot().last_model.as_deref(),
        Some("provider")
    );
}

#[test]
fn handler_projects_cumulative_progress_before_langfuse_callback() {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let rx = Arc::new(Mutex::new(rx));
    let received = Arc::new(Mutex::new(Vec::new()));
    let captured = received.clone();
    let langfuse: WorkflowLangfuseEventHandler = Arc::new(move |_| {
        // The injected external observer must see the already-enqueued progress.
        let ProgressEvent::AgentProgress {
            token_count,
            tool_count,
            ..
        } = rx.lock().try_recv().expect("progress precedes Langfuse")
        else {
            panic!("expected agent progress")
        };
        captured.lock().push((token_count, tool_count));
    });
    let observation = WorkflowObservation::new(&params(), Some(tx), Some(langfuse));
    let handler = observation.handler();
    handler.on_event(ExecutorEvent::ToolStart {
        message_id: MessageId::new(),
        tool_call_id: "call".into(),
        name: "Read".into(),
        input: json!({}),
        source_agent_id: None,
    });
    handler.on_event(llm_end("first", Some(12)));
    handler.on_event(llm_end("second", Some(5)));
    handler.on_event(llm_end("last-without-usage", None));
    assert_eq!(
        *received.lock(),
        vec![
            (Some(0), Some(1)),
            (Some(12), Some(1)),
            (Some(17), Some(1)),
            (Some(17), Some(1))
        ]
    );
    let stats = observation.snapshot();
    assert_eq!((stats.output_tokens, stats.tool_count), (17, 1));
    assert_eq!(stats.last_model.as_deref(), Some("last-without-usage"));
}

#[tokio::test]
async fn workflow_forwarder_close_collects_buffered_usage_before_result_snapshot() {
    use crate::agent::events_v2::{
        observe_event_to_executor, EventBus, EventBusConfig, ObserveEvent,
    };
    use peri_acp_types::{identity::AgentId, session::TurnId};
    let observation = WorkflowObservation::new(&params(), None, None);
    let handler = observation.handler();
    let (bus, mut handles) = EventBus::new(EventBusConfig::default());
    bus.emit_observe(ObserveEvent::LlmCallEnd {
        turn_id: TurnId::new(),
        agent_id: AgentId::new(),
        step: 0,
        model: "buffered-model".into(),
        output: "done".into(),
        input_tokens: 10,
        output_tokens: 23,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
        request_id: None,
    });
    let forwarder = tokio::spawn(async move {
        while let Ok(event) = handles.observe_rx.recv().await {
            if let Some(event) = observe_event_to_executor(event) {
                handler.on_event(event);
            }
        }
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        super::super::await_workflow_forwarder(Arc::new(bus), forwarder),
    )
    .await
    .expect("close must converge")
    .expect("forwarder must complete");
    let stats = observation.snapshot();
    assert_eq!(stats.output_tokens, 23);
    assert_eq!(stats.last_model.as_deref(), Some("buffered-model"));
}
