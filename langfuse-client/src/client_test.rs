//! Tests for client_lf

use super::*;
use crate::config::BackpressurePolicy;
use crate::types::TraceBody;

fn create_test_client(server_url: &str, max_retries: usize) -> LangfuseClient {
    LangfuseClient::new("pk", "sk", server_url, max_retries)
}

fn create_test_event(id: &str) -> IngestionEvent {
    IngestionEvent::TraceCreate {
        id: id.to_string(),
        timestamp: "2026-01-01T00:00:00Z".to_string(),
        body: TraceBody {
            id: Some(format!("trace-{}", id)),
            name: Some("test".into()),
            ..Default::default()
        },
        metadata: None,
    }
}

#[test]
fn test_new_creates_client_with_correct_auth() {
    let client = create_test_client("http://localhost", 3);
    assert_eq!(client.auth_header, "Basic cGs6c2s=");
    assert_eq!(client.base_url, "http://localhost");
    assert_eq!(client.max_retries, 3);
}

#[test]
fn test_new_trims_trailing_slash() {
    let client = create_test_client("http://localhost/", 0);
    assert_eq!(client.base_url, "http://localhost");
}

#[tokio::test]
async fn test_ingest_success_200() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body("{}")
        .match_header("Authorization", "Basic cGs6c2s=")
        .match_header("x-langfuse-ingestion-version", "4")
        .match_header("Content-Type", "application/json")
        .create_async()
        .await;

    let client = create_test_client(&server.url(), 0);
    let result = client.ingest(vec![create_test_event("evt-1")]).await;
    assert!(result.is_ok());
    mock.assert_async().await;
}

#[tokio::test]
async fn test_ingest_empty_batch() {
    let client = create_test_client("http://unused", 0);
    let result = client.ingest(vec![]).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_ingest_4xx_no_retry() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(400)
        .with_body(r#"{"error":"bad request"}"#)
        .expect(1)
        .create_async()
        .await;

    let client = create_test_client(&server.url(), 3);
    let result = client.ingest(vec![create_test_event("1")]).await;
    assert!(result.is_err());
    match result.unwrap_err() {
        LangfuseError::IngestionApi(msg) => {
            assert!(msg.contains("OTLP"));
            assert!(msg.contains("HTTP 400"));
        }
        other => panic!("Expected IngestionApi, got: {:?}", other),
    }
    mock.assert_async().await;
}

#[tokio::test]
async fn test_ingest_5xx_retries_then_success() {
    let mut server = mockito::Server::new_async().await;
    let mock_fail = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(500)
        .with_body("internal error")
        .expect(1)
        .create_async()
        .await;
    let mock_success = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(200)
        .with_body("{}")
        .expect(1)
        .create_async()
        .await;

    let client = create_test_client(&server.url(), 3);
    let result = client.ingest(vec![create_test_event("1")]).await;
    assert!(result.is_ok());
    mock_fail.assert_async().await;
    mock_success.assert_async().await;
}

#[tokio::test]
async fn test_ingest_5xx_retries_exhausted() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(500)
        .with_body("internal error")
        .expect(3) // 1 initial + 2 retries
        .create_async()
        .await;

    let client = create_test_client(&server.url(), 2);
    let result = client.ingest(vec![create_test_event("1")]).await;
    assert!(result.is_err());
    match result.unwrap_err() {
        LangfuseError::IngestionApi(msg) => {
            assert!(msg.contains("after 2 retries"));
        }
        other => panic!("Expected IngestionApi, got: {:?}", other),
    }
    mock.assert_async().await;
}

#[tokio::test]
async fn test_ingest_network_error_retries() {
    // 注意：环境中存在 HTTP 代理时，连接失败会由代理返回 502 而非 reqwest 层网络错误，
    // 因此结果可能是 Http（直连网络错误）或 IngestionApi（代理返回 5xx 后重试耗尽）。
    let client = LangfuseClient::new("pk", "sk", "http://127.0.0.1:1", 1);
    let result = client.ingest(vec![create_test_event("1")]).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_ingest_payload_has_otel_format() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(200)
        .with_body("{}")
        .match_body(mockito::Matcher::Regex(
            "\"resourceSpans\".*\"scopeSpans\".*\"spans\"".to_string(),
        ))
        .create_async()
        .await;

    let client = create_test_client(&server.url(), 0);
    let result = client.ingest(vec![create_test_event("1")]).await;
    assert!(result.is_ok());
    mock.assert_async().await;
}

#[test]
fn test_from_config() {
    let config = crate::config::ClientConfig {
        public_key: "pk".into(),
        secret_key: "sk".into(),
        base_url: "https://cloud.langfuse.com".into(),
        trace_sampling: 1.0,
        error_span_always: true,
        batch_max_events: 50,
        batch_flush_interval_secs: 10,
        batch_backpressure: BackpressurePolicy::DropNew,
    };
    let client = LangfuseClient::from_config(&config, 2);
    assert_eq!(client.auth_header, "Basic cGs6c2s=");
    assert_eq!(client.max_retries, 2);
}
