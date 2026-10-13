#![cfg(all(not(target_arch = "wasm32"), not(target_os = "emscripten")))]

//! P5 验收：编码膨胀拆批不丢事件、超大单条精确拒收、超阈值冻结输入自动落盘（契约 §8 P5、§9）。
//!
//! 三件事都落在真实字节与真实状态上：
//! 1. 每个请求的正文按 `Content-Length` 真读、按 magic 真解压，断言用解压后的**精确**字节
//!    长度与 OTLP JSON 语义，不用「只判 200」代替；
//! 2. 编码膨胀的限额由**真实线包**校准（集成测试不可用 crate 内部计量器），被拒事件必须既不在
//!    任何请求里、也不让邻条失败；
//! 3. 落盘用例在请求到达（回响应之前）拍下暂存根与端口可见对象，线上 attr 与盘上 Input 对象
//!    正文逐字节比对，并核对低于阈值的 input 没有落盘。
//!
//! 服务端是本文件内的本地 TCP fixture：不依赖公网、不依赖真实 Langfuse，也不使用
//! `TestStore` 之类的进程内双；全部断言只经公开 API（`Batcher`、`SpoolStore` 与磁盘观察）。

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use langfuse_client::client::ExportConfig;
use langfuse_client::config::{BatcherConfig, Compression, TelemetryConfig};
use langfuse_client::snapshot::{snapshot_value, SnapshotSource};
use langfuse_client::spool::native::NativeSpool;
use langfuse_client::spool::{route_fingerprint, ObjectKind, ObjectMeta, SpoolState, SpoolStore};
use langfuse_client::types::{IngestionEvent, SpanBody, TraceBody};
use langfuse_client::{Batcher, LangfuseClient, LangfuseError, ShutdownReport, TelemetryEvent};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const PK: &str = "pk-spool-acceptance";
const SK: &str = "sk-spool-acceptance";
/// 端口配额：用例正文远小于它，且不依赖宿主机可用空间。
const QUOTA_BYTES: u64 = 16 * 1024 * 1024;
/// 契约 §9 的显式 512 KiB 限额：批原文上限与导出正文（解压后 JSON）上限各取同值或收紧其一。
const LIMIT_512_KIB: usize = 512 * 1024;
/// 拆批用例的单条载荷：两条可成批、三条必然越过任一层限额。
const EVENT_PAYLOAD_BYTES: usize = 200 * 1024;

// ─── 事件与来源 fixture ─────────────────────────────────────────────

/// 确定性伪随机 base36 片段：可复现，且不会被 gzip 压回内存前缀以下。
fn noise(len: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ALPHABET[(state >> 33) as usize % ALPHABET.len()] as char
        })
        .collect()
}

fn trace_event(id: &str) -> IngestionEvent {
    IngestionEvent::TraceCreate {
        id: id.into(),
        timestamp: "2026-01-01T00:00:00Z".into(),
        body: TraceBody {
            id: Some(id.into()),
            name: Some(id.into()),
            ..Default::default()
        },
        metadata: None,
    }
}

fn span_event(id: &str, trace_id: &str) -> IngestionEvent {
    IngestionEvent::SpanCreate {
        id: id.into(),
        timestamp: "2026-01-01T00:00:00Z".into(),
        body: SpanBody {
            id: Some(id.into()),
            trace_id: Some(trace_id.into()),
            name: Some(id.into()),
            ..Default::default()
        },
        metadata: None,
    }
}

/// 带载荷的事件：`input` 是 `payload_len` 字节的可复现文本，用于构造超限与编码膨胀。
fn event_with_payload(id: &str, payload_len: usize) -> IngestionEvent {
    IngestionEvent::TraceCreate {
        id: id.into(),
        timestamp: "2026-01-01T00:00:00Z".into(),
        body: TraceBody {
            id: Some(id.into()),
            name: Some(id.into()),
            input: Some(Value::String(noise(payload_len))),
            ..Default::default()
        },
        metadata: None,
    }
}

/// 准入计量的原文口径：事件紧凑 JSON 的字节数（与 `event_bytes` 同一序列化口径）。
fn raw_measure(event: &IngestionEvent) -> usize {
    serde_json::to_vec(event).expect("event json").len()
}

/// 直接写出冻结文本的来源：`SnapshotSource` 的公开实现路径（不依赖 crate 内部适配器）。
struct TextSource(String);

impl SnapshotSource for TextSource {
    fn retained_bytes(&self) -> Result<usize, LangfuseError> {
        Ok(self.0.len())
    }

    fn write_json(&self, out: &mut dyn std::io::Write) -> Result<(), LangfuseError> {
        out.write_all(self.0.as_bytes())
            .map_err(|error| LangfuseError::Config(format!("source write failed: {error}")))
    }
}

// ─── 构造 ───────────────────────────────────────────────────────────

fn base_config() -> BatcherConfig {
    BatcherConfig {
        max_events: 8,
        queue_capacity: 8,
        max_in_flight: 1,
        flush_interval: Duration::from_secs(60),
        ..Default::default()
    }
}

/// 导出配置与遥测配置独立构造：内容编码与 wire 上限必须一致，正文限额单独给。
fn batcher_with(
    url: &str,
    config: BatcherConfig,
    export: ExportConfig,
    spool: Option<Arc<dyn SpoolStore>>,
) -> Batcher {
    let client = LangfuseClient::new(PK, SK, url, 0)
        .with_export_config(export)
        .expect("export config");
    Batcher::try_new(client, config, spool).expect("batcher")
}

/// 真实临时根上的 native 暂存；route 必须与同一凭据/端点派生的导出 route 一致。
fn native_spool(root: &Path, url: &str) -> Arc<dyn SpoolStore> {
    let route = route_fingerprint(url, PK, SK).expect("route fingerprint");
    Arc::new(NativeSpool::open_at(root.to_path_buf(), route, QUOTA_BYTES).expect("native spool"))
}

// ─── 本地 OTLP 服务端 fixture ───────────────────────────────────────

const RESPONSE_OK: &[u8] = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
    content-length: 2\r\nconnection: close\r\n\r\n{}";

/// 绑定本地端口：URL 先于暂存根确定（暂存 route 由 URL 与凭据派生，两者必须同源）。
fn bind() -> (std::net::TcpListener, String) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind local collector");
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    let url = format!("http://{}", listener.local_addr().expect("local addr"));
    (listener, url)
}

/// 一次请求的可见事实：解压后正文的精确字节长度与解析出的 OTLP JSON。
struct Request {
    decoded_len: usize,
    body: Value,
}

impl Request {
    /// 正文文本视图；用于断言被拒事件不以截断或伪造形式出现在任何请求里。
    fn text(&self) -> String {
        self.body.to_string()
    }
}

/// 请求到达时刻的盘上证据：暂存根下普通文件正文、端口可见对象元数据与 Input 对象正文。
#[derive(Default)]
struct DiskShot {
    files: Vec<Vec<u8>>,
    objects: Vec<ObjectMeta>,
    inputs: Vec<Vec<u8>>,
}

/// 取证用的只读暂存视图：只 `list`/`open`，不领取、不写入、不删除。
#[derive(Clone)]
struct Probe {
    root: PathBuf,
    store: Arc<dyn SpoolStore>,
}

struct Collected {
    requests: Vec<Request>,
    disk: DiskShot,
}

struct Collector {
    requests: Arc<Mutex<Vec<Request>>>,
    disk: Arc<Mutex<DiskShot>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    worker: tokio::task::JoinHandle<()>,
}

impl Collector {
    /// 启动本地收集器（在 tokio 运行期内调用）；`probe` 存在时每次请求到达都拍下暂存证据。
    fn start(listener: std::net::TcpListener, probe: Option<Probe>) -> Self {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let disk = Arc::new(Mutex::new(DiskShot::default()));
        let recorded = Arc::clone(&requests);
        let shot = Arc::clone(&disk);
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn(async move {
            let listener = tokio::net::TcpListener::from_std(listener).expect("adopt listener");
            loop {
                let accepted = tokio::select! {
                    _ = &mut stopped => break,
                    accepted = listener.accept() => accepted,
                };
                let Ok((stream, _)) = accepted else { break };
                let recorded = Arc::clone(&recorded);
                let shot = Arc::clone(&shot);
                let probe = probe.clone();
                tokio::spawn(async move {
                    serve(stream, probe, &recorded, &shot).await;
                });
            }
        });
        Self {
            requests,
            disk,
            stop: Some(stop),
            worker,
        }
    }

    async fn finish(mut self) -> Collected {
        self.stop
            .take()
            .expect("stop handle")
            .send(())
            .expect("stop collector");
        self.worker.await.expect("collector join");
        Collected {
            requests: std::mem::take(&mut *self.requests.lock().expect("requests")),
            disk: std::mem::take(&mut *self.disk.lock().expect("disk shot")),
        }
    }
}

/// 读取一次 OTLP 请求：按 `Content-Length` 读满正文 → 取证 → 解码 → 回 200。
async fn serve(
    stream: tokio::net::TcpStream,
    probe: Option<Probe>,
    requests: &Mutex<Vec<Request>>,
    disk: &Mutex<DiskShot>,
) {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.expect("request line");
    assert!(
        line.starts_with("POST /api/public/otel/v1/traces "),
        "unexpected request line: {line}"
    );
    let mut length = 0usize;
    loop {
        line.clear();
        assert_ne!(reader.read_line(&mut line).await.expect("header line"), 0);
        if line == "\r\n" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().expect("content length");
            }
        }
    }
    assert!(length > 0, "导出请求必须带非空正文");
    let mut wire = vec![0u8; length];
    reader.read_exact(&mut wire).await.expect("request body");

    // 请求到达（回响应之前）：此刻 wire 与盘上对象都在原位，取证与请求一一对应。
    if let Some(probe) = &probe {
        shoot(probe, &mut disk.lock().expect("disk shot"));
    }
    let (bytes, body) = decode(&wire);
    requests.lock().expect("requests").push(Request {
        decoded_len: bytes.len(),
        body,
    });

    reader
        .get_mut()
        .write_all(RESPONSE_OK)
        .await
        .expect("response");
    reader.get_mut().shutdown().await.expect("close stream");
}

/// 取证：暂存根下普通文件正文 + 端口可见对象（Input 对象正文经公开读句柄读到底）。
fn shoot(probe: &Probe, shot: &mut DiskShot) {
    shot.files.extend(read_tree(&probe.root));
    let mut cursor = None;
    loop {
        let page = probe.store.list(cursor, 64).expect("spool page");
        for meta in &page.entries {
            shot.objects.push(meta.clone());
            if meta.kind == ObjectKind::Input {
                let mut body = Vec::new();
                probe
                    .store
                    .open(meta.id)
                    .expect("input body reader")
                    .read_to_end(&mut body)
                    .expect("input body bytes");
                shot.inputs.push(body);
            }
        }
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
}

/// 递归收集暂存根下所有非空普通文件的正文（描述符、锁文件与对象正文都在内）。
fn read_tree(dir: &Path) -> Vec<Vec<u8>> {
    let mut bodies = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return bodies;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            bodies.extend(read_tree(&path));
        } else if let Ok(bytes) = std::fs::read(&path) {
            if !bytes.is_empty() {
                bodies.push(bytes);
            }
        }
    }
    bodies
}

/// 线包按 magic 判定内容编码；返回解压后字节与解析出的 OTLP JSON。
fn decode(wire: &[u8]) -> (Vec<u8>, Value) {
    let bytes = if wire.starts_with(&[0x1f, 0x8b]) {
        let mut inflated = Vec::new();
        flate2::read::GzDecoder::new(wire)
            .read_to_end(&mut inflated)
            .expect("gzip body");
        inflated
    } else {
        wire.to_vec()
    };
    let body = serde_json::from_slice(&bytes).expect("OTLP JSON body");
    (bytes, body)
}

// ─── 断言辅助 ───────────────────────────────────────────────────────

fn spans(requests: &[Request]) -> Vec<Value> {
    requests
        .iter()
        .flat_map(|request| {
            request.body["resourceSpans"][0]["scopeSpans"][0]["spans"]
                .as_array()
                .expect("spans array")
                .clone()
        })
        .collect()
}

fn span_names(requests: &[Request]) -> Vec<String> {
    spans(requests)
        .iter()
        .map(|span| span["name"].as_str().expect("span name").to_string())
        .collect()
}

/// OTLP 字符串属性取值；冻结来源一律以 `stringValue` 承载原文。
fn attr(span: &Value, key: &str) -> Option<String> {
    span["attributes"]
        .as_array()?
        .iter()
        .find(|attribute| attribute["key"] == key)?["value"]["stringValue"]
        .as_str()
        .map(str::to_owned)
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

// ─── 用例 ───────────────────────────────────────────────────────────

/// 契约 §9「编码膨胀」：事件原文都在限额内、最终 OTLP 正文超限时按事件边界拆批。
///
/// 实验分两层限额跑同一个用例体：`raw_batch_limit` 为批原文聚合上限，`encoded_limit` 为导出
/// 正文（解压后 JSON）上限。两层都必须满足「每个请求合法 + 三条事件一条不丢」；第二轮把原文
/// 聚合上限放大到远超三条载荷，拆批只能由编码维度解释。
async fn splits_without_loss(raw_batch_limit: usize, encoded_limit: usize) {
    let ids = ["expand-1", "expand-2", "expand-3"];
    let (listener, url) = bind();
    let collector = Collector::start(listener, None);
    let config = BatcherConfig {
        max_event_bytes: encoded_limit,
        max_batch_bytes: raw_batch_limit,
        // 原文聚合 600 KiB：队列按原文记账，限额必须容得下全部待发事件（否则按策略拒收/驱逐）。
        max_queue_bytes: LIMIT_512_KIB * 2,
        ..base_config()
    };
    let export = ExportConfig {
        max_request_bytes: encoded_limit,
        ..Default::default()
    };
    let batcher = batcher_with(&url, config, export, None);

    for id in ids {
        batcher
            .try_add(event_with_payload(id, EVENT_PAYLOAD_BYTES))
            .expect("每条都在单条原文限额内");
    }
    assert_eq!(
        batcher.shutdown().await.expect("shutdown"),
        ShutdownReport::Complete
    );

    let collected = collector.finish().await;
    assert!(
        collected.requests.len() >= 2,
        "三条事件的最终正文无法装进一个请求，必须拆批（实际 {} 个请求）",
        collected.requests.len()
    );
    let encoded_total = collected
        .requests
        .iter()
        .map(|request| request.decoded_len)
        .sum::<usize>();
    for request in &collected.requests {
        assert!(
            request.decoded_len <= encoded_limit,
            "每个请求的解压正文（{} 字节）必须守在导出限额 {encoded_limit} 内",
            request.decoded_len
        );
    }
    assert!(
        encoded_total > encoded_limit,
        "三个请求合计 {encoded_total} 字节必须超过单请求上限 {encoded_limit}，否则拆批无从解释"
    );
    assert!(
        collected
            .requests
            .iter()
            .any(|request| spans(std::slice::from_ref(request)).len() > 1),
        "拆批必须按事件边界成批，不得退化成一条事件一个请求"
    );
    assert_eq!(
        span_names(&collected.requests),
        ids,
        "拆批不丢事件、顺序不变、无重复"
    );
    let stats = batcher.stats();
    assert_eq!(stats.failed_batches, 0, "拆批后的批次都不得记失败");
    assert_eq!(stats.rejected_events, 0, "限额内的正常拆批不产生拒收");
    assert_eq!(stats.partially_rejected_batches, 0);
    assert_eq!(
        stats.completed_batches,
        collected.requests.len(),
        "每个请求都必须对应一个完成批次"
    );
}

/// 原文限额与编码限额同值（512 KiB）：两个维度都逼出拆批。
#[tokio::test]
async fn encoded_expansion_splits_into_legal_requests_without_loss() {
    splits_without_loss(LIMIT_512_KIB, LIMIT_512_KIB).await;
}

/// 只收紧编码限额：原文聚合（3 × 200 KiB）远低于批原文上限，拆批只能由编码膨胀解释。
#[tokio::test]
async fn encoding_dimension_alone_forces_the_split() {
    let raw_batch_limit = LIMIT_512_KIB * 8;
    assert!(
        3 * EVENT_PAYLOAD_BYTES <= raw_batch_limit,
        "三条载荷的原文合计必须能被单个批原文限额装下"
    );
    splits_without_loss(raw_batch_limit, LIMIT_512_KIB).await;
}

/// 单条事件的最终编码字节数只能从真实线包观测：集成测试不可用 crate 内部计量器，
/// 因此用一次宽松限额的真实上线做校准（同 id、同载荷 ⇒ 同一份编码，可复现）。
async fn calibrated_encoded_len(id: &str, payload_len: usize) -> usize {
    let (listener, url) = bind();
    let collector = Collector::start(listener, None);
    let batcher = batcher_with(&url, base_config(), ExportConfig::default(), None);
    batcher
        .try_add(event_with_payload(id, payload_len))
        .expect("校准事件准入");
    assert_eq!(
        batcher.shutdown().await.expect("shutdown"),
        ShutdownReport::Complete
    );

    let collected = collector.finish().await;
    assert_eq!(collected.requests.len(), 1, "校准必须恰好一个请求");
    assert_eq!(span_names(&collected.requests), [id]);
    collected.requests[0].decoded_len
}

/// 契约 §9「超大单条」：超限事件只拒自身，邻条照常送达，不截断、不伪造、不记批次失败。
///
/// 编码维度是单条不可拆的硬约束：导出正文限额取「该条最终编码 - 1」（由真实线包校准，覆盖其
/// 原文长度），因此该条只能在本地被精确拒收，而不是发出后由服务端失败；原文超限则在准入即
/// 拒收，连队列都不进入。
#[tokio::test]
async fn oversized_single_event_is_rejected_exactly_without_affecting_its_batch() {
    const PAYLOAD: usize = 8 * 1024;
    let measured = calibrated_encoded_len("too-large", PAYLOAD).await;
    assert!(
        measured > PAYLOAD,
        "编码后正文（{measured} 字节）必须包含比载荷更多的信封与转义字节"
    );
    let limit = measured - 1;

    let (listener, url) = bind();
    let collector = Collector::start(listener, None);
    let config = BatcherConfig {
        max_event_bytes: limit,
        // 原文层留出余量：该条的准入判据只有编码维度，聚合上限不参与本次拒收。
        max_batch_bytes: limit * 4,
        max_queue_bytes: limit * 4,
        ..base_config()
    };
    let export = ExportConfig {
        max_request_bytes: limit,
        ..Default::default()
    };
    let batcher = batcher_with(&url, config, export, None);

    // 原文超限：`try_add` 同步拒收，限额即单条原文上限。
    let raw_over = event_with_payload("raw-over", limit + 1024);
    assert!(
        raw_measure(&raw_over) > limit,
        "构造样本的原文必须真的超过单条原文限额"
    );
    let error = batcher.try_add(raw_over).expect_err("原文超限必须立即拒收");
    assert!(
        matches!(error, LangfuseError::PayloadTooLarge { limit_bytes } if limit_bytes == limit),
        "原文超限必须报出原文限额而不是别的分类：{error:?}"
    );

    // 编码超限：原文在限额内（准入放行），最终正文超过导出限额。
    let oversized = event_with_payload("too-large", PAYLOAD);
    assert!(
        raw_measure(&oversized) < limit,
        "构造样本的原文必须留在单条原文限额内，被拒原因只能来自编码维度"
    );
    batcher
        .try_add(trace_event("small-before"))
        .expect("邻条准入");
    batcher
        .try_add(oversized)
        .expect("原文在限额内的事件必须通过准入");
    batcher
        .try_add(trace_event("small-after"))
        .expect("邻条准入");
    assert_eq!(
        batcher.shutdown().await.expect("shutdown"),
        ShutdownReport::Complete
    );

    let collected = collector.finish().await;
    assert_eq!(
        span_names(&collected.requests),
        ["small-before", "small-after"],
        "被拒事件不进入任何批次，邻条照常送达且顺序不变"
    );
    for request in &collected.requests {
        let text = request.text();
        assert!(
            !text.contains("too-large"),
            "被拒事件不得以截断或伪造形式上线"
        );
        assert!(!text.contains("raw-over"), "原文超限事件不得进入任何请求");
    }
    let stats = batcher.stats();
    assert_eq!(
        stats.rejected_events, 2,
        "编码膨胀与原文超限各计一次精确拒收"
    );
    assert_eq!(
        stats.rejected_spans, 0,
        "本地精确拒收不是服务端部分拒收，不得计入 rejected_spans"
    );
    assert_eq!(stats.partially_rejected_batches, 0);
    assert_eq!(stats.failed_batches, 0, "本地精确拒收不是批次提交失败");
    assert_eq!(stats.in_flight_batches, 0);
}

/// 契约 §3.3/§8 P5 行：超 `input_spool_threshold_bytes` 的冻结输入自动落到真实暂存对象。
///
/// 阈值是判据而不是「一律落盘」：大输入（`snapshot_value` 的零拷贝公开来源）在请求到达时已作为
/// Input 对象在盘上，且线上 attr 与源字节逐字节相等；小输出（自定义 `SnapshotSource`）留在内存，
/// 不产生任何对象。断言同时用两条独立路径读盘：暂存根扫描与端口读句柄。
#[tokio::test]
async fn oversized_frozen_input_is_spooled_and_arrives_byte_identically() {
    const THRESHOLD: usize = 64 * 1024;
    let root = tempfile::tempdir().expect("temporary spool root");
    let (listener, url) = bind();
    let store = native_spool(root.path(), &url);
    let collector = Collector::start(
        listener,
        Some(Probe {
            root: root.path().to_path_buf(),
            store: Arc::clone(&store),
        }),
    );
    let config = BatcherConfig {
        telemetry: TelemetryConfig {
            input_spool_threshold_bytes: THRESHOLD,
            // 导出正文也跨阈值：请求正文由文件读泵供给，与冻结输入同样经真实磁盘对象。
            payload_memory_threshold_bytes: 1024,
            spool_quota_bytes: QUOTA_BYTES,
            ..Default::default()
        },
        ..base_config()
    };
    let batcher = batcher_with(&url, config, ExportConfig::default(), Some(store));

    // 大输入：借用 `Arc<Value>` 的零拷贝公开来源；期望字节就是该树的紧凑 JSON。
    let value = Arc::new(json!({
        "messages": [{"role": "user", "content": noise(THRESHOLD * 4)}],
        "marker": "spool-marker-3f",
    }));
    let expected_input = serde_json::to_vec(&value).expect("compact json");
    assert!(
        expected_input.len() >= THRESHOLD,
        "样本（{} 字节）必须达到落盘阈值 {THRESHOLD}",
        expected_input.len()
    );
    let input = batcher
        .try_freeze(snapshot_value(&value))
        .expect("freeze ticket");

    // 小输出：自定义公开来源，低于阈值 ⇒ 留在内存。
    let small_output = r#"{"answer":"ok","tokens":7}"#.to_owned();
    assert!(small_output.len() < THRESHOLD, "对照样本必须低于阈值");
    let output = batcher
        .try_freeze(Arc::new(TextSource(small_output.clone())))
        .expect("freeze ticket");

    for event in [
        TelemetryEvent::try_new(
            trace_event("spooled-input"),
            Some(input),
            None,
            batcher.memory_budget(),
        )
        .expect("prepared trace"),
        TelemetryEvent::try_new(
            span_event("inline-output", "spooled-input"),
            None,
            Some(output),
            batcher.memory_budget(),
        )
        .expect("prepared span"),
    ] {
        batcher.try_add_prepared(event).expect("admitted");
    }
    assert_eq!(
        batcher.shutdown().await.expect("shutdown"),
        ShutdownReport::Complete
    );

    let collected = collector.finish().await;
    assert_eq!(
        span_names(&collected.requests),
        ["spooled-input", "inline-output"],
        "两个事件必须在同一批里送达"
    );
    let spans = spans(&collected.requests);
    let expected_input_text = std::str::from_utf8(&expected_input).expect("utf-8 input");
    assert_eq!(
        attr(&spans[0], "langfuse.trace.input").as_deref(),
        Some(expected_input_text),
        "落盘后的冻结输入必须逐字节送达"
    );
    assert_eq!(
        attr(&spans[1], "langfuse.observation.output").as_deref(),
        Some(small_output.as_str()),
        "内存来源的冻结输出必须逐字节送达"
    );

    assert!(
        collected
            .disk
            .files
            .iter()
            .any(|body| body.as_slice() == expected_input.as_slice()),
        "请求到达时冻结输入必须已作为对象正文落盘（拍下 {} 份盘上文件正文）",
        collected.disk.files.len()
    );
    let inputs: Vec<&ObjectMeta> = collected
        .disk
        .objects
        .iter()
        .filter(|meta| meta.kind == ObjectKind::Input)
        .collect();
    assert_eq!(inputs.len(), 1, "只有超过阈值的大输入落盘");
    assert_eq!(
        inputs[0].state,
        SpoolState::Ready,
        "落盘输入必须是已发布对象"
    );
    assert_eq!(inputs[0].raw_len, expected_input.len() as u64);
    assert_eq!(
        inputs[0].encoding,
        Compression::None,
        "冻结快照按原文存储，不加内容编码"
    );
    assert_eq!(
        inputs[0].sha256,
        sha256(&expected_input),
        "对象摘要必须等于冻结字节的 SHA-256"
    );
    assert_eq!(
        collected.disk.inputs,
        vec![expected_input.clone()],
        "经端口读句柄读回的对象正文与源字节逐字节相等"
    );

    let stats = batcher.stats();
    assert_eq!(stats.failed_batches, 0);
    assert_eq!(stats.rejected_events, 0);
    assert_eq!(stats.completed_batches, 1);
}
