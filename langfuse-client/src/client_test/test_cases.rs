use super::Digest as _;
use super::{
    Arc, BackpressurePolicy, Compression, Duration, ExportConfig, IngestionEvent, LangfuseClient,
    LangfuseError, Mutex, Sha256, TraceBody, GZIP_BILLING_BYTES,
};

/// 测试客户端：短退避便于观察重试，内容编码按用例显式指定（gzip 是生产默认）。
pub(super) fn create_test_client_with(
    server_url: &str,
    max_retries: usize,
    compression: Compression,
) -> LangfuseClient {
    LangfuseClient::new("pk", "sk", server_url, max_retries)
        .with_export_config(ExportConfig {
            initial_retry_delay: Duration::from_millis(5),
            max_retry_delay: Duration::from_millis(20),
            compression,
            ..Default::default()
        })
        .unwrap()
}

pub(super) fn create_test_client(server_url: &str, max_retries: usize) -> LangfuseClient {
    create_test_client_with(server_url, max_retries, Compression::Gzip)
}

pub(super) fn create_test_event(id: &str) -> IngestionEvent {
    IngestionEvent::TraceCreate {
        id: id.to_string(),
        timestamp: "2026-01-01T00:00:00Z".to_string(),
        body: TraceBody {
            id: Some(format!("trace-{}", id)),
            name: Some("test".into()),
            ..Default::default()
        },
        metadata: None,
    }
}

/// 服务端实际收到的请求：线包正文与长度/编码头。
pub(super) struct Captured {
    pub(super) body: Vec<u8>,
    pub(super) content_length: Option<String>,
    pub(super) content_encoding: Option<String>,
}

/// mock 服务端的请求捕获回调；响应体固定为空 OTLP 回执。
pub(super) fn capture_requests(
    captured: &Arc<Mutex<Vec<Captured>>>,
) -> impl Fn(&mockito::Request) -> Vec<u8> + Send + Sync + 'static {
    let captured = Arc::clone(captured);
    move |request| {
        let header = |name: &str| {
            request
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        captured.lock().expect("capture lock").push(Captured {
            body: request.body().expect("request body").clone(),
            content_length: header("content-length"),
            content_encoding: header("content-encoding"),
        });
        b"{}".to_vec()
    }
}

/// 独立重算的 wire 摘要；不与编码侧共用实现。
pub(super) fn sha2_of(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

pub(super) fn gunzip(bytes: &[u8]) -> Vec<u8> {
    use std::io::Read;
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .read_to_end(&mut out)
        .expect("gunzip");
    out
}

/// OTLP 正文里第一个 span 的 traceId（观测 ID 语义断言用）。
pub(super) fn trace_id_of(json: &[u8]) -> String {
    let value: serde_json::Value = serde_json::from_slice(json).expect("OTLP JSON");
    value["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["traceId"]
        .as_str()
        .expect("traceId")
        .to_owned()
}

#[test]
pub(super) fn test_new_creates_client_with_correct_auth() {
    let client = create_test_client("http://localhost", 3);
    assert_eq!(client.auth_header, "Basic cGs6c2s=");
    assert_eq!(client.base_url, "http://localhost");
    assert_eq!(client.max_retries, 3);
}

#[test]
pub(super) fn test_new_trims_trailing_slash() {
    let client = create_test_client("http://localhost/", 0);
    assert_eq!(client.base_url, "http://localhost");
}

#[tokio::test]
pub(super) async fn test_ingest_success_200() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body("{}")
        .match_header("Authorization", "Basic cGs6c2s=")
        .match_header("x-langfuse-ingestion-version", "4")
        .match_header("Content-Type", "application/json")
        .create_async()
        .await;

    let client = create_test_client(&server.url(), 0);
    let result = client.ingest(vec![create_test_event("evt-1")]).await;
    assert!(result.is_ok());
    mock.assert_async().await;
}

#[tokio::test]
pub(super) async fn test_ingest_empty_batch() {
    let client = create_test_client("http://unused", 0);
    let result = client.ingest(vec![]).await;
    assert!(result.is_ok());
}

#[tokio::test]
pub(super) async fn test_ingest_4xx_no_retry() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(400)
        .with_body(r#"{"error":"bad request"}"#)
        .expect(1)
        .create_async()
        .await;

    let client = create_test_client(&server.url(), 3);
    let result = client.ingest(vec![create_test_event("1")]).await;
    assert!(result.is_err());
    match result.unwrap_err() {
        LangfuseError::IngestionApi(msg) => {
            assert!(msg.contains("OTLP"));
            assert!(msg.contains("HTTP 400"));
        }
        other => panic!("Expected IngestionApi, got: {:?}", other),
    }
    mock.assert_async().await;
}

#[tokio::test]
pub(super) async fn test_ingest_5xx_retries_then_success() {
    let mut server = mockito::Server::new_async().await;
    let mock_fail = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(503)
        .with_body("internal error")
        .expect(1)
        .create_async()
        .await;
    let mock_success = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(200)
        .with_body("{}")
        .expect(1)
        .create_async()
        .await;

    let client = create_test_client(&server.url(), 3);
    let result = client.ingest(vec![create_test_event("1")]).await;
    assert!(result.is_ok());
    mock_fail.assert_async().await;
    mock_success.assert_async().await;
}

#[tokio::test]
pub(super) async fn test_ingest_5xx_retries_exhausted() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(503)
        .with_body("internal error")
        .expect(3) // 1 initial + 2 retries
        .create_async()
        .await;

    let client = create_test_client(&server.url(), 2);
    let result = client.ingest(vec![create_test_event("1")]).await;
    assert!(result.is_err());
    match result.unwrap_err() {
        LangfuseError::IngestionApi(msg) => {
            assert!(msg.contains("after 2 retries"));
        }
        other => panic!("Expected IngestionApi, got: {:?}", other),
    }
    mock.assert_async().await;
}

#[tokio::test]
pub(super) async fn test_ingest_network_error_retries() {
    // 注意：环境中存在 HTTP 代理时，连接失败会由代理返回 502 而非 reqwest 层网络错误，
    // 因此结果可能是 Http（直连网络错误）或 IngestionApi（代理返回 5xx 后重试耗尽）。
    let client = LangfuseClient::new("pk", "sk", "http://127.0.0.1:1", 1);
    let result = client.ingest(vec![create_test_event("1")]).await;
    assert!(result.is_err());
}

#[tokio::test]
pub(super) async fn test_ingest_payload_has_otel_format() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(200)
        .with_body_from_request(capture_requests(&captured))
        .expect(1)
        .create_async()
        .await;

    // none 模式继续按未压缩正文断言 OTLP 结构，不弱化为「只要 200」。
    let client = create_test_client_with(&server.url(), 0, Compression::None);
    let result = client.ingest(vec![create_test_event("1")]).await;
    assert!(result.is_ok());
    mock.assert_async().await;

    let request = captured
        .lock()
        .expect("capture lock")
        .pop()
        .expect("captured request");
    assert_eq!(
        request.content_encoding, None,
        "none 模式不得带 Content-Encoding"
    );
    assert_eq!(
        request
            .content_length
            .as_deref()
            .and_then(|value| value.parse::<usize>().ok()),
        Some(request.body.len()),
        "Content-Length 必须等于线包字节数"
    );
    let json = String::from_utf8(request.body).expect("未压缩正文必须是 UTF-8 JSON");
    assert!(json.contains("\"resourceSpans\""));
    assert!(json.contains("\"scopeSpans\""));
    assert!(json.contains("\"spans\""));
    assert!(json.contains("\"traceId\":\"") && json.contains("\"spanId\":\""));
}

#[tokio::test]
pub(super) async fn test_ingest_gzip_body_matches_wire_length_and_headers() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(200)
        .with_body_from_request(capture_requests(&captured))
        .expect(1)
        .create_async()
        .await;

    let client = create_test_client(&server.url(), 0);
    let result = client.ingest(vec![create_test_event("evt-1")]).await;
    assert!(result.is_ok());
    mock.assert_async().await;

    let request = captured
        .lock()
        .expect("capture lock")
        .pop()
        .expect("captured request");
    assert_eq!(request.content_encoding.as_deref(), Some("gzip"));
    assert_eq!(request.body.first().copied(), Some(0x1f), "gzip magic");
    assert_eq!(request.body.get(1).copied(), Some(0x8b));
    assert_eq!(
        request
            .content_length
            .as_deref()
            .and_then(|value| value.parse::<usize>().ok()),
        Some(request.body.len()),
        "Content-Length 必须是 wire 长度，不是解压后长度"
    );
    let json = String::from_utf8(gunzip(&request.body)).expect("解压后是 UTF-8 JSON");
    assert!(json.contains("\"resourceSpans\""));
    assert!(json.contains("\"scopeSpans\""));
    assert!(json.contains("\"spans\""));
}

#[test]
pub(super) fn test_from_config() {
    let config = crate::config::ClientConfig {
        public_key: "pk".into(),
        secret_key: "sk".into(),
        base_url: "https://cloud.langfuse.com".into(),
        trace_sampling: 1.0,
        error_span_always: true,
        batch_max_events: 50,
        batch_flush_interval_secs: 10,
        batch_backpressure: BackpressurePolicy::DropNew,
        telemetry: Default::default(),
    };
    let client = LangfuseClient::from_config(&config, 2);
    assert_eq!(client.auth_header, "Basic cGs6c2s=");
    assert_eq!(client.max_retries, 2);
}

/// direct ingest 的 retained 上界必须按真实挂账量推导，不得再用「限额×倍数」估算（契约 §4）。
///
/// 口径 = Σ 事件原文实测 + writer 一次性内存前缀 + 启用压缩时的 gzip 状态账单；事件原文与
/// `TelemetryEvent::try_new` 的挂账同源，因此预算恰好覆盖本次全部预约，既不虚高也不短缺。
#[test]
pub(super) fn direct_budget_is_measured_not_multiplied() {
    let events = vec![create_test_event("a"), create_test_event("b")];
    let measured: usize = events
        .iter()
        .map(|event| crate::snapshot::measure_event(event).expect("measure event"))
        .sum();
    let gzip = ExportConfig {
        max_wire_bytes: 64 * 1024,
        compression: Compression::Gzip,
        ..Default::default()
    };
    assert_eq!(
        super::super::direct_budget(&events, &gzip).expect("direct budget"),
        measured + 64 * 1024 + GZIP_BILLING_BYTES
    );
    let none = ExportConfig {
        compression: Compression::None,
        ..gzip.clone()
    };
    assert_eq!(
        super::super::direct_budget(&events, &none).expect("direct budget"),
        measured + 64 * 1024,
        "未启用压缩时不得计入 gzip 账单"
    );
    // 预算里不得再有任何与请求限额成比例的项：只放大 `max_request_bytes` 不改变上界。
    let wider_limit = ExportConfig {
        max_request_bytes: gzip.max_request_bytes * 4,
        ..gzip.clone()
    };
    assert_eq!(
        super::super::direct_budget(&events, &wider_limit).expect("direct budget"),
        super::super::direct_budget(&events, &gzip).expect("direct budget"),
        "预算必须与请求限额解耦（旧实现的 limit×2 余量在此处暴露）"
    );
}

/// 逐条实测的预算必须恰好覆盖 direct 编码的全部预约：正文填满内存前缀也不得虚假超预算。
#[tokio::test]
pub(super) async fn direct_ingest_fills_the_memory_prefix_without_budget_failure() {
    let server = mockito::Server::new_async().await;
    let mut server = server;
    let _mock = server
        .mock("POST", "/api/public/otel/v1/traces")
        .with_status(200)
        .with_body("{}")
        .create_async()
        .await;
    let client = LangfuseClient::new("pk", "sk", &server.url(), 0)
        .with_export_config(ExportConfig {
            // 前缀上限压到很小，正文必然吃满该额度。
            max_wire_bytes: 4096,
            max_request_bytes: 512 * 1024,
            compression: Compression::None,
            ..Default::default()
        })
        .expect("export config");
    let events = vec![
        create_test_event("fill-1"),
        create_test_event("fill-2"),
        create_test_event("fill-3"),
    ];
    client
        .ingest(events)
        .await
        .expect("填满内存前缀的 direct ingest 必须成功，而不是虚假 MemoryBudgetExceeded");
}
