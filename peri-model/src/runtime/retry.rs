use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use futures::StreamExt;
use rand::RngExt;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::{ModelError, ModelResult, ModelStream, ModelStreamEvent, RetryErrorKind};

/// 可配置的 retryable 失败分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryableErrorClasses {
    transport: bool,
    request_timeout: bool,
    rate_limited: bool,
    server_error: bool,
    protocol: bool,
}

impl Default for RetryableErrorClasses {
    fn default() -> Self {
        Self {
            transport: true,
            request_timeout: true,
            rate_limited: true,
            server_error: true,
            protocol: true,
        }
    }
}

impl RetryableErrorClasses {
    pub fn with_transport(mut self, enabled: bool) -> Self {
        self.transport = enabled;
        self
    }

    pub fn with_request_timeout(mut self, enabled: bool) -> Self {
        self.request_timeout = enabled;
        self
    }

    pub fn with_rate_limited(mut self, enabled: bool) -> Self {
        self.rate_limited = enabled;
        self
    }

    pub fn with_server_error(mut self, enabled: bool) -> Self {
        self.server_error = enabled;
        self
    }

    pub fn with_protocol(mut self, enabled: bool) -> Self {
        self.protocol = enabled;
        self
    }

    fn matches(self, error: &ModelError) -> Option<RetryErrorKind> {
        if self.transport && error.transport_kind().is_some() {
            return Some(RetryErrorKind::Transport);
        }
        match error.http_status_code() {
            Some(408) if self.request_timeout => return Some(RetryErrorKind::HttpStatus),
            Some(429) if self.rate_limited => return Some(RetryErrorKind::HttpStatus),
            Some(500..=599) if self.server_error => return Some(RetryErrorKind::HttpStatus),
            _ => {}
        }
        if self.protocol {
            // 瞬态集合；ToolCallMissingId/Name/InvalidArguments、AssistantMessageRequired、
            // InvalidEndpoint、Other 保持不重试。
            if let Some(protocol) = error.protocol_error() {
                if matches!(
                    protocol.kind(),
                    crate::ProtocolErrorKind::Provider
                        | crate::ProtocolErrorKind::InvalidJsonObject
                        | crate::ProtocolErrorKind::StreamEndedWithoutCompleted
                ) {
                    return Some(RetryErrorKind::Protocol);
                }
            }
        }
        None
    }
}

/// 协议层 retry 策略。`max_attempts` 包含首次请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryConfig {
    max_attempts: u32,
    base_delay: Duration,
    max_delay: Duration,
    jitter: bool,
    retryable: RetryableErrorClasses,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 6,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(32),
            jitter: true,
            retryable: RetryableErrorClasses::default(),
        }
    }
}

impl RetryConfig {
    pub fn with_max_attempts(mut self, max_attempts: u32) -> Self {
        self.max_attempts = max_attempts.max(1);
        self
    }

    pub fn with_base_delay(mut self, base_delay: Duration) -> Self {
        self.base_delay = base_delay;
        self
    }

    pub fn with_max_delay(mut self, max_delay: Duration) -> Self {
        self.max_delay = max_delay;
        self
    }

    pub fn with_jitter(mut self, jitter: bool) -> Self {
        self.jitter = jitter;
        self
    }

    pub fn with_retryable_error_classes(mut self, retryable: RetryableErrorClasses) -> Self {
        self.retryable = retryable;
        self
    }

    pub fn max_attempts(&self) -> u32 {
        self.max_attempts
    }

    pub fn delay_for_retry(&self, completed_attempts: u32) -> Duration {
        let multiplier = 1_u32
            .checked_shl(completed_attempts.min(31))
            .unwrap_or(u32::MAX);
        let delay = self
            .base_delay
            .saturating_mul(multiplier)
            .min(self.max_delay);
        if self.jitter && !delay.is_zero() {
            let upper = delay.as_millis().min(u128::from(u64::MAX)) as u64 / 4;
            delay
                .saturating_add(Duration::from_millis(rand::rng().random_range(0..=upper)))
                .min(self.max_delay)
        } else {
            delay
        }
    }
}

/// 可安全发送给上层的 retry 观测；不包含 request、response、Agent 或 telemetry 类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RetryObservation {
    attempt: u32,
    max_attempts: u32,
    delay: Duration,
    error_kind: RetryErrorKind,
}

impl RetryObservation {
    pub fn new(
        attempt: u32,
        max_attempts: u32,
        delay: Duration,
        error_kind: RetryErrorKind,
    ) -> Self {
        Self {
            attempt,
            max_attempts,
            delay,
            error_kind,
        }
    }

    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    pub fn max_attempts(&self) -> u32 {
        self.max_attempts
    }

    pub fn delay(&self) -> Duration {
        self.delay
    }

    pub fn error_kind(&self) -> RetryErrorKind {
        self.error_kind
    }
}

/// 上层可注册的、安全 retry 观测回调。
///
/// 回调只接收 attempt、最大尝试数、实际退避与失败分类，不会收到请求、响应、headers 或 telemetry
/// 对象。
pub trait RetryObserver: Send + Sync {
    fn on_retry(&self, observation: RetryObservation);
}

impl<F> RetryObserver for F
where
    F: Fn(RetryObservation) + Send + Sync,
{
    fn on_retry(&self, observation: RetryObservation) {
        self(observation);
    }
}

type AttemptFuture = Pin<Box<dyn Future<Output = ModelResult<ModelStream>> + Send>>;
pub(crate) type StreamAttempt = Arc<dyn Fn(CancellationToken) -> AttemptFuture + Send + Sync>;

const EVENT_CHANNEL_CAPACITY: usize = 16;

pub(crate) fn retrying_stream(
    config: RetryConfig,
    cancellation: CancellationToken,
    observer: Option<Arc<dyn RetryObserver>>,
    attempt: StreamAttempt,
) -> ModelStream {
    let (sender, receiver) = tokio::sync::mpsc::channel(EVENT_CHANNEL_CAPACITY);
    let stream_cancellation = cancellation.child_token();
    let task_cancellation = stream_cancellation.clone();
    tokio::spawn(async move {
        run_retrying_stream(config, task_cancellation, observer, attempt, sender).await;
    });
    ModelStream::with_cancellation(
        futures::stream::unfold(receiver, |mut receiver| async move {
            receiver.recv().await.map(|event| (event, receiver))
        }),
        stream_cancellation,
    )
}

async fn run_retrying_stream(
    config: RetryConfig,
    cancellation: CancellationToken,
    observer: Option<Arc<dyn RetryObserver>>,
    attempt: StreamAttempt,
    sender: tokio::sync::mpsc::Sender<ModelResult<ModelStreamEvent>>,
) {
    for attempt_number in 1..=config.max_attempts() {
        if cancellation.is_cancelled() {
            return;
        }
        let mut stream = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return,
            result = attempt(cancellation.clone()) => match result {
                Ok(stream) => stream,
                Err(error) => {
                    if !retry_or_finish(&config, &cancellation, observer.as_ref(), &sender, attempt_number, error).await {
                        return;
                    }
                    continue;
                }
            },
        };
        let mut visible = false;
        let mut usage_after_visible = false;
        let mut committed = false;
        let mut provisional_usage = Vec::new();
        loop {
            let item = tokio::select! {
                biased;
                _ = cancellation.cancelled() => {
                    stream.abort();
                    return;
                }
                item = stream.next() => item,
            };
            match item {
                Some(Ok(event)) => {
                    let event_commits_attempt = matches!(
                        event,
                        ModelStreamEvent::TextDelta { .. }
                            | ModelStreamEvent::ReasoningDelta { .. }
                            | ModelStreamEvent::ToolCallDelta { .. }
                            | ModelStreamEvent::Completed(_)
                    );
                    visible |= matches!(
                        event,
                        ModelStreamEvent::TextDelta { .. }
                            | ModelStreamEvent::ReasoningDelta { .. }
                            | ModelStreamEvent::ToolCallDelta { .. }
                    );
                    usage_after_visible |= visible && matches!(event, ModelStreamEvent::Usage(_));
                    let completed = matches!(event, ModelStreamEvent::Completed(_));
                    if event_commits_attempt && !committed {
                        committed = true;
                        for usage in provisional_usage.drain(..) {
                            if !send_event(&sender, &cancellation, Ok(usage)).await {
                                stream.abort();
                                return;
                            }
                        }
                    }
                    if committed {
                        if !send_event(&sender, &cancellation, Ok(event)).await {
                            stream.abort();
                            return;
                        }
                    } else if matches!(event, ModelStreamEvent::Usage(_)) {
                        provisional_usage.push(event);
                    }
                    if completed {
                        return;
                    }
                }
                Some(Err(error)) if error.is_cancelled() => return,
                Some(Err(error)) if visible => {
                    let error = if error.transport_kind().is_some() {
                        interrupted_from(&error)
                    } else {
                        error
                    };
                    let _ = send_event(&sender, &cancellation, Err(error)).await;
                    return;
                }
                Some(Err(error)) => {
                    if !retry_or_finish(
                        &config,
                        &cancellation,
                        observer.as_ref(),
                        &sender,
                        attempt_number,
                        error,
                    )
                    .await
                    {
                        return;
                    }
                    break;
                }
                None if usage_after_visible => {
                    let _ = send_event(
                        &sender,
                        &cancellation,
                        Err(ModelError::protocol(
                            crate::ProtocolErrorKind::StreamEndedWithoutCompleted,
                        )),
                    )
                    .await;
                    return;
                }
                None if visible => {
                    let _ = send_event(
                        &sender,
                        &cancellation,
                        Err(ModelError::stream_interrupted(None::<&str>, None::<&str>)),
                    )
                    .await;
                    return;
                }
                None => {
                    if !retry_or_finish(
                        &config,
                        &cancellation,
                        observer.as_ref(),
                        &sender,
                        attempt_number,
                        ModelError::protocol(crate::ProtocolErrorKind::StreamEndedWithoutCompleted),
                    )
                    .await
                    {
                        return;
                    }
                    break;
                }
            }
        }
    }
}

async fn retry_or_finish(
    config: &RetryConfig,
    cancellation: &CancellationToken,
    observer: Option<&Arc<dyn RetryObserver>>,
    sender: &tokio::sync::mpsc::Sender<ModelResult<ModelStreamEvent>>,
    attempt: u32,
    error: ModelError,
) -> bool {
    if error.is_cancelled() || cancellation.is_cancelled() {
        return false;
    }
    let Some(error_kind) = config.retryable.matches(&error) else {
        let _ = send_event(sender, cancellation, Err(error)).await;
        return false;
    };
    if attempt >= config.max_attempts() {
        let _ = send_event(
            sender,
            cancellation,
            Err(ModelError::retry_exhausted(attempt, error_kind)),
        )
        .await;
        return false;
    }
    let delay = config.delay_for_retry(attempt);
    if let Some(observer) = observer {
        observer.on_retry(RetryObservation {
            attempt,
            max_attempts: config.max_attempts(),
            delay,
            error_kind,
        });
    }
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => false,
        _ = tokio::time::sleep(delay) => true,
    }
}

async fn send_event(
    sender: &tokio::sync::mpsc::Sender<ModelResult<ModelStreamEvent>>,
    cancellation: &CancellationToken,
    event: ModelResult<ModelStreamEvent>,
) -> bool {
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => false,
        result = sender.send(event) => result.is_ok(),
    }
}

fn interrupted_from(error: &ModelError) -> ModelError {
    ModelError::stream_interrupted(error.provider(), error.request_id())
}

#[cfg(test)]
#[path = "retry_test.rs"]
mod retry_test;
