use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use async_trait::async_trait;
use futures::{stream, StreamExt};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{
    transport::{HttpBody, HttpRequest, HttpResponse, HttpTransport},
    ContentBlock, Model, ModelError, ModelMessage, ModelRequest, ModelResult, ModelRuntimeConfig,
    ModelStreamEvent, RetryConfig, RetryableErrorClasses, TransportErrorKind,
};

use super::{AnthropicConfig, AnthropicModel};

struct FakeTransport {
    bodies: Mutex<Vec<Value>>,
    responses: Mutex<Vec<FakeResponse>>,
    calls: AtomicUsize,
}

struct FakeResponse {
    status: u16,
    request_id: Option<String>,
    body: FakeBody,
}

enum FakeBody {
    Chunks(Vec<ModelResult<Vec<u8>>>),
    Pending,
}

impl FakeTransport {
    fn with_response(response: FakeResponse) -> Self {
        Self::with_responses(vec![response])
    }

    fn with_responses(responses: Vec<FakeResponse>) -> Self {
        Self {
            bodies: Mutex::new(Vec::new()),
            responses: Mutex::new(responses),
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
        request: HttpRequest,
        cancellation: CancellationToken,
    ) -> ModelResult<HttpResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let body = request
            .request
            .body()
            .and_then(reqwest::Body::as_bytes)
            .expect("JSON request body");
        self.bodies
            .lock()
            .expect("lock available")
            .push(serde_json::from_slice(body).expect("valid request JSON"));
        let response = self.responses.lock().expect("lock available").remove(0);
        let body: HttpBody = match response.body {
            FakeBody::Chunks(chunks) => Box::pin(stream::iter(chunks)),
            FakeBody::Pending => Box::pin(stream::pending()),
        };
        Ok(HttpResponse::new(
            response.status,
            response.request_id,
            body,
            cancellation,
        ))
    }
}

fn config() -> AnthropicConfig {
    config_with_retry(1)
}

fn config_with_retry(max_attempts: u32) -> AnthropicConfig {
    AnthropicConfig::new(
        Url::parse("https://proxy.example.test/custom/").expect("valid endpoint"),
        "test-credential",
        "claude-test",
    )
    .with_runtime(
        ModelRuntimeConfig::default().with_retry(
            RetryConfig::default()
                .with_max_attempts(max_attempts)
                .with_base_delay(Duration::ZERO)
                .with_jitter(false),
        ),
    )
}

/// 关闭 Protocol 分类重试的配置，用于 fail-closed 分类断言（保留原始协议错误而非
/// `RetryExhausted(Protocol)`）。
fn config_without_protocol_retry() -> AnthropicConfig {
    AnthropicConfig::new(
        Url::parse("https://proxy.example.test/custom/").expect("valid endpoint"),
        "test-credential",
        "claude-test",
    )
    .with_runtime(
        ModelRuntimeConfig::default().with_retry(
            RetryConfig::default()
                .with_max_attempts(1)
                .with_base_delay(Duration::ZERO)
                .with_jitter(false)
                .with_retryable_error_classes(
                    RetryableErrorClasses::default().with_protocol(false),
                ),
        ),
    )
}

/// [回归测试] `message_start` 后首个可见 delta 前断连必须重试，并为新 attempt 重置 decoder 状态。
///
/// 历史背景：Anthropic 的 input Usage 来自 `message_start`；把它误当终态会禁用重试，且
/// provider decoder 若跨 attempt 复用 state，重试后的合法 `message_start` 会被误判重复。
#[tokio::test]
async fn message_start_then_transport_failure_retries_with_fresh_anthropic_decoder_state() {
    let transport = Arc::new(FakeTransport::with_responses(vec![
        FakeResponse {
            status: 200,
            request_id: None,
            body: FakeBody::Chunks(vec![
                Ok(
                    "event: message_start\ndata: {\"message\":{\"id\":\"first-id\",\"usage\":{\"input_tokens\":1}}}\n\n"
                        .as_bytes()
                        .to_vec(),
                ),
                Err(ModelError::transport(
                    TransportErrorKind::Connection,
                    None::<&str>,
                )),
            ]),
        },
        FakeResponse {
            status: 200,
            request_id: None,
            body: FakeBody::Chunks(vec![Ok(concat!(
                "event: message_start\ndata: {\"message\":{\"id\":\"second-id\",\"usage\":{\"input_tokens\":2}}}\n\n",
                "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
                "event: message_stop\ndata: {}\n\n"
            )
            .as_bytes()
            .to_vec())]),
        },
    ]));
    let events = AnthropicModel::with_transport(config_with_retry(2), transport.clone())
        .stream(
            ModelRequest::new(vec![ModelMessage::user_text("go")]),
            CancellationToken::new(),
        )
        .await
        .expect("stream")
        .collect::<Vec<_>>()
        .await;
    assert!(events.iter().all(ModelResult::is_ok));
    assert!(events
        .iter()
        .any(|event| matches!(event, Ok(ModelStreamEvent::Completed(response)) if response.request_id() == Some("second-id"))));
    assert_eq!(transport.calls(), 2);
}

#[tokio::test]
async fn malformed_stream_retries_then_exhausts_with_protocol_kind() {
    let malformed = concat!(
        "event: message_start\ndata: {\"message\":{\"id\":\"body-id\"}}\n\n",
        "event: message_stop\ndata: {}\n\n"
    );
    let transport = Arc::new(FakeTransport::with_responses(vec![
        FakeResponse {
            status: 200,
            request_id: None,
            body: FakeBody::Chunks(vec![Ok(malformed.as_bytes().to_vec())]),
        },
        FakeResponse {
            status: 200,
            request_id: None,
            body: FakeBody::Chunks(vec![Ok(malformed.as_bytes().to_vec())]),
        },
    ]));
    let events = AnthropicModel::with_transport(config_with_retry(2), transport.clone())
        .stream(
            ModelRequest::new(vec![ModelMessage::user_text("go")]),
            CancellationToken::new(),
        )
        .await
        .expect("stream")
        .collect::<Vec<_>>()
        .await;
    assert!(
        matches!(events.last(), Some(Err(error)) if error.retry_error_kind() == Some(crate::RetryErrorKind::Protocol))
    );
    assert_eq!(transport.calls(), 2);
}

/// [回归测试] Anthropic 事件必须从唯一的 message_start 开始，block index 必须连续递增。
///
/// 历史背景：早期 decoder 仅校验 active block 的局部 index，允许没有 message_start 的
/// 完整 block 序列和跳跃/回退 index 生成 Completed，导致损坏的 provider stream fail-open。
#[tokio::test]
async fn anthropic_lifecycle_requires_message_start_and_contiguous_block_indexes() {
    for sequence in [
        concat!(
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: message_stop\ndata: {}\n\n"
        ),
        concat!(
            "event: message_start\ndata: {\"message\":{\"id\":\"body-id\"}}\n\n",
            "event: content_block_start\ndata: {\"index\":1,\"content_block\":{\"type\":\"text\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":1}\n\n",
            "event: message_stop\ndata: {}\n\n"
        ),
        concat!(
            "event: message_start\ndata: {\"message\":{\"id\":\"body-id\"}}\n\n",
            "event: message_start\ndata: {\"message\":{\"id\":\"second-id\"}}\n\n"
        ),
    ] {
        let transport = Arc::new(FakeTransport::with_response(FakeResponse {
            status: 200,
            request_id: None,
            body: FakeBody::Chunks(vec![Ok(sequence.as_bytes().to_vec())]),
        }));
        let events = AnthropicModel::with_transport(config_without_protocol_retry(), transport)
            .stream(
                ModelRequest::new(vec![ModelMessage::user_text("go")]),
                CancellationToken::new(),
            )
            .await
            .expect("stream")
            .collect::<Vec<_>>()
            .await;
        assert!(events
            .iter()
            .all(|event| !matches!(event, Ok(ModelStreamEvent::Completed(_)))));
        assert!(matches!(events.last(), Some(Err(error)) if error.protocol_error().map(|protocol| protocol.kind()) == Some(crate::ProtocolErrorKind::Provider)));
    }
}

/// [回归测试] 组成总输入 usage 的合法分量相加也必须 checked，不能 panic 或回绕。
///
/// 历史背景：单字段 conversion 已改为 checked，但 input token、cache creation 与 cache read
/// 在归一化为 TokenUsage 时仍使用普通 u32 加法，多个合法分量可使 debug panic/release 回绕。
#[tokio::test]
async fn anthropic_total_input_usage_overflow_is_provider_error_without_completed() {
    let transport = Arc::new(FakeTransport::with_response(FakeResponse {
        status: 200,
        request_id: None,
        body: FakeBody::Chunks(vec![Ok(concat!(
            "event: message_start\ndata: {\"message\":{\"id\":\"body-id\",\"usage\":{\"input_tokens\":4294967295,\"cache_read_input_tokens\":1}}}\n\n",
            "event: message_stop\ndata: {}\n\n"
        )
        .as_bytes()
        .to_vec())]),
    }));
    let events = AnthropicModel::with_transport(config_without_protocol_retry(), transport)
        .stream(
            ModelRequest::new(vec![ModelMessage::user_text("go")]),
            CancellationToken::new(),
        )
        .await
        .expect("stream")
        .collect::<Vec<_>>()
        .await;
    assert!(events
        .iter()
        .all(|event| !matches!(event, Ok(ModelStreamEvent::Completed(_)))));
    assert!(
        matches!(events.last(), Some(Err(error)) if error.protocol_error().map(|protocol| protocol.kind()) == Some(crate::ProtocolErrorKind::Provider))
    );
}

#[tokio::test]
async fn malformed_content_block_sequences_are_provider_errors_without_completed() {
    let sequences = [
        concat!(
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n",
            "event: content_block_start\ndata: {\"index\":1,\"content_block\":{\"type\":\"text\"}}\n\n"
        ),
        concat!(
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"wrong\"}}\n\n"
        ),
        concat!(
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":1}\n\n"
        ),
        concat!(
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n",
            "event: message_stop\ndata: {}\n\n"
        ),
    ];

    for sequence in sequences {
        let transport = Arc::new(FakeTransport::with_response(FakeResponse {
            status: 200,
            request_id: None,
            body: FakeBody::Chunks(vec![Ok(sequence.as_bytes().to_vec())]),
        }));
        let model = AnthropicModel::with_transport(config_without_protocol_retry(), transport);
        let events = model
            .stream(
                ModelRequest::new(vec![ModelMessage::user_text("go")]),
                CancellationToken::new(),
            )
            .await
            .expect("stream")
            .collect::<Vec<_>>()
            .await;

        assert!(events
            .iter()
            .all(|event| !matches!(event, Ok(ModelStreamEvent::Completed(_)))));
        assert!(matches!(events.last(), Some(Err(error)) if error.protocol_error().is_some()));
    }
}

#[tokio::test]
async fn out_of_range_anthropic_usage_is_a_provider_error_without_completed() {
    for usage in [
        json!({ "input_tokens": 4_294_967_296_u64 }),
        json!({ "cache_creation_input_tokens": 4_294_967_296_u64 }),
        json!({ "cache_read_input_tokens": 4_294_967_296_u64 }),
        json!({ "output_tokens": 4_294_967_296_u64 }),
    ] {
        let events_data = if usage.get("output_tokens").is_some() {
            format!(
                "event: message_start\ndata: {{\"message\":{{\"id\":\"body-id\"}}}}\n\n\
                 event: message_delta\ndata: {{\"usage\":{usage}}}\n\n"
            )
        } else {
            format!(
                "event: message_start\ndata: {{\"message\":{{\"id\":\"body-id\",\"usage\":{usage}}}}}\n\n"
            )
        };
        let transport = Arc::new(FakeTransport::with_response(FakeResponse {
            status: 200,
            request_id: None,
            body: FakeBody::Chunks(vec![Ok(events_data.into_bytes())]),
        }));
        let model = AnthropicModel::with_transport(config_without_protocol_retry(), transport);
        let events = model
            .stream(
                ModelRequest::new(vec![ModelMessage::user_text("go")]),
                CancellationToken::new(),
            )
            .await
            .expect("stream")
            .collect::<Vec<_>>()
            .await;

        assert!(events
            .iter()
            .all(|event| !matches!(event, Ok(ModelStreamEvent::Completed(_)))));
        assert!(
            matches!(events.last(), Some(Err(error)) if error.protocol_error().is_some()),
            "unexpected events: {events:?}"
        );
    }
}

#[tokio::test]
async fn stream_uses_message_start_id_when_response_header_is_absent() {
    let transport = Arc::new(FakeTransport::with_response(FakeResponse {
        status: 200,
        request_id: None,
        body: FakeBody::Chunks(vec![Ok(concat!(
            "event: message_start\ndata: {\"message\":{\"id\":\"body-id\",\"usage\":{\"input_tokens\":1}}}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
            "event: message_stop\ndata: {}\n\n"
        )
        .as_bytes()
        .to_vec())]),
    }));
    let model = AnthropicModel::with_transport(config(), transport);
    let events = model
        .stream(
            ModelRequest::new(vec![ModelMessage::user_text("go")]),
            CancellationToken::new(),
        )
        .await
        .expect("stream")
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<ModelResult<Vec<_>>>()
        .expect("events");

    let completed = events
        .iter()
        .find_map(|event| match event {
            ModelStreamEvent::Completed(response) => Some(response),
            _ => None,
        })
        .expect("completed");
    assert_eq!(completed.request_id(), Some("body-id"));
}

#[tokio::test]
async fn stream_cancellation_with_anthropic_fixture_returns_cancelled() {
    let transport = Arc::new(FakeTransport::with_response(FakeResponse {
        status: 200,
        request_id: None,
        body: FakeBody::Pending,
    }));
    let model = AnthropicModel::with_transport(config(), transport);
    let cancellation = CancellationToken::new();
    let mut stream = model
        .stream(
            ModelRequest::new(vec![ModelMessage::user_text("go")]),
            cancellation.clone(),
        )
        .await
        .expect("stream");

    cancellation.cancel();
    assert!(matches!(stream.next().await, Some(Err(error)) if error.is_cancelled()));
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn visible_anthropic_delta_then_transport_failure_is_interrupted_without_retry() {
    let transport = Arc::new(FakeTransport::with_response(FakeResponse {
        status: 200,
        request_id: None,
        body: FakeBody::Chunks(vec![
            Ok(concat!(
                "event: message_start\ndata: {\"message\":{\"id\":\"body-id\"}}\n\n",
                "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n",
                "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n\n"
            )
            .as_bytes()
            .to_vec()),
            Err(ModelError::transport(TransportErrorKind::Connection, None::<&str>)),
        ]),
    }));
    let model = AnthropicModel::with_transport(config_with_retry(2), transport.clone());
    let events = model
        .stream(
            ModelRequest::new(vec![ModelMessage::user_text("go")]),
            CancellationToken::new(),
        )
        .await
        .expect("stream")
        .collect::<Vec<_>>()
        .await;

    assert!(events
        .iter()
        .any(|event| matches!(event, Ok(ModelStreamEvent::TextDelta { text }) if text == "hello")));
    assert!(events
        .iter()
        .all(|event| !matches!(event, Ok(ModelStreamEvent::Completed(_)))));
    assert!(
        matches!(events.last(), Some(Ok(ModelStreamEvent::Interrupted { error, attempts: 1, max_attempts: 2 })) if error.diagnostic().category_name() == "stream_interrupted")
    );
    assert_eq!(transport.calls(), 1);
}

#[test]
fn response_decoder_preserves_reasoning_signature_and_redacted_thinking() {
    let response = super::response::decode_completed_response(
        &json!({
            "id": "body-id",
            "content": [
                { "type": "thinking", "thinking": "reason", "signature": "sig" },
                { "type": "redacted_thinking", "data": "opaque" },
                { "type": "text", "text": "answer" },
            ],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 3, "cache_creation_input_tokens": 2, "output_tokens": 5 },
        }),
        Some("header-id".into()),
    )
    .expect("response");
    assert_eq!(response.request_id(), Some("header-id"));
    assert_eq!(response.usage().expect("usage").input_tokens, 5);
    let ModelMessage::Assistant { content, .. } = response.message() else {
        panic!("assistant response")
    };
    assert!(
        matches!(&content[0], ContentBlock::Reasoning { text, signature } if text == "reason" && signature.as_deref() == Some("sig"))
    );
    assert!(
        matches!(&content[1], ContentBlock::RedactedReasoning { data } if data.as_deref() == Some("opaque"))
    );
}

#[tokio::test]
async fn stream_without_message_stop_does_not_emit_completed() {
    let transport = Arc::new(FakeTransport::with_response(FakeResponse {
        status: 200,
        request_id: None,
        body: FakeBody::Chunks(vec![Ok(
            b"event: message_start\ndata: {\"message\":{\"id\":\"body-id\"}}\n\n".to_vec(),
        )]),
    }));
    let model = AnthropicModel::with_transport(config(), transport);
    let events = model
        .stream(
            ModelRequest::new(vec![ModelMessage::user_text("go")]),
            CancellationToken::new(),
        )
        .await
        .expect("stream")
        .collect::<Vec<_>>()
        .await;
    assert!(events
        .iter()
        .all(|event| !matches!(event, Ok(ModelStreamEvent::Completed(_)))));
    assert!(
        matches!(events.last(), Some(Err(error)) if error.retry_error_kind() == Some(crate::RetryErrorKind::Transport))
    );
}

/// [回归测试] 后续帧的 input/cache 必须替换初始值，缺失字段保留，零值可覆盖。
#[tokio::test]
async fn test_stream_final_usage_replaces_initial_input_and_cache() {
    for (delta_usage, stop_usage, expected) in [
        (
            json!({"input_tokens": 40, "cache_read_input_tokens": 60, "output_tokens": 7}),
            json!({}),
            (105, 5, 60, 7),
        ),
        (
            json!({"output_tokens": 7}),
            json!({"input_tokens": 80, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 20}),
            (100, 0, 20, 7),
        ),
        (
            json!({"input_tokens": 0, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0, "output_tokens": 2}),
            json!({}),
            (0, 0, 0, 2),
        ),
    ] {
        let response = [
            ("message_start", json!({"message": {"id": "usage-fixture", "usage": {
                "input_tokens": 10, "cache_creation_input_tokens": 5, "cache_read_input_tokens": 0
            }}})),
            ("message_delta", json!({"delta": {"stop_reason": "end_turn"}, "usage": delta_usage})),
            ("message_stop", json!({"usage": stop_usage})),
        ].into_iter().map(|(event, data)| format!("event: {event}\ndata: {data}\n\n")).collect::<String>();
        let transport = Arc::new(FakeTransport::with_response(FakeResponse {
            status: 200,
            request_id: None,
            body: FakeBody::Chunks(vec![Ok(response.into_bytes())]),
        }));
        let model = AnthropicModel::with_transport(config(), transport);
        let response = model
            .complete(
                ModelRequest::new(vec![ModelMessage::user_text("go")]),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let usage = response.usage().unwrap();
        assert_eq!(
            (
                usage.input_tokens,
                usage.cache_creation_input_tokens.unwrap(),
                usage.cache_read_input_tokens.unwrap(),
                usage.output_tokens
            ),
            expected
        );
    }
}

/// [回归测试] 最终 input/cache 溢出不得被旧的合法 start usage 掩盖。
#[tokio::test]
async fn test_stream_final_usage_overflow_rejected() {
    let transport = Arc::new(FakeTransport::with_response(FakeResponse {
        status: 200, request_id: None,
        body: FakeBody::Chunks(vec![Ok(concat!(
            "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":1}}}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":4294967295,\"cache_read_input_tokens\":1}}\n\n",
            "event: message_stop\ndata: {}\n\n"
        ).as_bytes().to_vec())]),
    }));
    let model = AnthropicModel::with_transport(config(), transport);
    let error = model
        .complete(
            ModelRequest::new(vec![ModelMessage::user_text("go")]),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.protocol_error().map(|error| error.kind()),
        Some(crate::ProtocolErrorKind::Provider)
    );
}

/// [回归测试] P1-4：`input_json_delta` 缺少 `partial_json` 或类型错都是解码诊断，
/// 不得发出空分片让残缺参数继续累积；诊断须随可见中断一起上报。
#[tokio::test]
async fn stream_tool_partial_json_type_error_is_diagnosed_without_partial_delta() {
    for (delta, expected_summary) in [
        (
            "{\"type\":\"input_json_delta\",\"partial_json\":{\"path\":\"a.rs\"}}",
            "anthropic_stream_partial_json_not_string",
        ),
        (
            "{\"type\":\"input_json_delta\"}",
            "anthropic_stream_partial_json_missing",
        ),
    ] {
        let transport = Arc::new(FakeTransport::with_response(FakeResponse {
            status: 200,
            request_id: None,
            body: FakeBody::Chunks(vec![Ok([
                "event: message_start\ndata: {\"message\":{\"id\":\"body-id\"}}\n\n".to_string(),
                "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tool-1\",\"name\":\"Read\"}}\n\n".to_string(),
                format!("event: content_block_delta\ndata: {{\"index\":0,\"delta\":{delta}}}\n\n"),
                "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n".to_string(),
                "event: message_stop\ndata: {}\n\n".to_string(),
            ]
            .join("")
            .into_bytes())]),
        }));
        let model =
            AnthropicModel::with_transport(config_without_protocol_retry(), transport.clone());
        let events = model
            .stream(
                ModelRequest::new(vec![ModelMessage::user_text("go")]),
                CancellationToken::new(),
            )
            .await
            .expect("stream")
            .collect::<Vec<_>>()
            .await;

        let tool_deltas = events
            .iter()
            .filter_map(|event| match event {
                Ok(ModelStreamEvent::ToolCallDelta {
                    arguments_delta, ..
                }) => Some(arguments_delta.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            tool_deltas,
            vec![""],
            "只有 content_block_start 的身份分片可见，畸形参数分片不得进入 ToolCallDelta：{events:?}"
        );
        let Some(Ok(ModelStreamEvent::Interrupted { error, .. })) = events.last() else {
            panic!("畸形 partial_json 必须终止为携带诊断的中断：{events:?}");
        };
        let protocol = error.protocol_error().expect("provider protocol error");
        assert_eq!(protocol.kind(), crate::ProtocolErrorKind::Provider);
        assert_eq!(
            protocol.summary(),
            Some(expected_summary),
            "解码摘要必须可读：{protocol}"
        );
        assert!(events
            .iter()
            .all(|event| !matches!(event, Ok(ModelStreamEvent::Completed(_)))));
        assert_eq!(transport.calls(), 1, "已产生可见分片的请求不得重放");
    }
}
