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
    ContentBlock, Model, ModelMessage, ModelRequest, ModelResult, ModelRuntimeConfig,
    ModelStreamEvent, RetryConfig, RetryableErrorClasses, StopReason,
};

use super::{request::body_for_test, AnthropicConfig, AnthropicModel};

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

    fn bodies(&self) -> Vec<Value> {
        self.bodies.lock().expect("lock available").clone()
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

#[tokio::test]
async fn stream_response_roundtrip_serializes_tool_use_once() {
    let transport = Arc::new(FakeTransport::with_response(FakeResponse {
        status: 200,
        request_id: None,
        body: FakeBody::Chunks(vec![Ok(concat!(
            "event: message_start\ndata: {\"message\":{\"id\":\"body-id\",\"usage\":{\"input_tokens\":1}}}\n\n",
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tool-1\",\"name\":\"Read\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"a.rs\\\"}\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n",
            "event: message_stop\ndata: {}\n\n"
        ).as_bytes().to_vec())]),
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
    let response = events
        .into_iter()
        .find_map(|event| match event {
            ModelStreamEvent::Completed(response) => Some(response),
            _ => None,
        })
        .expect("completed response");

    let body = body_for_test(
        &config(),
        &ModelRequest::new(vec![response.message().clone()]),
    );
    let tool_uses = body["messages"][0]["content"]
        .as_array()
        .expect("assistant content")
        .iter()
        .filter(|block| block["type"] == "tool_use" && block["id"] == "tool-1")
        .count();
    assert_eq!(tool_uses, 1);
}

#[tokio::test]
async fn prepared_body_and_sent_body_share_one_request_builder_without_headers() {
    let transport = Arc::new(FakeTransport::with_response(FakeResponse {
        status: 200,
        request_id: Some("header-id".into()),
        body: FakeBody::Chunks(vec![Ok(concat!(
            "event: message_start\ndata: {\"message\":{\"id\":\"body-id\",\"usage\":{\"input_tokens\":1}}}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
            "event: message_stop\ndata: {}\n\n"
        )
        .as_bytes()
        .to_vec())]),
    }));
    let model = AnthropicModel::with_transport(config(), transport.clone());
    let request = ModelRequest::new(vec![ModelMessage::user_text("go")]);
    let prepared = model.prepare_request(&request).expect("prepared request");
    let events = model
        .stream(request, CancellationToken::new())
        .await
        .expect("stream")
        .collect::<Vec<_>>()
        .await;
    assert!(events.iter().all(ModelResult::is_ok));
    assert_eq!(transport.bodies(), vec![prepared.body().as_value().clone()]);
    assert!(!serde_json::to_string(&prepared)
        .expect("serialize")
        .contains("header-id"));
}

/// [回归测试] Anthropic extended thinking 的 `signature_delta` 必须累积到最终 reasoning block。
///
/// 历史背景：decoder 仅接受 thinking_delta，合法 provider 的 signature_delta 会被拒绝；
/// 已经发出的 reasoning 还会使该协议错误被错误归类为连接中断。
#[tokio::test]
async fn anthropic_extended_thinking_preserves_signature_delta() {
    let transport = Arc::new(FakeTransport::with_response(FakeResponse {
        status: 200,
        request_id: None,
        body: FakeBody::Chunks(vec![Ok(concat!(
            "event: message_start\ndata: {\"message\":{\"id\":\"body-id\"}}\n\n",
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"thinking\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"think\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig-a\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig-b\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
            "event: message_stop\ndata: {}\n\n"
        )
        .as_bytes()
        .to_vec())]),
    }));
    let events = AnthropicModel::with_transport(config(), transport)
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
    let ModelMessage::Assistant { content, .. } = completed.message() else {
        panic!("assistant response");
    };
    assert!(
        matches!(&content[0], ContentBlock::Reasoning { text, signature } if text == "think" && signature.as_deref() == Some("sig-asig-b"))
    );
}

#[tokio::test]
async fn stream_emits_standard_events_with_header_first_request_id_and_completed_only_on_message_stop(
) {
    let transport = Arc::new(FakeTransport::with_response(FakeResponse {
        status: 200,
        request_id: Some("header-id".into()),
        body: FakeBody::Chunks(vec![Ok(concat!(
            "event: message_start\ndata: {\"message\":{\"id\":\"body-id\",\"usage\":{\"input_tokens\":3,\"cache_read_input_tokens\":2}}}\n\n",
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"thinking\",\"signature\":\"sig\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"think\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: content_block_start\ndata: {\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tool-1\",\"name\":\"Read\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"a.rs\\\"}\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":1}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":5}}\n\n",
            "event: message_stop\ndata: {}\n\n"
        ).as_bytes().to_vec())]),
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
    assert!(events.iter().any(
        |event| matches!(event, ModelStreamEvent::ReasoningDelta { text } if text == "think")
    ));
    assert!(events.iter().any(|event| matches!(event, ModelStreamEvent::ToolCallDelta { index: 1, id: Some(id), name: Some(name), .. } if id == "tool-1" && name == "Read")));
    assert!(events.iter().any(|event| matches!(
        event,
        ModelStreamEvent::Usage(usage)
            if usage.input_tokens == 5 && usage.output_tokens == 0
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        ModelStreamEvent::Usage(usage)
            if usage.input_tokens == 5 && usage.output_tokens == 5
    )));
    assert!(events.iter().any(|event| matches!(event, ModelStreamEvent::ToolCallDelta { index: 1, id: None, name: None, arguments_delta } if arguments_delta == "{\"path\":\"a.rs\"}")));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ModelStreamEvent::Completed(_)))
            .count(),
        1
    );
    let completed = events
        .iter()
        .find_map(|event| match event {
            ModelStreamEvent::Completed(response) => Some(response),
            _ => None,
        })
        .expect("completed");
    assert_eq!(completed.request_id(), Some("header-id"));
    assert_eq!(completed.stop_reason(), &StopReason::ToolUse);
    assert_eq!(completed.usage().expect("usage").input_tokens, 5);
    let ModelMessage::Assistant {
        content,
        tool_calls,
    } = completed.message()
    else {
        panic!("assistant response")
    };
    assert!(
        matches!(&content[0], ContentBlock::Reasoning { text, signature } if text == "think" && signature.as_deref() == Some("sig"))
    );
    assert_eq!(tool_calls[0].arguments().as_map()["path"], "a.rs");
}

/// [回归测试] Anthropic 必需的 message 与终态 delta payload 缺失时不得产生 Completed。
///
/// 历史背景：decoder 曾把缺失 message 当 Null、缺失 delta 当默认 EndTurn，因此只含空对象的
/// lifecycle 也会完成。此类损坏 provider payload 必须在任何响应对外可见前 fail closed。
#[tokio::test]
async fn anthropic_requires_message_start_and_message_delta_payloads() {
    for sequence in [
        concat!(
            "event: message_start\ndata: {}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
            "event: message_stop\ndata: {}\n\n"
        ),
        concat!(
            "event: message_start\ndata: {\"message\":{\"id\":\"body-id\"}}\n\n",
            "event: message_delta\ndata: {}\n\n",
            "event: message_stop\ndata: {}\n\n"
        ),
        concat!(
            "event: message_start\ndata: {\"message\":{\"id\":\"body-id\"}}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":null}}\n\n",
            "event: message_stop\ndata: {}\n\n"
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
        assert!(
            matches!(events.last(), Some(Err(error)) if error.protocol_error().map(|protocol| protocol.kind()) == Some(crate::ProtocolErrorKind::Provider))
        );
    }
}

/// [回归测试] Anthropic `message_stop` 必须由唯一的 `message_delta` 终态事件前置。
///
/// 历史背景：状态机曾把 `message_start -> message_stop` 当成完整响应，丢失 provider 的
/// stop reason/最终 usage 阶段也仍发出 Completed，形成不完整 lifecycle 的 fail-open。
#[tokio::test]
async fn anthropic_message_stop_requires_message_delta() {
    let transport = Arc::new(FakeTransport::with_response(FakeResponse {
        status: 200,
        request_id: None,
        body: FakeBody::Chunks(vec![Ok(concat!(
            "event: message_start\ndata: {\"message\":{\"id\":\"body-id\"}}\n\n",
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

/// [回归测试] Anthropic SSE 的 JSON `type` 存在时必须是字符串且与 event 一致。
///
/// 历史背景：decoder 使用 `as_str()` 读取 type，把 `null` 或对象与 type 缺失混同；在
/// 有 `event:` 时该损坏 payload 会被接受，绕过 event/type 冲突校验。
#[tokio::test]
async fn anthropic_rejects_non_string_payload_type() {
    let transport = Arc::new(FakeTransport::with_response(FakeResponse {
        status: 200,
        request_id: None,
        body: FakeBody::Chunks(vec![Ok(
            "event: message_start\ndata: {\"type\":null,\"message\":{\"id\":\"body-id\"}}\n\n"
                .as_bytes()
                .to_vec(),
        )]),
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
    assert!(
        matches!(events.last(), Some(Err(error)) if error.protocol_error().map(|protocol| protocol.kind()) == Some(crate::ProtocolErrorKind::Provider))
    );
}

/// [回归测试] Anthropic 完成阶段必须拒绝重复/矛盾的生命周期事件。
///
/// 历史背景：状态机最初只校验 active block，重复 `message_stop`、`message_delta` 后新 block
/// 以及 SSE event 与 JSON type 相冲突时仍可能完成，导致损坏 stream 被 fail-open 接受。
#[tokio::test]
async fn anthropic_completed_phase_rejects_repeated_or_conflicting_events() {
    for sequence in [
        concat!(
            "event: message_start\ndata: {\"message\":{\"id\":\"body-id\"}}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n"
        ),
        concat!(
            "event: message_start\ndata: {\"message\":{\"id\":\"body-id\"}}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n"
        ),
        "event: message_start\ndata: {\"type\":\"message_stop\",\"message\":{\"id\":\"body-id\"}}\n\n",
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

/// [回归测试] 完整响应解码的 usage 总和也必须拒绝溢出。
///
/// 历史背景：stream decoder 已对总 input usage 做 checked_add，但测试用完整响应 decoder
/// 仍使用普通 u32 加法，导致同一协议数据在不同解码入口出现 panic 或静默回绕。
#[test]
fn response_decoder_rejects_total_input_usage_overflow() {
    let error = super::response::decode_completed_response(
        &json!({
            "content": [],
            "usage": { "input_tokens": 4_294_967_295_u64, "cache_read_input_tokens": 1, "output_tokens": 0 },
        }),
        None,
    )
    .expect_err("overflow must be rejected");
    assert_eq!(
        error.protocol_error().map(|protocol| protocol.kind()),
        Some(crate::ProtocolErrorKind::Provider)
    );
}
