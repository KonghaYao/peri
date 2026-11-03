use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

const ERROR_BODY_READ_TIMEOUT: Duration = Duration::from_secs(2);

use futures::StreamExt;
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
type AuthenticatedRequestFactory =
    Arc<dyn Fn(&crate::RequestCredential) -> ModelResult<HttpRequest> + Send + Sync>;

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
            .await
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

/// Resolve authentication inside the shared attempt budget, never during prepare.
/// Admission failures can renew once; the next send is a normal retry attempt.
pub(crate) fn runtime_authenticated_http_sse_stream(
    runtime: &ModelRuntimeConfig,
    cancellation: CancellationToken,
    transport: Arc<dyn HttpTransport>,
    request: AuthenticatedRequestFactory,
    provider: Arc<str>,
    decoders: SseDecoderFactory,
    auth: Arc<dyn crate::RequestAuth>,
) -> ModelStream {
    let call_cancellation = cancellation.child_token();
    let watcher_stop = CancellationToken::new();
    if let Some(session_cancellation) = auth.cancellation_token() {
        if session_cancellation.is_cancelled() {
            call_cancellation.cancel();
        } else {
            let call = call_cancellation.clone();
            let stop = watcher_stop.clone();
            tokio::spawn(async move {
                tokio::select! {
                    biased;
                    _ = call.cancelled() => {},
                    _ = stop.cancelled() => {},
                    _ = session_cancellation.cancelled() => call.cancel(),
                }
            });
        }
    }
    let refreshed = Arc::new(AtomicBool::new(false));
    let attempt: StreamAttempt = Arc::new(move |attempt_cancellation| {
        let transport = transport.clone();
        let request = request.clone();
        let provider = provider.clone();
        let auth = auth.clone();
        let refreshed = refreshed.clone();
        let (decoder, completion_decoder) = decoders();
        Box::pin(async move {
            let credential = tokio::select! {
                biased;
                _ = attempt_cancellation.cancelled() => return Err(ModelError::cancelled()),
                value = auth.credential(attempt_cancellation.clone()) => value?,
            };
            let response = transport
                .send(request(&credential)?, attempt_cancellation.clone())
                .await?;
            if response.status == 401 && !refreshed.swap(true, Ordering::AcqRel) {
                let renewed = tokio::select! {
                    biased;
                    _ = attempt_cancellation.cancelled() => return Err(ModelError::cancelled()),
                    value = auth.refresh_after_unauthorized(&credential, attempt_cancellation.clone()) => value?,
                };
                if renewed {
                    return Err(ModelError::protocol(
                        crate::ProtocolErrorKind::AuthenticationRefreshed,
                    ));
                }
            }
            response_to_sse_stream(
                response,
                attempt_cancellation,
                provider,
                decoder,
                completion_decoder,
            )
            .await
        })
    });
    let stream = retrying_stream(
        runtime.retry().clone(),
        call_cancellation.clone(),
        runtime.retry_observer(),
        attempt,
    );
    // Own a child of the caller token. Drop/abort also stops the logout watcher,
    // without ever cancelling sibling calls or the caller's session token.
    let events = futures::stream::unfold(
        (stream, WatcherStopGuard(watcher_stop)),
        |(mut stream, guard)| async move {
            let event = stream.next().await;
            if matches!(
                event,
                None | Some(Err(_))
                    | Some(Ok(
                        ModelStreamEvent::Completed(_) | ModelStreamEvent::Interrupted { .. }
                    ))
            ) {
                guard.0.cancel();
            }
            event.map(|event| (event, (stream, guard)))
        },
    );
    ModelStream::with_cancellation(events, call_cancellation)
}

struct WatcherStopGuard(CancellationToken);

impl Drop for WatcherStopGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

async fn response_to_sse_stream(
    response: HttpResponse,
    cancellation: CancellationToken,
    provider: Arc<str>,
    decoder: SseDecoder,
    completion_decoder: SseCompletionDecoder,
) -> ModelResult<ModelStream> {
    if !(200..=299).contains(&response.status) {
        return Err(
            read_http_error(response, cancellation, provider, ERROR_BODY_READ_TIMEOUT).await,
        );
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
                        return Some((event, state));
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
                            for event in state.parser.push(&bytes) {
                                match event.and_then(|event| {
                                    (state.decoder)(event, state.request_id.clone())
                                }) {
                                    Ok(events) => {
                                        for event in events {
                                            state.done =
                                                matches!(event, ModelStreamEvent::Completed(_));
                                            state.pending.push_back(Ok(event));
                                            if state.done {
                                                break;
                                            }
                                        }
                                    }
                                    Err(error) => {
                                        // 已解码的 delta 先交付，retry 才能识别为中断并保留断点。
                                        state.pending.push_back(Err(error));
                                        state.done = true;
                                    }
                                }
                                if state.done {
                                    break;
                                }
                            }
                            if !state.done && state.parser.is_done() {
                                match (state.completion_decoder)() {
                                    Ok(events) => state.pending.extend(events.into_iter().map(Ok)),
                                    Err(error) => state.pending.push_back(Err(error)),
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
                                )
                                .with_message("HTTP response ended before SSE completion")),
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

async fn read_http_error(
    mut response: HttpResponse,
    cancellation: CancellationToken,
    provider: Arc<str>,
    budget: Duration,
) -> ModelError {
    let mut error = ModelError::http_status(
        response.status,
        provider.as_ref(),
        response.request_id.as_deref(),
    );
    let deadline = peri_time::monotonic_now() + budget;
    let mut bytes = Vec::new();
    let limit = super::error::MAX_DIAGNOSTIC_BYTES + 1;
    while bytes.len() < limit {
        let chunk = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return ModelError::cancelled(),
            _ = peri_time::sleep_until(deadline) => {
                error = error.with_causes(vec![format!(
                    "HTTP error body read timed out after {} ms", budget.as_millis(),
                )]);
                break;
            }
            chunk = response.body.next() => chunk,
        };
        match chunk {
            Some(Ok(chunk)) => {
                let remaining = limit - bytes.len();
                bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
                tokio::task::yield_now().await;
            }
            Some(Err(cause)) if cause.is_cancelled() => return cause,
            Some(Err(cause)) => {
                let diagnostic = cause.diagnostic();
                error = error.with_causes(
                    std::iter::once(cause.to_string()).chain(diagnostic.causes().iter().cloned()),
                );
                break;
            }
            None => break,
        }
    }
    if cancellation.is_cancelled() {
        return ModelError::cancelled();
    }
    let body = String::from_utf8_lossy(&bytes);
    let hard_quota = matches!(response.status, 402 | 403 | 429)
        && serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .unwrap_or(&value)
                    .get("code")?
                    .as_str()
                    .map(str::to_owned)
            })
            .is_some_and(|code| {
                matches!(
                    code.as_str(),
                    "subscription_sharing_usage_limit_exceeded"
                        | "usage_limit_exceeded"
                        | "usage_limit_reached"
                        | "insufficient_quota"
                        | "billing_hard_limit_reached"
                )
            });
    if hard_quota {
        ModelError::protocol(crate::ProtocolErrorKind::ProviderTerminal)
            .with_message("Provider quota or plan allowance is exhausted")
            .with_body(body)
            .with_causes(
                std::iter::once(error.to_string())
                    .chain(error.diagnostic().causes().iter().cloned()),
            )
    } else {
        error.with_body(body)
    }
}

struct SseReadState {
    body: crate::transport::HttpBody,
    parser: SseParser,
    pending: VecDeque<ModelResult<ModelStreamEvent>>,
    cancellation: CancellationToken,
    decoder: SseDecoder,
    completion_decoder: SseCompletionDecoder,
    request_id: Option<String>,
    done: bool,
}

#[cfg(test)]
#[path = "stream_test.rs"]
mod stream_test;

#[cfg(test)]
#[path = "diagnostic_test.rs"]
mod diagnostic_test;
