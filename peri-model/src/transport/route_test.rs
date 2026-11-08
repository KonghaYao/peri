use std::{collections::BTreeMap, sync::Arc, time::Duration};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use futures::StreamExt;
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
    time::timeout,
};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{
    AnthropicConfig, AnthropicModel, ContentBlock, Model, ModelCallContext, ModelCallParent,
    ModelMessage, ModelRequest, ModelRuntimeConfig, ModelStreamEvent, ModelTransportRoute,
    OpenAiConfig, OpenAiModel, ResponsesConfig, ResponsesModel, RetryConfig,
};

const BUDGET: Duration = Duration::from_secs(5);

#[derive(Debug)]
struct Captured {
    path: String,
    headers: BTreeMap<String, String>,
    body: Value,
}

impl Captured {
    fn context(&self) -> Value {
        serde_json::from_slice(
            &URL_SAFE_NO_PAD
                .decode(&self.headers["x-peri-replay-context"])
                .unwrap(),
        )
        .unwrap()
    }
}

async fn read_request(socket: &mut TcpStream) -> Captured {
    timeout(BUDGET, async {
        let mut bytes = Vec::new();
        let header_end = loop {
            let mut chunk = [0_u8; 4096];
            let size = socket.read(&mut chunk).await.unwrap();
            assert_ne!(size, 0, "请求头完成前连接结束");
            bytes.extend_from_slice(&chunk[..size]);
            assert!(bytes.len() < 1_000_000, "测试请求必须有界");
            if let Some(index) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let header = std::str::from_utf8(&bytes[..header_end]).unwrap();
        let mut lines = header.split("\r\n");
        let path = lines
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .to_owned();
        let headers: BTreeMap<_, _> = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
            .collect();
        let length: usize = headers["content-length"].parse().unwrap();
        while bytes.len() < header_end + length {
            let mut chunk = [0_u8; 4096];
            let size = socket.read(&mut chunk).await.unwrap();
            assert_ne!(size, 0, "请求体完成前连接结束");
            bytes.extend_from_slice(&chunk[..size]);
        }
        Captured {
            path,
            headers,
            body: serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap(),
        }
    })
    .await
    .expect("必须收到真实 HTTP 请求")
}

async fn respond(socket: &mut TcpStream, status: u16, body: &str) {
    let response = format!(
        "HTTP/1.1 {status} Reply\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    socket.write_all(response.as_bytes()).await.unwrap();
    socket.shutdown().await.unwrap();
}

async fn listener() -> (TcpListener, Url) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    (listener, endpoint)
}

async fn serve(replies: Vec<(u16, String)>) -> (Url, JoinHandle<Vec<Captured>>) {
    let (listener, endpoint) = listener().await;
    let task = tokio::spawn(async move {
        let mut captured = Vec::new();
        for (status, body) in replies {
            let (mut socket, _) = timeout(BUDGET, listener.accept()).await.unwrap().unwrap();
            captured.push(read_request(&mut socket).await);
            respond(&mut socket, status, &body).await;
        }
        captured
    });
    (endpoint, task)
}

fn runtime(proxy: Url) -> ModelRuntimeConfig {
    ModelRuntimeConfig::default()
        .with_transport_route(ModelTransportRoute::new(proxy, "run-fixture").unwrap())
        .with_retry(
            RetryConfig::default()
                .with_max_attempts(3)
                .with_base_delay(Duration::ZERO)
                .with_jitter(false),
        )
}

fn request(call: &str) -> ModelRequest {
    ModelRequest {
        messages: vec![ModelMessage::user_text("相同正文")],
        call_context: Some(ModelCallContext {
            owner: format!("owner-{call}"),
            execution: format!("execution-{call}"),
            call: call.into(),
            purpose: "reason".into(),
            parent: Some(ModelCallParent {
                owner: "parent-owner".into(),
                execution: "parent-execution".into(),
                call: "parent-call".into(),
                tool_call: "tool-call-1".into(),
                slot: Some("child-slot".into()),
            }),
        }),
        ..Default::default()
    }
}

fn chat_reply(text: &str) -> String {
    format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"id":"reply","choices":[{"delta":{"content":text},"finish_reason":"stop"}]})
    )
}

fn chat_model(proxy: Url) -> OpenAiModel {
    OpenAiModel::new(
        OpenAiConfig::new(
            "https://provider.invalid/v1?tenant=logical"
                .parse()
                .unwrap(),
            "synthetic-key",
            "fixture-model",
        )
        .with_runtime(runtime(proxy)),
    )
}

#[tokio::test]
async fn transport_route_preserves_all_provider_bodies_and_logical_identities() {
    let anthropic_reply = [
        json!({"type":"message_start","message":{"id":"reply","usage":{"input_tokens":1,"output_tokens":0}}}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":0}}),
        json!({"type":"message_stop"}),
    ].into_iter().map(|value| format!("event: {}\ndata: {value}\n\n",value["type"].as_str().unwrap())).collect::<String>();
    let responses_reply = format!(
        "event: response.completed\ndata: {}\n\n",
        json!({"type":"response.completed","response":{"id":"resp-a","status":"completed","output":[],"usage":{"input_tokens":1,"output_tokens":0}}})
    );
    let (proxy, server) = serve(vec![
        (200, anthropic_reply),
        (200, chat_reply("ok")),
        (200, responses_reply),
    ])
    .await;
    let models: Vec<Box<dyn Model>> = vec![
        Box::new(AnthropicModel::new(
            AnthropicConfig::new(
                "https://api.deepseek.com/anthropic?tenant=logical"
                    .parse()
                    .unwrap(),
                "synthetic-key",
                "claude-sonnet-4",
            )
            .with_runtime(runtime(proxy.clone())),
        )),
        Box::new(chat_model(proxy.clone())),
        Box::new(ResponsesModel::new(
            ResponsesConfig::new(
                "https://api.openai.com/v1".parse().unwrap(),
                "synthetic-key",
                "fixture-model",
            )
            .with_credential_ref("fixture-grant")
            .with_runtime(runtime(proxy)),
        )),
    ];
    let mut checkpoints = Vec::new();
    for (index, model) in models.into_iter().enumerate() {
        let mut input = request(&format!("call-{index}"));
        input
            .messages
            .insert(0, ModelMessage::assistant_text("历史回复"));
        let encoded = serde_json::to_value(&input).unwrap();
        assert!(encoded.get("call_context").is_none());
        let prepared = model.prepare_stream(input.clone()).unwrap();
        input.call_context.as_mut().unwrap().call = "modified-after-prepare".into();
        checkpoints.push(prepared.checkpoint().clone());
        let mut events = prepared.start(CancellationToken::new()).unwrap();
        let completed = timeout(BUDGET, async {
            while let Some(event) = events.next().await {
                if let ModelStreamEvent::Completed(response) = event.unwrap() {
                    return response;
                }
            }
            panic!("真实 HTTP 必须产生完成事件")
        })
        .await
        .unwrap();
        if index == 2 {
            let data = match completed.message() {
                ModelMessage::Assistant { content, .. } => content,
                _ => panic!("assistant response expected"),
            }
            .iter()
            .find_map(|block| {
                if let ContentBlock::ProviderState { data, .. } = block {
                    Some(data)
                } else {
                    None
                }
            })
            .expect("Responses 需保留原始 replay binding");
            assert_eq!(data["binding"]["credential_ref"], "fixture-grant");
            assert_eq!(data["binding"]["endpoint"], checkpoints[index]["endpoint"]);
        }
    }
    let captured = server.await.unwrap();
    assert_eq!(captured.len(), 3);
    for (index, (sent, checkpoint)) in captured.iter().zip(checkpoints).enumerate() {
        assert_eq!(sent.body, checkpoint["body"]);
        assert_eq!(
            sent.headers["x-peri-replay-target"],
            checkpoint["endpoint"].as_str().unwrap()
        );
        let logical: Url = sent.headers["x-peri-replay-target"].parse().unwrap();
        let expected_path = match logical.query() {
            Some(query) => format!("{}?{query}", logical.path()),
            None => logical.path().to_owned(),
        };
        assert_eq!(sent.path, expected_path);
        let context = sent.context();
        assert_eq!(context["call"], format!("call-{index}"));
        assert_eq!(context["attempt"], 1);
        assert_eq!(context["run"], "run-fixture");
        assert_eq!(context["parent"]["tool_call"], "tool-call-1");
    }
    assert_eq!(
        captured[0].body["messages"][0]["content"][0]["type"],
        "thinking"
    );
}

#[tokio::test]
async fn transport_route_records_each_physical_retry_with_one_logical_call() {
    let (proxy, server) = serve(vec![
        (
            429,
            "{\"error\":{\"message\":\"temporary rate limit\"}}".into(),
        ),
        (503, "{\"error\":{\"message\":\"temporary outage\"}}".into()),
        (200, chat_reply("recovered")),
    ])
    .await;
    let response = timeout(
        BUDGET,
        chat_model(proxy).complete(request("retry"), CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(response.assistant_text().unwrap(), "recovered");
    let captured = server.await.unwrap();
    assert_eq!(captured.len(), 3);
    for (index, wire) in captured.iter().enumerate() {
        assert_eq!(wire.context()["call"], "retry");
        assert_eq!(wire.context()["attempt"], index + 1);
        assert_eq!(wire.body, captured[0].body);
    }
}

#[tokio::test]
async fn transport_route_auth_preparation_failure_does_not_consume_wire_attempt() {
    use crate::{ModelError, ModelResult, RequestAuth, RequestCredential, TransportErrorKind};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    };
    struct Auth {
        credentials: AtomicUsize,
        refreshes: AtomicUsize,
        endpoints: Mutex<Vec<Url>>,
    }
    #[async_trait::async_trait]
    impl RequestAuth for Auth {
        fn credential_ref(&self) -> &str {
            "fixture-auth-grant"
        }
        fn validate_endpoint(&self, endpoint: &Url) -> ModelResult<()> {
            assert_eq!(endpoint.host_str(), Some("api.openai.com"));
            self.endpoints.lock().unwrap().push(endpoint.clone());
            Ok(())
        }
        async fn credential(&self, _: CancellationToken) -> ModelResult<RequestCredential> {
            let attempt = self.credentials.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                return Err(ModelError::transport(
                    TransportErrorKind::Connection,
                    None::<&str>,
                ));
            }
            Ok(RequestCredential::new(
                format!("synthetic-{attempt}"),
                attempt.to_string(),
            ))
        }
        async fn refresh_after_unauthorized(
            &self,
            _: &RequestCredential,
            _: CancellationToken,
        ) -> ModelResult<bool> {
            self.refreshes.fetch_add(1, Ordering::SeqCst);
            Ok(true)
        }
    }
    let completed = format!(
        "event: response.completed\ndata: {}\n\n",
        json!({"type":"response.completed","response":{"id":"resp-a","status":"completed","output":[]}})
    );
    let (proxy, server) = serve(vec![(401, "expired".into()), (200, completed)]).await;
    let auth = Arc::new(Auth {
        credentials: AtomicUsize::new(0),
        refreshes: AtomicUsize::new(0),
        endpoints: Mutex::new(Vec::new()),
    });
    let observed = Arc::new(Mutex::new(Vec::new()));
    let capture = observed.clone();
    let runtime =
        runtime(proxy).with_retry_observer(Arc::new(move |event: crate::RetryObservation| {
            capture.lock().unwrap().push(event.attempt());
        }));
    let model = ResponsesModel::new(
        ResponsesConfig::new(
            "https://api.openai.com/v1".parse().unwrap(),
            "unused",
            "fixture-model",
        )
        .with_auth(auth.clone())
        .with_runtime(runtime),
    );
    timeout(
        BUDGET,
        model.complete(request("auth"), CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap();
    let captured = server.await.unwrap();
    assert_eq!(captured.len(), 2);
    assert_eq!(captured[0].context()["attempt"], 1);
    assert_eq!(captured[1].context()["attempt"], 2);
    assert_eq!(captured[0].body, captured[1].body);
    assert_eq!(captured[0].headers["authorization"], "Bearer synthetic-1");
    assert_eq!(captured[1].headers["authorization"], "Bearer synthetic-2");
    assert_eq!(auth.credentials.load(Ordering::SeqCst), 3);
    assert_eq!(auth.refreshes.load(Ordering::SeqCst), 1);
    // Endpoint identity is checked at prepare and again before both physical sends.
    assert_eq!(auth.endpoints.lock().unwrap().len(), 3);
    assert_eq!(*observed.lock().unwrap(), [1, 2]);
}

#[tokio::test]
async fn transport_route_concurrent_identical_requests_keep_separate_contexts() {
    let (listener, proxy) = listener().await;
    let server = tokio::spawn(async move {
        let (mut first, _) = timeout(BUDGET, listener.accept()).await.unwrap().unwrap();
        let first_wire = read_request(&mut first).await;
        let (mut second, _) = timeout(BUDGET, listener.accept()).await.unwrap().unwrap();
        let second_wire = read_request(&mut second).await;
        respond(
            &mut second,
            200,
            &chat_reply(second_wire.context()["call"].as_str().unwrap()),
        )
        .await;
        respond(
            &mut first,
            200,
            &chat_reply(first_wire.context()["call"].as_str().unwrap()),
        )
        .await;
        vec![first_wire, second_wire]
    });
    let model = Arc::new(chat_model(proxy));
    let (a, b) = timeout(BUDGET, async {
        tokio::join!(
            model.complete(request("A"), CancellationToken::new()),
            model.complete(request("B"), CancellationToken::new()),
        )
    })
    .await
    .unwrap();
    assert_eq!(a.unwrap().assistant_text().unwrap(), "A");
    assert_eq!(b.unwrap().assistant_text().unwrap(), "B");
    let captured = server.await.unwrap();
    assert_eq!(captured[0].body, captured[1].body);
    assert_ne!(
        captured[0].context()["owner"],
        captured[1].context()["owner"]
    );
    assert!(captured.iter().all(|wire| wire.context()["attempt"] == 1));
}

#[tokio::test]
async fn transport_route_cancel_closes_only_its_stream_without_retry() {
    let (listener, proxy) = listener().await;
    let server = tokio::spawn(async move {
        let (mut a, _) = timeout(BUDGET, listener.accept()).await.unwrap().unwrap();
        let a_wire = read_request(&mut a).await;
        a.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n").await.unwrap();
        let (mut b, _) = timeout(BUDGET, listener.accept()).await.unwrap().unwrap();
        let b_wire = read_request(&mut b).await;
        respond(&mut b, 200, &chat_reply("sibling")).await;
        let mut byte = [0_u8; 1];
        let closed = timeout(BUDGET, a.read(&mut byte)).await.unwrap();
        assert!(matches!(closed, Ok(0) | Err(_)), "取消必须关闭对应连接");
        assert!(
            timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "可见输出后的取消不能触发重试"
        );
        vec![a_wire, b_wire]
    });
    let model = chat_model(proxy);
    let mut a = model
        .stream(request("A"), CancellationToken::new())
        .await
        .unwrap();
    assert!(
        matches!(timeout(BUDGET, a.next()).await.unwrap(), Some(Ok(ModelStreamEvent::TextDelta { text })) if text == "partial")
    );
    a.abort();
    let cancelled = timeout(BUDGET, a.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(cancelled.is_cancelled());
    let b = timeout(
        BUDGET,
        model.complete(request("B"), CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(b.assistant_text().unwrap(), "sibling");
    let captured = server.await.unwrap();
    assert_eq!(
        captured
            .iter()
            .map(|wire| wire.context()["call"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>(),
        ["A", "B"]
    );
}

#[tokio::test]
async fn transport_route_missing_context_fails_before_any_http_request() {
    let (listener, proxy) = listener().await;
    let error = timeout(
        BUDGET,
        chat_model(proxy).complete(
            ModelRequest::new(vec![ModelMessage::user_text("missing context")]),
            CancellationToken::new(),
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.to_string().contains("requires a call context"));
    assert!(timeout(Duration::from_millis(100), listener.accept())
        .await
        .is_err());
}

#[tokio::test]
async fn transport_route_disabled_does_not_send_correlation_headers() {
    let (endpoint, server) = serve(vec![(200, chat_reply("direct"))]).await;
    let model = OpenAiModel::new(OpenAiConfig::new(
        endpoint,
        "synthetic-key",
        "fixture-model",
    ));
    let response = timeout(
        BUDGET,
        model.complete(request("direct"), CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(response.assistant_text().unwrap(), "direct");
    let captured = server.await.unwrap();
    assert_eq!(captured.len(), 1);
    assert!(!captured[0].headers.contains_key("x-peri-replay-context"));
    assert!(!captured[0].headers.contains_key("x-peri-replay-target"));
    assert!(captured[0].body.get("call_context").is_none());
}

#[test]
fn transport_route_rejects_ambiguous_origins_and_empty_run() {
    for endpoint in [
        "ftp://localhost/",
        "http://user:password@localhost/",
        "http://localhost/prefix",
        "http://localhost/?query=1",
        "http://localhost/#fragment",
    ] {
        assert!(
            ModelTransportRoute::new(endpoint.parse().unwrap(), "run").is_err(),
            "必须拒绝非origin路由: {endpoint}"
        );
    }
    assert!(ModelTransportRoute::new("http://localhost/".parse().unwrap(), " ").is_err());
}
