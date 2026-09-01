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
    prompt_cache::{combine_system_prompt_with_dynamic, SYSTEM_PROMPT_DYNAMIC_BOUNDARY},
    transport::{HttpBody, HttpRequest, HttpResponse, HttpTransport},
    ContentBlock, JsonObject, Model, ModelError, ModelMessage, ModelRequest, ModelResult,
    ModelRuntimeConfig, ModelStreamEvent, RetryConfig, RetryableErrorClasses, StopReason, ToolCall,
    ToolDefinition, ToolResult, TransportErrorKind,
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

    fn bodies(&self) -> Vec<Value> {
        self.bodies.lock().expect("lock available").clone()
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

fn request() -> ModelRequest {
    let schema = JsonObject::from_value(json!({ "type": "object" })).expect("object");
    ModelRequest::new(vec![
        ModelMessage::system_text(format!("static\n{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}\ndynamic")),
        ModelMessage::system_text("middleware content"),
        ModelMessage::user_text("first question"),
        ModelMessage::assistant(
            vec![ContentBlock::Reasoning {
                text: "previous reasoning".into(),
                signature: Some("sig-1".into()),
            }],
            vec![ToolCall::new(
                "call-a",
                "Read",
                JsonObject::from_value(json!({ "path": "a.rs" })).expect("object"),
            )],
        ),
        ModelMessage::tool_result(ToolResult::success("call-a", "Read", "a")),
        ModelMessage::tool_result(ToolResult::error("call-b", "Write", "denied")),
    ])
    .with_tools(vec![
        ToolDefinition::new("Read", schema).with_description("read a file")
    ])
    .with_max_tokens(123)
}

#[test]
fn request_contract_preserves_system_cache_thinking_and_tool_result_order() {
    let body = body_for_test(&config().with_extended_thinking(456, "high"), &request());
    let system = body["system"].as_array().expect("cached system blocks");
    assert_eq!(system[0]["text"], "static");
    assert_eq!(system[0]["cache_control"]["type"], "ephemeral");
    assert!(system[1]["text"]
        .as_str()
        .expect("text")
        .contains("middleware content"));
    assert!(system[1].get("cache_control").is_none());
    assert_eq!(
        body["thinking"],
        json!({ "type": "enabled", "budget_tokens": 456 })
    );
    assert_eq!(body["output_config"], json!({ "effort": "high" }));
    assert_eq!(
        body["tools"][0]["input_schema"],
        json!({ "type": "object" })
    );
    assert_eq!(body["messages"][1]["content"][0]["type"], "thinking");
    assert_eq!(body["messages"][1]["content"][0]["signature"], "sig-1");
    let results = body["messages"][2]["content"]
        .as_array()
        .expect("tool results");
    assert_eq!(results[0]["tool_use_id"], "call-a");
    assert_eq!(results[1]["tool_use_id"], "call-b");
    assert_eq!(results[1]["is_error"], true);
}

/// [回归测试] 连续工具轮次必须把 prompt cache 断点推进到最新 tool_result；
/// 否则长上下文会把新增工具历史持续留在未缓存后缀，命中率逐轮下降。
#[test]
fn test_cache_breakpoint_advances_to_latest_tool_result_round() {
    let history = ModelRequest::new(vec![
        ModelMessage::user_text("run a tool"),
        ModelMessage::assistant(
            Vec::new(),
            vec![
                ToolCall::new(
                    "call-cache-a",
                    "Read",
                    JsonObject::from_value(json!({ "path": "a.rs" })).expect("object"),
                ),
                ToolCall::new(
                    "call-cache-b",
                    "Read",
                    JsonObject::from_value(json!({ "path": "b.rs" })).expect("object"),
                ),
            ],
        ),
        ModelMessage::tool_result(ToolResult::success(
            "call-cache-a",
            "Read",
            "first tool output",
        )),
        ModelMessage::tool_result(ToolResult::success(
            "call-cache-b",
            "Read",
            "second tool output",
        )),
    ]);
    let body = body_for_test(&config(), &history);
    let latest_results = body["messages"][2]["content"]
        .as_array()
        .expect("最后一条 user message 必须包含 tool results");
    assert_eq!(
        latest_results.len(),
        2,
        "同轮工具结果必须合并为一条 user message"
    );
    assert!(
        latest_results[0].get("cache_control").is_none(),
        "同一 user message 只标记最后一个可缓存 block"
    );
    assert_eq!(
        latest_results.last().expect("至少一个 tool result")["cache_control"]["type"],
        "ephemeral"
    );
    assert!(
        cache_breakpoint_count(&body) <= 4,
        "总断点数不得超过 Anthropic 上限"
    );
}

/// [回归测试] append 新工具轮次时，旧请求的语义前缀必须逐字节保持；仅允许
/// cache_control 从旧 latest 滑动为新 second-last，并在新 latest 增加断点。
#[test]
fn test_cache_breakpoint_preserves_prefix_when_tool_round_is_appended() {
    let before = tool_history_request(3, false);
    let after = tool_history_request(4, false);
    let before_body = body_for_test(&config(), &before);
    let after_body = body_for_test(&config(), &after);

    assert_eq!(
        serde_json::to_vec(&before_body["system"]).expect("system serializes"),
        serde_json::to_vec(&after_body["system"]).expect("system serializes"),
        "固定 system 必须逐字节稳定"
    );
    assert_eq!(
        serde_json::to_vec(&before_body["tools"]).expect("tools serialize"),
        serde_json::to_vec(&after_body["tools"]).expect("tools serialize"),
        "固定 tools 必须逐字节稳定"
    );
    let before_messages = before_body["messages"].as_array().expect("messages array");
    let after_messages = after_body["messages"].as_array().expect("messages array");
    assert_eq!(
        strip_cache_control(Value::Array(before_messages.clone())),
        strip_cache_control(Value::Array(
            after_messages[..before_messages.len()].to_vec()
        )),
        "append 前后的旧 message 语义前缀必须相同"
    );
    assert_eq!(
        before_messages.last().expect("before latest user")["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
    assert_eq!(
        after_messages[before_messages.len() - 1]["content"][0]["cache_control"]["type"],
        "ephemeral",
        "旧 latest tool_result 在 append 后必须保留为 second-last breakpoint"
    );
    assert_eq!(
        after_messages.last().expect("after latest user")["content"][0]["cache_control"]["type"],
        "ephemeral",
        "新 latest tool_result 必须成为最新 breakpoint"
    );
    assert!(cache_breakpoint_count(&before_body) <= 4);
    assert!(cache_breakpoint_count(&after_body) <= 4);
}

/// [回归测试] 快速输入普通 Human prompt 时，上一轮 tool_result 必须仍处于
/// second-last breakpoint；否则第一次请求只能命中更旧的文本历史。
#[test]
fn test_cache_breakpoint_keeps_latest_tool_result_before_new_human_prompt() {
    let body = body_for_test(&config(), &tool_history_request(3, true));
    let messages = body["messages"].as_array().expect("messages array");
    let previous_tool_result = &messages[messages.len() - 2]["content"][0];
    let latest_human = &messages[messages.len() - 1]["content"][0];

    assert_eq!(previous_tool_result["type"], "tool_result");
    assert_eq!(previous_tool_result["cache_control"]["type"], "ephemeral");
    assert_eq!(latest_human["type"], "text");
    assert_eq!(latest_human["cache_control"]["type"], "ephemeral");
    assert!(cache_breakpoint_count(&body) <= 4);
}

fn tool_history_request(rounds: usize, append_human: bool) -> ModelRequest {
    let schema = JsonObject::from_value(json!({
        "type": "object",
        "properties": { "path": { "type": "string" } },
    }))
    .expect("object");
    let mut messages = vec![
        ModelMessage::system_text("stable system"),
        ModelMessage::user_text("inspect files"),
    ];
    for round in 0..rounds {
        let call_id = format!("call-{round}");
        messages.push(ModelMessage::assistant(
            Vec::new(),
            vec![ToolCall::new(
                call_id.clone(),
                "Read",
                JsonObject::from_value(json!({ "path": format!("{round}.rs") })).expect("object"),
            )],
        ));
        messages.push(ModelMessage::tool_result(ToolResult::success(
            call_id,
            "Read",
            format!("result-{round}"),
        )));
    }
    if append_human {
        messages.push(ModelMessage::user_text("continue quickly"));
    }
    ModelRequest::new(messages).with_tools(vec![
        ToolDefinition::new("Read", schema).with_description("read a file")
    ])
}

fn strip_cache_control(mut value: Value) -> Value {
    match &mut value {
        Value::Array(values) => {
            for value in values {
                *value = strip_cache_control(value.take());
            }
        }
        Value::Object(map) => {
            map.remove("cache_control");
            for value in map.values_mut() {
                *value = strip_cache_control(value.take());
            }
        }
        _ => {}
    }
    value
}

fn cache_breakpoint_count(value: &Value) -> usize {
    match value {
        Value::Array(values) => values.iter().map(cache_breakpoint_count).sum(),
        Value::Object(map) => {
            usize::from(map.contains_key("cache_control"))
                + map.values().map(cache_breakpoint_count).sum::<usize>()
        }
        _ => 0,
    }
}

#[test]
fn request_contract_without_cache_uses_plain_top_level_system() {
    let body = body_for_test(&config().without_cache(), &request());
    assert!(body["system"].is_string());
    assert!(!body["system"]
        .as_str()
        .expect("system text")
        .contains(SYSTEM_PROMPT_DYNAMIC_BOUNDARY));
    assert!(body["messages"][0]["content"][0]
        .get("cache_control")
        .is_none());
}

#[test]
fn system_cache_boundary_four_state_matrix_and_fallback_are_explicit() {
    let cases = [
        (
            format!("STATIC{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}\n\nDYNAMIC"),
            json!([
                { "type": "text", "text": "STATIC", "cache_control": { "type": "ephemeral" } },
                { "type": "text", "text": "DYNAMIC" },
            ]),
        ),
        (
            format!("STATIC{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}"),
            json!([
                { "type": "text", "text": "STATIC", "cache_control": { "type": "ephemeral" } },
            ]),
        ),
        (
            format!("{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}\n\nDYNAMIC"),
            json!([{ "type": "text", "text": "DYNAMIC" }]),
        ),
        (
            "LEGACY-WITHOUT-BOUNDARY".to_string(),
            json!([
                { "type": "text", "text": "LEGACY-WITHOUT-BOUNDARY", "cache_control": { "type": "ephemeral" } },
            ]),
        ),
    ];
    for (system_text, expected) in cases {
        let body = body_for_test(
            &config(),
            &ModelRequest::new(vec![
                ModelMessage::system_text(system_text),
                ModelMessage::user_text("go"),
            ]),
        );
        assert_eq!(body["system"], expected);
        assert!(!serde_json::to_string(&body["system"])
            .expect("system serializes")
            .contains(SYSTEM_PROMPT_DYNAMIC_BOUNDARY));
    }

    let empty = body_for_test(
        &config(),
        &ModelRequest::new(vec![
            ModelMessage::system_text(""),
            ModelMessage::user_text("go"),
        ]),
    );
    assert!(empty.get("system").is_none());
}

#[test]
fn repeated_system_cache_boundary_fails_closed_without_wire_leak() {
    let system = format!(
        "STATIC{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}DYNAMIC{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}TAIL"
    );
    let body = body_for_test(
        &config(),
        &ModelRequest::new(vec![
            ModelMessage::system_text(system),
            ModelMessage::user_text("go"),
        ]),
    );
    let blocks = body["system"].as_array().expect("system blocks");

    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0]["text"], "STATICDYNAMICTAIL");
    assert!(blocks[0].get("cache_control").is_none());
    assert!(!serde_json::to_string(blocks)
        .expect("blocks serialize")
        .contains(SYSTEM_PROMPT_DYNAMIC_BOUNDARY));
}

#[test]
fn duplicate_boundary_plus_dynamic_contribution_remains_uncached_and_byte_preserving() {
    let base = format!("A{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}B{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}C");
    let combined = combine_system_prompt_with_dynamic(Some(&base), "REQUEST").unwrap();
    let body = body_for_test(
        &config(),
        &ModelRequest::new(vec![
            ModelMessage::system_text(combined),
            ModelMessage::user_text("go"),
        ]),
    );
    let blocks = body["system"].as_array().expect("system blocks");
    assert_eq!(
        blocks,
        &[json!({ "type": "text", "text": "ABC\n\nREQUEST" })]
    );
    assert!(blocks[0].get("cache_control").is_none());
}

#[test]
fn system_cache_prefix_is_stable_and_dynamic_order_is_preserved() {
    let body = |dynamic: &str| {
        body_for_test(
            &config(),
            &ModelRequest::new(vec![
                ModelMessage::system_text(format!(
                    "BASE-STATIC{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}\n\n{dynamic}"
                )),
                ModelMessage::system_text("REQUEST-MIDDLEWARE"),
                ModelMessage::user_text("go"),
            ]),
        )
    };
    let first = body("BASE-DYNAMIC-A");
    let second = body("BASE-DYNAMIC-B");
    let first_system = first["system"].as_array().expect("system blocks");
    let second_system = second["system"].as_array().expect("system blocks");

    assert_eq!(
        serde_json::to_vec(&first_system[0]).expect("static block serializes"),
        serde_json::to_vec(&second_system[0]).expect("static block serializes"),
        "动态 suffix 改变时 cached block 必须逐字节稳定"
    );
    assert_eq!(
        first_system[1]["text"],
        "BASE-DYNAMIC-A\n\nREQUEST-MIDDLEWARE"
    );
    assert_eq!(
        second_system[1]["text"],
        "BASE-DYNAMIC-B\n\nREQUEST-MIDDLEWARE"
    );
    assert!(first_system[1].get("cache_control").is_none());
    assert!(second_system[1].get("cache_control").is_none());
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

#[test]
fn config_debug_does_not_expose_credential_or_endpoint_secret() {
    let config = AnthropicConfig::new(
        Url::parse("https://user:password@proxy.example.test/private?api_key=secret#fragment")
            .expect("valid URL"),
        "test-credential",
        "claude-test",
    );
    let rendered = format!("{config:?}");
    for sensitive in [
        "user",
        "password",
        "private",
        "api_key=secret",
        "fragment",
        "test-credential",
    ] {
        assert!(
            !rendered.contains(sensitive),
            "Debug output exposed {sensitive:?}: {rendered}"
        );
    }
    assert!(rendered.contains("[REDACTED]"));
}

#[test]
fn messages_endpoint_preserves_base_path_and_rejects_userinfo() {
    let endpoint = super::request::messages_endpoint(
        &Url::parse("https://proxy.example.test/custom/").expect("valid URL"),
    )
    .expect("messages endpoint");
    assert_eq!(
        endpoint.as_str(),
        "https://proxy.example.test/custom/v1/messages"
    );
    let error = super::request::messages_endpoint(
        &Url::parse("https://user:password@proxy.example.test/custom").expect("valid URL"),
    )
    .expect_err("userinfo must be rejected");
    assert_eq!(
        error.protocol_error().map(|error| error.kind()),
        Some(crate::ProtocolErrorKind::InvalidEndpoint)
    );
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
    assert!(matches!(events.last(), Some(Err(error)) if error.is_stream_interrupted()));
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
