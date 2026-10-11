use super::*;
use crate::{
    client::LangfuseClient,
    types::{IngestionEvent, TraceBody},
};
use std::cell::Cell;

#[test]
fn default_budgets_are_explicit() {
    let config = ExportConfig::default();
    assert_eq!(config.max_request_bytes, 4 * 1024 * 1024);
    assert_eq!(config.max_response_bytes, 64 * 1024);
    assert_eq!(config.retry_budget, Duration::from_secs(60));
}

#[test]
fn invalid_export_configurations_are_rejected() {
    let invalid = [
        ExportConfig {
            max_request_bytes: 0,
            ..Default::default()
        },
        ExportConfig {
            max_response_bytes: 0,
            ..Default::default()
        },
        ExportConfig {
            retry_budget: Duration::ZERO,
            ..Default::default()
        },
        ExportConfig {
            retry_budget: Duration::MAX,
            ..Default::default()
        },
        ExportConfig {
            initial_retry_delay: Duration::ZERO,
            ..Default::default()
        },
        ExportConfig {
            max_retry_delay: Duration::from_millis(10),
            ..Default::default()
        },
    ];
    for config in invalid {
        assert!(matches!(
            LangfuseClient::new("pk", "sk", "http://unused", 1).with_export_config(config),
            Err(LangfuseError::Config(_))
        ));
    }
}

#[test]
fn writer_stops_at_limit_and_never_grows_capacity_past_it() {
    let mut writer = LimitedWriter {
        bytes: Vec::new(),
        limit: 5,
        exceeded: false,
    };
    writer.write_all(b"123").unwrap();
    writer.write_all(b"45").unwrap();
    assert!(writer.write_all(b"6").is_err());
    assert_eq!(writer.bytes, b"12345");
    assert!(writer.exceeded);
    assert!(writer.bytes.capacity() <= 5);
    assert!(matches!(
        encode(&"1234", 5),
        Err(LangfuseError::PayloadTooLarge { limit_bytes: 5 })
    ));
    assert_eq!(encode(&"123", 5).unwrap(), b"\"123\"");
}

struct Counted<'counter>(&'counter Cell<usize>);

impl Serialize for Counted<'_> {
    fn serialize<Serializer: serde::Serializer>(
        &self,
        serializer: Serializer,
    ) -> Result<Serializer::Ok, Serializer::Error> {
        self.0.set(self.0.get() + 1);
        serializer.serialize_str("body")
    }
}

#[test]
fn request_clones_share_the_once_serialized_body() {
    let counter = Cell::new(0);
    let body = encode(&Counted(&counter), 32).unwrap();
    let request = reqwest::Client::new()
        .post("http://unused")
        .body(body)
        .build()
        .unwrap();
    for _ in 0..3 {
        let cloned = request.try_clone().unwrap();
        assert_eq!(
            cloned.body().unwrap().as_bytes().unwrap().as_ptr(),
            request.body().unwrap().as_bytes().unwrap().as_ptr()
        );
    }
    assert_eq!(counter.get(), 1);
}

struct InvalidPayload;

impl Serialize for InvalidPayload {
    fn serialize<Serializer: serde::Serializer>(
        &self,
        _: Serializer,
    ) -> Result<Serializer::Ok, Serializer::Error> {
        Err(serde::ser::Error::custom("secret-payload"))
    }
}

#[test]
fn serialization_error_does_not_expose_payload() {
    let error = encode(&InvalidPayload, 32).unwrap_err();
    assert!(error.to_string().contains("serialization failed"));
    assert!(!error.to_string().contains("secret-payload"));
    let error = preflight(&InvalidPayload, 32).unwrap_err();
    assert!(error.to_string().contains("preflight serialization failed"));
    assert!(!error.to_string().contains("secret-payload"));
}

#[test]
fn counting_writer_is_exact_bounded_and_overflow_safe() {
    let mut writer = CountingWriter {
        count: 0,
        limit: 5,
        exceeded: false,
    };
    writer.write_all(b"12345").unwrap();
    assert_eq!(writer.count, 5);
    assert!(writer.write_all(b"6").is_err());
    assert_eq!(writer.count, 5);
    assert!(writer.exceeded);
    assert!(preflight(&"123", 5).is_ok());
    assert!(matches!(
        preflight(&"1234", 5),
        Err(LangfuseError::PayloadTooLarge { limit_bytes: 5 })
    ));
    let mut writer = CountingWriter {
        count: usize::MAX - 1,
        limit: usize::MAX,
        exceeded: false,
    };
    assert!(writer.write_all(b"12").is_err());
    assert_eq!(writer.count, usize::MAX - 1);
    writer.write_all(b"1").unwrap();
    assert_eq!(writer.count, usize::MAX);
    assert!(writer.write_all(b"2").is_err());
}

#[test]
fn preflight_counts_json_escaping_and_total_array_envelope() {
    let payload = vec!["\"\\\n", "文本"];
    let expected = serde_json::to_vec(&payload).unwrap().len();
    preflight(&payload, expected).unwrap();
    assert!(
        matches!(preflight(&payload, expected - 1), Err(LangfuseError::PayloadTooLarge { limit_bytes }) if limit_bytes == expected - 1)
    );
}

#[tokio::test]
async fn direct_ingest_oversized_input_is_rejected_before_conversion_or_http() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/api/public/otel/v1/traces")
        .expect(0)
        .create_async()
        .await;
    let limit = ExportConfig::default().max_request_bytes;
    for root in [Some("root".into()), None] {
        let event = IngestionEvent::TraceCreate {
            id: "event".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            body: TraceBody {
                id: root,
                input: Some(serde_json::Value::String("x".repeat(limit + 1))),
                ..Default::default()
            },
            metadata: None,
        };
        let error = LangfuseClient::new("pk", "sk", &server.url(), 3)
            .ingest(vec![event])
            .await
            .unwrap_err();
        assert!(
            matches!(error, LangfuseError::PayloadTooLarge { limit_bytes } if limit_bytes == limit)
        );
    }
    mock.assert_async().await;
}

#[tokio::test]
async fn worker_and_client_request_limits_use_the_minimum_before_sending() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/api/public/otel/v1/traces")
        .expect(0)
        .create_async()
        .await;
    for (client_limit, worker_limit, expected) in [(1, 1024, 1), (1024, 1, 1), (1024, 0, 0)] {
        let client = LangfuseClient::new("pk", "sk", &server.url(), 3)
            .with_export_config(ExportConfig {
                max_request_bytes: client_limit,
                ..Default::default()
            })
            .unwrap();
        let event = IngestionEvent::TraceCreate {
            id: "event".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            body: TraceBody {
                id: Some("root".into()),
                ..Default::default()
            },
            metadata: None,
        };
        assert!(
            matches!(client.ingest_with_limit(vec![event], worker_limit).await, Err(LangfuseError::PayloadTooLarge { limit_bytes }) if limit_bytes == expected)
        );
    }
    mock.assert_async().await;
}

#[tokio::test]
async fn encoded_request_exactly_at_limit_is_accepted() {
    let event = IngestionEvent::TraceCreate {
        id: "event".into(),
        timestamp: "2026-01-01T00:00:00Z".into(),
        body: TraceBody {
            id: Some("root".into()),
            ..Default::default()
        },
        metadata: None,
    };
    let payload = crate::types::ingestion_events_to_otel(std::slice::from_ref(&event)).unwrap();
    let encoded = encode(&payload, ExportConfig::default().max_request_bytes).unwrap();
    let limit = encoded.len();
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(200)
        .match_body(encoded)
        .with_body("{}")
        .expect(1)
        .create_async()
        .await;
    let client = LangfuseClient::new("pk", "sk", &server.url(), 0)
        .with_export_config(ExportConfig {
            max_request_bytes: limit,
            ..Default::default()
        })
        .unwrap();
    client
        .ingest_with_limit(vec![event], usize::MAX)
        .await
        .unwrap();
    mock.assert_async().await;
}
