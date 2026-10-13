//! Tests for client_lf

use std::sync::{Arc, Mutex};
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::*;
use crate::config::{BackpressurePolicy, Compression};
use crate::types::TraceBody;

/// 测试客户端：短退避便于观察重试，内容编码按用例显式指定（gzip 是生产默认）。
fn create_test_client_with(
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

fn create_test_client(server_url: &str, max_retries: usize) -> LangfuseClient {
    create_test_client_with(server_url, max_retries, Compression::Gzip)
}

fn create_test_event(id: &str) -> IngestionEvent {
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
struct Captured {
    body: Vec<u8>,
    content_length: Option<String>,
    content_encoding: Option<String>,
}

/// mock 服务端的请求捕获回调；响应体固定为空 OTLP 回执。
fn capture_requests(
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
fn sha2_of(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn gunzip(bytes: &[u8]) -> Vec<u8> {
    use std::io::Read;
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .read_to_end(&mut out)
        .expect("gunzip");
    out
}

/// OTLP 正文里第一个 span 的 traceId（观测 ID 语义断言用）。
fn trace_id_of(json: &[u8]) -> String {
    let value: serde_json::Value = serde_json::from_slice(json).expect("OTLP JSON");
    value["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["traceId"]
        .as_str()
        .expect("traceId")
        .to_owned()
}

#[test]
fn test_new_creates_client_with_correct_auth() {
    let client = create_test_client("http://localhost", 3);
    assert_eq!(client.auth_header, "Basic cGs6c2s=");
    assert_eq!(client.base_url, "http://localhost");
    assert_eq!(client.max_retries, 3);
}

#[test]
fn test_new_trims_trailing_slash() {
    let client = create_test_client("http://localhost/", 0);
    assert_eq!(client.base_url, "http://localhost");
}

#[tokio::test]
async fn test_ingest_success_200() {
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
async fn test_ingest_empty_batch() {
    let client = create_test_client("http://unused", 0);
    let result = client.ingest(vec![]).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_ingest_4xx_no_retry() {
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
async fn test_ingest_5xx_retries_then_success() {
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
async fn test_ingest_5xx_retries_exhausted() {
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
async fn test_ingest_network_error_retries() {
    // 注意：环境中存在 HTTP 代理时，连接失败会由代理返回 502 而非 reqwest 层网络错误，
    // 因此结果可能是 Http（直连网络错误）或 IngestionApi（代理返回 5xx 后重试耗尽）。
    let client = LangfuseClient::new("pk", "sk", "http://127.0.0.1:1", 1);
    let result = client.ingest(vec![create_test_event("1")]).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_ingest_payload_has_otel_format() {
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
async fn test_ingest_gzip_body_matches_wire_length_and_headers() {
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
fn test_from_config() {
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

/// 文件正文 + 真实 TCP 断连的发送用例（需求「文件流重试」、契约 §8 P2）。
///
/// native 目标才有的暂存读泵与 TCP 服务端都在这里，非 native 目标不编译本模块
/// （WASM/emscripten 只有有界内存路径，见 `client_body.rs`）。
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "emscripten")))]
mod native_file_body {
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    use super::{
        capture_requests, create_test_client, create_test_event, gunzip, sha2_of, trace_id_of,
    };
    use crate::client::send::{FileCheckpoint, SendOutcome};
    use crate::client::ExportConfig;
    use crate::config::{BatcherConfig, TelemetryConfig};
    use crate::encoding::{BudgetedWriter, ExportContext};
    use crate::memory::MemoryBudget;
    use crate::payload::{ExportPayload, PayloadBody};
    use crate::snapshot::TelemetryEvent;
    use crate::spool::cleanup::CleanupRegistry;
    use crate::spool::native::NativeSpool;
    use crate::spool::{ObjectId, RouteFingerprint, SpoolState, SpoolStore};

    /// 真实 TCP 用例的超时上限：到点即失败，不无限等待。
    const TEST_TIMEOUT: Duration = Duration::from_secs(10);
    /// 最小 OTLP 回执。
    const RESPONSE_OK: &[u8] = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
        content-length: 2\r\nconnection: close\r\n\r\n{}";

    /// 服务端脚本的记录：首轮只读正文前缀就断连，第二轮读满完整正文。
    struct DisconnectLog {
        first_length: usize,
        first_prefix: Vec<u8>,
        first_state: SpoolState,
        first_attempts: u64,
        retry_length: usize,
        retry_body: Vec<u8>,
        retry_state: SpoolState,
        retry_attempts: u64,
    }

    /// 从持久描述信息直接读水位（不经内存快照），用于证明 execute 前已写盘。
    fn persisted(store: &Arc<dyn SpoolStore>, id: ObjectId) -> (SpoolState, u64) {
        let meta = store.inspect(id).expect("persisted meta");
        (meta.state, meta.attempts)
    }

    /// 当前墙上时钟毫秒：临时对象的期限必须落在真实时间线上，否则扫描会把它投影成过期终态。
    fn now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("wall clock")
            .as_millis() as i64
    }

    /// 真实 native 暂存上的文件正文：同一编码入口，前缀阈值强制转盘。
    fn file_payload(root: &std::path::Path, events: Vec<super::IngestionEvent>) -> ExportPayload {
        let telemetry = TelemetryConfig {
            payload_memory_threshold_bytes: 128,
            ..Default::default()
        };
        let config = BatcherConfig {
            max_in_flight: 1,
            telemetry: telemetry.clone(),
            ..Default::default()
        };
        let export = ExportConfig {
            max_wire_bytes: 1 << 20,
            ..Default::default()
        };
        let budget = MemoryBudget::for_batcher(&config, &export).expect("deployment budget");
        let route = RouteFingerprint::from_bytes([0x2b; 32]);
        let store: Arc<dyn SpoolStore> = Arc::new(
            NativeSpool::open_at(root.to_path_buf(), route, 8 * 1024 * 1024).expect("native spool"),
        );
        let registry = CleanupRegistry::new(Arc::clone(&store), Arc::clone(&budget), 4);
        let registration = registry.try_register().expect("cleanup slot");
        let telemetry_events = events
            .into_iter()
            .map(|event| {
                TelemetryEvent::try_new(event, None, None, &budget).expect("telemetry event")
            })
            .collect::<Vec<_>>();
        let accepted_at_ms = now_ms();
        let context = ExportContext {
            route,
            accepted_at_ms,
            expires_at_ms: accepted_at_ms + 600_000,
        };
        let writer = BudgetedWriter::new(
            &telemetry,
            budget,
            Some(store),
            context,
            export.max_wire_bytes,
            registration,
        )
        .expect("budgeted writer");
        let payload = super::super::export::encode(&telemetry_events, writer, &export)
            .expect("encoded file payload");
        assert!(
            matches!(payload.body, PayloadBody::File(_)),
            "跨阈值必须转盘"
        );
        payload
    }

    /// 同一事件在健康服务端上真实发送的线包正文，作为摘要与观测 ID 的对照。
    async fn healthy_wire_body(events: Vec<super::IngestionEvent>) -> Vec<u8> {
        let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/public/otel/v1/traces")
            .with_status(200)
            .with_body_from_request(capture_requests(&captured))
            .expect(1)
            .create_async()
            .await;
        create_test_client(&server.url(), 0)
            .ingest(events)
            .await
            .expect("healthy ingest");
        mock.assert_async().await;
        let request = captured
            .lock()
            .expect("capture lock")
            .pop()
            .expect("captured request");
        assert_eq!(request.content_encoding.as_deref(), Some("gzip"));
        request.body
    }

    /// 读取请求头，返回 Content-Length 与同缓冲里已带出的正文前缀。
    async fn read_request_head(socket: &mut TcpStream) -> (usize, Vec<u8>) {
        let mut buffer = Vec::new();
        loop {
            if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buffer[..end]).to_ascii_lowercase();
                let length = head
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .map(|value| value.trim().parse::<usize>().expect("content-length value"))
                    .expect("每个 OTLP 请求都带 Content-Length");
                return (length, buffer[end + 4..].to_vec());
            }
            let read = socket
                .read_buf(&mut buffer)
                .await
                .expect("read request head");
            assert!(read > 0, "连接在请求头完成前关闭");
        }
    }

    /// 补足到 `wanted` 字节；正文没读满就断开说明服务端脚本与客户端行为不符。
    async fn read_body_upto(socket: &mut TcpStream, mut body: Vec<u8>, wanted: usize) -> Vec<u8> {
        while body.len() < wanted {
            let read = socket.read_buf(&mut body).await.expect("read request body");
            assert!(read > 0, "连接在正文读满前关闭");
        }
        body.truncate(wanted);
        body
    }

    /// 首轮读到正文中途立刻关闭（不回响应，客户端按可恢复传输错误重试），第二轮读完并 200。
    ///
    /// 每一轮都在读到请求之后、回响应之前读一次持久描述信息：`attempts` 必须是本次 attempt
    /// 的序号，证明 checkpoint 已经写盘才发出请求。
    async fn mid_body_disconnect_server(
        listener: TcpListener,
        store: Arc<dyn SpoolStore>,
        id: ObjectId,
    ) -> DisconnectLog {
        let (mut first, _) = listener.accept().await.expect("first connection");
        let (first_length, buffered) = read_request_head(&mut first).await;
        let (first_state, first_attempts) = persisted(&store, id);
        let prefix_len = (first_length / 2).max(1);
        let first_prefix = read_body_upto(&mut first, buffered, prefix_len).await;
        drop(first);

        let (mut retry, _) = listener.accept().await.expect("retry connection");
        let (retry_length, buffered) = read_request_head(&mut retry).await;
        let (retry_state, retry_attempts) = persisted(&store, id);
        let retry_body = read_body_upto(&mut retry, buffered, retry_length).await;
        retry.write_all(RESPONSE_OK).await.expect("write response");
        retry.flush().await.expect("flush response");
        DisconnectLog {
            first_length,
            first_prefix,
            first_state,
            first_attempts,
            retry_length,
            retry_body,
            retry_state,
            retry_attempts,
        }
    }

    #[tokio::test]
    async fn test_retry_reopens_file_body_at_zero_after_mid_body_disconnect() {
        let events = vec![create_test_event("evt-1")];
        let control = healthy_wire_body(events.clone()).await;

        let dir = tempfile::TempDir::new().expect("temp dir");
        let payload = file_payload(dir.path(), events);
        let (store, object_id, claim) = match &payload.body {
            PayloadBody::File(object) => (
                Arc::clone(&object.store),
                object.meta().id,
                object.take_claim().expect("交付句柄只能取一次"),
            ),
            PayloadBody::Memory(_) => panic!("用例必须走文件正文"),
        };
        let mut checkpoint = FileCheckpoint::new(claim);

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        let server = tokio::spawn(mid_body_disconnect_server(
            listener,
            Arc::clone(&store),
            object_id,
        ));

        let client = create_test_client(&url, 3);
        let outcome =
            tokio::time::timeout(TEST_TIMEOUT, client.send_payload(&payload, &mut checkpoint))
                .await
                .expect("发送超时");
        assert!(
            matches!(outcome, SendOutcome::Delivered),
            "中途断连后的第二次必须交付"
        );

        let seen = tokio::time::timeout(TEST_TIMEOUT, server)
            .await
            .expect("断连脚本超时")
            .expect("server task");

        // 每次请求发出前，持久描述信息里的水位已经加一且状态是 sending。
        assert_eq!(
            seen.first_attempts, 1,
            "首次 execute 前必须已 checkpoint 第 1 次"
        );
        assert_eq!(seen.first_state, SpoolState::Sending);
        assert_eq!(
            seen.retry_attempts, 2,
            "重试 execute 前必须已 checkpoint 第 2 次"
        );
        assert_eq!(seen.retry_state, SpoolState::Sending);
        // (a) 第二轮从字节 0 重开：两轮长度都是 wire 长度，第二轮以首轮已读前缀开头。
        assert_eq!(seen.first_length, payload.wire_len as usize);
        assert_eq!(seen.retry_length, payload.wire_len as usize);
        assert_eq!(seen.retry_body.len(), payload.wire_len as usize);
        assert_eq!(
            seen.retry_body[..seen.first_prefix.len()],
            seen.first_prefix[..],
            "第二次必须从字节 0 重发同一份正文，而不是从断点续传"
        );
        // (b) 完整正文摘要：测试独立重算，并与 payload 登记的 wire 摘要、健康发送对照一致。
        assert_eq!(sha2_of(&seen.retry_body), payload.sha256);
        assert_eq!(sha2_of(&seen.retry_body), sha2_of(&control));
        // (c) 观测 ID 不变：重试正文里的 traceId 与健康发送一致，且不是全零。
        let trace_id = trace_id_of(&gunzip(&seen.retry_body));
        assert_eq!(trace_id, trace_id_of(&gunzip(&control)));
        assert_eq!(trace_id.len(), 32);
        assert!(trace_id.bytes().any(|byte| byte != b'0'));
        // 周期结束后句柄回到发送 owner 手里，水位与正文身份都没有被改写。
        let claim = checkpoint.into_claim().await.expect("交付句柄必须收回");
        assert_eq!(claim.meta().attempts, 2);
        assert_eq!(claim.meta().sha256, payload.sha256);
        assert_eq!(claim.meta().stored_len, payload.wire_len);
    }
}

/// direct ingest 的 retained 上界必须按真实挂账量推导，不得再用「限额×倍数」估算（契约 §4）。
///
/// 口径 = Σ 事件原文实测 + writer 一次性内存前缀 + 启用压缩时的 gzip 状态账单；事件原文与
/// `TelemetryEvent::try_new` 的挂账同源，因此预算恰好覆盖本次全部预约，既不虚高也不短缺。
#[test]
fn direct_budget_is_measured_not_multiplied() {
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
        direct_budget(&events, &gzip).expect("direct budget"),
        measured + 64 * 1024 + GZIP_BILLING_BYTES
    );
    let none = ExportConfig {
        compression: Compression::None,
        ..gzip.clone()
    };
    assert_eq!(
        direct_budget(&events, &none).expect("direct budget"),
        measured + 64 * 1024,
        "未启用压缩时不得计入 gzip 账单"
    );
    // 预算里不得再有任何与请求限额成比例的项：只放大 `max_request_bytes` 不改变上界。
    let wider_limit = ExportConfig {
        max_request_bytes: gzip.max_request_bytes * 4,
        ..gzip.clone()
    };
    assert_eq!(
        direct_budget(&events, &wider_limit).expect("direct budget"),
        direct_budget(&events, &gzip).expect("direct budget"),
        "预算必须与请求限额解耦（旧实现的 limit×2 余量在此处暴露）"
    );
}

/// 逐条实测的预算必须恰好覆盖 direct 编码的全部预约：正文填满内存前缀也不得虚假超预算。
#[tokio::test]
async fn direct_ingest_fills_the_memory_prefix_without_budget_failure() {
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
