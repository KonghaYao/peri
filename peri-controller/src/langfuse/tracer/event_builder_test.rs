use std::future::Future;
use std::pin::Pin;

use langfuse_client::types::TraceBody;
use langfuse_client::{IngestionEvent, LangfuseError};

use super::*;
use crate::langfuse::drop_telemetry::{
    LangfuseDropReason, LangfuseDropRegistry, LangfuseEventKind,
};

struct FailingSession {
    drops: LangfuseDropRegistry,
    turn_traces: crate::langfuse::TurnTraceRegistry,
    failure: FailureMode,
}

enum FailureMode {
    QueueFull,
    Closed,
    Unknown,
}

impl LangfuseSessionLike for FailingSession {
    fn try_add(&self, _event: IngestionEvent) -> Result<(), LangfuseError> {
        Err(match self.failure {
            FailureMode::QueueFull => LangfuseError::QueueFull,
            FailureMode::Closed => LangfuseError::ChannelClosed,
            FailureMode::Unknown => {
                LangfuseError::IngestionApi("private-server-payload".to_string())
            }
        })
    }

    fn flush(&self) -> Pin<Box<dyn Future<Output = Result<(), LangfuseError>> + Send + '_>> {
        Box::pin(async { Ok(()) })
    }

    fn session_id(&self) -> &str {
        "session-test"
    }

    fn drop_registry(&self) -> &LangfuseDropRegistry {
        &self.drops
    }

    fn turn_traces(&self) -> &crate::langfuse::TurnTraceRegistry {
        &self.turn_traces
    }
}

#[test]
fn test_try_add_failure_records_safe_trace_drop_snapshot() {
    let session = FailingSession {
        drops: LangfuseDropRegistry::new(1),
        turn_traces: crate::langfuse::TurnTraceRegistry::default(),
        failure: FailureMode::QueueFull,
    };
    let event = IngestionEvent::TraceCreate {
        id: "event-id".to_string(),
        timestamp: "2026-01-01T00:00:00Z".to_string(),
        body: TraceBody {
            id: Some("trace-id".to_string()),
            ..Default::default()
        },
        metadata: None,
    };

    try_add_or_warn_via_session(&session, event, "trace-id", "must not be logged");

    let snapshot = session
        .drop_registry()
        .snapshot("trace-id")
        .expect("queue full 应记录 trace 丢弃快照");
    assert_eq!(snapshot.total, 1);
    assert_eq!(
        snapshot.by_event_kind.get(&LangfuseEventKind::Trace),
        Some(&1)
    );
    assert_eq!(
        snapshot
            .by_reason
            .get(&LangfuseDropReason::DropNewQueueFull),
        Some(&1)
    );
}

#[derive(Clone, Default)]
struct EventRecorder {
    events: std::sync::Arc<parking_lot::Mutex<Vec<String>>>,
}

impl tracing::Subscriber for EventRecorder {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _attributes: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = CollectedFields(String::new());
        event.record(&mut fields);
        self.events.lock().push(fields.0);
    }

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}
}

struct CollectedFields(String);

impl tracing::field::Visit for CollectedFields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write;
        write!(&mut self.0, "{}={:?};", field.name(), value).unwrap();
    }
}

fn trace_event() -> IngestionEvent {
    IngestionEvent::TraceCreate {
        id: "event-id".to_string(),
        timestamp: "2026-09-30T00:00:00Z".to_string(),
        body: TraceBody {
            id: Some("trace-id".to_string()),
            input: Some(serde_json::json!("private-input")),
            ..Default::default()
        },
        metadata: None,
    }
}

#[test]
fn congestion_records_every_drop_without_per_event_logs() {
    for (failure, reason) in [
        (FailureMode::QueueFull, LangfuseDropReason::DropNewQueueFull),
        (FailureMode::Closed, LangfuseDropReason::BatcherClosed),
    ] {
        let session = FailingSession {
            drops: LangfuseDropRegistry::new(1),
            turn_traces: crate::langfuse::TurnTraceRegistry::default(),
            failure,
        };
        let recorder = EventRecorder::default();
        tracing::subscriber::with_default(recorder.clone(), || {
            for _ in 0..128 {
                try_add_or_warn_via_session(&session, trace_event(), "trace-id", "private-context");
            }
        });
        let snapshot = session.drop_registry().snapshot("trace-id").unwrap();
        assert_eq!(snapshot.total, 128);
        assert_eq!(snapshot.by_reason.get(&reason), Some(&128));
        assert_eq!(
            snapshot.by_event_kind.get(&LangfuseEventKind::Trace),
            Some(&128)
        );
        assert!(recorder.events.lock().is_empty());
    }
}

#[test]
fn unknown_admission_error_is_diagnosed_without_error_or_event_payload() {
    let session = FailingSession {
        drops: LangfuseDropRegistry::new(1),
        turn_traces: crate::langfuse::TurnTraceRegistry::default(),
        failure: FailureMode::Unknown,
    };
    let recorder = EventRecorder::default();
    tracing::subscriber::with_default(recorder.clone(), || {
        try_add_or_warn_via_session(&session, trace_event(), "trace-id", "private-context");
    });
    let events = recorder.events.lock();
    assert_eq!(events.len(), 1);
    assert!(events[0].contains("not queued"));
    assert!(!events[0].contains("private"));
    assert!(session.drop_registry().snapshot("trace-id").is_none());
}
