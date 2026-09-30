use std::sync::{Arc, LazyLock};

use langfuse_client::types::ObservationLevel;
use langfuse_client::{IngestionEvent, LangfuseError};
use peri_agent::metrics::{MetricEvent, MetricsSink};

use crate::langfuse::drop_telemetry::{
    LangfuseDropReason, LangfuseDropRegistry, LangfuseEventKind,
};
use crate::langfuse::session_like::LangfuseSessionLike;
use crate::langfuse::{FakeLangfuseSession, LangfuseMetricsSink};

/// 与宿主装配同样的形状：`Arc<LangfuseSession>` → `Arc<dyn MetricsSink>`。
fn sink_with(session: &Arc<FakeLangfuseSession>) -> Arc<dyn MetricsSink> {
    let session: Arc<dyn LangfuseSessionLike> = session.clone();
    Arc::new(LangfuseMetricsSink::new(session))
}

fn metric(
    event: &str,
    data: serde_json::Value,
    sid: Option<&str>,
    rid: Option<&str>,
) -> MetricEvent {
    MetricEvent {
        ts: "2026-09-30T10:00:00.000Z".to_string(),
        sid: sid.map(str::to_owned),
        rid: rid.map(str::to_owned),
        event: event.to_string(),
        data,
    }
}

#[test]
fn error_metric_is_reported_as_langfuse_event() {
    let session = FakeLangfuseSession::new("sess_metrics");
    let sink = sink_with(&session);

    sink.record(metric(
        "tool.error",
        serde_json::json!({"name": "Bash", "error": "exit 1"}),
        Some("sid-1"),
        Some("rid-1"),
    ));

    let events = session.events_snapshot();
    assert_eq!(events.len(), 1);
    let IngestionEvent::EventCreate {
        body, timestamp, ..
    } = &events[0]
    else {
        panic!(
            "metrics must be reported as EventCreate, got {:?}",
            events[0]
        );
    };
    assert_eq!(body.name.as_deref(), Some("tool.error"));
    assert_eq!(body.input.as_ref().unwrap()["error"], "exit 1");
    assert_eq!(body.metadata.as_ref().unwrap()["event_type"], "metric");
    assert_eq!(body.metadata.as_ref().unwrap()["sid"], "sid-1");
    assert_eq!(body.metadata.as_ref().unwrap()["rid"], "rid-1");
    assert_eq!(body.level, Some(ObservationLevel::Error));
    assert!(body.parent_observation_id.is_none());
    assert_eq!(body.start_time.as_deref(), Some("2026-09-30T10:00:00.000Z"));
    assert!(!timestamp.is_empty());
}

#[test]
fn metric_event_wire_type_is_event_create() {
    let session = FakeLangfuseSession::new("sess_metrics_wire");
    let sink = sink_with(&session);

    sink.record(metric(
        "mcp.error",
        serde_json::json!({"server": "x"}),
        None,
        None,
    ));

    let events = session.events_snapshot();
    let wire = serde_json::to_value(&events[0]).unwrap();
    assert_eq!(wire["type"], "event-create");
    assert!(wire["body"]["traceId"]
        .as_str()
        .is_some_and(|id| !id.is_empty()));
}

#[test]
fn non_error_metric_keeps_default_level() {
    let session = FakeLangfuseSession::new("sess_metrics_level");
    let sink = sink_with(&session);

    sink.record(metric("mcp.warn", serde_json::json!({}), None, None));

    let events = session.events_snapshot();
    let IngestionEvent::EventCreate { body, .. } = &events[0] else {
        panic!("metrics must be reported as EventCreate");
    };
    assert_eq!(body.level, Some(ObservationLevel::Default));
    assert!(body.metadata.as_ref().unwrap()["sid"].is_null());
}

/// 入队被背压拒绝的出口：指标不 panic、不丢证据（丢弃记入 drop registry）。
struct QueueFullSession {
    drop_registry: LangfuseDropRegistry,
    last_trace_id: LazyLock<parking_lot::Mutex<Option<String>>>,
}

impl LangfuseSessionLike for QueueFullSession {
    fn try_add(&self, event: IngestionEvent) -> Result<(), LangfuseError> {
        if let IngestionEvent::EventCreate { body, .. } = event {
            *self.last_trace_id.lock() = body.trace_id;
        }
        Err(LangfuseError::QueueFull)
    }

    fn flush(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), LangfuseError>> + Send + '_>>
    {
        Box::pin(async { Ok(()) })
    }

    fn session_id(&self) -> &str {
        "sess_queue_full"
    }

    fn drop_registry(&self) -> &LangfuseDropRegistry {
        &self.drop_registry
    }
}

#[test]
fn rejected_metric_is_recorded_in_drop_registry() {
    let session = Arc::new(QueueFullSession {
        drop_registry: LangfuseDropRegistry::default(),
        last_trace_id: LazyLock::new(|| parking_lot::Mutex::new(None)),
    });
    let sink = LangfuseMetricsSink::new(Arc::clone(&session) as Arc<dyn LangfuseSessionLike>);

    sink.record(metric("tool.error", serde_json::json!({}), None, None));

    let trace_id = session
        .last_trace_id
        .lock()
        .clone()
        .expect("event must reach the session before rejection");
    let snapshot = session
        .drop_registry()
        .snapshot(&trace_id)
        .expect("rejected metric must be visible in drop registry");
    assert_eq!(snapshot.total, 1);
    assert_eq!(
        snapshot.by_event_kind.get(&LangfuseEventKind::Event),
        Some(&1)
    );
    assert_eq!(
        snapshot
            .by_reason
            .get(&LangfuseDropReason::DropNewQueueFull),
        Some(&1)
    );
}
