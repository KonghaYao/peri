use std::{sync::Arc, time::Duration};

use futures::{stream, StreamExt};
use tokio_util::sync::CancellationToken;

use super::{read_http_error, response_to_sse_stream, SseDecoders};
use crate::{
    runtime::retry::{retrying_stream, StreamAttempt},
    transport::{HttpBody, HttpResponse},
    ModelError, ModelStream, ModelStreamEvent, ProtocolErrorKind, RetryConfig, TransportErrorKind,
};

fn decoders() -> SseDecoders {
    (Arc::new(|_, _| Ok(Vec::new())), Arc::new(|| Ok(Vec::new())))
}

async fn http_error(status: u16, chunks: Vec<crate::ModelResult<Vec<u8>>>) -> ModelError {
    let cancellation = CancellationToken::new();
    let response = HttpResponse::new(
        status,
        Some("request secret 诊断".into()),
        Box::pin(stream::iter(chunks)),
        cancellation.clone(),
    );
    let (decoder, completion) = decoders();
    response_to_sse_stream(
        response,
        cancellation,
        "provider secret".into(),
        decoder,
        completion,
    )
    .await
    .err()
    .expect("HTTP error")
}

#[tokio::test]
async fn non_success_http_preserves_chunked_body_and_identity() {
    let error = http_error(
        401,
        vec![
            Ok(b"{\"error\":\"sk-live-".to_vec()),
            Ok(b"secret\"}".to_vec()),
        ],
    )
    .await;
    let diagnostic = error.diagnostic();
    assert_eq!(diagnostic.status(), Some(401));
    assert_eq!(diagnostic.request_id(), Some("request secret 诊断"));
    assert_eq!(diagnostic.body(), Some("{\"error\":\"sk-live-secret\"}"));
    assert!(error.to_string().contains("sk-live-secret"));
}

#[tokio::test]
async fn http_body_read_failure_preserves_status_partial_body_and_cause() {
    let cause = ModelError::transport(TransportErrorKind::Connection, None::<&str>)
        .with_message("connection reset at secret endpoint")
        .with_causes(vec!["socket cause".into()]);
    let error = http_error(503, vec![Ok(b"partial body".to_vec()), Err(cause)]).await;
    let diagnostic = error.diagnostic();
    assert_eq!(diagnostic.status(), Some(503));
    assert_eq!(diagnostic.body(), Some("partial body"));
    assert!(diagnostic.causes()[0].contains("connection reset at secret endpoint"));
    assert_eq!(diagnostic.causes()[1], "socket cause");
}

#[tokio::test]
async fn http_error_body_limit_stops_reading_without_waiting_for_eof() {
    let cancellation = CancellationToken::new();
    let body: HttpBody = Box::pin(
        stream::once(async { Ok(vec![b'x'; super::super::error::MAX_DIAGNOSTIC_BYTES + 1]) })
            .chain(stream::pending()),
    );
    let response = HttpResponse::new(429, None, body, cancellation.clone());
    let (decoder, completion) = decoders();
    let error = tokio::time::timeout(
        Duration::from_secs(1),
        response_to_sse_stream(
            response,
            cancellation,
            "provider".into(),
            decoder,
            completion,
        ),
    )
    .await
    .expect("body must be bounded")
    .err()
    .expect("HTTP error");
    assert_eq!(
        error.diagnostic().body().unwrap().len(),
        super::super::error::MAX_DIAGNOSTIC_BYTES
    );
    assert!(error.diagnostic().body().unwrap().ends_with("[TRUNCATED]"));
}

#[tokio::test]
async fn http_error_body_has_total_timeout_for_pending_and_empty_chunks() {
    for endless_empty_chunks in [false, true] {
        let cancellation = CancellationToken::new();
        let body: HttpBody = if endless_empty_chunks {
            Box::pin(
                stream::once(async { Ok(b"partial secret body".to_vec()) })
                    .chain(stream::repeat(Ok(Vec::new()))),
            )
        } else {
            Box::pin(
                stream::once(async { Ok(b"partial secret body".to_vec()) })
                    .chain(stream::pending()),
            )
        };
        let response = HttpResponse::new(503, None, body, cancellation.clone());
        let error = peri_time::timeout(
            Duration::from_secs(1),
            read_http_error(
                response,
                cancellation,
                "provider".into(),
                Duration::from_millis(20),
            ),
        )
        .await
        .expect("error body collection must have a total deadline");
        let diagnostic = error.diagnostic();
        assert_eq!(diagnostic.status(), Some(503));
        assert_eq!(diagnostic.body(), Some("partial secret body"));
        assert!(diagnostic.causes()[0].contains("timed out after 20 ms"));
        assert!(!error.is_cancelled());
    }
}

#[tokio::test]
async fn cancellation_interrupts_http_error_body_collection() {
    let cancellation = CancellationToken::new();
    let response = HttpResponse::new(503, None, Box::pin(stream::pending()), cancellation.clone());
    let (decoder, completion) = decoders();
    let task_cancellation = cancellation.clone();
    let task = tokio::spawn(async move {
        response_to_sse_stream(
            response,
            task_cancellation,
            "provider".into(),
            decoder,
            completion,
        )
        .await
    });
    tokio::task::yield_now().await;
    cancellation.cancel();
    let error = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .err()
        .unwrap();
    assert!(error.is_cancelled());
}

#[tokio::test]
async fn retry_exhaustion_and_observer_preserve_actual_error_details() {
    let error = http_error(503, vec![Ok(b"provider secret response".to_vec())])
        .await
        .with_message("provider failed")
        .with_causes(vec!["actual cause".into()]);
    let expected = error.diagnostic();
    let attempt: StreamAttempt = Arc::new(move |_| {
        let error = error.clone();
        Box::pin(async move { Err(error) })
    });
    let observations = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = observations.clone();
    let observer = Arc::new(move |observation: crate::RetryObservation| {
        observed.lock().unwrap().push(observation);
    });
    let mut events = retrying_stream(
        RetryConfig::default()
            .with_max_attempts(2)
            .with_base_delay(Duration::ZERO)
            .with_jitter(false),
        CancellationToken::new(),
        Some(observer),
        attempt,
    );
    let exhausted = events.next().await.unwrap().unwrap_err().diagnostic();
    assert_eq!(exhausted.retry_attempts(), Some(2));
    assert_eq!(exhausted.body(), expected.body());
    assert_eq!(exhausted.message(), expected.message());
    assert_eq!(exhausted.causes(), expected.causes());
    let observations = observations.lock().unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(
        observations[0].diagnostic().unwrap().body(),
        expected.body()
    );
}

#[tokio::test]
async fn visible_delta_interruption_preserves_transport_and_protocol_diagnostics() {
    for error in [
        ModelError::transport(TransportErrorKind::Connection, Some("provider secret")),
        ModelError::protocol(ProtocolErrorKind::Provider),
    ] {
        let error = error
            .with_message("actual failure 诊断")
            .with_body("provider raw body")
            .with_causes(vec!["root cause".into()]);
        let expected = error.diagnostic();
        let attempt: StreamAttempt = Arc::new(move |_| {
            let error = error.clone();
            Box::pin(async move {
                Ok(ModelStream::new(stream::iter(vec![
                    Ok(ModelStreamEvent::TextDelta {
                        text: "partial".into(),
                    }),
                    Err(error),
                ])))
            })
        });
        let mut events = retrying_stream(
            RetryConfig::default(),
            CancellationToken::new(),
            None,
            attempt,
        );
        assert!(matches!(
            events.next().await,
            Some(Ok(ModelStreamEvent::TextDelta { .. }))
        ));
        let ModelStreamEvent::Interrupted {
            error, attempts, ..
        } = events.next().await.unwrap().unwrap()
        else {
            panic!("interrupted expected");
        };
        assert_eq!(attempts, 1);
        assert_eq!(error.diagnostic().message(), expected.message());
        assert_eq!(error.diagnostic().body(), expected.body());
        assert_eq!(error.diagnostic().causes(), expected.causes());
        assert_eq!(error.interruption_diagnostic(), Some(&expected));
        assert!(events.next().await.is_none());
    }
}
