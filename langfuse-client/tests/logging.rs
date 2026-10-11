use std::sync::{Arc, Mutex};
use std::time::Duration;

use tracing::{field::Visit, span, Event, Metadata, Subscriber};

use langfuse_client::{
    client::ExportConfig, types::TraceBody, Batcher, BatcherConfig, IngestionEvent, LangfuseClient,
};

#[derive(Clone, Default)]
struct LogCapture(Arc<Mutex<String>>);

impl Visit for LogCapture {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write;
        write!(self.0.lock().unwrap(), " {}={value:?}", field.name()).unwrap();
    }
}

impl Subscriber for LogCapture {
    fn register_callsite(
        &self,
        _metadata: &'static Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::sometimes()
    }

    fn max_level_hint(&self) -> Option<tracing::metadata::LevelFilter> {
        Some(tracing::metadata::LevelFilter::INFO)
    }

    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        *metadata.level() <= tracing::Level::INFO
    }

    fn new_span(&self, _attributes: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }

    fn record(&self, _span: &span::Id, _values: &span::Record<'_>) {}

    fn record_follows_from(&self, _span: &span::Id, _follows: &span::Id) {}

    fn event(&self, event: &Event<'_>) {
        event.record(&mut self.clone());
    }

    fn enter(&self, _span: &span::Id) {}

    fn exit(&self, _span: &span::Id) {}
}

#[tokio::test]
async fn retry_and_batch_failure_logs_include_reason_without_secrets() {
    let mut server = mockito::Server::new_async().await;
    let failed = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(503)
        .with_body("private-response-body")
        .expect(2)
        .create_async()
        .await;
    let capture = LogCapture::default();
    let _subscriber = tracing::subscriber::set_default(capture.clone());
    let client = LangfuseClient::new("private-public-key", "private-secret-key", &server.url(), 1)
        .with_export_config(ExportConfig {
            initial_retry_delay: Duration::from_millis(5),
            max_retry_delay: Duration::from_millis(5),
            ..Default::default()
        })
        .unwrap();
    let batcher = Batcher::new(
        client,
        BatcherConfig {
            flush_interval: Duration::from_secs(60),
            ..Default::default()
        },
    );
    batcher
        .add(IngestionEvent::TraceCreate {
            id: "event".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            body: TraceBody {
                id: Some("root".into()),
                name: Some("private-event-payload".into()),
                ..Default::default()
            },
            metadata: None,
        })
        .await
        .unwrap();
    assert!(batcher.flush().await.is_err());
    assert!(batcher.shutdown().await.is_err());
    failed.assert_async().await;
    let logs = capture.0.lock().unwrap();
    for expected in [
        "OTLP export transient failure; retrying",
        "Batcher ingestion submission failed",
        "error=Ingestion API returned errors: OTLP ingestion HTTP 503 after 0 retries",
        "error=Ingestion API returned errors: OTLP ingestion HTTP 503 after 1 retries",
    ] {
        assert!(logs.contains(expected), "missing {expected}: {logs}");
    }
    for private in [
        "private-public-key",
        "private-secret-key",
        "private-response-body",
        "private-event-payload",
    ] {
        assert!(!logs.contains(private), "leaked {private}: {logs}");
    }
}
