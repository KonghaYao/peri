use super::*;
use crate::types::TraceBody;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn event() -> IngestionEvent {
    IngestionEvent::TraceCreate {
        id: "event".into(),
        timestamp: "2026-01-01T00:00:00Z".into(),
        body: TraceBody {
            id: Some("root".into()),
            ..Default::default()
        },
        metadata: None,
    }
}

fn client(url: &str, retries: usize) -> LangfuseClient {
    LangfuseClient::new("pk", "sk", url, retries)
        .with_export_config(ExportConfig {
            initial_retry_delay: Duration::from_millis(5),
            max_retry_delay: Duration::from_millis(20),
            ..Default::default()
        })
        .unwrap()
}

#[tokio::test]
async fn transient_statuses_retry_then_succeed() {
    for status in [429, 502, 503, 504] {
        let mut server = mockito::Server::new_async().await;
        let failed = server
            .mock("POST", "/api/public/otel/v1/traces")
            .with_status(status)
            .with_header("Retry-After", "0")
            .with_body("secret-payload")
            .expect(1)
            .create_async()
            .await;
        let success = server
            .mock("POST", "/api/public/otel/v1/traces")
            .with_status(200)
            .with_body("{}")
            .expect(1)
            .create_async()
            .await;
        client(&server.url(), 1)
            .ingest(vec![event()])
            .await
            .unwrap();
        failed.assert_async().await;
        success.assert_async().await;
    }
}

#[tokio::test]
async fn retry_after_seconds_enforces_a_minimum_wait() {
    let mut server = mockito::Server::new_async().await;
    let failed = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(429)
        .with_header("Retry-After", "1")
        .expect(1)
        .create_async()
        .await;
    let success = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(200)
        .with_body("{}")
        .expect(1)
        .create_async()
        .await;
    let started = peri_time::monotonic_now();
    client(&server.url(), 1)
        .ingest(vec![event()])
        .await
        .unwrap();
    assert!(started.elapsed() >= Duration::from_secs(1));
    failed.assert_async().await;
    success.assert_async().await;
}

#[tokio::test]
async fn permanent_http_errors_and_redirects_never_retry_or_expose_body() {
    for status in [301, 400, 401, 403, 404, 408, 500, 501, 505] {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/public/otel/v1/traces")
            .with_status(status)
            .with_header("Location", "/redirected")
            .with_body("secret-payload")
            .expect(1)
            .create_async()
            .await;
        let error = client(&server.url(), 10)
            .ingest(vec![event()])
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains(&format!("HTTP {}", status)));
        assert!(!error.contains("secret-payload"));
        mock.assert_async().await;
    }
}

#[test]
fn retry_after_supports_seconds_http_date_and_past_date() {
    let now = chrono::DateTime::parse_from_rfc3339("2026-09-30T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert_eq!(
        retry::parse_retry_after_at(" 7 ", now),
        Some(Duration::from_secs(7))
    );
    assert_eq!(
        retry::parse_retry_after_at("Wed, 30 Sep 2026 00:00:09 GMT", now),
        Some(Duration::from_secs(9))
    );
    assert_eq!(
        retry::parse_retry_after_at("Tue, 29 Sep 2026 00:00:00 GMT", now),
        Some(Duration::ZERO)
    );
    assert_eq!(retry::parse_retry_after_at("-1", now), None);
    assert_eq!(retry::parse_retry_after_at("invalid", now), None);
    assert_eq!(
        retry::parse_retry_after_at("18446744073709551616", now),
        Some(Duration::MAX)
    );
}

#[test]
fn jitter_is_capped_exponential_and_overflow_safe() {
    let config = ExportConfig::default();
    for (attempt, upper) in [(0, 1), (1, 2), (2, 4), (usize::MAX, 30)] {
        for jitter in [0, 250_000, 500_000, u64::MAX] {
            let actual = retry::jittered_backoff(&config, attempt, jitter);
            assert!(actual >= Duration::from_secs(upper) / 2);
            assert!(actual <= Duration::from_secs(upper));
        }
    }
    let extreme = ExportConfig {
        initial_retry_delay: Duration::MAX,
        max_retry_delay: Duration::MAX,
        ..config
    };
    assert_eq!(
        retry::jittered_backoff(&extreme, usize::MAX, 500_000),
        Duration::MAX
    );
    let minimal = ExportConfig {
        initial_retry_delay: Duration::from_nanos(1),
        max_retry_delay: Duration::from_nanos(1),
        ..Default::default()
    };
    assert_eq!(
        retry::jittered_backoff(&minimal, usize::MAX, 0),
        Duration::from_nanos(1)
    );
}

#[tokio::test]
async fn retry_after_exceeding_budget_does_not_retry() {
    for header in ["999999999999999999999999", "Wed, 30 Sep 2037 00:00:00 GMT"] {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/public/otel/v1/traces")
            .with_status(429)
            .with_header("Retry-After", header)
            .expect(1)
            .create_async()
            .await;
        let error = client(&server.url(), usize::MAX)
            .ingest(vec![event()])
            .await
            .unwrap_err();
        assert!(error.to_string().contains("time budget"));
        mock.assert_async().await;
    }
}

#[tokio::test]
async fn retry_wait_is_limited_by_cumulative_budget() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(503)
        .expect(1)
        .create_async()
        .await;
    let client = LangfuseClient::new("pk", "sk", &server.url(), usize::MAX)
        .with_export_config(ExportConfig {
            retry_budget: Duration::from_millis(100),
            ..Default::default()
        })
        .unwrap();
    let error = client.ingest(vec![event()]).await.unwrap_err();
    assert!(error.to_string().contains("time budget"));
    mock.assert_async().await;
}

async fn transport_server(
    responses: Vec<&'static [u8]>,
) -> (String, tokio::task::JoinHandle<Vec<Vec<u8>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut bodies = Vec::new();
        for response in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut buffer = [0; 4096];
                let count = socket.read(&mut buffer).await.unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&buffer[..count]);
                if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                    let length: usize = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .unwrap()
                        .parse()
                        .unwrap();
                    if request.len() >= end + 4 + length {
                        bodies.push(request[end + 4..end + 4 + length].to_vec());
                        break;
                    }
                }
            }
            socket.write_all(response).await.unwrap();
        }
        bodies
    });
    (url, task)
}

#[tokio::test]
async fn recoverable_transport_error_reuses_encoded_body_on_retry() {
    let (url, task) = transport_server(vec![
        b"",
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
    ])
    .await;
    tokio::time::timeout(
        Duration::from_secs(2),
        client(&url, 1).ingest(vec![event()]),
    )
    .await
    .unwrap()
    .unwrap();
    let bodies = task.await.unwrap();
    assert_eq!(bodies.len(), 2);
    assert!(!bodies[0].is_empty());
    assert_eq!(bodies[0], bodies[1]);
}

#[tokio::test]
async fn recoverable_transport_error_respects_retry_count() {
    let (url, task) = transport_server(vec![b"", b"", b""]).await;
    let error = tokio::time::timeout(
        Duration::from_secs(2),
        client(&url, 2).ingest(vec![event()]),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.to_string().contains("after 2 retries"));
    assert_eq!(task.await.unwrap().len(), 3);
}
