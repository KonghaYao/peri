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
}

impl LangfuseSessionLike for FailingSession {
    fn try_add(&self, _event: IngestionEvent) -> Result<(), LangfuseError> {
        Err(LangfuseError::QueueFull)
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
}

#[test]
fn test_try_add_failure_records_safe_trace_drop_snapshot() {
    let session = FailingSession {
        drops: LangfuseDropRegistry::new(1),
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
