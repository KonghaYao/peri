use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    task::Poll,
    time::Duration,
};

use async_trait::async_trait;
use futures::{stream, StreamExt};
use tokio::{sync::Notify, time::timeout};
use tokio_util::sync::CancellationToken;

use super::{
    cancellable_stream, retrying_http_sse_stream, retrying_http_sse_stream_async,
    runtime_http_sse_stream, AsyncSseDecoder, SseCompletionDecoder, SseDecoder, SseDecoderFactory,
};
use crate::{
    transport::{HttpBody, HttpRequest, HttpResponse, HttpTransport, SseEvent},
    ModelError, ModelResult, ModelRuntimeConfig, ModelStreamEvent, RetryConfig, RetryObservation,
    TransportErrorKind,
};

#[derive(Clone)]
enum Response {
    Ready {
        status: u16,
        chunks: Vec<ModelResult<Vec<u8>>>,
    },
    PendingConnect {
        started: Option<Arc<Notify>>,
        cancelled: Option<Arc<Notify>>,
    },
    PendingBody {
        started: Option<Arc<Notify>>,
        cancelled: Option<Arc<Notify>>,
    },
    PendingDecoder,
}

struct FakeTransport {
    responses: Mutex<VecDeque<Response>>,
    calls: AtomicUsize,
}

struct NotifyOnDrop(Option<Arc<Notify>>);

impl Drop for NotifyOnDrop {
    fn drop(&mut self) {
        if let Some(notify) = self.0.as_ref() {
            notify.notify_one();
        }
    }
}

impl FakeTransport {
    fn new(responses: Vec<Response>) -> Self {
        Self {
            responses: Mutex::new(VecDeque::from(responses)),
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl HttpTransport for FakeTransport {
    async fn send(
        &self,
        _request: HttpRequest,
        cancellation: CancellationToken,
    ) -> ModelResult<HttpResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let response = self.responses.lock().expect("response lock").pop_front();
        match response {
            Some(Response::Ready { status, chunks }) => Ok(HttpResponse::new(
                status,
                Some("request_123".into()),
                Box::pin(stream::iter(chunks)),
                cancellation,
            )),
            Some(Response::PendingConnect { started, cancelled }) => {
                let _cancelled = NotifyOnDrop(cancelled);
                if let Some(started) = started {
                    started.notify_one();
                }
                cancellation.cancelled().await;
                Err(ModelError::cancelled())
            }
            Some(Response::PendingBody { started, cancelled }) => {
                let cancellation_for_body = cancellation.clone();
                let cancelled = NotifyOnDrop(cancelled);
                let body: HttpBody = Box::pin(stream::poll_fn(move |_| {
                    let _ = &cancelled;
                    if let Some(started) = started.as_ref() {
                        started.notify_one();
                    }
                    if cancellation_for_body.is_cancelled() {
                        return Poll::Ready(None);
                    }
                    Poll::Pending
                }));
                Ok(HttpResponse::new(200, None, body, cancellation))
            }
            Some(Response::PendingDecoder) => Ok(HttpResponse::new(
                200,
                None,
                Box::pin(stream::iter(vec![Ok(b"data: decode\n\n".to_vec())])),
                cancellation,
            )),
            None => Err(ModelError::transport(
                TransportErrorKind::Other,
                None::<&str>,
            )),
        }
    }
}

fn request() -> ModelResult<HttpRequest> {
    Ok(HttpRequest::new(reqwest::Request::new(
        reqwest::Method::GET,
        "https://example.test/stream".parse().expect("URL"),
    )))
}

fn decoder() -> SseDecoder {
    Arc::new(|event: SseEvent, _header_request_id: Option<String>| {
        if event.data == "retry" {
            return Err(ModelError::transport(
                TransportErrorKind::Connection,
                Some("fake"),
            ));
        }
        Ok(vec![ModelStreamEvent::TextDelta { text: event.data }])
    })
}

fn completion_decoder() -> SseCompletionDecoder {
    Arc::new(|| Ok(Vec::new()))
}

fn decoders() -> SseDecoderFactory {
    Arc::new(|| (decoder(), completion_decoder()))
}

fn config() -> RetryConfig {
    RetryConfig::default()
        .with_max_attempts(2)
        .with_base_delay(Duration::ZERO)
        .with_jitter(false)
}

fn stream_for(
    transport: Arc<FakeTransport>,
    cancellation: CancellationToken,
    config: RetryConfig,
) -> crate::ModelStream {
    retrying_http_sse_stream(
        config,
        cancellation,
        None,
        transport,
        Arc::new(request),
        Arc::<str>::from("fake"),
        decoders(),
    )
}

#[tokio::test]
async fn abort_cancels_only_model_stream_child() {
    let parent = CancellationToken::new();
    let mut stream = Box::pin(cancellable_stream(
        stream::pending::<ModelResult<ModelStreamEvent>>(),
        parent.clone(),
    ));
    stream.abort();
    let error = timeout(Duration::from_millis(100), stream.next())
        .await
        .expect("abort must wake pending consumer")
        .expect("cancelled event")
        .unwrap_err();
    assert!(error.is_cancelled());
    assert!(!parent.is_cancelled());
}

#[tokio::test]
async fn external_cancellation_wakes_pending_stream() {
    let parent = CancellationToken::new();
    let mut stream = Box::pin(cancellable_stream(
        stream::pending::<ModelResult<ModelStreamEvent>>(),
        parent.clone(),
    ));
    parent.cancel();
    let error = timeout(Duration::from_millis(100), stream.next())
        .await
        .expect("external cancellation must wake consumer")
        .expect("cancelled event")
        .unwrap_err();
    assert!(error.is_cancelled());
}

#[tokio::test]
async fn fake_http_sse_chain_retries_before_decoded_delta() {
    let transport = Arc::new(FakeTransport::new(vec![
        Response::Ready {
            status: 200,
            chunks: vec![],
        },
        Response::Ready {
            status: 200,
            chunks: vec![Ok(b"data: hello\n\ndata: [DONE]\n\n".to_vec())],
        },
    ]));
    let mut stream = Box::pin(stream_for(
        transport.clone(),
        CancellationToken::new(),
        config(),
    ));

    assert!(matches!(
        stream.next().await,
        Some(Ok(ModelStreamEvent::TextDelta { text })) if text == "hello"
    ));
    assert_eq!(transport.calls(), 2);
}

#[tokio::test]
async fn fake_http_sse_chain_retries_invalid_utf8_before_decoded_delta() {
    let transport = Arc::new(FakeTransport::new(vec![
        Response::Ready {
            status: 200,
            chunks: vec![Ok(b"data: \xff\n\n".to_vec())],
        },
        Response::Ready {
            status: 200,
            chunks: vec![Ok(b"data: hello\n\n".to_vec())],
        },
    ]));
    let mut stream = Box::pin(stream_for(
        transport.clone(),
        CancellationToken::new(),
        config(),
    ));

    assert!(matches!(
        stream.next().await,
        Some(Ok(ModelStreamEvent::TextDelta { text })) if text == "hello"
    ));
    assert_eq!(transport.calls(), 2);
}

#[tokio::test]
async fn fake_http_sse_chain_returns_midstream_failure_after_delta_without_retry() {
    let transport = Arc::new(FakeTransport::new(vec![Response::Ready {
        status: 200,
        chunks: vec![
            Ok(b"data: partial\n\n".to_vec()),
            Err(ModelError::transport(
                TransportErrorKind::Connection,
                Some("fake"),
            )),
        ],
    }]));
    let mut stream = Box::pin(stream_for(
        transport.clone(),
        CancellationToken::new(),
        config(),
    ));

    assert!(matches!(
        stream.next().await,
        Some(Ok(ModelStreamEvent::TextDelta { text })) if text == "partial"
    ));
    assert!(matches!(stream.next().await, Some(Err(error)) if error.provider() == Some("fake")));
    assert_eq!(transport.calls(), 1);
}

#[tokio::test]
async fn runtime_config_forwards_safe_retry_observations_to_registered_observer() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let runtime = {
        let observed = observed.clone();
        ModelRuntimeConfig::default()
            .with_retry(config())
            .with_retry_observer(Arc::new(move |observation: RetryObservation| {
                observed.lock().expect("observation lock").push(observation);
            }))
    };
    let transport = Arc::new(FakeTransport::new(vec![
        Response::Ready {
            status: 429,
            chunks: vec![],
        },
        Response::Ready {
            status: 200,
            chunks: vec![Ok(b"data: hello\n\n".to_vec())],
        },
    ]));
    let mut stream = Box::pin(runtime_http_sse_stream(
        &runtime,
        CancellationToken::new(),
        transport,
        Arc::new(request),
        Arc::<str>::from("fake"),
        decoders(),
    ));

    assert!(matches!(
        stream.next().await,
        Some(Ok(ModelStreamEvent::TextDelta { .. }))
    ));
    let observed = observed.lock().expect("observation lock");
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].attempt(), 1);
    assert_eq!(observed[0].max_attempts(), 2);
    assert_eq!(observed[0].delay(), Duration::ZERO);
    assert_eq!(observed[0].error_kind(), crate::RetryErrorKind::HttpStatus);
}

#[tokio::test]
async fn external_cancellation_stops_fake_transport_connect() {
    let transport = Arc::new(FakeTransport::new(vec![Response::PendingConnect {
        started: None,
        cancelled: None,
    }]));
    let cancellation = CancellationToken::new();
    let mut stream = Box::pin(stream_for(
        transport.clone(),
        cancellation.clone(),
        config(),
    ));

    tokio::task::yield_now().await;
    cancellation.cancel();
    let error = timeout(Duration::from_millis(100), stream.next())
        .await
        .expect("connect cancellation must resolve")
        .expect("cancelled event")
        .unwrap_err();
    assert!(error.is_cancelled());
    assert_eq!(transport.calls(), 1);
}

#[tokio::test]
async fn external_cancellation_stops_fake_sse_body_read() {
    let transport = Arc::new(FakeTransport::new(vec![Response::PendingBody {
        started: None,
        cancelled: None,
    }]));
    let cancellation = CancellationToken::new();
    let mut stream = Box::pin(stream_for(
        transport.clone(),
        cancellation.clone(),
        config(),
    ));

    tokio::task::yield_now().await;
    cancellation.cancel();
    let error = timeout(Duration::from_millis(100), stream.next())
        .await
        .expect("SSE read cancellation must resolve")
        .expect("cancelled event")
        .unwrap_err();
    assert!(error.is_cancelled());
    assert_eq!(transport.calls(), 1);
}

#[tokio::test]
async fn external_cancellation_stops_fake_sse_decoder() {
    let transport = Arc::new(FakeTransport::new(vec![Response::PendingDecoder]));
    let cancellation = CancellationToken::new();
    let decoder_started = Arc::new(Notify::new());
    let decoder: AsyncSseDecoder = {
        let decoder_started = decoder_started.clone();
        Arc::new(move |_, cancellation| {
            let decoder_started = decoder_started.clone();
            Box::pin(async move {
                decoder_started.notify_one();
                cancellation.cancelled().await;
                Err(ModelError::cancelled())
            })
        })
    };
    let stream = retrying_http_sse_stream_async(
        config(),
        cancellation.clone(),
        None,
        transport.clone(),
        Arc::new(request),
        Arc::<str>::from("fake"),
        decoder,
    );
    let read = tokio::spawn(async move { stream.into_future().await.0 });

    timeout(Duration::from_millis(100), decoder_started.notified())
        .await
        .expect("decoder must begin before cancellation");
    cancellation.cancel();
    let error = timeout(Duration::from_millis(100), read)
        .await
        .expect("decoder cancellation must resolve")
        .expect("read task must not panic")
        .expect("cancelled event")
        .unwrap_err();
    assert!(error.is_cancelled());
    assert_eq!(transport.calls(), 1);
}

#[tokio::test]
async fn external_cancellation_stops_fake_transport_backoff() {
    let transport = Arc::new(FakeTransport::new(vec![Response::Ready {
        status: 429,
        chunks: vec![],
    }]));
    let cancellation = CancellationToken::new();
    let retry = RetryConfig::default()
        .with_max_attempts(2)
        .with_base_delay(Duration::from_secs(60))
        .with_jitter(false);
    let mut stream = Box::pin(stream_for(transport.clone(), cancellation.clone(), retry));

    tokio::task::yield_now().await;
    cancellation.cancel();
    let error = timeout(Duration::from_millis(100), stream.next())
        .await
        .expect("backoff cancellation must resolve")
        .expect("cancelled event")
        .unwrap_err();
    assert!(error.is_cancelled());
    assert_eq!(transport.calls(), 1);
}

#[tokio::test]
async fn abort_stops_fake_transport_connect_without_cancelling_parent() {
    let parent = CancellationToken::new();
    let connect_started = Arc::new(Notify::new());
    let connect_cancelled = Arc::new(Notify::new());
    let transport = Arc::new(FakeTransport::new(vec![Response::PendingConnect {
        started: Some(connect_started.clone()),
        cancelled: Some(connect_cancelled.clone()),
    }]));
    let mut stream = Box::pin(stream_for(transport.clone(), parent.clone(), config()));

    timeout(Duration::from_millis(100), connect_started.notified())
        .await
        .expect("fake transport connect must begin before abort");
    stream.abort();
    timeout(Duration::from_millis(100), connect_cancelled.notified())
        .await
        .expect("abort must cancel fake transport connect");
    let error = timeout(Duration::from_millis(100), stream.next())
        .await
        .expect("abort must wake consumer")
        .expect("cancelled event")
        .unwrap_err();
    assert!(error.is_cancelled());
    assert!(stream.next().await.is_none());
    assert_eq!(transport.calls(), 1);
    assert!(!parent.is_cancelled());
}

#[tokio::test]
async fn abort_stops_fake_sse_body_read_without_cancelling_parent() {
    let parent = CancellationToken::new();
    let body_started = Arc::new(Notify::new());
    let body_cancelled = Arc::new(Notify::new());
    let transport = Arc::new(FakeTransport::new(vec![Response::PendingBody {
        started: Some(body_started.clone()),
        cancelled: Some(body_cancelled.clone()),
    }]));
    let mut stream = Box::pin(stream_for(transport.clone(), parent.clone(), config()));

    timeout(Duration::from_millis(100), body_started.notified())
        .await
        .expect("fake SSE body read must begin before abort");
    stream.abort();
    timeout(Duration::from_millis(100), body_cancelled.notified())
        .await
        .expect("abort must cancel fake SSE body read");
    let error = timeout(Duration::from_millis(100), stream.next())
        .await
        .expect("abort must wake consumer")
        .expect("cancelled event")
        .unwrap_err();
    assert!(error.is_cancelled());
    assert!(stream.next().await.is_none());
    assert_eq!(transport.calls(), 1);
    assert!(!parent.is_cancelled());
}

#[tokio::test]
async fn abort_stops_fake_sse_decoder_without_cancelling_parent() {
    let parent = CancellationToken::new();
    let decoder_started = Arc::new(Notify::new());
    let transport = Arc::new(FakeTransport::new(vec![Response::PendingDecoder]));
    let decoder: AsyncSseDecoder = {
        let decoder_started = decoder_started.clone();
        Arc::new(move |_, cancellation| {
            let decoder_started = decoder_started.clone();
            Box::pin(async move {
                decoder_started.notify_one();
                cancellation.cancelled().await;
                Err(ModelError::cancelled())
            })
        })
    };
    let mut stream = Box::pin(retrying_http_sse_stream_async(
        config(),
        parent.clone(),
        None,
        transport.clone(),
        Arc::new(request),
        Arc::<str>::from("fake"),
        decoder,
    ));

    timeout(Duration::from_millis(100), decoder_started.notified())
        .await
        .expect("fake SSE decoder must begin before abort");
    stream.abort();
    let error = timeout(Duration::from_millis(100), stream.next())
        .await
        .expect("abort must wake consumer")
        .expect("cancelled event")
        .unwrap_err();
    assert!(error.is_cancelled());
    assert!(stream.next().await.is_none());
    assert_eq!(transport.calls(), 1);
    assert!(!parent.is_cancelled());
}

#[tokio::test]
async fn abort_stops_fake_transport_backoff_without_cancelling_parent() {
    let parent = CancellationToken::new();
    let backoff_started = Arc::new(Notify::new());
    let observer = {
        let backoff_started = backoff_started.clone();
        Arc::new(move |_: RetryObservation| backoff_started.notify_one())
    };
    let transport = Arc::new(FakeTransport::new(vec![Response::Ready {
        status: 429,
        chunks: vec![],
    }]));
    let retry = RetryConfig::default()
        .with_max_attempts(2)
        .with_base_delay(Duration::from_secs(60))
        .with_jitter(false);
    let mut stream = Box::pin(retrying_http_sse_stream(
        retry,
        parent.clone(),
        Some(observer),
        transport.clone(),
        Arc::new(request),
        Arc::<str>::from("fake"),
        decoders(),
    ));

    timeout(Duration::from_millis(100), backoff_started.notified())
        .await
        .expect("fake transport must enter retry backoff before abort");
    stream.abort();
    let error = timeout(Duration::from_millis(100), stream.next())
        .await
        .expect("abort must wake consumer")
        .expect("cancelled event")
        .unwrap_err();
    assert!(error.is_cancelled());
    assert!(stream.next().await.is_none());
    assert_eq!(transport.calls(), 1);
    assert!(!parent.is_cancelled());
}

#[tokio::test]
async fn drop_stops_fake_transport_connect_without_cancelling_parent() {
    let parent = CancellationToken::new();
    let connect_started = Arc::new(Notify::new());
    let connect_cancelled = Arc::new(Notify::new());
    let transport = Arc::new(FakeTransport::new(vec![Response::PendingConnect {
        started: Some(connect_started.clone()),
        cancelled: Some(connect_cancelled.clone()),
    }]));
    let stream = stream_for(transport.clone(), parent.clone(), config());

    timeout(Duration::from_millis(100), connect_started.notified())
        .await
        .expect("fake transport connect must begin before drop");
    drop(stream);
    timeout(Duration::from_millis(100), connect_cancelled.notified())
        .await
        .expect("drop must release fake transport connect");
    assert_eq!(transport.calls(), 1);
    assert!(!parent.is_cancelled());
}

#[tokio::test]
async fn drop_stops_fake_sse_body_read_without_cancelling_parent() {
    let parent = CancellationToken::new();
    let body_started = Arc::new(Notify::new());
    let body_cancelled = Arc::new(Notify::new());
    let transport = Arc::new(FakeTransport::new(vec![Response::PendingBody {
        started: Some(body_started.clone()),
        cancelled: Some(body_cancelled.clone()),
    }]));
    let stream = stream_for(transport.clone(), parent.clone(), config());

    timeout(Duration::from_millis(100), body_started.notified())
        .await
        .expect("fake SSE body read must begin before drop");
    drop(stream);
    timeout(Duration::from_millis(100), body_cancelled.notified())
        .await
        .expect("drop must release fake SSE body read");
    assert_eq!(transport.calls(), 1);
    assert!(!parent.is_cancelled());
}

#[tokio::test]
async fn drop_stops_fake_transport_backoff_without_cancelling_parent() {
    let parent = CancellationToken::new();
    let backoff_started = Arc::new(Notify::new());
    let observer = {
        let backoff_started = backoff_started.clone();
        Arc::new(move |_: RetryObservation| backoff_started.notify_one())
    };
    let transport = Arc::new(FakeTransport::new(vec![Response::Ready {
        status: 429,
        chunks: vec![],
    }]));
    let retry = RetryConfig::default()
        .with_max_attempts(2)
        .with_base_delay(Duration::from_secs(60))
        .with_jitter(false);
    let stream = retrying_http_sse_stream(
        retry,
        parent.clone(),
        Some(observer),
        transport.clone(),
        Arc::new(request),
        Arc::<str>::from("fake"),
        decoders(),
    );

    timeout(Duration::from_millis(100), backoff_started.notified())
        .await
        .expect("fake transport must enter retry backoff before drop");
    drop(stream);
    tokio::task::yield_now().await;
    assert_eq!(transport.calls(), 1);
    assert!(!parent.is_cancelled());
}
