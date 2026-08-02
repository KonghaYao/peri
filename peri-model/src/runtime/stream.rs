use std::{collections::VecDeque, future::Future, pin::Pin, sync::Arc};

use futures::{Stream, StreamExt};
use tokio_util::sync::CancellationToken;

use crate::transport::{HttpRequest, HttpResponse, HttpTransport, SseEvent, SseParser};
use crate::{
    ModelError, ModelResult, ModelRuntimeConfig, ModelStream, ModelStreamEvent, RetryConfig,
    RetryObserver,
};

use super::retry::{retrying_stream, StreamAttempt};

/// Provider decoder 使用的 crate-private SSE 事件转换器。
pub(crate) type SseDecoder =
    Arc<dyn Fn(SseEvent, Option<String>) -> ModelResult<Vec<ModelStreamEvent>> + Send + Sync>;
pub(crate) type SseCompletionDecoder =
    Arc<dyn Fn() -> ModelResult<Vec<ModelStreamEvent>> + Send + Sync>;
pub(crate) type SseDecoders = (SseDecoder, SseCompletionDecoder);
pub(crate) type SseDecoderFactory = Arc<dyn Fn() -> SseDecoders + Send + Sync>;
type DecoderFuture = Pin<Box<dyn Future<Output = ModelResult<Vec<ModelStreamEvent>>> + Send>>;
pub(crate) type AsyncSseDecoder =
    Arc<dyn Fn(SseEvent, CancellationToken) -> DecoderFuture + Send + Sync>;

/// 使用独立 child token 包装事件流，确保外部取消与 `abort()` 都能终止消费，且不会取消父 token。
pub(crate) fn cancellable_stream<S>(events: S, cancellation: CancellationToken) -> ModelStream
where
    S: Stream<Item = ModelResult<ModelStreamEvent>> + Send + 'static,
{
    ModelStream::with_cancellation(events, cancellation.child_token())
}

/// 将 crate-private HTTP seam、SSE parser、provider decoder 与通用 retry 串为一条可取消的流。
///
/// request factory 与 decoder 由后续 provider adapter 提供；本函数不认识 provider-native payload。
pub(crate) fn retrying_http_sse_stream(
    config: RetryConfig,
    cancellation: CancellationToken,
    observer: Option<Arc<dyn RetryObserver>>,
    transport: Arc<dyn HttpTransport>,
    request: Arc<dyn Fn() -> ModelResult<HttpRequest> + Send + Sync>,
    provider: Arc<str>,
    decoders: SseDecoderFactory,
) -> ModelStream {
    let attempt: StreamAttempt = Arc::new(move |attempt_cancellation| {
        let transport = transport.clone();
        let request = request.clone();
        let provider = provider.clone();
        let (decoder, completion_decoder) = decoders();
        Box::pin(async move {
            let request = request()?;
            let response = transport
                .send(request, attempt_cancellation.clone())
                .await?;
            response_to_sse_stream(
                response,
                attempt_cancellation,
                provider,
                decoder,
                completion_decoder,
            )
        })
    });
    retrying_stream(config, cancellation, observer, attempt)
}

/// 使用运行时配置构造 HTTP/SSE/retry 链路，确保上层注册的安全 observer 生效。
pub(crate) fn runtime_http_sse_stream(
    runtime: &ModelRuntimeConfig,
    cancellation: CancellationToken,
    transport: Arc<dyn HttpTransport>,
    request: Arc<dyn Fn() -> ModelResult<HttpRequest> + Send + Sync>,
    provider: Arc<str>,
    decoders: SseDecoderFactory,
) -> ModelStream {
    retrying_http_sse_stream(
        runtime.retry().clone(),
        cancellation,
        runtime.retry_observer(),
        transport,
        request,
        provider,
        decoders,
    )
}

/// 将 HTTP seam、SSE parser、可取消的 provider decoder 与 retry 串为一条通用内部链路。
pub(crate) fn retrying_http_sse_stream_async(
    config: RetryConfig,
    cancellation: CancellationToken,
    observer: Option<Arc<dyn RetryObserver>>,
    transport: Arc<dyn HttpTransport>,
    request: Arc<dyn Fn() -> ModelResult<HttpRequest> + Send + Sync>,
    provider: Arc<str>,
    decoder: AsyncSseDecoder,
) -> ModelStream {
    let attempt: StreamAttempt = Arc::new(move |attempt_cancellation| {
        let transport = transport.clone();
        let request = request.clone();
        let provider = provider.clone();
        let decoder = decoder.clone();
        Box::pin(async move {
            let request = request()?;
            let response = transport
                .send(request, attempt_cancellation.clone())
                .await?;
            response_to_async_sse_stream(response, attempt_cancellation, provider, decoder)
        })
    });
    retrying_stream(config, cancellation, observer, attempt)
}

fn response_to_async_sse_stream(
    response: HttpResponse,
    cancellation: CancellationToken,
    provider: Arc<str>,
    decoder: AsyncSseDecoder,
) -> ModelResult<ModelStream> {
    if !(200..=299).contains(&response.status) {
        return Err(ModelError::http_status(
            response.status,
            provider.as_ref(),
            response.request_id.as_deref(),
        ));
    }

    let stream_provider = provider.clone();
    let events = futures::stream::unfold(
        AsyncSseReadState {
            body: response.body,
            parser: SseParser::new(),
            pending: VecDeque::new(),
            cancellation: cancellation.clone(),
            decoder,
            done: false,
        },
        move |mut state| {
            let provider = stream_provider.clone();
            async move {
                loop {
                    if let Some(event) = state.pending.pop_front() {
                        return Some((Ok(event), state));
                    }
                    if state.done {
                        return None;
                    }
                    let chunk = tokio::select! {
                        biased;
                        _ = state.cancellation.cancelled() => return Some((Err(ModelError::cancelled()), state)),
                        chunk = state.body.next() => chunk,
                    };
                    match chunk {
                        Some(Ok(bytes)) => {
                            let parsed = match state.parser.push(&bytes) {
                                Ok(events) => events,
                                Err(error) => return Some((Err(error), state)),
                            };
                            for event in parsed {
                                let decoded = tokio::select! {
                                    biased;
                                    _ = state.cancellation.cancelled() => return Some((Err(ModelError::cancelled()), state)),
                                    decoded = (state.decoder)(event, state.cancellation.clone()) => decoded,
                                };
                                match decoded {
                                    Ok(events) => state.pending.extend(events),
                                    Err(error) => return Some((Err(error), state)),
                                }
                            }
                            state.done = state.parser.is_done();
                        }
                        Some(Err(error)) => return Some((Err(error), state)),
                        None => {
                            return Some((
                                Err(ModelError::transport(
                                    crate::TransportErrorKind::Other,
                                    Some(provider.as_ref()),
                                )),
                                state,
                            ));
                        }
                    }
                }
            }
        },
    );
    Ok(ModelStream::with_cancellation(
        events,
        cancellation.child_token(),
    ))
}

fn response_to_sse_stream(
    response: HttpResponse,
    cancellation: CancellationToken,
    provider: Arc<str>,
    decoder: SseDecoder,
    completion_decoder: SseCompletionDecoder,
) -> ModelResult<ModelStream> {
    if !(200..=299).contains(&response.status) {
        return Err(ModelError::http_status(
            response.status,
            provider.as_ref(),
            response.request_id.as_deref(),
        ));
    }

    let stream_provider = provider.clone();
    let events = futures::stream::unfold(
        SseReadState {
            body: response.body,
            parser: SseParser::new(),
            pending: VecDeque::new(),
            cancellation: cancellation.clone(),
            decoder,
            completion_decoder,
            request_id: response.request_id,
            done: false,
        },
        move |mut state| {
            let provider = stream_provider.clone();
            async move {
                loop {
                    if let Some(event) = state.pending.pop_front() {
                        return Some((Ok(event), state));
                    }
                    if state.done {
                        return None;
                    }
                    let chunk = tokio::select! {
                        biased;
                        _ = state.cancellation.cancelled() => return Some((Err(ModelError::cancelled()), state)),
                        chunk = state.body.next() => chunk,
                    };
                    match chunk {
                        Some(Ok(bytes)) => {
                            let parsed = match state.parser.push(&bytes) {
                                Ok(events) => events,
                                Err(error) => return Some((Err(error), state)),
                            };
                            for event in parsed {
                                match (state.decoder)(event, state.request_id.clone()) {
                                    Ok(events) => state.pending.extend(events),
                                    Err(error) => return Some((Err(error), state)),
                                }
                            }
                            if state.parser.is_done() {
                                match (state.completion_decoder)() {
                                    Ok(events) => state.pending.extend(events),
                                    Err(error) => return Some((Err(error), state)),
                                }
                                state.done = true;
                            }
                        }
                        Some(Err(error)) => return Some((Err(error), state)),
                        None => {
                            return Some((
                                Err(ModelError::transport(
                                    crate::TransportErrorKind::Other,
                                    Some(provider.as_ref()),
                                )),
                                state,
                            ));
                        }
                    }
                }
            }
        },
    );
    Ok(ModelStream::with_cancellation(
        events,
        cancellation.child_token(),
    ))
}

struct AsyncSseReadState {
    body: crate::transport::HttpBody,
    parser: SseParser,
    pending: VecDeque<ModelStreamEvent>,
    cancellation: CancellationToken,
    decoder: AsyncSseDecoder,
    done: bool,
}

struct SseReadState {
    body: crate::transport::HttpBody,
    parser: SseParser,
    pending: VecDeque<ModelStreamEvent>,
    cancellation: CancellationToken,
    decoder: SseDecoder,
    completion_decoder: SseCompletionDecoder,
    request_id: Option<String>,
    done: bool,
}

#[cfg(test)]
#[path = "stream_test.rs"]
mod stream_test;
