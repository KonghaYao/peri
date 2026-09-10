//! 真实 HTTP 背压、flush 屏障与关闭 owner 的排空/join 生命周期回归。

use std::{collections::HashMap, time::Duration};

use super::*;
use crate::types::TraceBody;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

fn event(id: &str) -> IngestionEvent {
    IngestionEvent::TraceCreate {
        id: id.into(),
        timestamp: "2026-01-01T00:00:00Z".into(),
        body: TraceBody {
            id: Some(id.into()),
            ..Default::default()
        },
        metadata: None,
    }
}

fn batcher(url: &str) -> Batcher {
    Batcher::new(
        LangfuseClient::new("test-public", "test-secret", url, 0),
        BatcherConfig {
            max_events: 1,
            flush_interval: Duration::from_secs(60),
            backpressure: BackpressurePolicy::DropNew,
            max_retries: 0,
        },
    )
}

struct GatedIngestion {
    url: String,
    started: oneshot::Receiver<()>,
    release: oneshot::Sender<()>,
    worker: tokio::task::JoinHandle<Vec<serde_json::Value>>,
}

/// A real local HTTP gate, with no elapsed-time assumptions. The worker reads
/// the complete first OTLP request before announcing the barrier, then waits for
/// release. Connection: close makes each expected request use a separate socket.
async fn gated_ingestion(statuses: Vec<u16>) -> GatedIngestion {
    gated_ingestion_at(statuses, 0).await
}

async fn gated_ingestion_at(statuses: Vec<u16>, gate_index: usize) -> GatedIngestion {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (started_tx, started) = oneshot::channel();
    let (release, release_rx) = oneshot::channel();
    let worker = tokio::spawn(async move {
        let mut first_gate = Some((started_tx, release_rx));
        let mut bodies = Vec::new();
        for (index, status) in statuses.into_iter().enumerate() {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            let mut headers = HashMap::new();
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            assert!(line.starts_with("POST /api/public/otel/v1/traces "));
            loop {
                line.clear();
                assert_ne!(reader.read_line(&mut line).await.unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
                if let Some((key, value)) = line.split_once(':') {
                    headers.insert(key.to_ascii_lowercase(), value.trim().to_string());
                }
            }
            let length = headers["content-length"].parse::<usize>().unwrap();
            let mut body = vec![0; length];
            reader.read_exact(&mut body).await.unwrap();
            bodies.push(serde_json::from_slice(&body).unwrap());
            if index == gate_index {
                let (started, release) = first_gate.take().expect("one controlled HTTP gate");
                started.send(()).unwrap();
                release.await.unwrap();
            }
            let body = if status == 200 {
                "{}"
            } else {
                "private-response-marker"
            };
            let response = format!(
                "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            reader
                .get_mut()
                .write_all(response.as_bytes())
                .await
                .unwrap();
            reader.get_mut().shutdown().await.unwrap();
        }
        bodies
    });
    GatedIngestion {
        url,
        started,
        release,
        worker,
    }
}

async fn park_pending_result(
    mut shutdown: std::pin::Pin<&mut impl std::future::Future<Output = Result<(), LangfuseError>>>,
) {
    std::future::poll_fn(|cx| {
        assert!(shutdown.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
}

#[tokio::test]
async fn test_shutdown_owner_closes_admission_with_a_full_command_queue() {
    let server = gated_ingestion(vec![200, 200]).await;
    let batcher = batcher(&server.url);
    batcher.add(event("first")).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), server.started)
        .await
        .unwrap()
        .unwrap();
    batcher.try_add(event("second")).unwrap();
    assert!(matches!(
        batcher.try_add(event("full")),
        Err(LangfuseError::QueueFull)
    ));

    let mut shutdown = Box::pin(batcher.shutdown());
    park_pending_result(shutdown.as_mut()).await;
    // This must change immediately even though HTTP is gated and the command
    // queue is full: closing cannot depend on enqueueing a Shutdown command.
    assert!(matches!(
        batcher.try_add(event("late")),
        Err(LangfuseError::ChannelClosed)
    ));
    server.release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), shutdown)
        .await
        .unwrap()
        .unwrap();
    assert!(batcher.worker_is_joined().await);
    let bodies = server.worker.await.unwrap();
    let ids = bodies
        .iter()
        .flat_map(|body| {
            body["resourceSpans"][0]["scopeSpans"][0]["spans"]
                .as_array()
                .unwrap()
                .iter()
                .map(|span| span["spanId"].as_str().unwrap())
        })
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        ["first", "second"],
        "only accepted events drain, in their original order"
    );
}

#[tokio::test]
async fn test_shutdown_owner_cancelled_waiter_keeps_join_available_for_retry() {
    let server = gated_ingestion(vec![200]).await;
    let batcher = Arc::new(batcher(&server.url));
    batcher.add(event("accepted")).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), server.started)
        .await
        .unwrap()
        .unwrap();
    let mut cancelled = Box::pin(batcher.shutdown());
    park_pending_result(cancelled.as_mut()).await;
    drop(cancelled);
    assert!(
        !batcher.worker_is_joined().await,
        "HTTP is still gated; cancellation cannot claim a join"
    );
    assert!(matches!(
        batcher.try_add(event("late")),
        Err(LangfuseError::ChannelClosed)
    ));

    let retry = tokio::spawn({
        let batcher = Arc::clone(&batcher);
        async move { batcher.shutdown().await }
    });
    server.release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), retry)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        batcher.worker_is_joined().await,
        "retry must actually join the original worker"
    );
    batcher.shutdown().await.unwrap();
    assert_eq!(
        server.worker.await.unwrap().len(),
        1,
        "retry must not submit the batch twice"
    );
}

#[tokio::test]
async fn test_shutdown_owner_concurrent_callers_keep_the_same_failed_but_joined_terminal() {
    let server = gated_ingestion(vec![500]).await;
    let batcher = batcher(&server.url);
    batcher.add(event("failed")).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), server.started)
        .await
        .unwrap()
        .unwrap();
    let mut first = Box::pin(batcher.shutdown());
    let mut second = Box::pin(batcher.shutdown());
    park_pending_result(first.as_mut()).await;
    park_pending_result(second.as_mut()).await;
    server.release.send(()).unwrap();
    let (first, second) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(first, second)
    })
    .await
    .unwrap();
    let first = first.unwrap_err();
    let second = second.unwrap_err();
    assert!(matches!(first, LangfuseError::IngestionApi(_)));
    assert_eq!(first.to_string(), second.to_string());
    assert!(!first.to_string().contains("private-response-marker"));
    assert!(
        batcher.worker_is_joined().await,
        "HTTP delivery error is not an unjoined worker"
    );
    assert_eq!(
        batcher.shutdown().await.unwrap_err().to_string(),
        first.to_string()
    );
    assert_eq!(server.worker.await.unwrap().len(), 1);
}

#[tokio::test]
async fn test_shutdown_owner_does_not_wait_for_an_unpolled_blocked_producer() {
    let server = gated_ingestion(vec![200, 200]).await;
    let batcher = Batcher::new(
        LangfuseClient::new("test-public", "test-secret", &server.url, 0),
        BatcherConfig {
            max_events: 1,
            flush_interval: Duration::from_secs(60),
            backpressure: BackpressurePolicy::Block,
            max_retries: 0,
        },
    );
    batcher.add(event("first")).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), server.started)
        .await
        .unwrap()
        .unwrap();
    batcher.add(event("second")).await.unwrap();
    let mut blocked = Box::pin(batcher.add(event("unadmitted")));
    // The helper polls any Result-returning operation once. Do not poll this
    // producer again until shutdown finishes: its capacity waiter stays alive.
    park_pending_result(blocked.as_mut()).await;
    let mut shutdown = Box::pin(batcher.shutdown());
    park_pending_result(shutdown.as_mut()).await;
    server.release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), shutdown)
        .await
        .expect("shutdown must not require an external producer to be polled")
        .unwrap();
    assert!(batcher.worker_is_joined().await);
    assert!(matches!(blocked.await, Err(LangfuseError::ChannelClosed)));
    assert_eq!(server.worker.await.unwrap().len(), 2);
}

#[tokio::test]
async fn test_shutdown_owner_reports_cancelled_worker_separately_from_http_failure() {
    let batcher = batcher("http://127.0.0.1:1");
    {
        let owner = batcher.worker.lock().await;
        match &*owner {
            WorkerOwner::Running(handle) => handle.abort(),
            WorkerOwner::Joined(_) => panic!("尚未调用 shutdown，不能已有 join 终态"),
        }
    }
    let error = batcher.shutdown().await.unwrap_err();
    assert!(matches!(
        error,
        LangfuseError::WorkerJoinFailed { cancelled: true }
    ));
    assert_eq!(
        error.to_string(),
        "Batch worker join failed (cancelled: true)"
    );
    assert!(batcher.worker_is_joined().await);
    assert_eq!(
        batcher.shutdown().await.unwrap_err().to_string(),
        error.to_string()
    );
    assert_eq!(
        batcher.flush().await.unwrap_err().to_string(),
        error.to_string()
    );
}

#[tokio::test]
async fn test_shutdown_owner_join_panic_keeps_safe_terminal_without_payload() {
    let failures = FailureLedger::default();
    let mut owner = WorkerOwner::Running(tokio::spawn(async {
        panic!("private-worker-panic-marker");
    }));
    let error = owner.join(&failures).await.unwrap_err();
    assert!(matches!(
        error,
        LangfuseError::WorkerJoinFailed { cancelled: false }
    ));
    assert_eq!(
        error.to_string(),
        "Batch worker join failed (cancelled: false)"
    );
    assert!(!error.to_string().contains("private-worker-panic-marker"));
    assert!(matches!(owner, WorkerOwner::Joined(_)));
    assert_eq!(
        owner.join(&failures).await.unwrap_err().to_string(),
        error.to_string()
    );
}

/// [回归测试] HTTP 发送阻塞期间，DropOldest 必须替换命令队列中的最旧事件。
/// 先完整收齐在途请求再填满队列，避免靠调度或 sleep 猜测背压窗口。
async fn assert_drop_oldest_admission_keeps_newest(use_try_add: bool) {
    let server = gated_ingestion(vec![200, 200]).await;
    let batcher = Batcher::new(
        LangfuseClient::new("test-public", "test-secret", &server.url, 0),
        BatcherConfig {
            max_events: 2,
            flush_interval: Duration::from_secs(60),
            backpressure: BackpressurePolicy::DropOldest,
            max_retries: 0,
        },
    );
    batcher.add(event("inflight0")).await.unwrap();
    batcher.add(event("inflight1")).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), server.started)
        .await
        .expect("首批 HTTP 必须到达门控")
        .unwrap();
    batcher.try_add(event("oldest")).unwrap();
    batcher.try_add(event("kept")).unwrap();
    let admission = if use_try_add {
        batcher.try_add(event("newest"))
    } else {
        batcher.add(event("newest")).await
    };
    let dropped = batcher.dropped_count();
    // 即使旧实现返回 QueueFull，也先收敛真实任务，再断言事件身份。
    server.release.send(()).unwrap();
    let bodies = tokio::time::timeout(Duration::from_secs(5), async {
        batcher.shutdown().await.unwrap();
        server.worker.await.unwrap()
    })
    .await
    .expect("已准入事件必须排空且 HTTP task 必须退出");
    let ids = bodies
        .iter()
        .flat_map(|body| {
            body["resourceSpans"][0]["scopeSpans"][0]["spans"]
                .as_array()
                .unwrap()
                .iter()
                .map(|span| span["spanId"].as_str().unwrap())
        })
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        ["inflight0", "inflight1", "kept", "newest"],
        "在途批次不变，队列应丢 oldest 并保留 newest；准入结果: {admission:?}"
    );
    assert!(admission.is_ok(), "替换最旧事件后新事件必须准入");
    assert_eq!(dropped, 1, "替换的旧事件必须计入丢弃统计");
    assert!(batcher.worker_is_joined().await);
}

#[tokio::test]
async fn test_backpressure_drop_oldest_add_preserves_newest_http_identity() {
    assert_drop_oldest_admission_keeps_newest(false).await;
}

#[tokio::test]
async fn test_backpressure_drop_oldest_try_add_preserves_newest_http_identity() {
    assert_drop_oldest_admission_keeps_newest(true).await;
}

fn http_span_ids(bodies: &[serde_json::Value]) -> Vec<&str> {
    bodies
        .iter()
        .flat_map(|body| {
            body["resourceSpans"][0]["scopeSpans"][0]["spans"]
                .as_array()
                .unwrap()
                .iter()
                .map(|span| span["spanId"].as_str().unwrap())
        })
        .collect()
}

#[tokio::test]
async fn test_backpressure_flush_prefix_is_protected_even_after_waiter_cancellation() {
    for cancel_flush in [false, true] {
        let server = gated_ingestion(vec![200, 200]).await;
        let batcher = Batcher::try_new(
            LangfuseClient::new("pk-test", "sk-test", &server.url, 0),
            BatcherConfig {
                max_events: 2,
                flush_interval: Duration::from_secs(60),
                backpressure: BackpressurePolicy::DropOldest,
                max_retries: 0,
            },
        )
        .unwrap();
        batcher.try_add(event("inflight0")).unwrap();
        batcher.try_add(event("inflight1")).unwrap();
        tokio::time::timeout(Duration::from_secs(5), server.started)
            .await
            .unwrap()
            .unwrap();
        batcher.try_add(event("protected")).unwrap();
        let mut flush = Some(Box::pin(batcher.flush()));
        park_pending_result(flush.as_mut().unwrap().as_mut()).await;
        // flush 已入队，保护它之前的 protected；取消调用方不得撤销该屏障。
        if cancel_flush {
            drop(flush.take());
        }
        let replacement = batcher.try_add(event("rejected"));
        server.release.send(()).unwrap();
        let bodies = tokio::time::timeout(Duration::from_secs(5), async {
            if let Some(flush) = flush {
                flush.await.unwrap();
            }
            batcher.shutdown().await.unwrap();
            server.worker.await.unwrap()
        })
        .await
        .expect("flush 与关闭必须完成");
        assert!(matches!(replacement, Err(LangfuseError::QueueFull)));
        assert_eq!(
            http_span_ids(&bodies),
            ["inflight0", "inflight1", "protected"]
        );
        assert!(batcher.worker_is_joined().await);
    }
}

#[tokio::test]
async fn test_backpressure_replacement_stays_after_flush_barrier() {
    let server = gated_ingestion(vec![200, 200]).await;
    let batcher = Batcher::new(
        LangfuseClient::new("pk-test", "sk-test", &server.url, 0),
        BatcherConfig {
            max_events: 3,
            flush_interval: Duration::from_secs(60),
            backpressure: BackpressurePolicy::DropOldest,
            max_retries: 0,
        },
    );
    for id in ["inflight0", "inflight1", "inflight2"] {
        batcher.try_add(event(id)).unwrap();
    }
    tokio::time::timeout(Duration::from_secs(5), server.started)
        .await
        .unwrap()
        .unwrap();
    let (ack, response) = oneshot::channel();
    batcher
        .admission
        .send(BatcherCommand::Flush(ack))
        .await
        .unwrap();
    batcher.try_add(event("oldest")).unwrap();
    batcher.try_add(event("kept")).unwrap();
    let replacement = batcher.try_add(event("newest"));
    server.release.send(()).unwrap();
    let (snapshot, bodies) = tokio::time::timeout(Duration::from_secs(5), async {
        let snapshot = response.await.expect("Flush ack 不得被驱逐");
        batcher.shutdown().await.unwrap();
        (snapshot, server.worker.await.unwrap())
    })
    .await
    .unwrap();
    assert!(replacement.is_ok());
    batcher.failures.observe(snapshot).unwrap();
    assert_eq!(
        http_span_ids(&bodies),
        ["inflight0", "inflight1", "inflight2", "kept", "newest"]
    );
}

#[tokio::test]
async fn test_flush_barrier_does_not_wait_for_later_http_batch() {
    // 第二个 HTTP 批次暂停；第一批后的公共 flush 必须可以先返回。
    let server = gated_ingestion_at(vec![200, 200], 1).await;
    let batcher = Batcher::new(
        LangfuseClient::new("pk-test", "sk-test", &server.url, 0),
        BatcherConfig {
            max_events: 2,
            flush_interval: Duration::from_secs(60),
            backpressure: BackpressurePolicy::Block,
            max_retries: 0,
        },
    );
    batcher.try_add(event("before")).unwrap();
    let mut flush = Box::pin(batcher.flush());
    park_pending_result(flush.as_mut()).await;
    batcher.add(event("after0")).await.unwrap();
    batcher.add(event("after1")).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), server.started)
        .await
        .unwrap()
        .unwrap();
    let flush_result = tokio::time::timeout(Duration::from_secs(1), flush).await;
    server.release.send(()).unwrap();
    let bodies = tokio::time::timeout(Duration::from_secs(5), async {
        batcher.shutdown().await.unwrap();
        server.worker.await.unwrap()
    })
    .await
    .unwrap();
    flush_result
        .expect("屏障不能等待它之后的 HTTP 请求")
        .unwrap();
    assert_eq!(http_span_ids(&bodies), ["before", "after0", "after1"]);
}

#[tokio::test]
async fn test_batcher_legacy_retry_field_never_overrides_client_http_policy() {
    for (client_retries, legacy_retries, statuses) in [(1, 0, vec![500, 200]), (0, 9, vec![500])] {
        let expected_requests = statuses.len();
        let server = gated_ingestion(statuses).await;
        let batcher = Batcher::try_new(
            LangfuseClient::new("pk-test", "sk-test", &server.url, client_retries),
            BatcherConfig {
                max_events: 1,
                flush_interval: Duration::from_secs(60),
                backpressure: BackpressurePolicy::DropNew,
                max_retries: legacy_retries,
            },
        )
        .unwrap();
        batcher.try_add(event("retryevent")).unwrap();
        tokio::time::timeout(Duration::from_secs(5), server.started)
            .await
            .unwrap()
            .unwrap();
        server.release.send(()).unwrap();
        let (result, bodies) = tokio::time::timeout(Duration::from_secs(5), async {
            let result = batcher.shutdown().await;
            (result, server.worker.await.unwrap())
        })
        .await
        .expect("实际 retry 与 worker join 必须收敛");
        assert_eq!(bodies.len(), expected_requests);
        assert_eq!(result.is_ok(), client_retries == 1);
        assert!(batcher.worker_is_joined().await);
        if let Err(error) = result {
            assert!(!error.to_string().contains("private-response-marker"));
        }
    }
}
