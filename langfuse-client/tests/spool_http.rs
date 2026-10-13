#![cfg(all(not(target_arch = "wasm32"), not(target_os = "emscripten")))]

//! 真实 HTTP 的 spool 断言（契约 §3.5、§8 的 `tests/spool_http.rs` 行）。
//!
//! 覆盖三件事，全部落在真实字节上，不以「只判 200」代替：
//! 1. 首次请求被服务端读到正文中途即断连，重试必须**从字节 0 重开**同一份正文（内存与文件两种
//!    正文来源都试，且与健康发送的对照正文逐字节相等）；
//! 2. gzip 与 none 两模式的 `Content-Encoding`/`Content-Length` 与真实线包一致（同 OTLP 语义）；
//! 3. 文件正文由磁盘上已发布的 spool 对象逐字节供给，请求带 `Content-Length` 而**不带**
//!    `Transfer-Encoding`（不是 chunked 流）。
//!
//! 服务端是本文件内的本地 TCP fixture：真实读取请求字节、真断连、真回响应；不依赖公网、
//! 不依赖真实 Langfuse，也不使用 crate 内私有构造器与 `TestStore` 之类的进程内双。

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use langfuse_client::client::ExportConfig;
use langfuse_client::config::{BatcherConfig, Compression, TelemetryConfig};
use langfuse_client::spool::native::NativeSpool;
use langfuse_client::spool::{route_fingerprint, RouteFingerprint, SpoolStore};
use langfuse_client::types::{GenerationBody, TraceBody};
use langfuse_client::{Batcher, IngestionEvent, LangfuseClient};
use sha2::{Digest, Sha256};

const PK: &str = "pk-spool-http";
const SK: &str = "sk-spool-http";
/// 端口配额：用例正文都很小，8 MiB 足够且不依赖宿主机可用空间。
const QUOTA_BYTES: u64 = 8 * 1024 * 1024;
/// 正文驻留内存的阈值：远大于用例正文、且不超默认内存预算，走内存正文路径。
const MEMORY_RESIDENT_BYTES: usize = 1024 * 1024;
/// 正文落盘阈值：1 字节即越界，强制正文以磁盘对象供给。
const FILE_BACKED_BYTES: usize = 1;

// ─── 事件 fixture ───────────────────────────────────────────────────

fn trace_event(id: &str) -> IngestionEvent {
    IngestionEvent::TraceCreate {
        id: id.into(),
        timestamp: "2026-01-01T00:00:00Z".into(),
        body: TraceBody {
            id: Some(format!("trace-{id}")),
            name: Some(id.into()),
            ..Default::default()
        },
        metadata: None,
    }
}

fn generation_event(id: &str) -> IngestionEvent {
    IngestionEvent::GenerationCreate {
        id: id.into(),
        timestamp: "2026-01-01T00:00:00Z".into(),
        body: GenerationBody {
            id: Some(format!("gen-{id}")),
            trace_id: Some("019a0000-0000-7000-8000-000000000001".into()),
            name: Some(id.into()),
            start_time: Some("2026-01-01T00:00:00Z".into()),
            end_time: Some("2026-01-01T00:00:01Z".into()),
            input: Some(serde_json::json!({"messages": ["payload"]})),
            output: Some(serde_json::json!("done")),
            ..Default::default()
        },
        metadata: None,
    }
}

/// 契约用例要同时覆盖内存与文件正文：spool 存在且阈值极小即强制走磁盘正文。
fn events() -> Vec<IngestionEvent> {
    vec![trace_event("http-1"), generation_event("http-2")]
}

// ─── 本地 HTTP fixture ──────────────────────────────────────────────

/// 一条请求记录：头部原文（保留大小写，便于逐字节断言）与按 `Content-Length` 读到的正文。
#[derive(Clone)]
struct Request {
    head: String,
    body: Vec<u8>,
}

/// 按连接序号取用的服务端脚本；序号用尽后默认「读满回 200」。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Policy {
    /// 读到正文前缀立刻断开（客户端按可恢复传输错误重试）。
    DisconnectMidBody,
    /// 读满正文后回 200。
    ReadAndOk,
    /// 读满正文后停在响应之前，等用例放行；期间正文仍以文件形式留在 spool 上。
    StallUntilReleased,
}

const RESPONSE_OK: &[u8] = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
    content-length: 2\r\nconnection: close\r\n\r\n{}";

struct Server {
    url: String,
    requests: Arc<Mutex<Vec<Request>>>,
    gate: Arc<tokio::sync::Semaphore>,
    join: tokio::task::JoinHandle<()>,
}

impl Server {
    fn start(policies: Vec<Policy>) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind local server");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let url = format!("http://{}", listener.local_addr().expect("local addr"));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let recorded = Arc::clone(&requests);
        let released = Arc::clone(&gate);
        let join = tokio::spawn(async move {
            let listener = tokio::net::TcpListener::from_std(listener).expect("adopt listener");
            let mut index = 0usize;
            while let Ok((stream, _)) = listener.accept().await {
                let policy = policies.get(index).copied().unwrap_or(Policy::ReadAndOk);
                index += 1;
                let recorded = Arc::clone(&recorded);
                let released = Arc::clone(&released);
                tokio::spawn(async move {
                    serve(stream, policy, &recorded, &released).await;
                });
            }
        });
        Self {
            url,
            requests,
            gate,
            join,
        }
    }

    /// 放行一条被 `StallUntilReleased` 停住的响应。
    fn release(&self) {
        self.gate.add_permits(1);
    }

    fn records(&self) -> Vec<Request> {
        self.requests.lock().expect("request records").clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.join.abort();
    }
}

async fn serve(
    mut stream: tokio::net::TcpStream,
    policy: Policy,
    recorded: &Mutex<Vec<Request>>,
    gate: &tokio::sync::Semaphore,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buffer = Vec::new();
    let head_end = loop {
        if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break end;
        }
        let mut chunk = [0u8; 8192];
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut body = buffer[head_end + 4..].to_vec();
    let length = header(&head, "content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .expect("每个 OTLP 请求都必须带 Content-Length");
    assert!(length > 4, "用例正文必须足够长以观察中途断连");
    match policy {
        Policy::DisconnectMidBody => {
            // 读到正文中途（一半）就关闭连接：既读满正文，也不回响应。
            let prefix = (length / 2).max(1);
            while body.len() < prefix {
                let mut chunk = [0u8; 8192];
                match stream.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(read) => body.extend_from_slice(&chunk[..read]),
                }
            }
            body.truncate(prefix);
            recorded
                .lock()
                .expect("request records")
                .push(Request { head, body });
            drop(stream);
        }
        Policy::ReadAndOk | Policy::StallUntilReleased => {
            while body.len() < length {
                let mut chunk = [0u8; 8192];
                match stream.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(read) => body.extend_from_slice(&chunk[..read]),
                }
            }
            body.truncate(length);
            recorded
                .lock()
                .expect("request records")
                .push(Request { head, body });
            if policy == Policy::StallUntilReleased {
                let _permit = gate.acquire().await;
            }
            let _ = stream.write_all(RESPONSE_OK).await;
            let _ = stream.flush().await;
            let _ = stream.shutdown().await;
        }
    }
}

/// 头部逐字查找（HTTP 头名大小写不敏感）。
fn header(head: &str, name: &str) -> Option<String> {
    head.lines().skip(1).find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_string())
    })
}

fn assert_content_length(request: &Request, expected: usize) {
    assert_eq!(
        header(&request.head, "content-length"),
        Some(expected.to_string()),
        "Content-Length 必须等于真实线包长度"
    );
}

fn assert_no_transfer_encoding(request: &Request) {
    assert_eq!(
        header(&request.head, "transfer-encoding"),
        None,
        "正文必须按 Content-Length 直送，不允许 chunked 编码"
    );
}

// ─── 正文解码与摘要 ─────────────────────────────────────────────────

fn gunzip(bytes: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .read_to_end(&mut decoded)
        .expect("gzip body");
    decoded
}

fn otlp_json(body: &[u8]) -> serde_json::Value {
    let raw = if body.starts_with(&[0x1f, 0x8b]) {
        gunzip(body)
    } else {
        body.to_vec()
    };
    serde_json::from_slice(&raw).expect("OTLP JSON")
}

/// OTLP 观测名，按到达顺序展开。
fn span_names(body: &[u8]) -> Vec<String> {
    otlp_json(body)["resourceSpans"][0]["scopeSpans"][0]["spans"]
        .as_array()
        .expect("spans")
        .iter()
        .map(|span| span["name"].as_str().expect("span name").to_string())
        .collect()
}

/// OTLP 观测 ID（traceId/spanId），用于证明重试没有换掉正文身份。
fn observation_ids(body: &[u8], key: &str) -> Vec<String> {
    otlp_json(body)["resourceSpans"][0]["scopeSpans"][0]["spans"]
        .as_array()
        .expect("spans")
        .iter()
        .map(|span| span[key].as_str().expect("observation id").to_string())
        .collect()
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

// ─── Batcher / spool 装配 ───────────────────────────────────────────

/// 导出编码必须与端口配置一致，否则 Batcher 拒绝装配。
fn telemetry(threshold: usize, compression: Compression) -> TelemetryConfig {
    TelemetryConfig {
        payload_memory_threshold_bytes: threshold,
        compression,
        ..Default::default()
    }
}

fn batcher_config(telemetry: TelemetryConfig, max_events: usize) -> BatcherConfig {
    BatcherConfig {
        max_events,
        queue_capacity: 8,
        max_in_flight: 2,
        flush_interval: Duration::from_secs(30),
        telemetry,
        ..Default::default()
    }
}

/// 导出配置：短退避便于观察周期内重试；编码与 wire 上限必须与 telemetry 一致。
fn export(compression: Compression) -> ExportConfig {
    ExportConfig {
        compression,
        initial_retry_delay: Duration::from_millis(5),
        max_retry_delay: Duration::from_millis(20),
        ..Default::default()
    }
}

fn client(url: &str, compression: Compression) -> LangfuseClient {
    LangfuseClient::new(PK, SK, url, 3)
        .with_export_config(export(compression))
        .expect("export config")
}

fn route(url: &str) -> RouteFingerprint {
    route_fingerprint(url, PK, SK).expect("route fingerprint")
}

fn spool_at(root: &Path, url: &str) -> Arc<dyn SpoolStore> {
    Arc::new(
        NativeSpool::open_at(root.to_path_buf(), route(url), QUOTA_BYTES).expect("native spool"),
    )
}

/// 递归收集名为 `name` 的文件；用于「线包正文 == 磁盘 spool 正文」这类真实字节断言。
fn collect_files(root: &Path, name: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, name, out);
        } else if path.file_name().and_then(|value| value.to_str()) == Some(name) {
            out.push(path);
        }
    }
}

async fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("等待超时: {what}"));
}

// ─── 用例 ───────────────────────────────────────────────────────────

/// 契约 §8 P2 行：首轮读到正文中途断连，第二轮必须从字节 0 重开同一份正文。
///
/// 内存正文与文件正文各跑一遍：两轮长度都等于 wire 长度，第二轮以首轮已读前缀开头，并且与
/// 健康发送的对照正文逐字节（含摘要与观测 ID）相等。
#[tokio::test]
async fn retry_reopens_at_zero_after_mid_body_disconnect() {
    let events = events();
    let control = {
        let server = Server::start(vec![Policy::ReadAndOk]);
        let batcher = Batcher::try_new(
            client(&server.url, Compression::Gzip),
            batcher_config(
                telemetry(MEMORY_RESIDENT_BYTES, Compression::Gzip),
                events.len(),
            ),
            None,
        )
        .expect("memory batcher");
        for event in events.clone() {
            batcher.try_add(event).expect("admit");
        }
        batcher.flush().await.expect("healthy flush");
        batcher.shutdown().await.expect("healthy shutdown");
        let records = server.records();
        assert_eq!(records.len(), 1, "健康对照必须是单包一次请求");
        records[0].body.clone()
    };

    for file_backed in [false, true] {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let server = Server::start(vec![Policy::DisconnectMidBody, Policy::ReadAndOk]);
        let threshold = if file_backed {
            FILE_BACKED_BYTES
        } else {
            MEMORY_RESIDENT_BYTES
        };
        let spool = file_backed.then(|| spool_at(dir.path(), &server.url));
        let batcher = Batcher::try_new(
            client(&server.url, Compression::Gzip),
            batcher_config(telemetry(threshold, Compression::Gzip), events.len()),
            spool.clone(),
        )
        .expect("batcher");
        for event in events.clone() {
            batcher.try_add(event).expect("admit");
        }
        batcher.flush().await.expect("flush");
        batcher.shutdown().await.expect("shutdown");

        let records = server.records();
        assert_eq!(records.len(), 2, "中途断连后必须重试【file={file_backed}】");
        let (first, retry) = (&records[0], &records[1]);
        assert_content_length(first, control.len());
        assert_content_length(retry, control.len());
        assert_no_transfer_encoding(retry);
        assert!(
            first.body.len() > 0 && first.body.len() < control.len(),
            "首轮必须只读到正文中途【file={file_backed}】"
        );
        assert_eq!(
            retry.body[..first.body.len()],
            first.body[..],
            "第二次必须从字节 0 重发，而不是从断点续传【file={file_backed}】"
        );
        assert_eq!(
            retry.body, control,
            "重试正文必须与健康发送逐字节相等【file={file_backed}】"
        );
        assert_eq!(sha256(&retry.body), sha256(&control));
        let ids = observation_ids(&retry.body, "traceId");
        assert_eq!(ids, observation_ids(&control, "traceId"));
        assert!(
            ids.iter().any(|id| id.bytes().any(|byte| byte != b'0')),
            "观测 ID 不得退化为全零"
        );
        // 文件正文来源：交付后正文对象由清理协议删除，磁盘不残留已交付正文。
        if let Some(spool) = spool {
            assert!(
                spool.list(None, 64).expect("list").entries.is_empty(),
                "已交付对象必须被清理"
            );
        }
    }
}

/// 契约 §8 P2 行：gzip 与 none 两模式的编码头与长度都必须与真实线包一致，且解码后 OTLP 语义相同。
#[tokio::test]
async fn gzip_and_none_requests_match_encoding_and_length() {
    let events = events();
    let mut bodies = Vec::new();
    for compression in [Compression::Gzip, Compression::None] {
        let dir = tempfile::TempDir::new().expect("temp dir");
        // 阈值 1 强制文件正文：两种模式都从磁盘对象直送。
        let server = Server::start(vec![Policy::ReadAndOk]);
        let spool = spool_at(dir.path(), &server.url);
        let batcher = Batcher::try_new(
            client(&server.url, compression),
            batcher_config(telemetry(FILE_BACKED_BYTES, compression), events.len()),
            Some(spool),
        )
        .expect("batcher");
        for event in events.clone() {
            batcher.try_add(event).expect("admit");
        }
        batcher.flush().await.expect("flush");
        batcher.shutdown().await.expect("shutdown");

        let records = server.records();
        assert_eq!(records.len(), 1, "单包一次请求：{compression:?}");
        let request = &records[0];
        assert_content_length(request, request.body.len());
        assert_no_transfer_encoding(request);
        match compression {
            Compression::Gzip => {
                assert_eq!(
                    header(&request.head, "content-encoding").as_deref(),
                    Some("gzip"),
                    "gzip 模式必须声明 Content-Encoding: gzip"
                );
                assert_eq!(&request.body[..2], &[0x1f, 0x8b], "gzip magic");
            }
            Compression::None => {
                assert_eq!(
                    header(&request.head, "content-encoding"),
                    None,
                    "none 模式不得声明内容编码"
                );
                assert!(serde_json::from_slice::<serde_json::Value>(&request.body).is_ok());
            }
        }
        bodies.push(request.body.clone());
    }
    // 压缩只改变字节与 header，不改变解压后的 OTLP 正文：逐字节相等。
    assert_eq!(
        gunzip(&bodies[0]),
        bodies[1],
        "gzip 解压后必须与 none 模式逐字节相等"
    );
    assert_eq!(span_names(&bodies[0]), ["http-1", "http-2"]);
    assert_eq!(span_names(&bodies[1]), ["http-1", "http-2"]);
}

/// 契约 §8 P2 行：文件正文由磁盘上的 spool 对象逐字节供给，且请求不使用 chunked 编码。
///
/// 服务端在读完正文后停在响应之前，用例期间磁盘上仍持有该 Export 对象：把线包正文与磁盘
/// 文件逐字节对比，并断言 `Content-Length` 等于文件长度、没有 `Transfer-Encoding`。
#[tokio::test]
async fn file_body_is_served_from_spool_without_transfer_encoding() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let events = events();
    let server = Server::start(vec![Policy::StallUntilReleased]);
    let spool = spool_at(dir.path(), &server.url);
    let batcher = Batcher::try_new(
        client(&server.url, Compression::Gzip),
        batcher_config(
            telemetry(FILE_BACKED_BYTES, Compression::Gzip),
            events.len(),
        ),
        Some(Arc::clone(&spool)),
    )
    .expect("batcher");
    for event in events {
        batcher.try_add(event).expect("admit");
    }

    // 请求在途（响应被扣住）时观察磁盘：这正是文件正文被读泵逐块读出前的持久形态。
    wait_for("在途请求与磁盘正文", || {
        !server.records().is_empty()
    })
    .await;
    let request = server.records().remove(0);
    assert_content_length(&request, request.body.len());
    assert_no_transfer_encoding(&request);
    assert_eq!(span_names(&request.body), ["http-1", "http-2"]);

    let mut bodies = Vec::new();
    collect_files(dir.path(), "body", &mut bodies);
    assert!(!bodies.is_empty(), "文件正文必须先原子发布到磁盘再发送");
    let on_disk = std::fs::read(&bodies[0]).expect("read spool body");
    assert_eq!(
        on_disk, request.body,
        "线包正文必须与磁盘上的 spool 正文逐字节相同"
    );
    assert_eq!(on_disk.len(), request.body.len());

    server.release();
    batcher.flush().await.expect("flush");
    batcher.shutdown().await.expect("shutdown");
    assert_eq!(server.records().len(), 1, "交付后不得重复发送");
    assert!(
        spool.list(None, 64).expect("list").entries.is_empty(),
        "交付后磁盘对象必须被清理"
    );
    let mut residue = Vec::new();
    collect_files(dir.path(), "body", &mut residue);
    assert!(residue.is_empty(), "交付后不得残留正文文件");
}
