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
    ModelStreamEvent, ProtocolErrorKind, RetryConfig, RetryableErrorClasses, StopReason, ToolCall,
    ToolDefinition, ToolResult,
};

use super::{OpenAiResponsesConfig, OpenAiResponsesModel};

const CIPHERTEXT: &str = "enc-blob-1";

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

fn config(model: &str) -> OpenAiResponsesConfig {
    base_config(model, "https://proxy.example.test/v1/")
}

fn base_config(model: &str, endpoint: &str) -> OpenAiResponsesConfig {
    OpenAiResponsesConfig::new(
        Url::parse(endpoint).expect("valid endpoint"),
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

/// 关闭 Protocol 分类重试，用于断言原始协议错误而非 `RetryExhausted`。
fn config_without_protocol_retry(model: &str) -> OpenAiResponsesConfig {
    OpenAiResponsesConfig::new(
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

fn sse(events: &[(&str, Value)]) -> Vec<u8> {
    let mut body = String::new();
    for (event, data) in events {
        body.push_str(&format!("event: {event}\ndata: {data}\n\n"));
    }
    body.into_bytes()
}

fn reasoning_item() -> Value {
    json!({
        "id": "rs_1",
        "type": "reasoning",
        "status": "completed",
        "summary": [{ "type": "summary_text", "text": "thinking" }],
        "encrypted_content": CIPHERTEXT,
    })
}

fn message_item() -> Value {
    json!({
        "id": "msg_1",
        "type": "message",
        "status": "completed",
        "role": "assistant",
        "content": [{ "type": "output_text", "annotations": [], "text": "done" }],
    })
}

/// 按 input union 的 ResponseOutputMessage 保留 id 与 annotations。
fn projected_message_item() -> Value {
    message_item()
}

/// reasoning / function_call 的投影形状（官方 input union 中 id/status 可选）。
fn projected_reasoning_item() -> Value {
    json!({
        "type": "reasoning",
        "id": "rs_1",
        "summary": [{ "type": "summary_text", "text": "thinking" }],
        "encrypted_content": CIPHERTEXT,
    })
}

fn projected_function_call_item() -> Value {
    json!({
        "type": "function_call",
        "id": "fc_1",
        "call_id": "call_1",
        "name": "Read",
        "arguments": "{\"path\":\"a.rs\"}",
    })
}

fn function_call_item() -> Value {
    json!({
        "id": "fc_1",
        "type": "function_call",
        "status": "completed",
        "call_id": "call_1",
        "name": "Read",
        "arguments": "{\"path\":\"a.rs\"}",
    })
}

fn usage_fixture() -> Value {
    json!({
        "input_tokens": 10,
        "input_tokens_details": { "cached_tokens": 4, "cache_write_tokens": 2 },
        "output_tokens": 7,
        "output_tokens_details": { "reasoning_tokens": 3 },
        "total_tokens": 17,
    })
}

fn completed_event(output: Vec<Value>, usage: Value) -> (&'static str, Value) {
    (
        "response.completed",
        json!({
            "type": "response.completed",
            "sequence_number": 42,
            "response": {
                "id": "resp_1",
                "object": "response",
                "status": "completed",
                "output": output,
                "usage": usage,
            },
        }),
    )
}

fn terminal(status: &str, sequence: u64, output: Vec<Value>) -> (&'static str, Value) {
    let event_type = match status {
        "failed" => "response.failed",
        _ => "response.incomplete",
    };
    (
        event_type,
        json!({
            "type": event_type,
            "sequence_number": sequence,
            "response": {
                "id": "resp_1",
                "status": status,
                "incomplete_details": { "reason": "max_output_tokens" },
                "error": { "code": "server_error", "message": "upstream failed" },
                "output": output,
                "usage": usage_fixture(),
            },
        }),
    )
}

fn tool_round_stream() -> FakeResponse {
    FakeResponse {
        status: 200,
        request_id: Some("req-header-1".into()),
        chunks: vec![
            Ok(sse(&[
                (
                    "response.output_item.added",
                    json!({
                        "type": "response.output_item.added",
                        "output_index": 1,
                        "item": function_call_item(),
                    }),
                ),
                (
                    "response.function_call_arguments.delta",
                    json!({
                        "type": "response.function_call_arguments.delta",
                        "output_index": 1,
                        "item_id": "fc_1",
                        "delta": "{\"path\":",
                    }),
                ),
                (
                    "response.function_call_arguments.delta",
                    json!({
                        "type": "response.function_call_arguments.delta",
                        "output_index": 1,
                        "item_id": "fc_1",
                        "delta": "\"a.rs\"}",
                    }),
                ),
            ])),
            Ok(sse(&[completed_event(
                vec![reasoning_item(), message_item(), function_call_item()],
                usage_fixture(),
            )])),
        ],
    }
}

fn request() -> ModelRequest {
    let schema = JsonObject::from_value(json!({
        "type": "object",
        "properties": { "path": { "type": "string" } },
    }))
    .expect("object");
    ModelRequest::new(vec![
        ModelMessage::system_text("base system"),
        ModelMessage::user_text("read file"),
        ModelMessage::assistant(
            vec![
                ContentBlock::reasoning("previous reasoning"),
                ContentBlock::text("working"),
            ],
            vec![ToolCall::new(
                "call_0",
                "Read",
                JsonObject::from_value(json!({ "path": "a.rs" })).expect("object"),
            )],
        ),
        ModelMessage::tool_result(ToolResult::success("call_0", "Read", "file contents")),
    ])
    .with_tools(vec![
        ToolDefinition::new("Read", schema).with_description("read a file")
    ])
    .with_max_tokens(123)
}

async fn collect(
    model: &OpenAiResponsesModel,
    request: ModelRequest,
) -> Vec<ModelResult<ModelStreamEvent>> {
    model
        .stream(request, CancellationToken::new())
        .await
        .expect("stream")
        .collect::<Vec<_>>()
        .await
}

// 本地 TCP fixture 使用真实 reqwest/SSE/retry 链，不访问外部服务。
async fn local_server(
    responses: Vec<(u16, Vec<u8>)>,
) -> (String, tokio::task::JoinHandle<Vec<Value>>) {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut bodies = Vec::new();
        for (status, response) in responses {
            let (mut socket, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0; 4096];
            let (body_start, length) = loop {
                let count = socket.read(&mut chunk).await.unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&chunk[..count]);
                if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = std::str::from_utf8(&bytes[..end]).unwrap();
                    assert!(headers.starts_with("POST /v1/responses HTTP/1.1\r\n"));
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            let (key, value) = line.split_once(':')?;
                            key.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    break (end + 4, length);
                }
            };
            while bytes.len() < body_start + length {
                let count = socket.read(&mut chunk).await.unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&chunk[..count]);
            }
            bodies.push(serde_json::from_slice(&bytes[body_start..body_start + length]).unwrap());
            let headers = format!("HTTP/1.1 {status} Fixture\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nx-request-id: local-request\r\nConnection: close\r\n\r\n", response.len());
            socket.write_all(headers.as_bytes()).await.unwrap();
            // 故意切开 UTF-8 与 SSE 行，验证真实传输的增量解析。
            for part in response.chunks(7) {
                socket.write_all(part).await.unwrap();
            }
            socket.shutdown().await.unwrap();
        }
        bodies
    });
    (base, server)
}

#[tokio::test]
async fn real_http_tool_round_replays_history_and_error_result() {
    let (base, server) = local_server(vec![
        (
            200,
            sse(&[completed_event(
                vec![reasoning_item(), message_item(), function_call_item()],
                usage_fixture(),
            )]),
        ),
        (
            200,
            sse(&[completed_event(vec![message_item()], usage_fixture())]),
        ),
    ])
    .await;
    let model = OpenAiResponsesModel::new(base_config("gpt-5", &base));
    let events = collect(&model, request()).await;
    let Some(Ok(ModelStreamEvent::Completed(response))) = events.last() else {
        panic!("完成响应缺失");
    };
    assert_eq!(response.request_id(), Some("local-request"));
    let mut result = ToolResult::success("call_1", "Read", "file unavailable");
    result.is_error = true;
    let follow_up = ModelRequest::new(vec![
        ModelMessage::user_text("read file"),
        response.message().clone(),
        ModelMessage::tool_result(result),
    ]);
    let events = collect(&model, follow_up).await;
    assert!(matches!(
        events.last(),
        Some(Ok(ModelStreamEvent::Completed(_)))
    ));
    let bodies = server.await.unwrap();
    assert_eq!(bodies[0]["store"], false);
    assert_eq!(bodies[1]["input"][1]["encrypted_content"], CIPHERTEXT);
    assert_eq!(bodies[1]["input"][2], projected_message_item());
    assert_eq!(bodies[1]["input"][3]["call_id"], "call_1");
    assert_eq!(bodies[1]["input"][4]["call_id"], "call_1");
    assert_eq!(
        bodies[1]["input"][4]["output"],
        "Tool execution failed:\nfile unavailable"
    );
    assert_eq!(bodies[1]["input"].as_array().unwrap().len(), 5);
}

#[tokio::test]
async fn real_http_unsuccessful_terminals_never_produce_executable_tools() {
    let mut cases = vec![
        sse(&[terminal("failed", 1, vec![function_call_item()])]),
        sse(&[("error", json!({"type": "error", "code": "server_error"}))]),
        b"data: [DONE]\n\n".to_vec(),
        Vec::new(),
    ];
    for reason in [
        "max_output_tokens",
        "max_messages",
        "content_filter",
        "steered",
        "unknown",
    ] {
        let (name, mut event) = terminal("incomplete", 1, vec![function_call_item()]);
        event["response"]["incomplete_details"]["reason"] = json!(reason);
        cases.push(sse(&[(name, event)]));
    }
    for body in cases {
        let (base, server) = local_server(vec![(200, body)]).await;
        let model = OpenAiResponsesModel::new(base_config("gpt-5", &base));
        let events = collect(&model, request()).await;
        assert!(events.iter().any(ModelResult::is_err));
        assert!(!events
            .iter()
            .any(|event| matches!(event, Ok(ModelStreamEvent::Completed(_)))));
        assert_eq!(server.await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn real_http_terminal_classification_uses_default_retry_policy() {
    for event_type in ["response.failed", "error"] {
        for code in [
            "invalid_api_key",
            "invalid_request_error",
            "insufficient_quota",
            "unknown",
            "",
            "server_error",
            "rate_limit_exceeded",
        ] {
            let transient = matches!(code, "server_error" | "rate_limit_exceeded");
            let error = json!({"code": code, "message": "private-provider-detail"});
            let event = if event_type == "response.failed" {
                json!({"type": event_type, "response": {"error": error}})
            } else {
                json!({"type": event_type, "code": code, "message": "private-provider-detail"})
            };
            let mut script = vec![(200, sse(&[(event_type, event)]))];
            if transient {
                script.push((
                    200,
                    sse(&[completed_event(vec![message_item()], usage_fixture())]),
                ));
            }
            let (base, server) = local_server(script).await;
            let model = OpenAiResponsesModel::new(OpenAiResponsesConfig::new(
                Url::parse(&base).unwrap(),
                "test-credential",
                "gpt-5",
            ));
            let events = collect(&model, request()).await;
            if transient {
                assert!(matches!(
                    events.last(),
                    Some(Ok(ModelStreamEvent::Completed(_)))
                ));
            } else {
                let error = events.last().unwrap().as_ref().unwrap_err();
                assert_eq!(
                    error.protocol_error().unwrap().kind(),
                    ProtocolErrorKind::Other
                );
                assert!(!format!("{error:?}").contains("private-provider-detail"));
                assert!(!events
                    .iter()
                    .any(|event| matches!(event, Ok(ModelStreamEvent::Completed(_)))));
            }
            assert_eq!(server.await.unwrap().len(), if transient { 2 } else { 1 });
        }
    }
}

#[tokio::test]
async fn real_http_incomplete_reasons_are_safe_and_never_retried() {
    for (reason, summary) in [
        ("max_output_tokens", "incomplete.output_limit"),
        ("content_filter", "incomplete.content_filter"),
        ("private-provider-detail", "incomplete"),
    ] {
        let (name, mut event) = terminal("incomplete", 1, vec![function_call_item()]);
        event["response"]["incomplete_details"]["reason"] = json!(reason);
        let (base, server) = local_server(vec![(200, sse(&[(name, event)]))]).await;
        let model = OpenAiResponsesModel::new(OpenAiResponsesConfig::new(
            Url::parse(&base).unwrap(),
            "test-credential",
            "gpt-5",
        ));
        let events = collect(&model, request()).await;
        let error = events.last().unwrap().as_ref().unwrap_err();
        assert_eq!(error.protocol_error().unwrap().summary(), Some(summary));
        assert!(!events
            .iter()
            .any(|event| matches!(event, Ok(ModelStreamEvent::Completed(_)))));
        assert_eq!(server.await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn real_http_failed_terminal_usage_survives_retry() {
    // 默认 retry 策略：暂态 failed 必须真实重试，且失败尝试已发生的用量不得丢失。
    let (name, mut event) = terminal("failed", 1, Vec::new());
    event["response"]["usage"] = json!({"input_tokens": 30, "output_tokens": 2});
    let (base, server) = local_server(vec![
        (200, sse(&[(name, event)])),
        (
            200,
            sse(&[completed_event(vec![message_item()], usage_fixture())]),
        ),
    ])
    .await;
    let model = OpenAiResponsesModel::new(OpenAiResponsesConfig::new(
        Url::parse(&base).unwrap(),
        "test-credential",
        "gpt-5",
    ));
    let events = collect(&model, request()).await;
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                Ok(ModelStreamEvent::AttemptUsage(usage))
                    if *usage == crate::TokenUsage {
                        input_tokens: 30,
                        output_tokens: 2,
                        cache_creation_input_tokens: None,
                        cache_read_input_tokens: None,
                    }
            ))
            .count(),
        1,
        "失败尝试的用量必须恰好保留一次: {events:?}"
    );
    assert!(matches!(
        events.last(),
        Some(Ok(ModelStreamEvent::Completed(_)))
    ));
    assert_eq!(server.await.unwrap().len(), 2);
}

#[tokio::test]
async fn real_http_incomplete_terminal_reports_usage_without_retry() {
    let (name, mut event) = terminal("incomplete", 1, Vec::new());
    event["response"]["usage"] = json!({"input_tokens": 17, "output_tokens": 3});
    let (base, server) = local_server(vec![(200, sse(&[(name, event)]))]).await;
    let model = OpenAiResponsesModel::new(OpenAiResponsesConfig::new(
        Url::parse(&base).unwrap(),
        "test-credential",
        "gpt-5",
    ));
    let events = collect(&model, request()).await;
    assert!(!events
        .iter()
        .any(|event| matches!(event, Ok(ModelStreamEvent::Completed(_)))));
    let error = events.last().unwrap().as_ref().unwrap_err();
    assert_eq!(
        error.usage(),
        Some(&crate::TokenUsage {
            input_tokens: 17,
            output_tokens: 3,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        }),
        "非成功终态必须如实带回已发生的用量"
    );
    assert_eq!(server.await.unwrap().len(), 1, "incomplete 不得重试");
}

#[tokio::test]
async fn real_http_retry_reports_usage_only_for_successful_attempt() {
    let (base, server) = local_server(vec![
        (503, Vec::new()),
        (
            200,
            sse(&[completed_event(vec![message_item()], usage_fixture())]),
        ),
    ])
    .await;
    let model = OpenAiResponsesModel::new(
        base_config("gpt-5", &base).with_runtime(
            ModelRuntimeConfig::default().with_retry(
                RetryConfig::default()
                    .with_max_attempts(2)
                    .with_base_delay(Duration::ZERO)
                    .with_jitter(false),
            ),
        ),
    );
    let events = collect(&model, request()).await;
    assert!(events.iter().all(ModelResult::is_ok));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Ok(ModelStreamEvent::Usage(_))))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Ok(ModelStreamEvent::Completed(_))))
            .count(),
        1
    );
    let bodies = server.await.unwrap();
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies[0], bodies[1]);
}

#[tokio::test]
async fn completed_is_terminal_even_with_trailing_events_in_the_same_chunk() {
    for trailing in [
        ("error", json!({"type": "error", "code": "server_error"})),
        (
            "response.output_text.delta",
            json!({"type": "response.output_text.delta", "delta": "late"}),
        ),
        completed_event(vec![message_item()], usage_fixture()),
    ] {
        let transport = Arc::new(FakeTransport::with_response(FakeResponse {
            status: 200,
            request_id: None,
            chunks: vec![Ok(sse(&[
                completed_event(vec![message_item()], usage_fixture()),
                trailing,
            ]))],
        }));
        let model = OpenAiResponsesModel::with_transport(config("gpt-5"), transport);
        let events = collect(&model, request()).await;
        assert!(events.iter().all(ModelResult::is_ok));
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events.last(),
            Some(Ok(ModelStreamEvent::Completed(_)))
        ));
    }
}

// ── 请求契约 ────────────────────────────────────────────────────────────────

#[test]
fn request_contract_is_stateless_and_replays_generic_history() {
    let body = super::request::body_for_test(&config("gpt-5.6-sol"), &request());

    assert_eq!(body["model"], "gpt-5.6-sol");
    assert_eq!(body["stream"], true);
    // 显式无状态：默认不落服务端存储，并显式请求 reasoning 密文以支持续轮。
    assert_eq!(body["store"], false);
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(body["max_output_tokens"], 123);
    assert_eq!(body["instructions"], "base system");
    assert_eq!(body["tool_choice"], "auto");
    assert_eq!(body["tools"][0]["name"], "Read");
    assert_eq!(body["tools"][0]["parameters"]["type"], "object");
    // 省略 strict 会被官方尝试规范化；显式 false 保持项目 schema 语义。
    assert_eq!(body["tools"][0]["strict"], false);

    let input = body["input"].as_array().expect("input array");
    assert_eq!(input[0], json!({ "role": "user", "content": "read file" }));
    // 无原生记录的 assistant 消息：推理摘要与工具调用都回放到 input。
    assert_eq!(input[1]["type"], "reasoning");
    assert_eq!(input[1]["summary"][0]["text"], "previous reasoning");
    assert_eq!(
        input[2],
        json!({ "role": "assistant", "content": [{ "type": "input_text", "text": "working" }] })
    );
    assert_eq!(input[3]["type"], "function_call");
    assert_eq!(input[3]["call_id"], "call_0");
    assert_eq!(input[3]["name"], "Read");
    assert_eq!(input[3]["arguments"], "{\"path\":\"a.rs\"}");
    assert_eq!(
        input[4],
        json!({
            "type": "function_call_output",
            "call_id": "call_0",
            "output": "file contents",
        })
    );
    assert_eq!(input.len(), 5);
}

#[test]
fn request_endpoint_is_responses_under_the_configured_base() {
    for (base, expected) in [
        (
            "https://proxy.example.test/v1/",
            "https://proxy.example.test/v1/responses",
        ),
        (
            "https://proxy.example.test/v1",
            "https://proxy.example.test/v1/responses",
        ),
        (
            "https://proxy.example.test/proxy/openai/v1?key=1#frag",
            "https://proxy.example.test/proxy/openai/v1/responses",
        ),
    ] {
        let built = super::request::build_request(&base_config("gpt-5", base), &request())
            .expect("valid endpoint");
        assert_eq!(built.endpoint.as_str(), expected);
    }
}

#[test]
fn request_endpoint_rejects_userinfo_and_non_http_schemes() {
    for base in [
        "https://user:password@proxy.example.test/v1/",
        "ftp://proxy.example.test/v1/",
    ] {
        let error = build_error(&base_config("gpt-5", base), &request());
        assert_eq!(
            error.protocol_error().map(|error| error.kind()),
            Some(ProtocolErrorKind::InvalidEndpoint)
        );
    }
}

#[test]
fn unsupported_input_semantics_fail_explicitly() {
    // provider 私有推理 block 没有可见内容：跨协议丢弃不是信息损失，构建成功。
    let provider_private = ModelRequest::new(vec![ModelMessage::assistant(
        vec![ContentBlock::RedactedReasoning { data: None }],
        vec![tool_call("call_1")],
    )]);
    assert!(super::request::build_request(&config("gpt-5"), &provider_private).is_ok());

    let unsupported = [
        // user content 不能承载模型侧推理。
        ModelRequest::new(vec![ModelMessage::User {
            content: vec![ContentBlock::Reasoning {
                text: "hidden".into(),
                signature: None,
            }],
        }]),
        // assistant content 不能承载图片。
        ModelRequest::new(vec![ModelMessage::assistant(
            vec![ContentBlock::Image {
                source: crate::ImageSource::Url {
                    url: "https://example.test/a.png".into(),
                },
            }],
            Vec::new(),
        )]),
        // 未验证的二进制 MIME 仍然显式拒绝。
        ModelRequest::new(vec![ModelMessage::User {
            content: vec![ContentBlock::Document {
                source: crate::DocumentSource::Base64 {
                    media_type: crate::MediaType::new("application/octet-stream"),
                    data: "AA==".into(),
                },
                title: None,
            }],
        }]),
    ];
    for request in unsupported {
        assert_eq!(
            build_error(&config("gpt-5"), &request)
                .protocol_error()
                .map(|error| error.kind()),
            Some(ProtocolErrorKind::Other)
        );
    }
}

#[test]
fn document_inputs_preserve_text_url_and_pdf_data() {
    let request = ModelRequest::new(vec![ModelMessage::User {
        content: vec![
            ContentBlock::Document {
                source: crate::DocumentSource::Text {
                    text: "document text".into(),
                },
                title: None,
            },
            ContentBlock::Document {
                source: crate::DocumentSource::Url {
                    url: "https://example.test/file.pdf".into(),
                },
                title: None,
            },
            ContentBlock::Document {
                source: crate::DocumentSource::Base64 {
                    media_type: crate::MediaType::new("application/pdf"),
                    data: "JVBERi0=".into(),
                },
                title: Some("sample.pdf".into()),
            },
        ],
    }]);
    let body = super::request::body_for_test(&config("gpt-5"), &request);
    assert_eq!(
        body["input"][0]["content"],
        json!([
            {"type": "input_text", "text": "document text"},
            {"type": "input_file", "file_url": "https://example.test/file.pdf"},
            {"type": "input_file", "filename": "sample.pdf", "file_data": "data:application/pdf;base64,JVBERi0="},
        ])
    );
}

fn tool_call(call_id: &str) -> ToolCall {
    ToolCall::new(
        call_id,
        "Read",
        JsonObject::from_value(json!({ "path": "a.rs" })).expect("object"),
    )
}

fn build_error(config: &OpenAiResponsesConfig, request: &ModelRequest) -> crate::ModelError {
    match super::request::build_request(config, request) {
        Err(error) => error,
        Ok(_) => panic!("request must be rejected"),
    }
}

#[test]
fn image_content_is_projected_as_input_image() {
    let request = ModelRequest::new(vec![ModelMessage::User {
        content: vec![
            ContentBlock::text("look"),
            ContentBlock::Image {
                source: crate::ImageSource::Base64 {
                    media_type: crate::MediaType::new("image/png"),
                    data: "AAAA".into(),
                },
            },
        ],
    }]);
    let body = super::request::body_for_test(&config("gpt-5"), &request);
    assert_eq!(
        body["input"][0]["content"],
        json!([
            { "type": "input_text", "text": "look" },
            { "type": "input_image", "image_url": "data:image/png;base64,AAAA" },
        ])
    );
}

#[test]
fn config_debug_does_not_expose_credential() {
    let rendered = format!("{:?}", config("gpt-5"));
    assert!(!rendered.contains("test-credential"));
    assert!(rendered.contains("[REDACTED]"));
}

// ── 工具回合与原生保真 ──────────────────────────────────────────────────────

#[tokio::test]
async fn completed_tool_round_produces_native_history_and_executable_tool_call() {
    let transport = Arc::new(FakeTransport::with_response(tool_round_stream()));
    let model = OpenAiResponsesModel::with_transport(config("gpt-5"), transport.clone());

    let events = collect(&model, request()).await;
    assert!(events.iter().all(ModelResult::is_ok), "{events:?}");

    assert_eq!(
        events[0],
        Ok(ModelStreamEvent::ToolCallDelta {
            index: 1,
            id: Some("call_1".into()),
            name: Some("Read".into()),
            arguments_delta: "{\"path\":".into(),
        })
    );
    assert_eq!(
        events[1],
        Ok(ModelStreamEvent::ToolCallDelta {
            index: 1,
            id: Some("call_1".into()),
            name: Some("Read".into()),
            arguments_delta: "\"a.rs\"}".into(),
        })
    );
    let Some(Ok(ModelStreamEvent::Usage(usage))) = events.get(2) else {
        panic!("usage event expected: {events:?}");
    };
    assert_eq!(usage.input_tokens, 10);
    assert_eq!(usage.output_tokens, 7);
    assert_eq!(usage.cache_read_input_tokens, Some(4));
    assert_eq!(usage.cache_creation_input_tokens, Some(2));

    let Some(Ok(ModelStreamEvent::Completed(response))) = events.get(3) else {
        panic!("completed event expected: {events:?}");
    };
    assert_eq!(response.stop_reason(), &StopReason::ToolUse);
    // request-id 保真：HTTP 头优先。
    assert_eq!(response.request_id(), Some("req-header-1"));
    let usage = response.usage().expect("usage");
    assert_eq!(usage.output_tokens, 7);

    let tool_calls = match response.message() {
        ModelMessage::Assistant { tool_calls, .. } => tool_calls.clone(),
        other => panic!("assistant expected: {other:?}"),
    };
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].id(), "call_1");
    assert_eq!(tool_calls[0].name(), "Read");
    assert_eq!(
        tool_calls[0].arguments().as_map().get("path"),
        Some(&json!("a.rs"))
    );

    let content = match response.message() {
        ModelMessage::Assistant { content, .. } => content.clone(),
        other => panic!("assistant expected: {other:?}"),
    };
    assert_eq!(content[0], ContentBlock::reasoning("thinking"));
    assert_eq!(content[1], ContentBlock::text("done"));
    let native = content.last().expect("native history block");
    assert!(matches!(
        native,
        ContentBlock::ResponsesNativeHistory { .. }
    ));
    // 原生记录不重复暴露派生文本。
    assert_eq!(native.text_content(), None);
}

#[tokio::test]
async fn same_source_replay_replays_native_items_once() {
    let transport = Arc::new(FakeTransport::with_response(tool_round_stream()));
    let model = OpenAiResponsesModel::with_transport(config("gpt-5"), transport.clone());
    let events = collect(&model, request()).await;
    let Some(Ok(ModelStreamEvent::Completed(response))) = events.last() else {
        panic!("completed event expected: {events:?}");
    };

    let follow_up = ModelRequest::new(vec![
        ModelMessage::user_text("read file"),
        response.message().clone(),
        ModelMessage::tool_result(ToolResult::success("call_1", "Read", "file contents")),
    ]);
    let body = super::request::body_for_test(&config("gpt-5"), &follow_up);
    let input = body["input"].as_array().expect("input array");

    assert_eq!(input[0], json!({ "role": "user", "content": "read file" }));
    // 原生 item 逐项回放：reasoning（含密文）→ message → function_call。
    assert_eq!(input[1], projected_reasoning_item());
    assert_eq!(input[2], projected_message_item());
    assert_eq!(input[3], projected_function_call_item());
    assert_eq!(
        input[4],
        json!({
            "type": "function_call_output",
            "call_id": "call_1",
            "output": "file contents",
        })
    );
    assert_eq!(input.len(), 5);

    let serialized = serde_json::to_string(&body).expect("body serializes");
    assert_eq!(serialized.matches(CIPHERTEXT).count(), 1);
    // 派生文本与派生工具调用不重复回放。
    assert_eq!(serialized.matches("\"done\"").count(), 1);
    assert_eq!(serialized.matches("\"call_1\"").count(), 2);
}

#[tokio::test]
async fn other_source_replay_degrades_without_ciphertext_or_reasoning_items() {
    let transport = Arc::new(FakeTransport::with_response(tool_round_stream()));
    let model = OpenAiResponsesModel::with_transport(config("gpt-5"), transport.clone());
    let events = collect(&model, request()).await;
    let Some(Ok(ModelStreamEvent::Completed(response))) = events.last() else {
        panic!("completed event expected: {events:?}");
    };
    let messages = vec![
        ModelMessage::user_text("read file"),
        response.message().clone(),
        ModelMessage::tool_result(ToolResult::success("call_1", "Read", "file contents")),
    ];

    for (model_name, endpoint) in [
        ("gpt-5-mini", "https://proxy.example.test/v1/"),
        ("gpt-5", "https://other.example.test/v1/"),
    ] {
        let body = super::request::body_for_test(
            &base_config(model_name, endpoint),
            &ModelRequest::new(messages.clone()),
        );
        let input = body["input"].as_array().expect("input array");
        // 通用 message/function_call 仍然回放，保证对话可见内容不丢。
        assert_eq!(input[0], json!({ "role": "user", "content": "read file" }));
        assert_eq!(input[1], projected_message_item());
        assert_eq!(input[2], projected_function_call_item());
        assert_eq!(input.len(), 4);
        let serialized = serde_json::to_string(&body).expect("body serializes");
        assert!(
            !serialized.contains(CIPHERTEXT),
            "ciphertext must never cross source identity: {serialized}"
        );
        assert!(!serialized.contains("\"reasoning\""));
    }
}

#[tokio::test]
async fn derived_views_must_match_the_native_record() {
    let transport = Arc::new(FakeTransport::with_response(tool_round_stream()));
    let model = OpenAiResponsesModel::with_transport(config("gpt-5"), transport.clone());
    let events = collect(&model, request()).await;
    let Some(Ok(ModelStreamEvent::Completed(response))) = events.last() else {
        panic!("completed event expected: {events:?}");
    };
    let ModelMessage::Assistant {
        content,
        tool_calls,
    } = response.message().clone()
    else {
        panic!("assistant expected");
    };

    // 派生文本被外部改写：静默以记录为准会丢掉改写内容，必须显式失败。
    let mut injected = content.clone();
    injected.insert(0, ContentBlock::text("injected"));
    assert_eq!(
        build_error(
            &config("gpt-5"),
            &ModelRequest::new(vec![ModelMessage::assistant(injected, tool_calls.clone())])
        )
        .protocol_error()
        .map(|error| error.kind()),
        Some(ProtocolErrorKind::Other)
    );

    // 派生工具调用与记录不一致（记录里有 function_call，消息里没有）：同样失败。
    assert_eq!(
        build_error(
            &config("gpt-5"),
            &ModelRequest::new(vec![ModelMessage::assistant(content.clone(), Vec::new())])
        )
        .protocol_error()
        .map(|error| error.kind()),
        Some(ProtocolErrorKind::Other)
    );

    // 未被改写的派生视图可以回放。
    assert!(super::request::build_request(
        &config("gpt-5"),
        &ModelRequest::new(vec![ModelMessage::assistant(content, tool_calls)])
    )
    .is_ok());
}

#[tokio::test]
async fn prepared_body_and_sent_body_share_one_request_builder() {
    let transport = Arc::new(FakeTransport::with_response(FakeResponse {
        status: 200,
        request_id: None,
        chunks: vec![Ok(sse(&[completed_event(
            vec![message_item()],
            usage_fixture(),
        )]))],
    }));
    let model = OpenAiResponsesModel::with_transport(config("gpt-5"), transport.clone());
    let request = request();
    let prepared = model.prepare_request(&request).expect("prepared request");
    let _ = collect(&model, request).await;

    let mut sent = transport.bodies();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0]["input"][3]["arguments"],
        json!("{\"path\":\"a.rs\"}")
    );
    // 观测副本隐藏编码参数，真实 wire 保持原文，其余字段来自同一个 builder。
    sent[0]["input"][3]["arguments"] = json!("[REDACTED]");
    assert_eq!(sent, vec![prepared.body().as_value().clone()]);
    assert_eq!(
        prepared.protocol(),
        &crate::ProviderProtocol::OpenAiResponses
    );
}

// ── 终态门禁 ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn incomplete_terminal_is_rejected_without_executing_tools_and_without_retry() {
    let transport = Arc::new(FakeTransport::with_response(FakeResponse {
        status: 200,
        request_id: None,
        chunks: vec![
            Ok(sse(&[(
                "response.output_item.added",
                json!({
                    "type": "response.output_item.added",
                    "output_index": 1,
                    "item": function_call_item(),
                }),
            )])),
            Ok(sse(&[terminal(
                "incomplete",
                43,
                vec![function_call_item()],
            )])),
        ],
    }));
    // 默认 retry 配置：incomplete 属稳定失败，不允许重试。
    let model = OpenAiResponsesModel::with_transport(
        OpenAiResponsesConfig::new(
            Url::parse("https://proxy.example.test/v1/").expect("valid endpoint"),
            "test-credential",
            "gpt-5",
        ),
        transport.clone(),
    );

    let events = collect(&model, request()).await;
    assert!(matches!(
        events.as_slice(),
        [Err(error)]
            if error.protocol_error().map(|error| error.kind()) == Some(ProtocolErrorKind::Other)
    ));
    assert_eq!(transport.bodies().len(), 1);
}

#[tokio::test]
async fn failed_terminal_is_rejected_without_completed() {
    let transport = Arc::new(FakeTransport::with_response(FakeResponse {
        status: 200,
        request_id: None,
        chunks: vec![Ok(sse(&[terminal("failed", 44, Vec::new())]))],
    }));
    let model = OpenAiResponsesModel::with_transport(config("gpt-5"), transport.clone());

    let events = collect(&model, request()).await;
    assert!(matches!(
        events.as_slice(),
        [Err(error)]
            if error.protocol_error().map(|error| error.kind()) == Some(ProtocolErrorKind::Provider)
    ));
}

#[tokio::test]
async fn done_without_terminal_event_is_interrupted() {
    let transport = Arc::new(FakeTransport::with_response(FakeResponse {
        status: 200,
        request_id: None,
        chunks: vec![
            Ok(b"event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n".to_vec()),
            Ok(b"data: [DONE]\n\n".to_vec()),
        ],
    }));
    let model =
        OpenAiResponsesModel::with_transport(config_without_protocol_retry("gpt-5"), transport);

    let events = collect(&model, request()).await;
    assert!(matches!(
        events.as_slice(),
        [Ok(ModelStreamEvent::TextDelta { text }), Err(error)]
            if text == "partial"
                && error.protocol_error().map(|error| error.kind())
                    == Some(ProtocolErrorKind::StreamEndedWithoutCompleted)
    ));
}

#[tokio::test]
async fn eof_without_terminal_event_is_interrupted_without_completed() {
    let transport = Arc::new(FakeTransport::with_response(FakeResponse {
        status: 200,
        request_id: None,
        chunks: vec![Ok(
            b"event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n".to_vec(),
        )],
    }));
    let model = OpenAiResponsesModel::with_transport(config("gpt-5"), transport.clone());

    let events = collect(&model, request()).await;
    assert!(matches!(
        events.as_slice(),
        [Ok(ModelStreamEvent::TextDelta { text }), Err(error)]
            if text == "partial" && error.provider() == Some("openai-responses")
    ));
    assert!(events
        .iter()
        .all(|event| !matches!(event, Ok(ModelStreamEvent::Completed(_)))));
    assert_eq!(transport.bodies().len(), 1);
}

#[tokio::test]
async fn unknown_output_item_is_rejected_without_completed() {
    let transport = Arc::new(FakeTransport::with_response(FakeResponse {
        status: 200,
        request_id: None,
        chunks: vec![Ok(sse(&[completed_event(
            vec![json!({ "id": "ws_1", "type": "web_search_call", "status": "completed" })],
            usage_fixture(),
        )]))],
    }));
    let model =
        OpenAiResponsesModel::with_transport(config_without_protocol_retry("gpt-5"), transport);

    let events = collect(&model, request()).await;
    assert!(matches!(
        events.as_slice(),
        [Err(error)]
            if error.protocol_error().map(|error| error.kind()) == Some(ProtocolErrorKind::Other)
    ));
}

#[tokio::test]
async fn unknown_stream_event_is_rejected() {
    let transport = Arc::new(FakeTransport::with_response(FakeResponse {
        status: 200,
        request_id: None,
        chunks: vec![Ok(sse(&[(
            "response.web_search_call.in_progress",
            json!({
                "type": "response.web_search_call.in_progress",
                "output_index": 0,
                "item_id": "ws_1",
            }),
        )]))],
    }));
    let model =
        OpenAiResponsesModel::with_transport(config_without_protocol_retry("gpt-5"), transport);

    let events = collect(&model, request()).await;
    assert!(matches!(
        events.as_slice(),
        [Err(error)]
            if error.protocol_error().map(|error| error.kind()) == Some(ProtocolErrorKind::Other)
    ));
}

#[tokio::test]
async fn incomplete_status_inside_completed_event_is_rejected() {
    let mut response = json!({
        "id": "resp_1",
        "status": "incomplete",
        "output": [message_item()],
        "usage": usage_fixture(),
    });
    let transport = Arc::new(FakeTransport::with_response(FakeResponse {
        status: 200,
        request_id: None,
        chunks: vec![Ok(sse(&[(
            "response.completed",
            json!({
                "type": "response.completed",
                "sequence_number": 45,
                "response": response.take(),
            }),
        )]))],
    }));
    let model =
        OpenAiResponsesModel::with_transport(config_without_protocol_retry("gpt-5"), transport);

    let events = collect(&model, request()).await;
    assert!(matches!(
        events.as_slice(),
        [Err(error)]
            if error.protocol_error().map(|error| error.kind()) == Some(ProtocolErrorKind::Other)
    ));
}

// ── usage / request-id / 取消 ───────────────────────────────────────────────

#[tokio::test]
async fn usage_missing_is_not_reported_as_zero() {
    let transport = Arc::new(FakeTransport::with_response(FakeResponse {
        status: 200,
        request_id: None,
        chunks: vec![Ok(sse(&[(
            "response.completed",
            json!({
                "type": "response.completed",
                "sequence_number": 46,
                "response": {
                    "id": "resp_1",
                    "status": "completed",
                    "output": [message_item()],
                },
            }),
        )]))],
    }));
    let model = OpenAiResponsesModel::with_transport(config("gpt-5"), transport);

    let events = collect(&model, request()).await;
    assert_eq!(events.len(), 1);
    let Some(Ok(ModelStreamEvent::Completed(response))) = events.first() else {
        panic!("completed event expected: {events:?}");
    };
    assert_eq!(response.usage(), None);
    assert_eq!(response.stop_reason(), &StopReason::EndTurn);
    assert_eq!(response.request_id(), Some("resp_1"));
}

#[tokio::test]
async fn usage_without_cache_details_keeps_none_for_cache_fields() {
    let transport = Arc::new(FakeTransport::with_response(FakeResponse {
        status: 200,
        request_id: None,
        chunks: vec![Ok(sse(&[completed_event(
            vec![message_item()],
            json!({ "input_tokens": 5, "output_tokens": 0 }),
        )]))],
    }));
    let model = OpenAiResponsesModel::with_transport(config("gpt-5"), transport);

    let events = collect(&model, request()).await;
    let Some(Ok(ModelStreamEvent::Completed(response))) = events.last() else {
        panic!("completed event expected: {events:?}");
    };
    let usage = response.usage().expect("usage");
    assert_eq!(usage.input_tokens, 5);
    // 显式零与缺失不同：output_tokens 是零，缓存字段是缺失。
    assert_eq!(usage.output_tokens, 0);
    assert_eq!(usage.cache_read_input_tokens, None);
    assert_eq!(usage.cache_creation_input_tokens, None);
}

#[tokio::test]
async fn refusal_is_visible_text_and_kept_in_native_history() {
    let refusal_item = json!({
        "id": "msg_r",
        "type": "message",
        "status": "completed",
        "role": "assistant",
        "content": [{ "type": "refusal", "refusal": "cannot help" }],
    });
    let transport = Arc::new(FakeTransport::with_response(FakeResponse {
        status: 200,
        request_id: None,
        chunks: vec![Ok(sse(&[completed_event(
            vec![refusal_item.clone()],
            usage_fixture(),
        )]))],
    }));
    let model = OpenAiResponsesModel::with_transport(config("gpt-5"), transport);

    let events = collect(&model, request()).await;
    let Some(Ok(ModelStreamEvent::Completed(response))) = events.last() else {
        panic!("completed event expected: {events:?}");
    };
    assert_eq!(
        response.message().text_content().as_deref(),
        Some("cannot help")
    );

    let follow_up = ModelRequest::new(vec![response.message().clone()]);
    let body = super::request::body_for_test(&config("gpt-5"), &follow_up);
    assert_eq!(
        body["input"][0],
        json!({
            "id": "msg_r",
            "type": "message",
            "status": "completed",
            "role": "assistant",
            "content": [{ "type": "refusal", "refusal": "cannot help" }],
        })
    );
}

#[tokio::test]
async fn cancellation_rejects_mid_stream_without_completed() {
    let transport = Arc::new(FakeTransport::with_response(FakeResponse {
        status: 200,
        request_id: None,
        chunks: vec![Ok(sse(&[
            (
                "response.output_text.delta",
                json!({ "type": "response.output_text.delta", "delta": "partial" }),
            ),
            completed_event(vec![message_item()], usage_fixture()),
        ]))],
    }));
    let model = OpenAiResponsesModel::with_transport(config("gpt-5"), transport);
    let cancellation = CancellationToken::new();
    let mut stream = model
        .stream(request(), cancellation.clone())
        .await
        .expect("stream");

    let first = stream.next().await.expect("first event");
    assert!(matches!(first, Ok(ModelStreamEvent::TextDelta { .. })));
    cancellation.cancel();

    let next = stream.next().await.expect("cancellation event");
    let error = next.expect_err("cancellation must be reported as error");
    assert!(error.is_cancelled());
}

#[tokio::test]
async fn pre_cancelled_request_never_reaches_transport() {
    let transport = Arc::new(FakeTransport::with_response(FakeResponse {
        status: 200,
        request_id: None,
        chunks: vec![],
    }));
    let model = OpenAiResponsesModel::with_transport(config("gpt-5"), transport.clone());
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let error = match model.stream(request(), cancellation).await {
        Err(error) => error,
        Ok(_) => panic!("pre-cancelled request must not start a stream"),
    };
    assert!(error.is_cancelled());
    assert!(transport.bodies().is_empty());
}

// ── 旧 adapter 兼容回归 ─────────────────────────────────────────────────────

#[tokio::test]
async fn legacy_adapters_drop_native_history_without_leaking_ciphertext() {
    let transport = Arc::new(FakeTransport::with_response(tool_round_stream()));
    let model = OpenAiResponsesModel::with_transport(config("gpt-5"), transport.clone());
    let events = collect(&model, request()).await;
    let Some(Ok(ModelStreamEvent::Completed(response))) = events.last() else {
        panic!("completed event expected: {events:?}");
    };
    let messages = vec![
        ModelMessage::user_text("read file"),
        response.message().clone(),
        ModelMessage::tool_result(ToolResult::success("call_1", "Read", "file contents")),
    ];
    let request = ModelRequest::new(messages).with_tools(vec![ToolDefinition::new(
        "Read",
        JsonObject::from_value(json!({ "type": "object" })).expect("object"),
    )]);

    let chat = crate::OpenAiModel::new(crate::OpenAiConfig::new(
        Url::parse("https://proxy.example.test/v1/").expect("valid endpoint"),
        "test-credential",
        "gpt-4o",
    ))
    .prepare_request(&request)
    .expect("chat prepared request");
    let anthropic = crate::AnthropicModel::new(crate::AnthropicConfig::new(
        Url::parse("https://proxy.example.test/v1/").expect("valid endpoint"),
        "test-credential",
        "claude-sonnet-4-5",
    ))
    .prepare_request(&request)
    .expect("anthropic prepared request");

    let blocks = anthropic.body().as_value()["messages"][1]["content"]
        .as_array()
        .unwrap();
    assert!(blocks.iter().all(|block| block["type"] != "thinking"));
    assert!(blocks
        .iter()
        .any(|block| block["type"] == "text" && block["text"] == "thinking"));
    for prepared in [&chat, &anthropic] {
        assert!(prepared.responses_history_degraded());
        assert_eq!(
            prepared.metadata(),
            &std::collections::BTreeMap::from(
                [("responses_history_degraded".into(), json!(true)),]
            )
        );
        let serialized =
            serde_json::to_string(prepared.body().as_value()).expect("body serializes");
        assert!(
            !serialized.contains(CIPHERTEXT),
            "ciphertext must never cross provider protocols"
        );
        assert!(!serialized.contains("responses_native_history"));
        // 派生视图仍在，跨协议降级不丢可见内容。
        assert!(serialized.contains("done"));
        assert!(serialized.contains("thinking"));
        assert!(serialized.contains("read file"));
    }
}
