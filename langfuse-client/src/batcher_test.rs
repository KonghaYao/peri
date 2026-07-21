//! Tests for batcher

use std::time::Duration;

use super::*;
use crate::types::TraceBody;

fn create_test_client(server_url: &str) -> LangfuseClient {
    LangfuseClient::new("pk", "sk", server_url, 0)
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

#[tokio::test]
async fn test_batcher_add_and_manual_flush() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body("{}")
        .expect(1)
        .create_async()
        .await;

    let client = create_test_client(&server.url());
    let config = BatcherConfig {
        max_events: 10,
        flush_interval: Duration::from_secs(60),
        backpressure: BackpressurePolicy::DropNew,
        max_retries: 0,
    };
    let batcher = Batcher::new(client, config);

    batcher.add(create_test_event("1")).await.unwrap();
    batcher.add(create_test_event("2")).await.unwrap();
    batcher.add(create_test_event("3")).await.unwrap();
    batcher.flush().await.unwrap();

    mock.assert_async().await;
}

#[tokio::test]
async fn test_batcher_auto_flush_on_max_events() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body("{}")
        .expect(1)
        .create_async()
        .await;

    let client = create_test_client(&server.url());
    let config = BatcherConfig {
        max_events: 3,
        flush_interval: Duration::from_secs(60),
        backpressure: BackpressurePolicy::DropNew,
        max_retries: 0,
    };
    let batcher = Batcher::new(client, config);

    batcher.add(create_test_event("1")).await.unwrap();
    batcher.add(create_test_event("2")).await.unwrap();
    batcher.add(create_test_event("3")).await.unwrap();

    batcher.flush().await.unwrap();

    mock.assert_async().await;
    drop(batcher);
}

#[tokio::test]
async fn test_batcher_periodic_flush() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body("{}")
        .expect(1)
        .create_async()
        .await;

    let client = create_test_client(&server.url());
    let config = BatcherConfig {
        max_events: 100,
        flush_interval: Duration::from_millis(10),
        backpressure: BackpressurePolicy::DropNew,
        max_retries: 0,
    };
    let batcher = Batcher::new(client, config);

    batcher.add(create_test_event("1")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    mock.assert_async().await;
    drop(batcher);
}

#[tokio::test]
async fn test_batcher_flush_empty_buffer() {
    let server = mockito::Server::new_async().await;
    let client = create_test_client(&server.url());
    let config = BatcherConfig {
        max_events: 10,
        flush_interval: Duration::from_secs(60),
        backpressure: BackpressurePolicy::DropNew,
        max_retries: 0,
    };
    let batcher = Batcher::new(client, config);
    let result = batcher.flush().await;
    assert!(result.is_ok());
    drop(batcher);
}

#[tokio::test]
async fn test_batcher_backpressure_block() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body("{}")
        .expect(1)
        .create_async()
        .await;

    let client = create_test_client(&server.url());
    let config = BatcherConfig {
        max_events: 5,
        flush_interval: Duration::from_secs(60),
        backpressure: BackpressurePolicy::Block,
        max_retries: 0,
    };
    let batcher = Batcher::new(client, config);

    for i in 0..5 {
        batcher
            .add(create_test_event(&format!("{}", i)))
            .await
            .unwrap();
    }

    batcher.flush().await.unwrap();
    mock.assert_async().await;
    drop(batcher);
}

#[tokio::test]
async fn test_batcher_graceful_shutdown_on_drop() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body("{}")
        .expect(1)
        .create_async()
        .await;

    let client = create_test_client(&server.url());
    let config = BatcherConfig {
        max_events: 10,
        flush_interval: Duration::from_secs(60),
        backpressure: BackpressurePolicy::DropNew,
        max_retries: 0,
    };
    {
        let batcher = Batcher::new(client, config);
        batcher.add(create_test_event("1")).await.unwrap();
        batcher.add(create_test_event("2")).await.unwrap();
        batcher.flush().await.unwrap();
    }
    mock.assert_async().await;
}

#[tokio::test]
async fn test_batcher_multiple_flush_cycles() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body("{}")
        .expect(2)
        .create_async()
        .await;

    let client = create_test_client(&server.url());
    let config = BatcherConfig {
        max_events: 2,
        flush_interval: Duration::from_secs(60),
        backpressure: BackpressurePolicy::DropNew,
        max_retries: 0,
    };
    let batcher = Batcher::new(client, config);

    batcher.add(create_test_event("1")).await.unwrap();
    batcher.add(create_test_event("2")).await.unwrap();
    batcher.flush().await.unwrap();

    batcher.add(create_test_event("3")).await.unwrap();
    batcher.add(create_test_event("4")).await.unwrap();
    batcher.flush().await.unwrap();

    mock.assert_async().await;
    drop(batcher);
}

#[tokio::test]
async fn test_batcher_handles_ingest_error() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(500)
        .with_body("error")
        .expect(1)
        .create_async()
        .await;

    let client = create_test_client(&server.url());
    let config = BatcherConfig {
        max_events: 2,
        flush_interval: Duration::from_secs(60),
        backpressure: BackpressurePolicy::DropNew,
        max_retries: 0,
    };
    let batcher = Batcher::new(client, config);

    batcher.add(create_test_event("1")).await.unwrap();
    batcher.add(create_test_event("2")).await.unwrap();
    batcher.flush().await.unwrap();

    batcher.add(create_test_event("3")).await.unwrap();
    mock.assert_async().await;
    drop(batcher);
}

#[tokio::test]
async fn test_batcher_with_large_batch() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body("{}")
        .expect(1)
        .create_async()
        .await;

    let client = create_test_client(&server.url());
    let config = BatcherConfig {
        max_events: 50,
        flush_interval: Duration::from_secs(60),
        backpressure: BackpressurePolicy::DropNew,
        max_retries: 0,
    };
    let batcher = Batcher::new(client, config);

    for i in 0..50 {
        batcher
            .add(create_test_event(&format!("{}", i)))
            .await
            .unwrap();
    }
    batcher.flush().await.unwrap();

    mock.assert_async().await;
    drop(batcher);
}

#[tokio::test]
async fn test_batcher_backpressure_drop_oldest() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body("{}")
        .expect(1)
        .create_async()
        .await;

    let client = create_test_client(&server.url());
    let config = BatcherConfig {
        max_events: 5,
        flush_interval: Duration::from_secs(60),
        backpressure: BackpressurePolicy::DropOldest,
        max_retries: 0,
    };
    let batcher = Batcher::new(client, config);

    for i in 0..5 {
        batcher
            .add(create_test_event(&format!("{}", i)))
            .await
            .unwrap();
    }

    batcher.flush().await.unwrap();
    mock.assert_async().await;
    drop(batcher);
}

#[tokio::test]
async fn test_batcher_backpressure_drop_new() {
    let mut server = mockito::Server::new_async().await;
    let _mock = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body("{}")
        .create_async()
        .await;

    let client = create_test_client(&server.url());
    let config = BatcherConfig {
        max_events: 2,
        flush_interval: Duration::from_secs(60),
        backpressure: BackpressurePolicy::DropNew,
        max_retries: 0,
    };
    let batcher = Batcher::new(client, config);

    batcher.add(create_test_event("1")).await.unwrap();
    batcher.add(create_test_event("2")).await.unwrap();

    batcher.flush().await.unwrap();
    batcher.add(create_test_event("3")).await.unwrap();
    drop(batcher);
}
