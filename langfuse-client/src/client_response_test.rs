use super::*;
use crate::types::TraceBody;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn event() -> IngestionEvent {
    IngestionEvent::TraceCreate {
        id: "event".into(),
        timestamp: "2026-01-01T00:00:00Z".into(),
        body: TraceBody {
            id: Some("root".into()),
            name: Some("test".into()),
            ..Default::default()
        },
        metadata: None,
    }
}

#[tokio::test]
async fn partial_success_rejection_is_safe_and_never_retried() {
    for rejected in ["2", "\"2\""] {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/public/otel/v1/traces")
            .with_status(200)
            .with_body(format!(
                r#"{{"partialSuccess":{{"rejectedSpans":{},"errorMessage":"secret-payload"}}}}"#,
                rejected
            ))
            .expect(1)
            .create_async()
            .await;
        let client = LangfuseClient::new("pk", "sk", &server.url(), 3);
        let result = client.ingest(vec![event()]).await.unwrap_err();
        assert!(matches!(
            result,
            LangfuseError::PartialSuccess { rejected_spans: 2 }
        ));
        let error = result.to_string();
        assert!(error.contains("rejected 2 spans"));
        assert!(!error.contains("secret-payload"));
        mock.assert_async().await;
    }
}

#[tokio::test]
async fn zero_rejected_spans_with_warning_is_accepted() {
    for rejected in ["0", "\"0\""] {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/public/otel/v1/traces")
            .with_status(200)
            .with_body(format!(
                r#"{{"partialSuccess":{{"rejectedSpans":{},"errorMessage":"server-warning"}}}}"#,
                rejected
            ))
            .expect(1)
            .create_async()
            .await;
        LangfuseClient::new("pk", "sk", &server.url(), 3)
            .ingest(vec![event()])
            .await
            .unwrap();
        mock.assert_async().await;
    }
}

#[test]
fn valid_json_response_objects_accept_unknown_fields_and_full_count_range() {
    for body in [
        b"{}".as_slice(),
        br#"{"futureField":42}"#,
        br#"{"partialSuccess":{}}"#,
    ] {
        response::parse(body).unwrap();
    }
    for body in [
        br#"{"partialSuccess":{"rejectedSpans":18446744073709551615}}"#.as_slice(),
        br#"{"partialSuccess":{"rejectedSpans":"18446744073709551615"}}"#,
    ] {
        assert!(response::parse(body)
            .unwrap_err()
            .to_string()
            .contains("18446744073709551615"));
    }
}

#[test]
fn original_protobuf_field_names_cannot_hide_partial_rejection() {
    let error = response::parse(
        br#"{"partial_success":{"rejected_spans":"1","error_message":"secret-payload"}}"#,
    )
    .unwrap_err();
    assert!(error.to_string().contains("rejected 1 spans"));
    assert!(!error.to_string().contains("secret-payload"));
}

#[tokio::test]
async fn response_exactly_at_byte_limit_is_accepted() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(200)
        .with_body("{}")
        .expect(1)
        .create_async()
        .await;
    let client = LangfuseClient::new("pk", "sk", &server.url(), 0)
        .with_export_config(ExportConfig {
            max_response_bytes: 2,
            ..Default::default()
        })
        .unwrap();
    client.ingest(vec![event()]).await.unwrap();
    mock.assert_async().await;
}

#[tokio::test]
async fn malformed_success_responses_are_safe_errors_without_retry() {
    for body in [
        "secret-payload",
        "",
        "{",
        "[]",
        r#"{"partialSuccess":{"rejectedSpans":-1}}"#,
        r#"{"partialSuccess":{"rejectedSpans":"18446744073709551616"}}"#,
        r#"{"partialSuccess":{"rejectedSpans":"+1"}}"#,
    ] {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/public/otel/v1/traces")
            .with_status(200)
            .with_body(body)
            .expect(1)
            .create_async()
            .await;
        let error = LangfuseClient::new("pk", "sk", &server.url(), 3)
            .ingest(vec![event()])
            .await
            .unwrap_err()
            .to_string();
        assert!(!error.contains("secret-payload"));
        mock.assert_async().await;
    }
}

#[tokio::test]
async fn content_length_larger_than_limit_is_rejected() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(200)
        .with_body(r#"{"secret-payload":"content"}"#)
        .expect(1)
        .create_async()
        .await;
    let client = LangfuseClient::new("pk", "sk", &server.url(), 3)
        .with_export_config(ExportConfig {
            max_response_bytes: 8,
            ..Default::default()
        })
        .unwrap();
    let error = client.ingest(vec![event()]).await.unwrap_err().to_string();
    assert!(error.contains("8 byte limit"));
    assert!(!error.contains("secret-payload"));
    mock.assert_async().await;
}

async fn raw_server(
    response: &'static [u8],
    hold_open: Duration,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
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
                    break;
                }
            }
        }
        socket.write_all(response).await.unwrap();
        tokio::time::sleep(hold_open).await;
    });
    (url, task)
}

#[tokio::test]
async fn truncated_success_body_fails_without_retry() {
    let (url, task) = raw_server(
        b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{}",
        Duration::ZERO,
    )
    .await;
    let error = LangfuseClient::new("pk", "sk", &url, 3)
        .ingest(vec![event()])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("body read failed"));
    task.await.unwrap();
}

#[tokio::test]
async fn chunked_response_enforces_limit_without_content_length() {
    let (url, task) = raw_server(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n9\r\n123456789\r\n0\r\n\r\n", Duration::ZERO).await;
    let client = LangfuseClient::new("pk", "sk", &url, 3)
        .with_export_config(ExportConfig {
            max_response_bytes: 8,
            ..Default::default()
        })
        .unwrap();
    assert!(client
        .ingest(vec![event()])
        .await
        .unwrap_err()
        .to_string()
        .contains("8 byte limit"));
    task.await.unwrap();
}

#[tokio::test]
async fn cumulative_budget_covers_success_body_read() {
    let (url, task) = raw_server(
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n",
        Duration::from_millis(200),
    )
    .await;
    let client = LangfuseClient::new("pk", "sk", &url, 3)
        .with_export_config(ExportConfig {
            retry_budget: Duration::from_millis(100),
            ..Default::default()
        })
        .unwrap();
    assert!(client
        .ingest(vec![event()])
        .await
        .unwrap_err()
        .to_string()
        .contains("time budget"));
    task.await.unwrap();
}
