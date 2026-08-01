use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use async_trait::async_trait;
use futures::{stream, StreamExt};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use super::{HttpBody, HttpRequest, HttpResponse, HttpTransport};
use crate::{ModelError, ModelResult, TransportErrorKind};

#[derive(Clone)]
enum Response {
    Ready {
        status: u16,
        chunks: Vec<ModelResult<Vec<u8>>>,
    },
    PendingConnect,
}

struct FakeTransport {
    responses: Mutex<VecDeque<Response>>,
    calls: AtomicUsize,
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
            Some(Response::PendingConnect) => {
                cancellation.cancelled().await;
                Err(ModelError::cancelled())
            }
            None => Err(ModelError::transport(
                TransportErrorKind::Other,
                None::<&str>,
            )),
        }
    }
}

fn request() -> HttpRequest {
    HttpRequest::new(reqwest::Request::new(
        reqwest::Method::GET,
        "https://example.test/stream".parse().expect("URL"),
    ))
}

#[tokio::test]
async fn fake_transport_returns_connect_status_and_chunked_body() {
    let transport = FakeTransport::new(vec![Response::Ready {
        status: 201,
        chunks: vec![Ok(b"first".to_vec()), Ok(b"second".to_vec())],
    }]);
    let cancellation = CancellationToken::new();

    let mut response = transport
        .send(request(), cancellation)
        .await
        .expect("connect");
    assert_eq!(response.status, 201);
    assert_eq!(response.request_id.as_deref(), Some("request_123"));
    assert_eq!(
        response.body.next().await.expect("first chunk").unwrap(),
        b"first"
    );
    assert_eq!(
        response.body.next().await.expect("second chunk").unwrap(),
        b"second"
    );
    assert!(response.body.next().await.is_none());
    assert_eq!(transport.calls(), 1);
}

#[tokio::test]
async fn fake_transport_reports_midstream_body_failure() {
    let transport = FakeTransport::new(vec![Response::Ready {
        status: 200,
        chunks: vec![
            Ok(b"before".to_vec()),
            Err(ModelError::transport(
                TransportErrorKind::Connection,
                None::<&str>,
            )),
        ],
    }]);
    let cancellation = CancellationToken::new();

    let mut response = transport
        .send(request(), cancellation)
        .await
        .expect("connect");
    assert_eq!(
        response.body.next().await.expect("first chunk").unwrap(),
        b"before"
    );
    assert!(matches!(
        response.body.next().await.expect("body error"),
        Err(error) if error.transport_kind() == Some(TransportErrorKind::Connection)
    ));
}

#[tokio::test]
async fn fake_transport_connect_wait_observes_cancellation() {
    let transport = Arc::new(FakeTransport::new(vec![Response::PendingConnect]));
    let cancellation = CancellationToken::new();
    let send = {
        let transport = transport.clone();
        let cancellation = cancellation.clone();
        tokio::spawn(async move { transport.send(request(), cancellation).await })
    };

    tokio::task::yield_now().await;
    cancellation.cancel();
    let result = timeout(Duration::from_millis(100), send)
        .await
        .expect("connect cancellation must resolve")
        .expect("task must not panic");
    assert!(matches!(result, Err(error) if error.is_cancelled()));
    assert_eq!(transport.calls(), 1);
}

#[tokio::test]
async fn fake_transport_body_read_observes_cancellation() {
    let cancellation = CancellationToken::new();
    let body: HttpBody = Box::pin(stream::pending());
    let mut response = HttpResponse::new(200, None, body, cancellation.clone());
    let read = tokio::spawn(async move { response.body.next().await });

    tokio::task::yield_now().await;
    cancellation.cancel();
    let item = timeout(Duration::from_millis(100), read)
        .await
        .expect("body cancellation must resolve")
        .expect("task must not panic")
        .expect("cancelled body item");
    assert!(matches!(item, Err(error) if error.is_cancelled()));
}
