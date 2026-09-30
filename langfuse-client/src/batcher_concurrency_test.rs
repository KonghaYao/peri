use super::*;
use crate::types::TraceBody;
use std::{collections::HashMap, future::Future, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

fn event(name: &str) -> IngestionEvent {
    IngestionEvent::TraceCreate {
        id: name.into(),
        timestamp: "2026-09-30T00:00:00Z".into(),
        body: TraceBody {
            id: Some(name.into()),
            name: Some(name.into()),
            ..Default::default()
        },
        metadata: None,
    }
}

struct Server {
    url: String,
    started: mpsc::UnboundedReceiver<String>,
    release: Vec<oneshot::Sender<()>>,
    worker: tokio::task::JoinHandle<()>,
}

async fn server(statuses: Vec<u16>) -> Server {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (started_tx, started) = mpsc::unbounded_channel();
    let (release, receivers): (Vec<_>, Vec<_>) =
        statuses.iter().map(|_| oneshot::channel()).unzip();
    let worker = tokio::spawn(async move {
        let mut requests = tokio::task::JoinSet::new();
        for (status, gate) in statuses.into_iter().zip(receivers) {
            let (stream, _) = listener.accept().await.unwrap();
            let started = started_tx.clone();
            requests.spawn(async move {
                let mut reader = BufReader::new(stream);
                let mut headers = HashMap::new();
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
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
                let mut bytes = vec![0; headers["content-length"].parse::<usize>().unwrap()];
                reader.read_exact(&mut bytes).await.unwrap();
                let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                let name = body["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["name"]
                    .as_str()
                    .unwrap()
                    .to_string();
                started.send(name).unwrap();
                gate.await.unwrap();
                let body = "{}";
                let response = format!(
                    "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = reader.get_mut().write_all(response.as_bytes()).await;
            });
        }
        while let Some(result) = requests.join_next().await {
            result.unwrap();
        }
    });
    Server {
        url,
        started,
        release,
        worker,
    }
}

async fn started(receiver: &mut mpsc::UnboundedReceiver<String>) -> String {
    tokio::time::timeout(Duration::from_secs(5), receiver.recv())
        .await
        .unwrap()
        .unwrap()
}

fn batcher(url: &str) -> Batcher {
    Batcher::new(
        LangfuseClient::new("pk", "sk", url, 0),
        BatcherConfig {
            max_events: 1,
            queue_capacity: 2,
            max_in_flight: 2,
            flush_interval: Duration::from_secs(3600),
            ..Default::default()
        },
    )
}

#[tokio::test]
async fn slow_first_batch_allows_second_send_but_concurrency_and_queue_stay_bounded() {
    let mut server = server(vec![500, 200, 200, 200]).await;
    let batcher = batcher(&server.url);
    batcher.try_add(event("first")).unwrap();
    assert_eq!(started(&mut server.started).await, "first");
    batcher.try_add(event("second")).unwrap();
    assert_eq!(started(&mut server.started).await, "second");
    assert_eq!(batcher.stats().in_flight_batches, 2);
    batcher.try_add(event("third")).unwrap();
    batcher.try_add(event("fourth")).unwrap();
    assert!(matches!(
        batcher.try_add(event("full")),
        Err(LangfuseError::QueueFull)
    ));
    assert_eq!(batcher.stats().submitted_batches, 2);

    let mut release = server.release.into_iter();
    release.next().unwrap().send(()).unwrap();
    assert_eq!(started(&mut server.started).await, "third");
    assert_eq!(batcher.stats().in_flight_batches, 2);
    release.next().unwrap().send(()).unwrap();
    assert_eq!(started(&mut server.started).await, "fourth");
    for gate in release {
        gate.send(()).unwrap();
    }
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), batcher.shutdown())
            .await
            .unwrap(),
        Err(LangfuseError::IngestionApi(_))
    ));
    server.worker.await.unwrap();
    assert_eq!(
        batcher.stats(),
        BatcherStats {
            accepted_events: 4,
            rejected_events: 1,
            evicted_events: 0,
            submitted_batches: 4,
            completed_batches: 4,
            failed_batches: 1,
            partially_rejected_batches: 0,
            rejected_spans: 0,
            in_flight_batches: 0,
        }
    );
}

#[tokio::test]
async fn concurrent_flush_waits_for_its_prefix_and_reports_out_of_order_failure_once() {
    let mut server = server(vec![200, 500, 200]).await;
    let batcher = batcher(&server.url);
    batcher.try_add(event("first")).unwrap();
    assert_eq!(started(&mut server.started).await, "first");
    batcher.try_add(event("second")).unwrap();
    assert_eq!(started(&mut server.started).await, "second");

    let mut flush = Box::pin(batcher.flush());
    std::future::poll_fn(|context| {
        assert!(flush.as_mut().poll(context).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    batcher.try_add(event("after-barrier")).unwrap();
    let mut release = server.release.into_iter();
    let first = release.next().unwrap();
    let second = release.next().unwrap();
    second.send(()).unwrap();
    std::future::poll_fn(|context| {
        assert!(flush.as_mut().poll(context).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    first.send(()).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), flush)
        .await
        .unwrap();
    assert!(matches!(result, Err(LangfuseError::IngestionApi(_))));
    assert_eq!(started(&mut server.started).await, "after-barrier");
    release.next().unwrap().send(()).unwrap();
    assert!(batcher.flush().await.is_ok());
    assert!(batcher.shutdown().await.is_err());
    server.worker.await.unwrap();
    assert_eq!(batcher.stats().failed_batches, 1);
}

#[tokio::test]
async fn partial_rejection_is_visible_at_flush_and_remains_in_shutdown_report() {
    let mut server = mockito::Server::new_async().await;
    let response = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(200)
        .with_body(r#"{"partialSuccess":{"rejectedSpans":"1","errorMessage":"private-marker"}}"#)
        .expect(1)
        .create_async()
        .await;
    let batcher = batcher(&server.url());
    batcher.try_add(event("rejected")).unwrap();
    let error = batcher.flush().await.unwrap_err();
    assert!(!error.to_string().contains("private-marker"));
    assert!(batcher.flush().await.is_ok());
    assert!(batcher.shutdown().await.is_err());
    response.assert_async().await;
    assert_eq!(batcher.stats().failed_batches, 1);
    assert_eq!(batcher.stats().partially_rejected_batches, 1);
    assert_eq!(batcher.stats().rejected_spans, 1);
}

#[tokio::test]
async fn cancelled_shutdown_waiter_preserves_all_concurrent_sends_for_retry() {
    let mut server = server(vec![200, 200]).await;
    let batcher = batcher(&server.url);
    batcher.try_add(event("first")).unwrap();
    assert_eq!(started(&mut server.started).await, "first");
    batcher.try_add(event("second")).unwrap();
    assert_eq!(started(&mut server.started).await, "second");
    let mut shutdown = Box::pin(batcher.shutdown());
    std::future::poll_fn(|context| {
        assert!(shutdown.as_mut().poll(context).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    drop(shutdown);
    assert_eq!(batcher.stats().in_flight_batches, 2);
    assert!(matches!(
        batcher.try_add(event("closed")),
        Err(LangfuseError::ChannelClosed)
    ));
    for gate in server.release {
        gate.send(()).unwrap();
    }
    tokio::time::timeout(Duration::from_secs(5), batcher.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(batcher.worker_is_joined().await);
    assert_eq!(batcher.stats().completed_batches, 2);
    assert_eq!(batcher.stats().in_flight_batches, 0);
    server.worker.await.unwrap();
}

#[tokio::test]
async fn aborted_worker_cancels_owned_sends_and_releases_live_send_counters() {
    let mut server = server(vec![200, 200]).await;
    let batcher = batcher(&server.url);
    batcher.try_add(event("first")).unwrap();
    assert_eq!(started(&mut server.started).await, "first");
    batcher.try_add(event("second")).unwrap();
    assert_eq!(started(&mut server.started).await, "second");
    {
        let worker = batcher.worker.lock().await;
        let WorkerOwner::Running(handle) = &*worker else {
            panic!("worker should be running while both HTTP sends are gated");
        };
        handle.abort();
    }
    assert!(matches!(
        batcher.shutdown().await,
        Err(LangfuseError::WorkerJoinFailed { cancelled: true })
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        while batcher.stats().in_flight_batches != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(batcher.stats().completed_batches, 0);
    for gate in server.release {
        gate.send(()).unwrap();
    }
    server.worker.await.unwrap();
}
