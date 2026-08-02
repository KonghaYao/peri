use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use futures::{stream, StreamExt};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{
    transport::{HttpBody, HttpRequest, HttpResponse, HttpTransport},
    ContentBlock, JsonObject, Model, ModelMessage, ModelRequest, ModelResult, ModelRuntimeConfig,
    ModelStreamEvent, RetryConfig, RetryableErrorClasses, StopReason, ToolCall, ToolDefinition,
    ToolResult,
};

use super::{request::body_for_test, OpenAiConfig, OpenAiModel};

#[derive(Default)]
struct FakeTransport {
    bodies: Mutex<Vec<Value>>,
    responses: Mutex<Vec<FakeResponse>>,
}

struct FakeResponse {
    status: u16,
    request_id: Option<String>,
    chunks: Vec<ModelResult<Vec<u8>>>,
}

impl FakeTransport {
    fn with_response(response: FakeResponse) -> Self {
        Self {
            bodies: Mutex::new(Vec::new()),
            responses: Mutex::new(vec![response]),
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
        let body = request
            .request
            .body()
            .and_then(reqwest::Body::as_bytes)
            .expect("JSON request body")
            .to_vec();
        self.bodies
            .lock()
            .expect("lock available")
            .push(serde_json::from_slice(&body).expect("valid request JSON"));
        let response = self.responses.lock().expect("lock available").remove(0);
        let body: HttpBody = Box::pin(stream::iter(response.chunks));
        Ok(HttpResponse::new(
            response.status,
            response.request_id,
            body,
            cancellation,
        ))
    }
}

fn config(model: &str) -> OpenAiConfig {
    OpenAiConfig::new(
        Url::parse("https://proxy.example.test/v1/").expect("valid endpoint"),
        "test-credential",
        model,
    )
    .with_runtime(
        ModelRuntimeConfig::default().with_retry(
            RetryConfig::default()
                .with_max_attempts(1)
                .with_base_delay(Duration::ZERO)
                .with_jitter(false),
        ),
    )
}

/// 关闭 Protocol 分类重试的配置，用于 fail-closed 分类断言（保留原始协议错误而非
/// `RetryExhausted(Protocol)`）。
fn config_without_protocol_retry(model: &str) -> OpenAiConfig {
    OpenAiConfig::new(
        Url::parse("https://proxy.example.test/v1/").expect("valid endpoint"),
        "test-credential",
        model,
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

fn request() -> ModelRequest {
    let schema = JsonObject::from_value(json!({
        "type": "object",
        "properties": { "path": { "type": "string" } },
    }))
    .expect("object");
    ModelRequest::new(vec![
        ModelMessage::system_text("base system"),
        ModelMessage::system_text("second system __SYSTEM_PROMPT_DYNAMIC_BOUNDARY__"),
        ModelMessage::user_text("read file"),
        ModelMessage::assistant(
            vec![
                ContentBlock::reasoning("previous reasoning"),
                ContentBlock::text("working"),
            ],
            vec![ToolCall::new(
                "call_1",
                "Read",
                JsonObject::from_value(json!({ "path": "a.rs" })).expect("object"),
            )],
        ),
        ModelMessage::tool_result(ToolResult::success("call_1", "Read", "file contents")),
    ])
    .with_tools(vec![
        ToolDefinition::new("Read", schema).with_description("read a file")
    ])
    .with_max_tokens(123)
}

#[test]
fn request_contract_preserves_system_tools_tool_results_and_reasoning() {
    let body = body_for_test(&config("deepseek-r1"), &request());
    let messages = body["messages"].as_array().expect("messages array");
    assert_eq!(
        messages[0],
        json!({ "role": "system", "content": "base system\n\nsecond system " })
    );
    assert_eq!(
        messages[1],
        json!({ "role": "user", "content": "read file" })
    );
    assert_eq!(messages[2]["reasoning_content"], "previous reasoning");
    assert_eq!(messages[2]["tool_calls"][0]["function"]["name"], "Read");
    assert_eq!(
        messages[2]["tool_calls"][0]["function"]["arguments"],
        "{\"path\":\"a.rs\"}"
    );
    assert_eq!(
        messages[3],
        json!({
            "role": "tool",
            "tool_call_id": "call_1",
            "content": "file contents",
        })
    );
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["function"]["parameters"]["type"], "object");
    assert_eq!(body["tool_choice"], "auto");
    assert_eq!(body["max_tokens"], 123);
    assert_eq!(body["stream"], true);
}

#[test]
fn request_contract_covers_qwen_kimi_and_litellm_options() {
    let qwen = body_for_test(&config("qwen3"), &request());
    assert_eq!(qwen["stream_options"], json!({ "include_usage": true }));

    let kimi = body_for_test(
        &config("kimi-k2")
            .with_thinking_enabled(true)
            .with_reasoning_effort("high"),
        &request(),
    );
    assert_eq!(kimi["thinking"], json!({ "type": "enabled" }));
    assert!(kimi.get("reasoning_effort").is_none());

    let metadata = body_for_test(
        &config("litellm").with_thinking_content(true),
        &ModelRequest {
            session_id: Some("session-1".into()),
            ..request()
        },
    );
    assert_eq!(metadata["metadata"], json!({ "session_id": "session-1" }));
    assert_eq!(metadata["messages"][2]["content"][0]["type"], "thinking");
}

#[test]
fn config_debug_does_not_expose_credential() {
    let rendered = format!("{:?}", config("gpt-4o"));
    assert!(!rendered.contains("test-credential"));
    assert!(rendered.contains("[REDACTED]"));
}

#[test]
fn config_debug_redacts_all_endpoint_components() {
    let config = OpenAiConfig::new(
        Url::parse("https://user:sk-live-secret@api.example.test/v1?api_key=secret#fragment")
            .expect("valid endpoint"),
        "test-credential",
        "gpt-4o",
    );

    let rendered = format!("{config:?}");

    assert!(rendered.contains("https://api.example.test/[REDACTED]"));
    for sensitive_fragment in [
        "user",
        "sk-live-secret",
        "v1",
        "api_key=secret",
        "fragment",
        "test-credential",
    ] {
        assert!(
            !rendered.contains(sensitive_fragment),
            "Debug output exposed {sensitive_fragment:?}: {rendered}"
        );
    }
}

#[test]
fn chat_completions_endpoint_preserves_base_path_without_trailing_slash() {
    for (base_url, expected_endpoint) in [
        ("https://host/v1", "https://host/v1/chat/completions"),
        ("https://host/v1/", "https://host/v1/chat/completions"),
        (
            "https://host/custom/openai/v1",
            "https://host/custom/openai/v1/chat/completions",
        ),
    ] {
        let endpoint = super::request::chat_completions_endpoint(
            &Url::parse(base_url).expect("valid endpoint"),
        )
        .expect("chat completions endpoint");

        assert_eq!(endpoint.as_str(), expected_endpoint);
    }
}

#[tokio::test]
async fn chat_completions_endpoint_rejects_userinfo() {
    let transport = Arc::new(FakeTransport::with_response(FakeResponse {
        status: 200,
        request_id: None,
        chunks: vec![],
    }));
    let model = OpenAiModel::with_transport(
        OpenAiConfig::new(
            Url::parse("https://user:password@proxy.example.test/v1/").expect("valid endpoint URL"),
            "test-credential",
            "gpt-4o",
        ),
        transport.clone(),
    );

    let error = match model.stream(request(), CancellationToken::new()).await {
        Err(error) => error,
        Ok(_) => panic!("userinfo endpoint must be rejected before transport"),
    };

    assert_eq!(
        error.protocol_error().map(|error| error.kind()),
        Some(crate::ProtocolErrorKind::InvalidEndpoint)
    );
    assert!(transport.bodies().is_empty());
}

#[tokio::test]
async fn prepared_body_and_sent_body_share_one_request_builder() {
    let transport = Arc::new(FakeTransport::with_response(FakeResponse {
        status: 200,
        request_id: None,
        chunks: vec![Ok(b"data: {\"id\":\"resp-1\",\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".to_vec())],
    }));
    let model = OpenAiModel::with_transport(config("gpt-4o"), transport.clone());
    let request = request();
    let prepared = model.prepare_request(&request).expect("prepared request");
    let events = model
        .stream(request, CancellationToken::new())
        .await
        .expect("stream")
        .collect::<Vec<_>>()
        .await;

    assert!(events.iter().all(ModelResult::is_ok));
    assert_eq!(transport.bodies(), vec![prepared.body().as_value().clone()]);
}

#[tokio::test]
async fn stream_eof_without_done_after_delta_is_interrupted_without_retry() {
    let transport = Arc::new(FakeTransport::with_response(FakeResponse {
        status: 200,
        request_id: None,
        chunks: vec![Ok(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n".to_vec(),
        )],
    }));
    let model = OpenAiModel::with_transport(config("gpt-4o"), transport.clone());
    let events = model
        .stream(
            ModelRequest::new(vec![ModelMessage::user_text("go")]),
            CancellationToken::new(),
        )
        .await
        .expect("stream")
        .collect::<Vec<_>>()
        .await;

    assert!(matches!(
        events.as_slice(),
        [
            Ok(ModelStreamEvent::TextDelta { text }),
            Err(error),
        ] if text == "partial" && error.provider() == Some("openai-compatible")
    ));
    assert!(events
        .iter()
        .all(|event| !matches!(event, Ok(ModelStreamEvent::Completed(_)))));
    assert_eq!(transport.bodies().len(), 1);
}

#[tokio::test]
async fn stream_invalid_utf8_is_provider_protocol_error_without_completed() {
    let transport = Arc::new(FakeTransport::with_response(FakeResponse {
        status: 200,
        request_id: None,
        chunks: vec![Ok(b"data: \xff\n\n".to_vec())],
    }));
    let model =
        OpenAiModel::with_transport(config_without_protocol_retry("gpt-4o"), transport.clone());
    let events = model
        .stream(
            ModelRequest::new(vec![ModelMessage::user_text("go")]),
            CancellationToken::new(),
        )
        .await
        .expect("stream")
        .collect::<Vec<_>>()
        .await;

    assert!(matches!(
        events.as_slice(),
        [Err(error)]
            if error.protocol_error().map(|error| error.kind())
                == Some(crate::ProtocolErrorKind::Provider)
    ));
    assert!(events
        .iter()
        .all(|event| !matches!(event, Ok(ModelStreamEvent::Completed(_)))));
    assert_eq!(transport.bodies().len(), 1);
}

#[tokio::test]
async fn stream_emits_completed_after_trailing_qwen_usage() {
    let transport = Arc::new(FakeTransport::with_response(FakeResponse {
        status: 200,
        request_id: None,
        chunks: vec![Ok(concat!(
            "data: {\"id\":\"chatcmpl-usage\",\"choices\":[{\"delta\":{\"content\":\"answer\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":5}}\n\n",
            "data: [DONE]\n\n"
        )
        .as_bytes()
        .to_vec())],
    }));
    let model = OpenAiModel::with_transport(config("qwen3"), transport);
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
        .expect("valid events");

    assert!(matches!(
        events.as_slice(),
        [
            ModelStreamEvent::TextDelta { text },
            ModelStreamEvent::Usage(usage),
            ModelStreamEvent::Completed(response),
        ] if text == "answer"
            && usage.input_tokens == 3
            && usage.output_tokens == 5
            && response.usage() == Some(usage)
    ));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ModelStreamEvent::Completed(_)))
            .count(),
        1
    );
}

#[tokio::test]
async fn stream_emits_standard_events_and_aggregates_interleaved_tools() {
    let transport = Arc::new(FakeTransport::with_response(FakeResponse {
        status: 200,
        request_id: Some("header-ignored".into()),
        chunks: vec![Ok(concat!(
            "data: {\"id\":\"chatcmpl-1\",\"choices\":[{\"delta\":{\"reasoning\":\"think \"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"call-b\",\"function\":{\"name\":\"B\",\"arguments\":\"{\\\"b\\\":\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-a\",\"function\":{\"name\":\"A\",\"arguments\":\"{\\\"a\\\":\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":5,\"prompt_tokens_details\":{\"cached_tokens\":1}}}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"answer\",\"tool_calls\":[{\"index\":1,\"function\":{\"arguments\":\"2}\"}},{\"index\":0,\"function\":{\"arguments\":\"1}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n"
        ).as_bytes().to_vec())],
    }));
    let model = OpenAiModel::with_transport(config("qwen3"), transport);
    let events = model
        .stream(
            ModelRequest::new(vec![ModelMessage::user_text("go")]),
            CancellationToken::new(),
        )
        .await
        .expect("stream")
        .collect::<Vec<_>>()
        .await;
    let events = events
        .into_iter()
        .collect::<ModelResult<Vec<_>>>()
        .expect("valid events");

    assert!(events.iter().any(
        |event| matches!(event, ModelStreamEvent::ReasoningDelta { text } if text == "think ")
    ));
    assert!(events
        .iter()
        .any(|event| matches!(event, ModelStreamEvent::TextDelta { text } if text == "answer")));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ModelStreamEvent::ToolCallDelta { .. }))
            .count(),
        4
    );
    assert!(events.iter().any(|event| matches!(event, ModelStreamEvent::Usage(usage) if usage.input_tokens == 3 && usage.output_tokens == 5 && usage.cache_read_input_tokens == Some(1))));

    let completed = events
        .into_iter()
        .find_map(|event| match event {
            ModelStreamEvent::Completed(response) => Some(response),
            _ => None,
        })
        .expect("completed event");
    assert_eq!(completed.stop_reason(), &StopReason::ToolUse);
    assert_eq!(completed.request_id(), Some("chatcmpl-1"));
    assert_eq!(completed.assistant_text(), Some("answer".into()));
    assert_eq!(completed.usage().expect("usage").output_tokens, 5);
    let ModelMessage::Assistant {
        content,
        tool_calls,
    } = completed.message()
    else {
        panic!("assistant response required");
    };
    assert!(matches!(&content[0], ContentBlock::Reasoning { text, .. } if text == "think "));
    assert_eq!(tool_calls[0].id(), "call-a");
    assert_eq!(tool_calls[0].arguments().as_map()["a"], 1);
    assert_eq!(tool_calls[1].id(), "call-b");
    assert_eq!(tool_calls[1].arguments().as_map()["b"], 2);
}

#[test]
fn response_decoder_accepts_reasoning_content_and_reasoning() {
    let from_content = super::response::decode_assistant_message(&json!({
        "reasoning_content": "r1",
        "content": "answer",
    }))
    .expect("decode");
    assert!(matches!(&from_content.0[0], ContentBlock::Reasoning { text, .. } if text == "r1"));

    let from_reasoning = super::response::decode_assistant_message(&json!({
        "reasoning": "r2",
        "content": [{ "type": "text", "text": "answer" }],
    }))
    .expect("decode");
    assert!(matches!(&from_reasoning.0[0], ContentBlock::Reasoning { text, .. } if text == "r2"));
}

#[test]
fn response_decoder_uses_content_thinking_when_top_level_reasoning_is_empty() {
    let (content, _) = super::response::decode_assistant_message(&json!({
        "reasoning_content": "",
        "content": [{ "type": "thinking", "thinking": "actual thought" }],
    }))
    .expect("decode");

    assert!(
        matches!(&content[0], ContentBlock::Reasoning { text, .. } if text == "actual thought")
    );
}
