use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    task::Poll,
    time::Duration,
};

use futures::{stream, StreamExt};
use tokio::{sync::Notify, time::timeout};
use tokio_util::sync::CancellationToken;

use super::{
    retrying_stream, RetryConfig, RetryObservation, RetryableErrorClasses, StreamAttempt,
    EVENT_CHANNEL_CAPACITY,
};
use crate::{
    ModelError, ModelResult, ModelRuntimeConfig, ModelStream, ModelStreamEvent, TransportErrorKind,
};

fn scripted_attempt(
    steps: Vec<ModelResult<Vec<ModelResult<ModelStreamEvent>>>>,
    calls: Arc<AtomicUsize>,
) -> StreamAttempt {
    let steps = Arc::new(Mutex::new(VecDeque::from(steps)));
    Arc::new(move |cancellation| {
        calls.fetch_add(1, Ordering::SeqCst);
        let step = steps
            .lock()
            .expect("script lock")
            .pop_front()
            .expect("script step");
        Box::pin(async move {
            step.map(|events| {
                ModelStream::with_cancellation(stream::iter(events), cancellation.child_token())
            })
        })
    })
}

fn config() -> RetryConfig {
    RetryConfig::default()
        .with_max_attempts(3)
        .with_base_delay(Duration::ZERO)
        .with_jitter(false)
}

#[test]
fn jittered_delay_never_exceeds_max_delay() {
    let config = RetryConfig::default()
        .with_base_delay(Duration::from_millis(100))
        .with_max_delay(Duration::from_millis(100))
        .with_jitter(true);

    for _ in 0..128 {
        assert!(config.delay_for_retry(1) <= Duration::from_millis(100));
        assert!(config.delay_for_retry(31) <= Duration::from_millis(100));
    }
}

#[test]
fn runtime_config_hides_registered_retry_observer_from_debug() {
    let runtime =
        ModelRuntimeConfig::default().with_retry_observer(Arc::new(|_: RetryObservation| {}));
    let debug = format!("{runtime:?}");

    assert!(debug.contains("[REGISTERED]"));
    assert!(!debug.contains("closure"));
}

#[tokio::test]
async fn retries_before_first_visible_event() {
    let calls = Arc::new(AtomicUsize::new(0));
    let attempt = scripted_attempt(
        vec![
            Err(ModelError::http_status(429, "openai", None::<&str>)),
            Ok(vec![Ok(ModelStreamEvent::Completed(completed()))]),
        ],
        calls.clone(),
    );
    let mut stream = Box::pin(retrying_stream(
        config(),
        CancellationToken::new(),
        None,
        attempt,
    ));
    assert!(matches!(
        stream.next().await,
        Some(Ok(ModelStreamEvent::Completed(_)))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn retries_configured_status_classes_before_visible_event() {
    for status in [408, 429, 500, 599] {
        let calls = Arc::new(AtomicUsize::new(0));
        let attempt = scripted_attempt(
            vec![
                Err(ModelError::http_status(status, "openai", None::<&str>)),
                Ok(vec![Ok(ModelStreamEvent::Completed(completed()))]),
            ],
            calls.clone(),
        );
        let mut stream = Box::pin(retrying_stream(
            config(),
            CancellationToken::new(),
            None,
            attempt,
        ));
        assert!(matches!(
            stream.next().await,
            Some(Ok(ModelStreamEvent::Completed(_)))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}

#[tokio::test]
async fn configured_retry_class_can_disable_rate_limit_retries() {
    let calls = Arc::new(AtomicUsize::new(0));
    let attempt = scripted_attempt(
        vec![Err(ModelError::http_status(429, "openai", None::<&str>))],
        calls.clone(),
    );
    let config = config()
        .with_retryable_error_classes(RetryableErrorClasses::default().with_rate_limited(false));
    let mut stream = Box::pin(retrying_stream(
        config,
        CancellationToken::new(),
        None,
        attempt,
    ));
    assert!(
        matches!(stream.next().await, Some(Err(error)) if error.http_status_code() == Some(429))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn retry_observation_contains_only_safe_retry_fields() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::new(Mutex::new(Vec::new()));
    let attempt = scripted_attempt(
        vec![
            Err(ModelError::transport(
                TransportErrorKind::Connection,
                Some("openai"),
            )),
            Ok(vec![Ok(ModelStreamEvent::Completed(completed()))]),
        ],
        calls,
    );
    let observer = {
        let observed = observed.clone();
        Arc::new(move |observation: RetryObservation| {
            observed.lock().expect("observation lock").push(observation);
        })
    };
    let mut stream = Box::pin(retrying_stream(
        config(),
        CancellationToken::new(),
        Some(observer),
        attempt,
    ));
    assert!(matches!(
        stream.next().await,
        Some(Ok(ModelStreamEvent::Completed(_)))
    ));
    let observed = observed.lock().expect("observation lock");
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].attempt(), 1);
    assert_eq!(observed[0].max_attempts(), 3);
    assert_eq!(observed[0].delay(), Duration::ZERO);
    assert_eq!(observed[0].error_kind(), crate::RetryErrorKind::Transport);
}

#[tokio::test]
async fn never_retries_after_visible_delta() {
    for event in [
        ModelStreamEvent::TextDelta {
            text: "partial".into(),
        },
        ModelStreamEvent::ReasoningDelta {
            text: "partial reasoning".into(),
        },
        ModelStreamEvent::ToolCallDelta {
            index: 0,
            id: Some("call_1".into()),
            name: Some("tool".into()),
            arguments_delta: "{".into(),
        },
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let attempt = scripted_attempt(
            vec![Ok(vec![
                Ok(event),
                Err(ModelError::transport(
                    TransportErrorKind::Connection,
                    Some("openai"),
                )),
            ])],
            calls.clone(),
        );
        let mut stream = Box::pin(retrying_stream(
            config(),
            CancellationToken::new(),
            None,
            attempt,
        ));
        assert!(matches!(stream.next().await, Some(Ok(_))));
        assert!(
            matches!(stream.next().await, Some(Err(error)) if error.provider() == Some("openai"))
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

/// [回归测试] 首个可见 delta 后的 provider 协议错误必须原样传播。
///
/// 历史背景：retry 层曾把任何 visible delta 后的错误都伪装为 StreamInterrupted，导致
/// Anthropic 对损坏事件的 fail-closed Provider 分类丢失，调用方无法区分连接中断与协议失败。
#[tokio::test]
async fn visible_delta_preserves_protocol_error_without_retry() {
    let calls = Arc::new(AtomicUsize::new(0));
    let attempt = scripted_attempt(
        vec![Ok(vec![
            Ok(ModelStreamEvent::TextDelta {
                text: "partial".into(),
            }),
            Err(ModelError::protocol(crate::ProtocolErrorKind::Provider)),
        ])],
        calls.clone(),
    );
    let events = retrying_stream(config(), CancellationToken::new(), None, attempt)
        .collect::<Vec<_>>()
        .await;
    assert!(
        matches!(events.last(), Some(Err(error)) if error.protocol_error().map(|protocol| protocol.kind()) == Some(crate::ProtocolErrorKind::Provider))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// [回归测试] 未提交 attempt 的 Usage 不能穿透到重试后的成功响应。
///
/// 历史背景：Anthropic 在首个可见输出前发出 input usage；若该 attempt 随后断连并重试，
/// 旧 usage 曾被错误转发，`Model::complete()` 还可能把它聚合进下一次成功的响应。
#[tokio::test]
async fn retry_discards_provisional_usage_from_failed_attempt() {
    let calls = Arc::new(AtomicUsize::new(0));
    let attempt = scripted_attempt(
        vec![
            Ok(vec![
                Ok(ModelStreamEvent::Usage(crate::TokenUsage::new(1, 0))),
                Err(ModelError::http_status(429, "openai", None::<&str>)),
            ]),
            Ok(vec![Ok(ModelStreamEvent::Completed(completed()))]),
        ],
        calls.clone(),
    );
    let events = retrying_stream(config(), CancellationToken::new(), None, attempt)
        .collect::<Vec<_>>()
        .await;
    assert!(events.iter().all(ModelResult::is_ok));
    assert!(events
        .iter()
        .all(|event| !matches!(event, Ok(ModelStreamEvent::Usage(_)))));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

/// [回归测试] 首个可见 delta 前收到的 Usage 不能关闭流式重试窗口。
///
/// 历史背景：Anthropic 会在 `message_start` 先发送 input usage；将任何 Usage 视为 terminal
/// 会使此后的可重试断连无法重试，违背“首个可见事件前可重试”的契约。
#[tokio::test]
async fn usage_before_visible_event_does_not_stop_retrying() {
    let calls = Arc::new(AtomicUsize::new(0));
    let attempt = scripted_attempt(
        vec![
            Ok(vec![
                Ok(ModelStreamEvent::Usage(crate::TokenUsage::new(1, 0))),
                Err(ModelError::http_status(429, "openai", None::<&str>)),
            ]),
            Ok(vec![Ok(ModelStreamEvent::Completed(completed()))]),
        ],
        calls.clone(),
    );
    let events = retrying_stream(config(), CancellationToken::new(), None, attempt)
        .collect::<Vec<_>>()
        .await;
    assert!(events.iter().all(ModelResult::is_ok));
    assert!(events
        .iter()
        .any(|event| matches!(event, Ok(ModelStreamEvent::Completed(_)))));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

/// [回归测试] 可见 delta 后又发出 Usage 时，后续协议错误必须原样传播。
///
/// 历史背景：retry 层曾优先检查 visible 标志，把 Usage 后的 provider protocol error
/// 伪装为 StreamInterrupted，调用方因此丢失了可诊断的协议失败分类。
#[tokio::test]
async fn usage_after_visible_event_preserves_protocol_error_without_retry() {
    let calls = Arc::new(AtomicUsize::new(0));
    let attempt = scripted_attempt(
        vec![Ok(vec![
            Ok(ModelStreamEvent::TextDelta {
                text: "partial".into(),
            }),
            Ok(ModelStreamEvent::Usage(crate::TokenUsage::new(1, 1))),
            Err(ModelError::protocol(crate::ProtocolErrorKind::Provider)),
        ])],
        calls.clone(),
    );
    let events = retrying_stream(config(), CancellationToken::new(), None, attempt)
        .collect::<Vec<_>>()
        .await;
    assert!(
        matches!(events.last(), Some(Err(error)) if error.protocol_error().map(|protocol| protocol.kind()) == Some(crate::ProtocolErrorKind::Provider))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// [回归测试] 可见 delta 后即使已收到 Usage，传输中断仍必须映射为 StreamInterrupted。
///
/// 历史背景：为了保留 Usage 后的 Provider error，retry 层曾将所有 Usage 后错误原样透传；
/// 这错误地暴露了 partial output 后的 transport failure，破坏不可重放的中断语义。
#[tokio::test]
async fn usage_after_visible_event_maps_transport_error_to_stream_interrupted() {
    let calls = Arc::new(AtomicUsize::new(0));
    let attempt = scripted_attempt(
        vec![Ok(vec![
            Ok(ModelStreamEvent::TextDelta {
                text: "partial".into(),
            }),
            Ok(ModelStreamEvent::Usage(crate::TokenUsage::new(1, 1))),
            Err(ModelError::transport(
                TransportErrorKind::Connection,
                Some("openai"),
            )),
        ])],
        calls.clone(),
    );
    let events = retrying_stream(config(), CancellationToken::new(), None, attempt)
        .collect::<Vec<_>>()
        .await;
    assert!(matches!(events.last(), Some(Err(error)) if error.is_stream_interrupted()));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// [回归测试] Usage 后未收到 Completed 的 EOF 必须保留缺少完成事件的协议语义，并在
/// 首可见事件前按 Protocol 分类重试。
///
/// 历史背景：retry 层曾将 Usage 后 EOF 与可见内容后的中断混为一谈，错误报告为
/// StreamInterrupted，掩盖了 provider 没有发出完成事件的协议错误。
#[tokio::test]
async fn usage_then_eof_retries_stream_ended_without_completed() {
    let calls = Arc::new(AtomicUsize::new(0));
    let attempt = scripted_attempt(
        vec![
            Ok(vec![Ok(ModelStreamEvent::Usage(crate::TokenUsage::new(
                1, 0,
            )))]),
            Ok(vec![Ok(ModelStreamEvent::Completed(completed()))]),
        ],
        calls.clone(),
    );
    let events = retrying_stream(config(), CancellationToken::new(), None, attempt)
        .collect::<Vec<_>>()
        .await;
    assert!(events.iter().all(ModelResult::is_ok));
    assert!(events
        .iter()
        .all(|event| !matches!(event, Ok(ModelStreamEvent::Usage(_)))));
    assert!(events
        .iter()
        .any(|event| matches!(event, Ok(ModelStreamEvent::Completed(_)))));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn retries_protocol_provider_error_before_visible_event() {
    let calls = Arc::new(AtomicUsize::new(0));
    let attempt = scripted_attempt(
        vec![
            Err(ModelError::protocol(crate::ProtocolErrorKind::Provider)),
            Ok(vec![Ok(ModelStreamEvent::Completed(completed()))]),
        ],
        calls.clone(),
    );
    let mut stream = Box::pin(retrying_stream(
        config(),
        CancellationToken::new(),
        None,
        attempt,
    ));
    assert!(matches!(
        stream.next().await,
        Some(Ok(ModelStreamEvent::Completed(_)))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn retries_invalid_json_object_error() {
    let calls = Arc::new(AtomicUsize::new(0));
    let attempt = scripted_attempt(
        vec![
            Err(ModelError::protocol(
                crate::ProtocolErrorKind::InvalidJsonObject,
            )),
            Ok(vec![Ok(ModelStreamEvent::Completed(completed()))]),
        ],
        calls.clone(),
    );
    let mut stream = Box::pin(retrying_stream(
        config(),
        CancellationToken::new(),
        None,
        attempt,
    ));
    assert!(matches!(
        stream.next().await,
        Some(Ok(ModelStreamEvent::Completed(_)))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn retries_stream_ended_without_completed_via_eof() {
    let calls = Arc::new(AtomicUsize::new(0));
    let attempt = scripted_attempt(
        vec![
            Ok(vec![]),
            Ok(vec![Ok(ModelStreamEvent::Completed(completed()))]),
        ],
        calls.clone(),
    );
    let mut stream = Box::pin(retrying_stream(
        config(),
        CancellationToken::new(),
        None,
        attempt,
    ));
    assert!(matches!(
        stream.next().await,
        Some(Ok(ModelStreamEvent::Completed(_)))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn does_not_retry_non_transient_protocol_errors() {
    for kind in [
        crate::ProtocolErrorKind::ToolCallMissingId,
        crate::ProtocolErrorKind::ToolCallMissingName,
        crate::ProtocolErrorKind::ToolCallInvalidArguments,
        crate::ProtocolErrorKind::AssistantMessageRequired,
        crate::ProtocolErrorKind::InvalidEndpoint,
        crate::ProtocolErrorKind::Other,
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let attempt = scripted_attempt(vec![Err(ModelError::protocol(kind))], calls.clone());
        let mut stream = Box::pin(retrying_stream(
            config(),
            CancellationToken::new(),
            None,
            attempt,
        ));
        assert!(
            matches!(stream.next().await, Some(Err(error)) if error.protocol_error().map(|protocol| protocol.kind()) == Some(kind))
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn configured_retry_class_can_disable_protocol_retries() {
    let calls = Arc::new(AtomicUsize::new(0));
    let attempt = scripted_attempt(
        vec![Err(ModelError::protocol(
            crate::ProtocolErrorKind::Provider,
        ))],
        calls.clone(),
    );
    let config = config()
        .with_retryable_error_classes(RetryableErrorClasses::default().with_protocol(false));
    let mut stream = Box::pin(retrying_stream(
        config,
        CancellationToken::new(),
        None,
        attempt,
    ));
    assert!(
        matches!(stream.next().await, Some(Err(error)) if error.protocol_error().map(|protocol| protocol.kind()) == Some(crate::ProtocolErrorKind::Provider))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn retry_exhausted_reports_protocol_kind() {
    let calls = Arc::new(AtomicUsize::new(0));
    let attempt = scripted_attempt(
        vec![
            Err(ModelError::protocol(crate::ProtocolErrorKind::Provider)),
            Err(ModelError::protocol(crate::ProtocolErrorKind::Provider)),
            Err(ModelError::protocol(crate::ProtocolErrorKind::Provider)),
        ],
        calls.clone(),
    );
    let mut stream = Box::pin(retrying_stream(
        config(),
        CancellationToken::new(),
        None,
        attempt,
    ));
    assert!(
        matches!(stream.next().await, Some(Err(error)) if error.retry_error_kind() == Some(crate::RetryErrorKind::Protocol))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn retry_observation_reports_protocol_kind() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::new(Mutex::new(Vec::new()));
    let attempt = scripted_attempt(
        vec![
            Err(ModelError::protocol(crate::ProtocolErrorKind::Provider)),
            Ok(vec![Ok(ModelStreamEvent::Completed(completed()))]),
        ],
        calls,
    );
    let observer = {
        let observed = observed.clone();
        Arc::new(move |observation: RetryObservation| {
            observed.lock().expect("observation lock").push(observation);
        })
    };
    let mut stream = Box::pin(retrying_stream(
        config(),
        CancellationToken::new(),
        Some(observer),
        attempt,
    ));
    assert!(matches!(
        stream.next().await,
        Some(Ok(ModelStreamEvent::Completed(_)))
    ));
    let observed = observed.lock().expect("observation lock");
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].attempt(), 1);
    assert_eq!(observed[0].max_attempts(), 3);
    assert_eq!(observed[0].error_kind(), crate::RetryErrorKind::Protocol);
}

#[tokio::test]
async fn does_not_retry_non_retryable_http_statuses() {
    for status in [400, 401, 403, 404] {
        let calls = Arc::new(AtomicUsize::new(0));
        let attempt = scripted_attempt(
            vec![Err(ModelError::http_status(status, "openai", None::<&str>))],
            calls.clone(),
        );
        let mut stream = Box::pin(retrying_stream(
            config(),
            CancellationToken::new(),
            None,
            attempt,
        ));
        assert!(
            matches!(stream.next().await, Some(Err(error)) if error.http_status_code() == Some(status))
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn abort_stops_pending_connect_without_starting_another_attempt() {
    let calls = Arc::new(AtomicUsize::new(0));
    let attempt: StreamAttempt = {
        let calls = calls.clone();
        Arc::new(move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(futures::future::pending())
        })
    };
    let mut stream = Box::pin(retrying_stream(
        config(),
        CancellationToken::new(),
        None,
        attempt,
    ));
    tokio::task::yield_now().await;
    stream.abort();
    let error = timeout(Duration::from_millis(100), stream.next())
        .await
        .expect("abort must interrupt connect")
        .expect("cancelled event")
        .unwrap_err();
    assert!(error.is_cancelled());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn abort_stops_pending_sse_read_without_starting_another_attempt() {
    let calls = Arc::new(AtomicUsize::new(0));
    let attempt: StreamAttempt = {
        let calls = calls.clone();
        Arc::new(move |cancellation| {
            calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                Ok(ModelStream::with_cancellation(
                    stream::pending::<ModelResult<ModelStreamEvent>>(),
                    cancellation.child_token(),
                ))
            })
        })
    };
    let mut stream = Box::pin(retrying_stream(
        config(),
        CancellationToken::new(),
        None,
        attempt,
    ));
    tokio::task::yield_now().await;
    stream.abort();
    let error = timeout(Duration::from_millis(100), stream.next())
        .await
        .expect("abort must interrupt SSE read")
        .expect("cancelled event")
        .unwrap_err();
    assert!(error.is_cancelled());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn abort_stops_connect_read_and_backoff() {
    let cancellation = CancellationToken::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let attempt: StreamAttempt = {
        let calls = calls.clone();
        Arc::new(move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                Err(ModelError::transport(
                    TransportErrorKind::Connection,
                    None::<&str>,
                ))
            })
        })
    };
    let config = RetryConfig::default()
        .with_max_attempts(3)
        .with_base_delay(Duration::from_secs(60))
        .with_jitter(false);
    let mut stream = Box::pin(retrying_stream(config, cancellation, None, attempt));
    tokio::task::yield_now().await;
    stream.abort();
    let error = timeout(Duration::from_millis(100), stream.next())
        .await
        .expect("abort must interrupt backoff")
        .expect("cancelled event")
        .unwrap_err();
    assert!(error.is_cancelled());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn bounded_event_channel_blocks_producer_until_consumer_receives() {
    let started = Arc::new(Notify::new());
    let produced = Arc::new(AtomicUsize::new(0));
    let attempt: StreamAttempt = {
        let started = started.clone();
        let produced = produced.clone();
        Arc::new(move |cancellation| {
            let started = started.clone();
            let produced = produced.clone();
            Box::pin(async move {
                Ok(ModelStream::with_cancellation(
                    stream::poll_fn(move |_| {
                        let index = produced.fetch_add(1, Ordering::SeqCst);
                        if index == 0 {
                            started.notify_one();
                        }
                        Poll::Ready(Some(Ok(ModelStreamEvent::TextDelta {
                            text: index.to_string(),
                        })))
                    }),
                    cancellation.child_token(),
                ))
            })
        })
    };
    let mut stream = Box::pin(retrying_stream(
        config(),
        CancellationToken::new(),
        None,
        attempt,
    ));

    timeout(Duration::from_millis(100), started.notified())
        .await
        .expect("producer must start");
    tokio::time::sleep(Duration::from_millis(20)).await;
    let buffered_and_in_flight = produced.load(Ordering::SeqCst);
    assert_eq!(buffered_and_in_flight, EVENT_CHANNEL_CAPACITY + 1);
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(produced.load(Ordering::SeqCst), buffered_and_in_flight);

    assert!(matches!(
        stream.next().await,
        Some(Ok(ModelStreamEvent::TextDelta { .. }))
    ));
    timeout(Duration::from_millis(100), async {
        loop {
            if produced.load(Ordering::SeqCst) > buffered_and_in_flight {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("consumer capacity must unblock producer");
}

#[tokio::test]
async fn cancellation_releases_producer_blocked_on_bounded_send() {
    let parent = CancellationToken::new();
    let started = Arc::new(Notify::new());
    let dropped = Arc::new(Notify::new());
    let produced = Arc::new(AtomicUsize::new(0));
    let attempt: StreamAttempt = {
        let started = started.clone();
        let dropped = dropped.clone();
        let produced = produced.clone();
        Arc::new(move |cancellation| {
            let started = started.clone();
            let dropped = dropped.clone();
            let produced = produced.clone();
            Box::pin(async move {
                struct ReleaseOnDrop(Arc<Notify>);
                impl Drop for ReleaseOnDrop {
                    fn drop(&mut self) {
                        self.0.notify_one();
                    }
                }

                let resource = ReleaseOnDrop(dropped.clone());
                Ok(ModelStream::with_cancellation(
                    stream::poll_fn(move |_| {
                        let _ = &resource;
                        let index = produced.fetch_add(1, Ordering::SeqCst);
                        if index == 0 {
                            started.notify_one();
                        }
                        Poll::Ready(Some(Ok(ModelStreamEvent::TextDelta {
                            text: "event".into(),
                        })))
                    }),
                    cancellation.child_token(),
                ))
            })
        })
    };
    let mut stream = Box::pin(retrying_stream(config(), parent.clone(), None, attempt));

    timeout(Duration::from_millis(100), started.notified())
        .await
        .expect("producer must fill event channel");
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        produced.load(Ordering::SeqCst),
        EVENT_CHANNEL_CAPACITY + 1,
        "producer must be blocked waiting for channel capacity"
    );
    parent.cancel();
    timeout(Duration::from_millis(100), dropped.notified())
        .await
        .expect("cancellation must release blocked producer transport");
    let error = timeout(Duration::from_millis(100), stream.next())
        .await
        .expect("cancellation must wake consumer")
        .expect("cancelled event")
        .unwrap_err();
    assert!(error.is_cancelled());
    assert!(stream.next().await.is_none());
}

fn completed() -> crate::ModelResponse {
    crate::ModelResponse::new(
        crate::ModelMessage::assistant_text("ok"),
        crate::StopReason::EndTurn,
        None,
        None,
    )
    .expect("assistant response")
}
