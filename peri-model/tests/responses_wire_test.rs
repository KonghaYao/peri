//! Independent public-API checks using a real loopback HTTP server.

use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use futures::StreamExt;
use peri_model::{
    ContentBlock, JsonObject, Model, ModelError, ModelMessage, ModelRequest, ModelResponse,
    ModelResult, ModelRuntimeConfig, ModelStreamEvent, ProtocolErrorKind, RequestAuth,
    RequestCredential, ResponsesConfig, ResponsesModel, ResponsesProfile, RetryConfig,
    ToolDefinition, ToolResult,
};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Mutex,
    time::timeout,
};
use tokio_util::sync::CancellationToken;

fn event(kind: &str, mut value: Value) -> String {
    value["type"] = json!(kind);
    format!("event: {kind}\ndata: {value}\n\n")
}

fn message(id: &str, text: &str) -> Value {
    json!({"id": id, "type": "message", "role": "assistant", "status": "completed",
        "content": [{"type": "output_text", "text": text, "annotations": []}]})
}

fn call(id: &str, call_id: &str, name: &str, arguments: &str) -> Value {
    json!({"id": id, "type": "function_call", "call_id": call_id, "name": name,
        "arguments": arguments, "status": "completed"})
}

fn completed(output: Vec<Value>) -> String {
    event(
        "response.completed",
        json!({"response": {"id": "resp_fixture", "status": "completed", "output": output,
        "usage": {"input_tokens": 20, "output_tokens": 7, "input_tokens_details": {"cached_tokens": 12}}}}),
    )
}

fn tool(name: &str) -> ToolDefinition {
    ToolDefinition::new(
        name,
        JsonObject::from_value(
            json!({"type": "object", "properties": {"value": {"type": "string"}}}),
        )
        .unwrap(),
    )
}

fn config(endpoint: &str, attempts: u32) -> ResponsesConfig {
    ResponsesConfig::new(endpoint.parse().unwrap(), "fixture-secret", "fixture-model")
        .with_credential_ref("fixture-account")
        .with_runtime(
            ModelRuntimeConfig::default().with_retry(
                RetryConfig::default()
                    .with_max_attempts(attempts)
                    .with_base_delay(Duration::ZERO)
                    .with_jitter(false),
            ),
        )
}

#[derive(Debug)]
struct Captured {
    headers: String,
    body: Value,
}

async fn read_request(socket: &mut TcpStream) -> Captured {
    let mut bytes = Vec::new();
    loop {
        let mut chunk = [0_u8; 4096];
        let read = socket.read(&mut chunk).await.unwrap();
        assert_ne!(read, 0);
        bytes.extend_from_slice(&chunk[..read]);
        assert!(bytes.len() < 1_000_000);
        let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") else {
            continue;
        };
        let headers = std::str::from_utf8(&bytes[..end]).unwrap();
        let length: usize = headers
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .unwrap()
            .1
            .trim()
            .parse()
            .unwrap();
        if bytes.len() >= end + 4 + length {
            return Captured {
                headers: headers.to_owned(),
                body: serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap(),
            };
        }
    }
}

async fn send_sse(socket: &mut TcpStream, body: &str) {
    socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").await.unwrap();
    // Deliberately split JSON and multibyte UTF-8 across HTTP body chunks.
    for chunk in body.as_bytes().chunks(7) {
        socket
            .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
            .await
            .unwrap();
        socket.write_all(chunk).await.unwrap();
        socket.write_all(b"\r\n").await.unwrap();
    }
    socket.write_all(b"0\r\n\r\n").await.unwrap();
    socket.shutdown().await.unwrap();
}

#[tokio::test]
async fn interleaved_calls_roundtrip_and_replay_once_over_chunked_http() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model = ResponsesModel::new(config(
        &format!("http://{}/v1", listener.local_addr().unwrap()),
        1,
    ));
    let reason = json!({"id": "rs_one", "type": "reasoning", "summary": [], "encrypted_content": "fixture-opaque-ciphertext"});
    let first = call("fc_item_a", "call_a", "Read", r#"{"value":"甲"}"#);
    let second = call("fc_item_b", "call_b", "Search", r#"{"value":"乙"}"#);
    let output = vec![
        reason.clone(),
        first.clone(),
        second.clone(),
        message("msg_one", "正在查询"),
    ];
    let mut script = String::from(": fixture heartbeat\r\n\r\n");
    script += &event(
        "response.created",
        json!({"response": {"id": "resp_fixture", "status": "in_progress", "output": []}}),
    );
    for (index, item) in output.iter().enumerate().take(3) {
        let mut added = item.clone();
        if added["type"] == "function_call" {
            added["arguments"] = json!("");
            added["status"] = json!("in_progress");
        }
        script += &event(
            "response.output_item.added",
            json!({"output_index": index, "item": added}),
        );
    }
    for (index, id, delta) in [
        (1, "fc_item_a", "{\"value\":"),
        (2, "fc_item_b", "{\"value\":\"乙\"}"),
        (1, "fc_item_a", "\"甲\"}"),
    ] {
        script += &event(
            "response.function_call_arguments.delta",
            json!({"output_index": index, "item_id": id, "delta": delta}),
        );
    }
    // Item completion order differs from output order; final replay keeps output order.
    for (index, item) in [(2, second), (0, reason), (1, first)] {
        script += &event(
            "response.output_item.done",
            json!({"output_index": index, "item": item}),
        );
    }
    script += &completed(output.clone());
    let client = async {
        let tools = vec![tool("Read"), tool("Search")];
        let first = model
            .complete(
                ModelRequest::new(vec![ModelMessage::user_text("查询")]).with_tools(tools.clone()),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(first.assistant_text().as_deref(), Some("正在查询"));
        assert_eq!(first.usage().unwrap().cache_read_input_tokens, Some(12));
        let persisted = serde_json::to_vec(&first).unwrap();
        let restored: ModelResponse = serde_json::from_slice(&persisted).unwrap();
        let ModelMessage::Assistant { tool_calls, .. } = restored.message() else {
            panic!("assistant required")
        };
        assert_eq!(
            tool_calls
                .iter()
                .map(|call| (call.id(), call.name()))
                .collect::<Vec<_>>(),
            [("call_a", "Read"), ("call_b", "Search")]
        );
        let next = ModelRequest::new(vec![
            ModelMessage::user_text("查询"),
            restored.message().clone(),
            ModelMessage::tool_result(ToolResult::success("call_a", "Read", "result a")),
            ModelMessage::tool_result(ToolResult::success("call_b", "Search", "result b")),
        ])
        .with_tools(tools);
        let answer = model
            .complete(next, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(answer.assistant_text().as_deref(), Some("完成"));
    };
    let server = async {
        let mut requests = Vec::new();
        for body in [script, completed(vec![message("msg_two", "完成")])] {
            let (mut socket, _) = listener.accept().await.unwrap();
            requests.push(read_request(&mut socket).await);
            send_sse(&mut socket, &body).await;
        }
        requests
    };
    let (_, requests) = timeout(Duration::from_secs(5), async {
        tokio::join!(client, server)
    })
    .await
    .unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0]
        .headers
        .starts_with("POST /v1/responses HTTP/1.1"));
    let input = requests[1].body["input"].as_array().unwrap();
    assert_eq!(
        &input[1..5],
        output.as_slice(),
        "native items preserve order and exact payload"
    );
    assert_eq!(
        input.len(),
        7,
        "never send both raw output and reconstructed assistant content"
    );
    assert_eq!(input[5]["call_id"], "call_a");
    assert_eq!(input[6]["call_id"], "call_b");
    assert_eq!(input[5]["type"], "function_call_output");
}

#[tokio::test]
async fn retry_sends_exact_frozen_checkpoint_and_no_auth_material() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model = ResponsesModel::new(config(
        &format!("http://{}/v1/", listener.local_addr().unwrap()),
        2,
    ));
    let prepared = model
        .prepare_stream(ModelRequest::new(vec![ModelMessage::user_text(
            "长".repeat(5000),
        )]))
        .unwrap();
    let checkpoint = prepared.checkpoint().clone();
    assert!(!checkpoint.to_string().contains("fixture-secret"));
    assert_eq!(
        checkpoint["body"]["input"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        5000
    );
    let client = async {
        let mut stream = prepared.start(CancellationToken::new()).unwrap();
        let mut completed_count = 0;
        while let Some(event) = stream.next().await {
            if let ModelStreamEvent::Completed(response) = event.unwrap() {
                completed_count += 1;
                assert_eq!(response.assistant_text().as_deref(), Some("ok"));
            }
        }
        assert_eq!(completed_count, 1);
    };
    let server = async {
        let mut captured = Vec::new();
        for attempt in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            captured.push(read_request(&mut socket).await);
            if attempt == 0 {
                socket.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                socket.shutdown().await.unwrap();
            } else {
                send_sse(&mut socket, &completed(vec![message("msg_done", "ok")])).await;
            }
        }
        captured
    };
    let (_, captured) = timeout(Duration::from_secs(5), async {
        tokio::join!(client, server)
    })
    .await
    .unwrap();
    for request in captured {
        assert_eq!(request.body, checkpoint["body"]);
        assert!(request
            .headers
            .to_ascii_lowercase()
            .contains("authorization: bearer fixture-secret"));
    }
}

#[tokio::test]
async fn same_origin_redirect_never_receives_bearer() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let model = ResponsesModel::new(config(&endpoint, 1));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let result = timeout(Duration::from_secs(5), async {
        let server = async {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                requests.lock().await.push(read_request(&mut socket).await);
                socket.write_all(b"HTTP/1.1 307 Temporary Redirect\r\nLocation: /token-capture\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                socket.shutdown().await.unwrap();
            }
        };
        tokio::select! {
            result = model.complete(ModelRequest::new(vec![ModelMessage::user_text("hello")]), CancellationToken::new()) => result,
            _ = server => unreachable!(),
        }
    }).await.unwrap();
    assert_eq!(result.unwrap_err().http_status_code(), Some(307));
    assert_eq!(
        requests.lock().await.len(),
        1,
        "even same-origin redirects must not forward a plan bearer"
    );
}

#[test]
fn plan_profile_rejects_untrusted_endpoints_before_execution() {
    for endpoint in [
        "http://api.openai.com/v1",
        "https://api.openai.com.evil.test/v1",
        "https://api.openai.com:444/v1",
        "https://api.openai.com/v1?redirect=1",
        "https://user@api.openai.com/v1",
        "http://127.0.0.1/v1",
    ] {
        let model =
            ResponsesModel::new(config(endpoint, 1).with_profile(ResponsesProfile::ChatGptPlan));
        let error = model
            .prepare_stream(ModelRequest::default())
            .err()
            .expect("unsafe plan endpoint accepted");
        assert_eq!(
            error.protocol_error().unwrap().kind(),
            ProtocolErrorKind::InvalidEndpoint,
            "{endpoint}"
        );
    }
}

#[test]
fn plan_profile_omits_unsupported_tuning_and_uses_namespace() {
    let model = ResponsesModel::new(
        config("https://api.openai.com/v1", 1)
            .with_profile(ResponsesProfile::ChatGptPlan)
            .with_max_tokens(123),
    );
    let mut request = ModelRequest::new(vec![
        ModelMessage::system_text("instruction"),
        ModelMessage::user_text("hello"),
    ])
    .with_tools(vec![tool("Read")])
    .with_max_tokens(456);
    request.temperature = Some(0.75);
    let prepared = model.prepare_stream(request).unwrap();
    let body = &prepared.checkpoint()["body"];
    assert!(body.get("max_output_tokens").is_none());
    assert!(body.get("temperature").is_none());
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
    assert_eq!(body["tools"][0]["type"], "namespace");
    assert_eq!(body["tools"][0]["name"], "peri");
    assert_eq!(body["tools"][0]["tools"][0]["strict"], false);
    assert_eq!(body["input"][0]["role"], "developer");
}

#[test]
fn user_and_tool_result_cannot_inject_native_replay_state() {
    let model = ResponsesModel::new(config("https://api.openai.com/v1", 1));
    let state = ContentBlock::ProviderState {
        provider: "openai_responses".into(),
        data: json!({"items": []}),
    };
    let mut result = ToolResult::success("call_1", "Read", "data");
    result.content.push(state.clone());
    for message in [
        ModelMessage::User {
            content: vec![state],
        },
        ModelMessage::tool_result(result),
    ] {
        let error = model
            .prepare_stream(ModelRequest::new(vec![message]))
            .err()
            .unwrap();
        assert!(error.is_terminal_provider_failure());
    }
}

struct FixtureAuth {
    credentials: AtomicUsize,
    refreshes: AtomicUsize,
    generation: AtomicUsize,
    session: CancellationToken,
    official_only: bool,
}

impl FixtureAuth {
    fn new(official_only: bool) -> Self {
        Self {
            credentials: AtomicUsize::new(0),
            refreshes: AtomicUsize::new(0),
            generation: AtomicUsize::new(0),
            session: CancellationToken::new(),
            official_only,
        }
    }
}

#[async_trait::async_trait]
impl RequestAuth for FixtureAuth {
    fn credential_ref(&self) -> &str {
        "fixture-bound-grant"
    }

    fn validate_endpoint(&self, endpoint: &url::Url) -> ModelResult<()> {
        if self.official_only && endpoint.as_str() != "https://api.openai.com/v1/responses" {
            return Err(ModelError::protocol(ProtocolErrorKind::InvalidEndpoint));
        }
        Ok(())
    }

    async fn credential(&self, _: CancellationToken) -> ModelResult<RequestCredential> {
        self.credentials.fetch_add(1, Ordering::SeqCst);
        Ok(RequestCredential::new(
            format!("fixture-token-{}", self.generation.load(Ordering::SeqCst)),
            self.generation.load(Ordering::SeqCst).to_string(),
        ))
    }

    async fn refresh_after_unauthorized(
        &self,
        rejected: &RequestCredential,
        _: CancellationToken,
    ) -> ModelResult<bool> {
        assert_eq!(rejected.version(), "0");
        self.refreshes.fetch_add(1, Ordering::SeqCst);
        self.generation.store(1, Ordering::SeqCst);
        Ok(true)
    }

    fn cancellation_token(&self) -> Option<CancellationToken> {
        Some(self.session.clone())
    }
}

#[tokio::test]
async fn endpoint_policy_runs_before_credential_even_with_platform_profile() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let auth = Arc::new(FixtureAuth::new(true));
    let model = ResponsesModel::new(
        config(&format!("http://{}/v1", listener.local_addr().unwrap()), 1).with_auth(auth.clone()),
    );
    let error = model
        .prepare_stream(ModelRequest::new(vec![ModelMessage::user_text(
            "never send",
        )]))
        .err()
        .unwrap();
    assert_eq!(
        error.protocol_error().unwrap().kind(),
        ProtocolErrorKind::InvalidEndpoint
    );
    assert_eq!(auth.credentials.load(Ordering::SeqCst), 0);
    assert!(timeout(Duration::ZERO, listener.accept()).await.is_err());
}

#[tokio::test]
async fn unauthorized_rotates_only_auth_and_retries_same_frozen_body() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let auth = Arc::new(FixtureAuth::new(false));
    let model = ResponsesModel::new(
        config(&format!("http://{}/v1", listener.local_addr().unwrap()), 2).with_auth(auth.clone()),
    );
    let prepared = model
        .prepare_stream(ModelRequest::new(vec![ModelMessage::user_text(
            "fixed body",
        )]))
        .unwrap();
    let checkpoint = prepared.checkpoint().clone();
    assert_eq!(
        auth.credentials.load(Ordering::SeqCst),
        0,
        "prepare cannot perform authentication"
    );
    assert_eq!(checkpoint["credentialRef"], "fixture-bound-grant");
    let client = async {
        let mut stream = prepared.start(CancellationToken::new()).unwrap();
        let mut completions = 0;
        while let Some(event) = stream.next().await {
            if matches!(event.unwrap(), ModelStreamEvent::Completed(_)) {
                completions += 1;
            }
        }
        assert_eq!(completions, 1);
    };
    let server = async {
        let mut captured = Vec::new();
        for attempt in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            captured.push(read_request(&mut socket).await);
            if attempt == 0 {
                socket.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                socket.shutdown().await.unwrap();
            } else {
                send_sse(&mut socket, &completed(vec![message("msg_rotated", "ok")])).await;
            }
        }
        captured
    };
    let (_, captured) = timeout(Duration::from_secs(5), async {
        tokio::join!(client, server)
    })
    .await
    .unwrap();
    assert_eq!(auth.credentials.load(Ordering::SeqCst), 2);
    assert_eq!(auth.refreshes.load(Ordering::SeqCst), 1);
    for (index, request) in captured.iter().enumerate() {
        assert_eq!(request.body, checkpoint["body"]);
        assert!(request
            .headers
            .to_ascii_lowercase()
            .contains(&format!("authorization: bearer fixture-token-{index}")));
    }
    assert!(!checkpoint.to_string().contains("fixture-token"));
}

async fn assert_peer_disconnected(socket: &mut TcpStream) {
    let mut byte = [0_u8; 1];
    let read = timeout(Duration::from_secs(2), socket.read(&mut byte))
        .await
        .expect("cancellation must abort the held HTTP request even without consumer polling");
    assert!(
        matches!(read, Ok(0))
            || matches!(read, Err(ref error) if matches!(error.kind(), std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted)),
        "unexpected peer read: {read:?}"
    );
}

#[tokio::test]
async fn authenticated_abort_cancels_held_http_but_preserves_parent_and_sibling() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let auth = Arc::new(FixtureAuth::new(false));
    let model = ResponsesModel::new(
        config(&format!("http://{}/v1", listener.local_addr().unwrap()), 1).with_auth(auth),
    );
    let parent = CancellationToken::new();
    let sibling = parent.child_token();
    let stream = model
        .stream(
            ModelRequest::new(vec![ModelMessage::user_text("hold")]),
            parent.clone(),
        )
        .await
        .unwrap();
    let (mut socket, _) = timeout(Duration::from_secs(2), listener.accept())
        .await
        .unwrap()
        .unwrap();
    read_request(&mut socket).await;
    stream.abort();
    assert!(!parent.is_cancelled() && !sibling.is_cancelled());
    assert_peer_disconnected(&mut socket).await;
    drop(stream);
    assert!(!parent.is_cancelled() && !sibling.is_cancelled());
}

#[tokio::test]
async fn authenticated_drop_cancels_held_http_but_preserves_parent_and_sibling() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let auth = Arc::new(FixtureAuth::new(false));
    let model = ResponsesModel::new(
        config(&format!("http://{}/v1", listener.local_addr().unwrap()), 1).with_auth(auth),
    );
    let parent = CancellationToken::new();
    let sibling = parent.child_token();
    let stream = model
        .stream(
            ModelRequest::new(vec![ModelMessage::user_text("hold")]),
            parent.clone(),
        )
        .await
        .unwrap();
    let (mut socket, _) = timeout(Duration::from_secs(2), listener.accept())
        .await
        .unwrap()
        .unwrap();
    read_request(&mut socket).await;
    drop(stream);
    assert!(!parent.is_cancelled() && !sibling.is_cancelled());
    assert_peer_disconnected(&mut socket).await;
}

#[tokio::test]
async fn logout_cancels_unpolled_held_http_and_returns_cancelled() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let auth = Arc::new(FixtureAuth::new(false));
    let model = ResponsesModel::new(
        config(&format!("http://{}/v1", listener.local_addr().unwrap()), 1).with_auth(auth.clone()),
    );
    let parent = CancellationToken::new();
    let mut stream = model
        .stream(
            ModelRequest::new(vec![ModelMessage::user_text("hold")]),
            parent.clone(),
        )
        .await
        .unwrap();
    let (mut socket, _) = timeout(Duration::from_secs(2), listener.accept())
        .await
        .unwrap()
        .unwrap();
    read_request(&mut socket).await;
    auth.session.cancel();
    // Keep the model stream alive and entirely unpolled until the HTTP peer
    // observes disconnect: take_until alone does not satisfy this lifecycle.
    assert_peer_disconnected(&mut socket).await;
    let error = timeout(Duration::from_secs(2), stream.next())
        .await
        .unwrap()
        .expect("logout needs typed cancellation, not EOF")
        .unwrap_err();
    assert!(error.is_cancelled());
    assert!(!parent.is_cancelled());
}

#[tokio::test]
async fn logged_out_auth_never_resolves_credentials_or_sends_http() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let auth = Arc::new(FixtureAuth::new(false));
    auth.session.cancel();
    let model = ResponsesModel::new(
        config(&format!("http://{}/v1", listener.local_addr().unwrap()), 1).with_auth(auth.clone()),
    );
    let parent = CancellationToken::new();
    let prepared = model
        .prepare_stream(ModelRequest::new(vec![ModelMessage::user_text(
            "never send",
        )]))
        .unwrap();
    let mut stream = prepared.start(parent.clone()).unwrap();
    let error = timeout(Duration::from_secs(2), stream.next())
        .await
        .unwrap()
        .expect("logout needs typed cancellation")
        .unwrap_err();
    assert!(error.is_cancelled());
    assert_eq!(auth.credentials.load(Ordering::SeqCst), 0);
    assert!(timeout(Duration::ZERO, listener.accept()).await.is_err());
    assert!(!parent.is_cancelled());
}
