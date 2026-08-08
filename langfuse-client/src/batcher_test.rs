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

/// 极简慢速 HTTP server：接受请求后延迟 `delay` 再返回 200（模拟慢 flush）。
/// 返回 (base_url, 请求计数, JoinHandle)。
fn spawn_slow_server(
    delay: Duration,
) -> (
    String,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind slow server");
    listener.set_nonblocking(true).expect("nonblocking");
    let addr = listener.local_addr().expect("local addr");
    let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let req_counter = std::sync::Arc::clone(&requests);
    let handle = tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::from_std(listener).expect("from_std");
        loop {
            let (mut socket, _) = match listener.accept().await {
                Ok(pair) => pair,
                Err(_) => break,
            };
            req_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            tokio::spawn(async move {
                // 读取请求头（到 \r\n\r\n），然后延迟响应，模拟慢 flush
                let mut buf = [0u8; 8192];
                let mut read = 0usize;
                while read < buf.len() {
                    match socket.read(&mut buf[read..]).await {
                        Ok(0) => return,
                        Ok(n) => {
                            read += n;
                            if buf[..read].windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                        Err(_) => return,
                    }
                }
                tokio::time::sleep(delay).await;
                let body = r#"{}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(resp.as_bytes()).await;
            });
        }
    });
    (format!("http://{}", addr), requests, handle)
}

/// S5.2：慢 flush 期间命令通道满 → DropNew 丢弃事件，丢弃计数必须累加，
/// flush 完成后汇总输出并清零。
#[tokio::test]
async fn test_batcher_drop_new_increments_dropped_counter_during_slow_flush() {
    let (server_url, requests, _server) = spawn_slow_server(Duration::from_millis(500));

    let client = create_test_client(&server_url);
    let config = BatcherConfig {
        max_events: 2,
        flush_interval: Duration::from_secs(60),
        backpressure: BackpressurePolicy::DropNew,
        max_retries: 0,
    };
    let batcher = Batcher::new(client, config);

    // 前 2 个事件：run_loop 消费 → buffer 满 → 进入 do_flush（慢 server 延迟 500ms）
    batcher.add(create_test_event("1")).await.unwrap();
    batcher.add(create_test_event("2")).await.unwrap();
    // 等待 run_loop 进入 do_flush（慢 server 收到首个请求 = 首次 flush 已开始，
    // 此时无法消费命令通道中的 Add）。条件轮询替代固定 sleep(100ms)——
    // 慢 CI 下固定延时可能不足，导致 3/4 未被缓冲、5/6 未丢弃（flaky）。
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while requests.load(std::sync::atomic::Ordering::Relaxed) < 1 {
        assert!(
            std::time::Instant::now() < deadline,
            "超时：run_loop 未在 5s 内进入 do_flush（首个 flush 请求未到达）"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // 命令通道容量 = max_events = 2：再塞 2 个进通道，后续全部丢弃并计数
    batcher.add(create_test_event("3")).await.unwrap();
    batcher.add(create_test_event("4")).await.unwrap();
    assert!(
        batcher.add(create_test_event("5")).await.is_err(),
        "通道满时 DropNew 应丢弃并返回错误"
    );
    assert!(
        batcher.add(create_test_event("6")).await.is_err(),
        "通道满时 DropNew 应丢弃并返回错误"
    );
    assert_eq!(batcher.dropped_count(), 2, "通道满时应丢弃 2 条且计数可见");

    // flush：等待第一次 do_flush 完成 → run_loop 消费 "3","4" → 第二次 do_flush
    batcher.flush().await.unwrap();
    // flush 完成后 run_loop 已输出丢弃汇总日志并清零计数
    assert_eq!(
        batcher.dropped_count(),
        0,
        "flush 后丢弃计数应清零（汇总已输出）"
    );
    assert_eq!(
        requests.load(std::sync::atomic::Ordering::Relaxed),
        2,
        "应恰好发生 2 次 flush 请求"
    );
    drop(batcher);
}

#[test]
fn test_try_add_drop_new_returns_queue_full_when_command_channel_is_full() {
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let batcher = Batcher {
        tx,
        backpressure: BackpressurePolicy::DropNew,
        dropped: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    };

    batcher
        .try_add(create_test_event("first"))
        .expect("first event fits");
    let error = batcher
        .try_add(create_test_event("second"))
        .expect_err("full queue drops new event");

    assert!(matches!(error, LangfuseError::QueueFull));
    assert_eq!(batcher.dropped_count(), 1);
}
