use super::{translate_observation, RetryEventForwarder};
use peri_acp_types::event::{AgentEventHandler, ExecutorEvent};
use peri_model::{ModelError, RetryObservation, RetryObserver};
use std::{sync::Arc, time::Duration};

#[derive(Default)]
struct EventCollector(parking_lot::Mutex<Vec<ExecutorEvent>>);

impl AgentEventHandler for EventCollector {
    fn on_event(&self, event: ExecutorEvent) {
        self.0.lock().push(event);
    }
}

#[derive(Clone, Default)]
struct LogBuffer(Arc<parking_lot::Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn observation() -> RetryObservation {
    let error = ModelError::http_status(429, "provider.example", Some("request-retry"))
        .with_message("Authorization: Bearer synthetic")
        .with_body("token=synthetic-body")
        .with_causes(vec!["upstream throttle".to_owned()]);
    RetryObservation::from_model_error(2, 4, Duration::from_millis(10), &error).unwrap()
}

#[test]
fn retry_wire_keeps_message_body_and_ordered_causes() {
    let collector = Arc::new(EventCollector::default());
    let handler: Arc<dyn AgentEventHandler> = collector.clone();
    translate_observation(&observation(), &handler);
    let events = collector.0.lock();
    let ExecutorEvent::LlmRetrying {
        error, diagnostic, ..
    } = &events[0]
    else {
        panic!("expected retry event");
    };
    assert!(error.contains("Bearer synthetic"));
    assert!(error.contains("token=synthetic-body"));
    let diagnostic = diagnostic.as_ref().unwrap();
    let restored: peri_acp_types::error::SafeModelErrorDiagnostic =
        serde_json::from_value(serde_json::to_value(diagnostic).unwrap()).unwrap();
    assert_eq!(&restored, diagnostic);
    assert_eq!(restored.causes(), &["upstream throttle".to_owned()]);
}

#[test]
fn retry_and_interruption_warn_without_handler_keep_execution_identity() {
    let logs = LogBuffer::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .without_time()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let turn = Arc::new(crate::session::TurnContext::new(
        Arc::from("/tmp/retry-diagnostic"),
        Arc::new(tokio_util::sync::CancellationToken::new()),
    ));
    let forwarder = RetryEventForwarder::new();
    forwarder.set_context("session-retry", &turn);
    tracing::subscriber::with_default(subscriber, || {
        forwarder.on_retry(observation());
        assert!(forwarder.on_interrupted(observation()));
    });
    let text = String::from_utf8(logs.0.lock().clone()).unwrap();
    assert_eq!(text.lines().count(), 2);
    for line in text.lines() {
        for fact in [
            "WARN",
            "session-retry",
            "Bearer synthetic",
            "token=synthetic-body",
            "upstream throttle",
            "request-retry",
        ] {
            assert!(line.contains(fact), "missing {fact}: {line}");
        }
        assert!(line.contains(&turn.turn_id().to_string()));
    }
}
