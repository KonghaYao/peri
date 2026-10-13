//! 冻结来源真实发送的端到端回归（契约 §1、§3.3、§3.5、§6、§8）。
//!
//! 覆盖三类可观察行为：含冻结 input/output 的批次确实上线（不再被记失败）、超阈值冻结输入
//! 落到真实 native 对象后**逐字节**送达（含导出正文走文件读泵的多块读取）、显式工具参数与
//! 工具结果原文不被改写。断言只看 wire 正文与请求到达时刻的盘上对象，不靠 sleep 判进度。

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use crate::batcher::{Batcher, BatcherConfig};
use crate::config::TelemetryConfig;
use crate::error::LangfuseError;
use crate::snapshot::{InputSource, SnapshotSource, TelemetryEvent};
use crate::spool::native::NativeSpool;
use crate::spool::{route_fingerprint, SpoolStore};
use crate::types::{IngestionEvent, SpanBody, TraceBody};
use crate::LangfuseClient;

const PUBLIC_KEY: &str = "pk-test";
const SECRET_KEY: &str = "sk-test";
const SPOOL_QUOTA_BYTES: u64 = 16 * 1024 * 1024;

// ─── 冻结来源 ───────────────────────────────────────────────────────

/// 直接写出冻结文本的来源；`retained` 按文本长度计量。
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

fn frozen(batcher: &Batcher, text: &str) -> InputSource {
    batcher
        .try_freeze(Arc::new(TextSource(text.to_owned())))
        .expect("freeze ticket")
}

/// 冻结 input/output 并构造已准备事件；票据由 worker 在准备阶段收敛。
fn prepared(
    batcher: &Batcher,
    event: IngestionEvent,
    input: Option<&str>,
    output: Option<&str>,
) -> TelemetryEvent {
    TelemetryEvent::try_new(
        event,
        input.map(|text| frozen(batcher, text)),
        output.map(|text| frozen(batcher, text)),
        batcher.memory_budget(),
    )
    .expect("prepared telemetry event")
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

// ─── 构造 ───────────────────────────────────────────────────────────

fn batcher_with(url: &str, config: BatcherConfig, spool: Option<Arc<dyn SpoolStore>>) -> Batcher {
    let client = LangfuseClient::new(PUBLIC_KEY, SECRET_KEY, url, 0);
    Batcher::try_new(client, config, spool).expect("batcher")
}

fn base_config() -> BatcherConfig {
    BatcherConfig {
        max_events: 8,
        queue_capacity: 8,
        max_in_flight: 1,
        flush_interval: Duration::from_secs(60),
        ..Default::default()
    }
}

/// 冻结输入与导出正文都跨阈值落盘的配置：两条内容路径都必须经真实磁盘对象。
fn spooled_config(threshold: usize) -> BatcherConfig {
    BatcherConfig {
        telemetry: TelemetryConfig {
            input_spool_threshold_bytes: threshold,
            payload_memory_threshold_bytes: 1024,
            spool_quota_bytes: SPOOL_QUOTA_BYTES,
            ..Default::default()
        },
        ..base_config()
    }
}

/// 真实临时根上的 native 暂存；route 必须与同一凭据/端点派生出的导出 route 一致，
/// 否则描述符写入即被端口按身份不符拒绝。
fn native_spool(root: &Path, url: &str) -> Arc<dyn SpoolStore> {
    let route = route_fingerprint(url, PUBLIC_KEY, SECRET_KEY).expect("route");
    Arc::new(NativeSpool::open_at(root.to_path_buf(), route, SPOOL_QUOTA_BYTES).expect("native"))
}

// ─── 本地收集器 ─────────────────────────────────────────────────────

/// 一次运行收集到的结果：wire 正文与请求到达时刻的盘上对象正文。
struct Collected {
    bodies: Vec<Value>,
    disk: Vec<Vec<u8>>,
}

/// 本地 OTLP 收集器：按到达顺序累计正文（gzip 先解压），并在请求到达时拍下暂存根。
struct Collector {
    url: String,
    bodies: Arc<Mutex<Vec<Value>>>,
    disk: Arc<Mutex<Vec<Vec<u8>>>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    worker: tokio::task::JoinHandle<()>,
}

impl Collector {
    async fn start(spool_root: Option<PathBuf>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let disk = Arc::new(Mutex::new(Vec::new()));
        let collected = Arc::clone(&bodies);
        let on_disk = Arc::clone(&disk);
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    _ = &mut stopped => break,
                    accepted = listener.accept() => accepted,
                };
                let Ok((stream, _)) = accepted else { break };
                let collected = Arc::clone(&collected);
                let on_disk = Arc::clone(&on_disk);
                let root = spool_root.clone();
                tokio::spawn(async move {
                    // 正文先入账、后回响应：调用方的 shutdown 返回时收集必然完整。
                    let body = read_otlp_request(stream, move || {
                        if let Some(root) = root {
                            on_disk.lock().unwrap().extend(read_tree(&root));
                        }
                    })
                    .await;
                    collected.lock().unwrap().push(body);
                });
            }
        });
        Self {
            url,
            bodies,
            disk,
            stop: Some(stop),
            worker,
        }
    }

    async fn finish(mut self) -> Collected {
        self.stop.take().unwrap().send(()).unwrap();
        self.worker.await.unwrap();
        Collected {
            bodies: std::mem::take(&mut *self.bodies.lock().unwrap()),
            disk: std::mem::take(&mut *self.disk.lock().unwrap()),
        }
    }
}

/// 读取一次 OTLP 请求；`shoot` 在回响应之前执行（此刻盘上对象与正文都还在原位）。
async fn read_otlp_request(stream: tokio::net::TcpStream, shoot: impl FnOnce()) -> Value {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    assert!(
        line.starts_with("POST /api/public/otel/v1/traces "),
        "unexpected request line: {line}"
    );
    let mut length = 0usize;
    loop {
        line.clear();
        assert_ne!(reader.read_line(&mut line).await.unwrap(), 0);
        if line == "\r\n" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap();
            }
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await.unwrap();
    shoot();
    let decoded = decode_body(&body);
    let response =
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}";
    reader
        .get_mut()
        .write_all(response.as_bytes())
        .await
        .unwrap();
    reader.get_mut().shutdown().await.unwrap();
    decoded
}

/// 正文可能是 gzip（部署默认压缩）或明文：按 magic 判断，两种情况都解出 OTLP JSON。
fn decode_body(body: &[u8]) -> Value {
    let raw = if body.starts_with(&[0x1f, 0x8b]) {
        let mut inflated = Vec::new();
        flate2::read::GzDecoder::new(body)
            .read_to_end(&mut inflated)
            .unwrap();
        inflated
    } else {
        body.to_vec()
    };
    serde_json::from_slice(&raw).unwrap()
}

/// 递归收集暂存根下所有非空普通文件的正文（描述符、临时件与对象正文都在内）。
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

// ─── 断言辅助 ───────────────────────────────────────────────────────

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

fn spans(bodies: &[Value]) -> Vec<Value> {
    bodies
        .iter()
        .flat_map(|body| {
            body["resourceSpans"][0]["scopeSpans"][0]["spans"]
                .as_array()
                .unwrap()
                .clone()
        })
        .collect()
}

fn span_names(bodies: &[Value]) -> Vec<String> {
    spans(bodies)
        .iter()
        .map(|span| span["name"].as_str().unwrap().to_string())
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

// ─── 用例 ───────────────────────────────────────────────────────────

/// 无暂存的部署也必须真的把冻结 input/output 发出去；此前这类批次会被直接记失败。
#[tokio::test]
async fn frozen_input_and_output_reach_the_wire_verbatim() {
    let collector = Collector::start(None).await;
    let batcher = batcher_with(&collector.url, base_config(), None);
    let input = r#"{"messages":[{"role":"user","content":"你好 😀 \"quoted\" \n"}]}"#;
    let output = r#"{"answer":"ok","tokens":7}"#;

    batcher
        .try_add_prepared(prepared(
            &batcher,
            trace_event("frozen-trace"),
            Some(input),
            Some(output),
        ))
        .expect("admitted");
    batcher.shutdown().await.expect("shutdown");

    let collected = collector.finish().await;
    assert_eq!(span_names(&collected.bodies), ["frozen-trace"]);
    let span = &spans(&collected.bodies)[0];
    assert_eq!(
        attr(span, "langfuse.trace.input").as_deref(),
        Some(input),
        "冻结 input 必须逐字节上线"
    );
    assert_eq!(
        attr(span, "langfuse.trace.output").as_deref(),
        Some(output),
        "冻结 output 必须逐字节上线"
    );
    assert_eq!(
        batcher.stats().failed_batches,
        0,
        "冻结来源批次不得被记失败"
    );
    assert_eq!(batcher.stats().completed_batches, 1);
    assert_eq!(batcher.stats().rejected_events, 0);
}

/// 超阈值冻结输入：先落到真实 native 对象，再由文件读泵完整送达（导出正文同样落盘）。
#[tokio::test]
async fn oversized_frozen_input_is_spooled_and_arrives_byte_identically() {
    const THRESHOLD: usize = 64 * 1024;
    let root = tempfile::tempdir().expect("temporary spool root");
    let collector = Collector::start(Some(root.path().to_path_buf())).await;
    let config = spooled_config(THRESHOLD);
    let batcher = batcher_with(
        &collector.url,
        config,
        Some(native_spool(root.path(), &collector.url)),
    );

    // 结构化文本 + 唯一标记：既是合法 JSON，又能被盘上对象逐字节比对。
    let blob = "落盘正文中段".repeat(THRESHOLD / 8);
    let input = format!(r#"{{"blob":"{blob}","marker":"spool-marker-9f"}}"#);
    let output = r#"{"done":true}"#;
    assert!(input.len() > THRESHOLD, "样本必须超过落盘阈值");

    batcher
        .try_add_prepared(prepared(
            &batcher,
            trace_event("spooled-trace"),
            Some(&input),
            Some(output),
        ))
        .expect("admitted");
    batcher.shutdown().await.expect("shutdown");

    let collected = collector.finish().await;
    assert_eq!(span_names(&collected.bodies), ["spooled-trace"]);
    let span = &spans(&collected.bodies)[0];
    assert_eq!(
        attr(span, "langfuse.trace.input").as_deref(),
        Some(input.as_str()),
        "落盘后的冻结输入必须逐字节送达"
    );
    assert_eq!(attr(span, "langfuse.trace.output").as_deref(), Some(output));
    assert!(
        collected
            .disk
            .iter()
            .any(|body| body.as_slice() == input.as_bytes()),
        "请求到达时冻结输入必须已作为对象正文落盘（拍下 {} 份盘上正文）",
        collected.disk.len()
    );
    assert_eq!(batcher.stats().failed_batches, 0);
    assert_eq!(batcher.stats().completed_batches, 1);
}

/// 显式工具参数/结果样本：转义、控制字符与多字节字符经文件读泵后原样到达。
#[tokio::test]
async fn tool_arguments_and_result_text_survive_the_file_pump() {
    let root = tempfile::tempdir().expect("temporary spool root");
    let collector = Collector::start(Some(root.path().to_path_buf())).await;
    // 导出前缀压到 256 字节：即便工具样本很小也必然转盘，正文只能由读泵供给。
    let mut config = spooled_config(64 * 1024);
    config.telemetry.payload_memory_threshold_bytes = 256;
    let batcher = batcher_with(
        &collector.url,
        config,
        Some(native_spool(root.path(), &collector.url)),
    );

    // 工具正文里的伪随机段：避免 gzip 把正文压回内存前缀以下。
    let input = format!(
        concat!(
            r#"{{"tool":"bash","arguments":{{"command":"printf '%s\\n' \"a\\tb\\u0001c\"","#,
            r#""cwd":"/tmp/工作 目录","script":"{}","env":{{"LANG":"zh_CN.UTF-8"}}}}}}"#
        ),
        noise(4_000)
    );
    let output = format!(
        r#"{{"tool_result":"line1\n\"quoted\"\n中文 😀\u0000tail {}","exit_code":0}}"#,
        noise(4_000)
    );

    batcher
        .try_add_prepared(prepared(
            &batcher,
            span_event("tool-span", "frozen-trace"),
            Some(&input),
            Some(&output),
        ))
        .expect("admitted");
    batcher.shutdown().await.expect("shutdown");

    let collected = collector.finish().await;
    assert_eq!(span_names(&collected.bodies), ["tool-span"]);
    let span = &spans(&collected.bodies)[0];
    assert_eq!(
        attr(span, "langfuse.observation.input").as_deref(),
        Some(input.as_str()),
        "工具参数原文不得改写"
    );
    assert_eq!(
        attr(span, "langfuse.observation.output").as_deref(),
        Some(output.as_str()),
        "工具结果原文不得改写"
    );
    assert!(
        collected
            .disk
            .iter()
            .any(|body| body.starts_with(&[0x1f, 0x8b])),
        "请求到达时导出正文必须已作为 gzip 对象落盘（走文件读泵）"
    );
    assert_eq!(batcher.stats().failed_batches, 0);
}
