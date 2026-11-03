//! P3b：控制器侧输入来源与冻结状态的契约测试（契约 §7 P3b 行、§8「P3」行）。
//!
//! 覆盖四类可观察行为：request→end→cancel 顺序在并行 generation 下仍成立、大输入经真实磁盘
//! spool 后结束事件仍拿到逐字节一致的 input、迟到 join 的闸门来源已计费并可在重放后交付、
//! 取消/拒绝路径不遗留快照也不合成新的成功 generation。
//!
//! 另覆盖计量口径（契约 §4「source sizing」）：预留 `capacity` 与真实分配一起入账、计量随真实
//! 分配单调、`to_value` 副本按最大元素计一次、未知上界显式拒收；以及超预算拒收不截断、落盘
//! 来源的 retained 不随原文线性增长。
//!
//! 全部用例使用本地 fixture：真实 `Batcher`（唯一预算与 worker）+ 本地 TCP 捕获端点 +
//! 临时目录里的 `NativeSpool`，不共享 test_helpers、不新增 mockall、不依赖 sleep 判断进度。
//! 输入正文只通过真实链路（落盘对象 / OTLP wire）核对，因为 permit 与冻结字节不可伪造。

use std::collections::HashMap;
use std::future::Future;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use langfuse_client::client::ExportConfig;
use langfuse_client::config::{BatcherConfig, Compression, TelemetryConfig};
use langfuse_client::snapshot::SnapshotSource;
use langfuse_client::spool::native::NativeSpool;
use langfuse_client::spool::{route_fingerprint, SpoolStore};
use langfuse_client::{
    Batcher, FlushReport, IngestionEvent, InputSource, LangfuseClient, LangfuseError, MemoryBudget,
    TelemetryEvent,
};
use parking_lot::Mutex;
use peri_acp_types::command::PromptStopReason;
use peri_acp_types::session::TurnTelemetryOutcome;
use peri_agent::agent::events::Stage;
use peri_agent::messages::BaseMessage;
use peri_agent::tools::ToolDefinition;

use crate::langfuse::config::LangfuseConfig;
use crate::langfuse::drop_telemetry::LangfuseDropRegistry;
use crate::langfuse::session_like::LangfuseSessionLike;
use crate::langfuse::tracer::LangfuseTracer;
use crate::langfuse::turn_traces::TurnTraceRegistry;

use super::llm_input_source;

/// OTLP 中承载观测 input 的属性键。
const INPUT_ATTRIBUTE: &str = "langfuse.observation.input";
/// 冻结 input 超过该字节数时进入磁盘暂存（`TelemetryConfig` 默认阈值）。
const SPOOL_THRESHOLD: usize = 256 * 1024;

// ── 本地 HTTP 捕获端点 ───────────────────────────────────────────────────────

/// 捕获上报正文并回 200 `{}` 的本地端点；只读正文，不解析凭据。
struct CaptureServer {
    url: String,
    bodies: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl CaptureServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind capture server");
        let address = listener.local_addr().expect("capture server address");
        let bodies: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&bodies);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                if let Some(body) = read_http_body(&mut stream) {
                    sink.lock().push(body);
                }
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\r\n{}",
                );
            }
        });
        Self {
            url: format!("http://{address}"),
            bodies,
        }
    }

    fn captured(&self) -> Vec<Vec<u8>> {
        self.bodies.lock().clone()
    }

    /// 已发送的 generation 输入（span 名以 `step-` 开头），按到达顺序。
    fn generation_inputs(&self) -> Vec<String> {
        self.captured()
            .iter()
            .flat_map(|body| otlp_spans(body))
            .filter(|span| span.name.starts_with("step-"))
            .filter_map(|span| span.attributes.get(INPUT_ATTRIBUTE).cloned())
            .collect()
    }
}

/// 读取一个 HTTP 请求的正文（按 `content-length` 截断）。
fn read_http_body(stream: &mut TcpStream) -> Option<Vec<u8>> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(index) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let headers = String::from_utf8_lossy(&buffer[..header_end]).to_ascii_lowercase();
    let length = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse::<usize>().ok())?;
    let mut body = buffer[header_end..].to_vec();
    while body.len() < length {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(length);
    Some(body)
}

/// OTLP span 视图：名称与字符串属性。
struct SpanView {
    name: String,
    attributes: HashMap<String, String>,
}

fn otlp_spans(body: &[u8]) -> Vec<SpanView> {
    let value: serde_json::Value = serde_json::from_slice(body).expect("OTLP 正文必须是 JSON");
    let mut spans = Vec::new();
    collect_spans(&value, &mut spans);
    spans
}

fn collect_spans(value: &serde_json::Value, spans: &mut Vec<SpanView>) {
    match value {
        serde_json::Value::Object(object) => {
            let name = object.get("name").and_then(|value| value.as_str());
            let attributes = object.get("attributes").and_then(|value| value.as_array());
            if let (Some(name), Some(attributes)) = (name, attributes) {
                let mut map = HashMap::new();
                for attribute in attributes {
                    let key = attribute.get("key").and_then(|value| value.as_str());
                    let text = attribute
                        .get("value")
                        .and_then(|value| value.get("stringValue"))
                        .and_then(|value| value.as_str());
                    if let (Some(key), Some(text)) = (key, text) {
                        map.insert(key.to_string(), text.to_string());
                    }
                }
                spans.push(SpanView {
                    name: name.to_string(),
                    attributes: map,
                });
            }
            for nested in object.values() {
                collect_spans(nested, spans);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_spans(item, spans);
            }
        }
        _ => {}
    }
}

// ── 真实 Batcher 的 SessionLike 适配器 ──────────────────────────────────────

/// 把 `LangfuseSessionLike` 委托给真实部署 Batcher：冻结、准入、预算与 flush 全部走同一份
/// 真实实现（permit 与冻结字节不可伪造），因此测试观察到的是产品链路而非替身。
struct BatcherSession {
    batcher: Arc<Batcher>,
    drop_registry: LangfuseDropRegistry,
    turn_traces: TurnTraceRegistry,
    session_id: String,
}

impl LangfuseSessionLike for BatcherSession {
    fn try_add(&self, event: IngestionEvent) -> Result<(), LangfuseError> {
        self.batcher.try_add(event)
    }

    fn try_freeze(&self, source: Arc<dyn SnapshotSource>) -> Result<InputSource, LangfuseError> {
        self.batcher.try_freeze(source)
    }

    fn try_add_prepared(&self, event: TelemetryEvent) -> Result<(), LangfuseError> {
        self.batcher.try_add_prepared(event)
    }

    fn memory_budget(&self) -> &Arc<MemoryBudget> {
        self.batcher.memory_budget()
    }

    fn flush(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<FlushReport, LangfuseError>> + Send + '_>> {
        Box::pin(self.batcher.flush())
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn drop_registry(&self) -> &LangfuseDropRegistry {
        &self.drop_registry
    }

    fn turn_traces(&self) -> &TurnTraceRegistry {
        &self.turn_traces
    }
}

/// 冻结被拒的会话：其余能力仍走真实 Batcher，只让 `try_freeze` 失败。
struct RejectingSession {
    inner: Arc<BatcherSession>,
}

impl LangfuseSessionLike for RejectingSession {
    fn try_add(&self, event: IngestionEvent) -> Result<(), LangfuseError> {
        self.inner.try_add(event)
    }

    fn try_freeze(&self, _source: Arc<dyn SnapshotSource>) -> Result<InputSource, LangfuseError> {
        Err(LangfuseError::QueueFull)
    }

    fn try_add_prepared(&self, event: TelemetryEvent) -> Result<(), LangfuseError> {
        self.inner.try_add_prepared(event)
    }

    fn memory_budget(&self) -> &Arc<MemoryBudget> {
        self.inner.memory_budget()
    }

    fn flush(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<FlushReport, LangfuseError>> + Send + '_>> {
        self.inner.flush()
    }

    fn session_id(&self) -> &str {
        self.inner.session_id()
    }

    fn drop_registry(&self) -> &LangfuseDropRegistry {
        self.inner.drop_registry()
    }

    fn turn_traces(&self) -> &TurnTraceRegistry {
        self.inner.turn_traces()
    }
}

// ── fixture ─────────────────────────────────────────────────────────────────

fn build_session(url: &str, spool: Option<Arc<dyn SpoolStore>>) -> Arc<BatcherSession> {
    let spool_enabled = spool.is_some();
    let client = LangfuseClient::new("pk-input", "sk-input", url, 1)
        .with_export_config(ExportConfig {
            compression: Compression::None,
            ..Default::default()
        })
        .expect("export config");
    let config = BatcherConfig {
        telemetry: TelemetryConfig {
            compression: Compression::None,
            // 暂存开关必须与部署实际持有的 spool 一致：开关打开而没有 store 时，冻结来源
            // 超过阈值前缀额度即按 `PayloadTooLarge` 显式拒收（契约 §3.3/§4）。
            spool_enabled,
            ..Default::default()
        },
        flush_interval: Duration::from_secs(3600),
        ..Default::default()
    };
    let batcher = Batcher::try_new(client, config, spool).expect("batcher");
    Arc::new(BatcherSession {
        batcher: Arc::new(batcher),
        drop_registry: LangfuseDropRegistry::default(),
        turn_traces: TurnTraceRegistry::default(),
        session_id: "session-input-snapshot".to_string(),
    })
}

fn tracer(session: Arc<dyn LangfuseSessionLike>) -> LangfuseTracer {
    let mut tracer = LangfuseTracer::new(
        session,
        "session-input-snapshot".to_string(),
        LangfuseConfig {
            trace_sampling: 1.0,
            ..Default::default()
        },
    );
    tracer.set_main_agent_id("main".to_string());
    tracer
}

/// 超过暂存阈值的大输入（含多字节字符，验证跨块与转义）。
fn big_messages() -> Vec<BaseMessage> {
    vec![BaseMessage::human("输入快照🙂".repeat(SPOOL_THRESHOLD / 8))]
}

/// 工具参数专门样本：嵌套 object 参数，逐字节参与基准比较。
fn big_tools() -> Vec<ToolDefinition> {
    vec![ToolDefinition {
        name: "lookup".to_string(),
        description: "lookup description🙂".repeat(1024),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {"query": {"type": "string", "description": "查询🙂".repeat(2048)}},
        }),
    }]
}

/// 期望字节口径：对象键递归字典序、数组保序（冻结正文与构建 feature 无关，契约 §3.4）。
///
/// 本地实现而非复用产品代码，保持期望值独立于被测实现。
fn stable_bytes(value: &serde_json::Value) -> String {
    fn canonical(value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(fields) => {
                let mut keys: Vec<&String> = fields.keys().collect();
                keys.sort();
                serde_json::Value::Object(
                    keys.into_iter()
                        .map(|key| (key.clone(), canonical(&fields[key])))
                        .collect(),
                )
            }
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.iter().map(canonical).collect())
            }
            other => other.clone(),
        }
    }
    serde_json::to_string(&canonical(value)).expect("stable expectation json")
}

/// 冻结前的 fallback 基准字节（`json!` 树按键字典序的紧凑 JSON）。
fn fallback_baseline(messages: &[BaseMessage], tools: &[ToolDefinition]) -> String {
    stable_bytes(&serde_json::json!({"messages": messages, "tools": tools}))
}

/// 有界等待一个真实可观察条件成立（不把 sleep 当作进度证据）。
async fn wait_for(mut ready: impl FnMut() -> bool, what: &str) {
    let waited = tokio::time::timeout(Duration::from_secs(5), async {
        while !ready() {
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await;
    assert!(waited.is_ok(), "等待超时: {what}");
}

/// 注册一个 child，使闸门中的内容事件能够 join 重放。
fn start_child(tracer: &mut LangfuseTracer, child: &str) {
    tracer.on_turn_start("turn");
    let _ = tracer.on_stage_start_gated("main", Stage::Act, "turn");
    tracer.on_tool_start(
        "main",
        "invocation",
        "Agent",
        &serde_json::json!({"task": "late join"}),
    );
    tracer.on_subagent_start("main", child, "worker", false);
}

// ── 用例 ────────────────────────────────────────────────────────────────────

/// 结束先于快照就绪：结束路径只移动 ticket，不等 IO，input 仍逐字节交付。
#[tokio::test]
async fn end_before_snapshot_ready_keeps_input() {
    let server = CaptureServer::start();
    let session = build_session(&server.url, None);
    let mut tracer = tracer(session);
    let messages = big_messages();
    let tools = big_tools();
    let expected = fallback_baseline(&messages, &tools);

    tracer.on_turn_start("turn");
    tracer.on_llm_start("main", 0, &messages, &tools);
    // 单线程 runtime 下 start 与 end 之间没有 await：快照必然仍处于 Preparing。
    tracer.on_llm_end(
        "main",
        0,
        "claude-sonnet-4",
        "anthropic",
        "完成🙂",
        None,
        None,
    );
    assert!(server.captured().is_empty(), "结束不得等待 IO 抢先发出请求");
    tracer
        .on_turn_end(TurnTelemetryOutcome::Completed)
        .await
        .expect("flush task should finish");

    assert_eq!(server.generation_inputs(), vec![expected]);
}

/// 冻结成功即计费；迟到 join 的闸门事件持有已计费来源，并能在重放后交付原字节。
#[tokio::test]
async fn late_join_gate_is_budgeted() {
    let server = CaptureServer::start();
    let session = build_session(&server.url, None);
    let budget = Arc::clone(session.batcher.memory_budget());
    let mut tracer = tracer(session);
    let messages = big_messages();
    let tools = big_tools();
    let expected = fallback_baseline(&messages, &tools);
    let before = budget.used();

    tracer.on_turn_start("turn");
    tracer.on_llm_start("ghost", 0, &messages, &tools);
    assert_eq!(tracer.subagent.gated_len(), 1, "未知 agent 事件进闸门");
    assert!(
        budget.used() > before,
        "入闸门之前必须已完成计费: before={before} after={}",
        budget.used()
    );

    // 迟到 join：闸门按原顺序重放，来源随事件移动到真实 worker
    tracer.on_tool_start(
        "main",
        "invocation",
        "Agent",
        &serde_json::json!({"task": "late join"}),
    );
    tracer.on_subagent_start("main", "ghost", "worker", false);
    assert_eq!(tracer.subagent.gated_len(), 0, "join 后闸门必须清空");
    tracer.on_llm_end(
        "ghost",
        0,
        "claude-sonnet-4",
        "anthropic",
        "完成🙂",
        None,
        None,
    );
    tracer
        .on_turn_end(TurnTelemetryOutcome::Completed)
        .await
        .expect("flush task should finish");

    assert_eq!(server.generation_inputs(), vec![expected]);
}

/// 长调用：冻结后遥测才持有原文 Arc，真实发布（落盘/编码）完成后必须释放。
#[tokio::test]
async fn long_call_releases_telemetry_raw_arc_after_publish() {
    let server = CaptureServer::start();
    let session = build_session(&server.url, None);
    let mut tracer = tracer(session);
    let messages = Arc::new(big_messages());
    let tools = Arc::new(big_tools());
    let expected = fallback_baseline(&messages, &tools);
    let weak = Arc::downgrade(&messages);

    tracer.on_turn_start("turn");
    tracer.on_llm_start_snapshot("main", 0, Arc::clone(&messages), &tools);
    drop(messages);
    assert!(
        weak.upgrade().is_some(),
        "冻结后到发布前，遥测仍持有原文 Arc（跨事件保留）"
    );

    tracer.on_llm_end(
        "main",
        0,
        "claude-sonnet-4",
        "anthropic",
        "完成🙂",
        None,
        None,
    );
    tracer
        .on_turn_end(TurnTelemetryOutcome::Completed)
        .await
        .expect("flush task should finish");

    assert!(
        weak.upgrade().is_none(),
        "payload 完成后必须释放遥测持有的原文 Arc"
    );
    assert_eq!(server.generation_inputs(), vec![expected]);
}

/// 大输入走真实磁盘 spool：input 先落盘、结束事件拿到的仍是基准字节，交付后对象回收。
#[tokio::test]
async fn spooled_big_input_keeps_end_event_bytes_exact() {
    let server = CaptureServer::start();
    let directory = tempfile::tempdir().expect("spool tempdir");
    let route = route_fingerprint(&server.url, "pk-input", "sk-input").expect("route");
    let spool = Arc::new(
        NativeSpool::open_at(directory.path().to_path_buf(), route, 8 * 1024 * 1024)
            .expect("native spool"),
    );
    let session = build_session(&server.url, Some(Arc::clone(&spool) as Arc<dyn SpoolStore>));
    let budget = Arc::clone(session.batcher.memory_budget());
    let baseline = budget.used();
    let mut tracer = tracer(Arc::clone(&session) as Arc<dyn LangfuseSessionLike>);
    let messages = big_messages();
    let tools = big_tools();
    let expected = fallback_baseline(&messages, &tools);

    tracer.on_turn_start("turn");
    tracer.on_llm_start("main", 0, &messages, &tools);
    // 只推进准备（未 flush）：大输入必须先落盘成为句柄，而不是留在内存
    wait_for(
        || spool.usage().map(|usage| usage.objects).ok() == Some(1),
        "大输入应先落盘为 spool 对象",
    )
    .await;
    assert!(server.captured().is_empty(), "落盘不触发任何请求");

    tracer.on_llm_end(
        "main",
        0,
        "claude-sonnet-4",
        "anthropic",
        "完成🙂",
        None,
        None,
    );
    tracer
        .on_turn_end(TurnTelemetryOutcome::Completed)
        .await
        .expect("flush task should finish");

    assert_eq!(server.generation_inputs(), vec![expected]);
    // 磁盘删除不由 flush 边界下发：句柄交付后归原清理槽，worker 在下一次有界 drain
    // 才执行 delete，flush 之后的下一个确定性 drain 点是关闭。这里据此断言
    // 正常完成后不留快照对象、retained 回到基线。
    session.batcher.shutdown().await.expect("batcher shutdown");
    wait_for(
        || spool.usage().map(|usage| usage.objects).ok() == Some(0),
        "input 对象在关闭 drain 后删除",
    )
    .await;
    assert!(
        budget.used() <= baseline,
        "交付后 retained 必须回到基线: baseline={baseline} used={}",
        budget.used()
    );
}

/// 请求体 → 结束 → 取消的顺序在并行 generation 下仍成立：各自交付自己的请求体。
#[tokio::test]
async fn request_end_cancel_order_holds_for_parallel_generations() {
    let server = CaptureServer::start();
    let session = build_session(&server.url, None);
    let mut tracer = tracer(session);
    let messages = vec![BaseMessage::human("fallback".repeat(1024))];
    let tools = big_tools();
    let fallback = fallback_baseline(&messages, &tools);
    let first = serde_json::json!({
        "model": "provider-model",
        "messages": [{"role": "user", "content": "first🙂".repeat(4096)}],
    });
    let second = serde_json::json!({
        "model": "provider-model",
        "messages": [{"role": "user", "content": "second🙂".repeat(4096)}],
    });

    tracer.on_turn_start("turn");
    tracer.on_llm_start("main", 0, &messages, &tools);
    tracer.on_llm_start("main", 1, &messages, &tools);
    // 请求体到达顺序与 step 顺序相反，结束顺序与 step 顺序相同
    tracer.on_llm_request_payload("main", 1, Arc::new(second.clone()));
    tracer.on_llm_request_payload("main", 0, Arc::new(first.clone()));
    tracer.on_llm_end("main", 1, "m", "p", "second done", None, None);
    tracer.on_llm_end("main", 0, "m", "p", "first done", None, None);
    tracer
        .on_turn_end(TurnTelemetryOutcome::Stopped {
            reason: PromptStopReason::Cancelled,
        })
        .await
        .expect("flush task should finish");

    let inputs = server.generation_inputs();
    assert_eq!(inputs.len(), 2, "两个 generation 各交付一次: {inputs:?}");
    assert!(inputs.contains(&stable_bytes(&first)));
    assert!(inputs.contains(&stable_bytes(&second)));
    assert!(
        !inputs.contains(&fallback),
        "请求体到达后不得再用 fallback 冒充实际请求"
    );
}

/// 取消路径：终态一次、input 仍是实际请求体、快照与预算不残留。
#[tokio::test]
async fn cancel_keeps_terminal_once() {
    let server = CaptureServer::start();
    let directory = tempfile::tempdir().expect("spool tempdir");
    let route = route_fingerprint(&server.url, "pk-input", "sk-input").expect("route");
    let spool = Arc::new(
        NativeSpool::open_at(directory.path().to_path_buf(), route, 8 * 1024 * 1024)
            .expect("native spool"),
    );
    let session = build_session(&server.url, Some(Arc::clone(&spool) as Arc<dyn SpoolStore>));
    let budget = Arc::clone(session.batcher.memory_budget());
    let baseline = budget.used();
    let mut tracer = tracer(session);
    let messages = big_messages();
    let tools = big_tools();
    let raw = serde_json::json!({
        "model": "provider-model",
        "messages": [{"role": "user", "content": "取消🙂".repeat(4096)}],
    });

    tracer.on_turn_start("turn");
    tracer.on_llm_start("main", 0, &messages, &tools);
    tracer.on_llm_request_payload("main", 0, Arc::new(raw.clone()));
    tracer
        .on_turn_end(TurnTelemetryOutcome::Stopped {
            reason: PromptStopReason::Cancelled,
        })
        .await
        .expect("flush task should finish");

    let inputs = server.generation_inputs();
    assert_eq!(
        inputs,
        vec![stable_bytes(&raw)],
        "取消只上报一次终态，且 input 是实际请求体"
    );
    wait_for(
        || spool.usage().map(|usage| usage.objects).ok() == Some(0),
        "取消后不得遗留快照对象",
    )
    .await;
    assert!(
        budget.used() <= baseline,
        "取消后 retained 必须回到基线: baseline={baseline} used={}",
        budget.used()
    );
}

/// 冻结被拒：不缓存原文、不回退旧 fallback，metadata 显式标记。
#[tokio::test]
async fn rejected_freeze_never_reports_the_old_fallback() {
    let server = CaptureServer::start();
    let session = build_session(&server.url, None);
    let rejecting: Arc<dyn LangfuseSessionLike> = Arc::new(RejectingSession { inner: session });
    let mut tracer = tracer(rejecting);
    let messages = vec![BaseMessage::human("fallback".repeat(1024))];
    let tools = big_tools();

    tracer.on_turn_start("turn");
    tracer.on_llm_start("main", 0, &messages, &tools);
    tracer.on_llm_request_payload(
        "main",
        0,
        Arc::new(serde_json::json!({"model": "provider-model"})),
    );
    tracer.on_llm_end("main", 0, "m", "p", "done", None, None);
    tracer
        .on_turn_end(TurnTelemetryOutcome::Completed)
        .await
        .expect("flush task should finish");

    assert!(
        server.generation_inputs().is_empty(),
        "冻结被拒时不得把旧 fallback 当作实际请求上报"
    );
    let marked = server
        .captured()
        .iter()
        .flat_map(|body| otlp_spans(body))
        .any(|span| {
            span.name.starts_with("step-")
                && span
                    .attributes
                    .get("langfuse.observation.metadata.input_snapshot_rejected")
                    .is_some_and(|value| value == "true")
        });
    assert!(marked, "冻结被拒必须留下稳定 metadata 标记");
}

/// 借用适配器的 JSON 字节必须与冻结前路径一致（否则 OTLP input 字符串字节会变）。
#[test]
fn borrowed_adapter_matches_baseline_bytes() {
    let messages = big_messages();
    let tools = big_tools();
    let source = llm_input_source(&Arc::new(messages.clone()), &Arc::new(tools.clone()));
    let mut bytes = Vec::new();
    source.write_json(&mut bytes).expect("write json");
    assert_eq!(bytes, fallback_baseline(&messages, &tools).into_bytes());
    let retained = source.retained_bytes().expect("retained bound");
    assert!(
        retained >= bytes.len(),
        "保守上界必须覆盖 JSON 正文: retained={retained} json={}",
        bytes.len()
    );
}

// ── 超预算拒收与落盘来源的计量 ──────────────────────────────────────────────

/// 上报正文里是否出现冻结拒收标记（稳定字段，不含正文）。
fn rejected_marked(server: &CaptureServer) -> bool {
    server
        .captured()
        .iter()
        .flat_map(|body| otlp_spans(body))
        .any(|span| {
            span.name.starts_with("step-")
                && span
                    .attributes
                    .get("langfuse.observation.metadata.input_snapshot_rejected")
                    .is_some_and(|value| value == "true")
        })
}

/// 超预算显式拒收：预留 40 MiB 容量（JSON 正文只有几字节）的输入在默认 32 MiB 预算下被拒，
/// 既不截断也不回退旧 fallback；拒收在计数、metadata 与预算回收上都可见。
#[tokio::test]
async fn over_budget_freeze_is_rejected_without_truncation() {
    let server = CaptureServer::start();
    let session = build_session(&server.url, None);
    let batcher = Arc::clone(&session.batcher);
    let baseline = batcher.memory_budget().used();
    let rejected_before = batcher.stats().rejected_events;
    let mut tracer = tracer(Arc::clone(&session) as Arc<dyn LangfuseSessionLike>);
    let mut reserved = String::with_capacity(40 * 1024 * 1024);
    reserved.push_str("tiny");
    // 必须走 `on_llm_start_snapshot`：`on_llm_start` 会先 `to_vec()` 深拷贝，
    // 克隆出来的 `String` 容量退化为 len，预留容量在这一层就被抹掉。
    let messages = Arc::new(vec![BaseMessage::human(reserved)]);
    let tools = big_tools();

    tracer.on_turn_start("turn");
    tracer.on_llm_start_snapshot("main", 0, Arc::clone(&messages), &tools);
    tracer.on_llm_end("main", 0, "m", "p", "done", None, None);
    tracer
        .on_turn_end(TurnTelemetryOutcome::Completed)
        .await
        .expect("flush task should finish");

    assert!(
        batcher.stats().rejected_events > rejected_before,
        "超预算拒收必须计入可见计数"
    );
    assert!(rejected_marked(&server), "超预算拒收必须留下 metadata 标记");
    let inputs = server.generation_inputs();
    assert!(
        inputs.is_empty(),
        "超预算不得上报任何 input（不截断、不回退 fallback）: {inputs:?}"
    );
    assert!(
        batcher.memory_budget().used() <= baseline,
        "拒收后 retained 必须回基线: baseline={baseline} used={}",
        batcher.memory_budget().used()
    );
}

/// 大输入走落盘来源：冻结收敛后只留文件引用，retained 不随原文线性增长。
///
/// 每个 session 只冻结一个大输入：落盘对象会占用一个 spool 槽位直到清理，同一部署里并发
/// 两个未清理对象会走纯内存回退路径（那是另一条契约分支，由 freeze 测试覆盖）。
#[tokio::test]
async fn spooled_large_input_retained_does_not_grow_with_text() {
    let tools = Arc::new(Vec::new());
    let mut retained_by_text = Vec::new();

    // 单条事件逻辑上界（`max_event_bytes` = 4 MiB）之内取两个相差 3 倍的规模。
    for text_bytes in [1024 * 1024, 3 * 1024 * 1024] {
        let server = CaptureServer::start();
        let directory = tempfile::tempdir().expect("spool tempdir");
        let route = route_fingerprint(&server.url, "pk-input", "sk-input").expect("route");
        let spool = Arc::new(
            NativeSpool::open_at(directory.path().to_path_buf(), route, 8 * 1024 * 1024)
                .expect("native spool"),
        );
        let session = build_session(&server.url, Some(Arc::clone(&spool) as Arc<dyn SpoolStore>));
        let budget = Arc::clone(session.batcher.memory_budget());
        let baseline = budget.used();
        let source = llm_input_source(
            &Arc::new(vec![BaseMessage::human("b".repeat(text_bytes))]),
            &tools,
        );
        // 保持句柄存活到断言结束：观察到的才是落盘引用，而不是清理副作用。
        let _frozen = session
            .batcher
            .try_freeze(source)
            .expect("大输入冻结必须被接受");
        session
            .batcher
            .flush()
            .await
            .unwrap_or_else(|error| panic!("flush 必须等到准备收敛: text={text_bytes} {error:?}"));
        wait_for(
            || spool.usage().map(|usage| usage.objects).ok() == Some(1),
            "大输入必须先落盘为 spool 对象",
        )
        .await;
        wait_for(
            || budget.used() <= baseline + SPOOL_THRESHOLD,
            "落盘后 retained 不得保留原文",
        )
        .await;

        let retained = budget.used() - baseline;
        assert!(
            retained <= SPOOL_THRESHOLD,
            "原文 {text_bytes} 字节时 retained 仍须有界（阈值前缀口径）: retained={retained}"
        );
        assert!(
            retained < text_bytes,
            "retained 不得随原文线性增长: retained={retained} text={text_bytes}"
        );
        retained_by_text.push(retained);
    }

    // 原文扩大 3 倍，落盘后的 retained 增量不得随之扩大。
    assert!(
        retained_by_text[1] <= retained_by_text[0] + SPOOL_THRESHOLD,
        "retained 增量随原文增长: {:?}",
        retained_by_text
    );
}

// ── 计量口径（真实容量，而非 JSON 长度） ────────────────────────────────────

/// 只借用消息列表的来源；工具列表为空。
fn messages_source(messages: Vec<BaseMessage>) -> Arc<dyn SnapshotSource> {
    llm_input_source(&Arc::new(messages), &Arc::new(Vec::new()))
}

/// 预留容量必须计入：正文只有几字节、但 `capacity` 远大于 JSON 时不得按 JSON 长度放行。
#[test]
fn retained_accounts_reserved_capacity_not_json_length() {
    let mut reserved = String::with_capacity(1024 * 1024);
    reserved.push_str("tiny");
    let source = messages_source(vec![BaseMessage::human(reserved)]);
    let mut json = Vec::new();
    source.write_json(&mut json).expect("write json");
    let retained = source.retained_bytes().expect("retained");
    assert!(json.len() < 4096, "JSON 正文与预留容量无关: {}", json.len());
    assert!(
        retained >= 1024 * 1024,
        "retained={retained} 必须覆盖真实 capacity，JSON 长度={}",
        json.len()
    );
}

#[test]
fn provider_replay_snapshot_charges_json_capacity_and_preserves_canonical_bytes() {
    use peri_agent::messages::{ContentBlock, MessageContent};
    let mut opaque = String::with_capacity(1024 * 1024);
    opaque.push_str("opaque-replay");
    let message = BaseMessage::ai(MessageContent::Blocks(vec![
        ContentBlock::text("visible"),
        ContentBlock::ProviderState {
            provider: "openai_responses".into(),
            data: serde_json::Value::String(opaque),
        },
    ]));
    let expected = fallback_baseline(std::slice::from_ref(&message), &[]);
    let source = messages_source(vec![message]);
    let mut bytes = Vec::new();
    source.write_json(&mut bytes).unwrap();
    assert_eq!(bytes, expected.as_bytes());
    assert!(bytes.len() < 4096);
    assert!(source.retained_bytes().unwrap() >= 1024 * 1024);
}

/// 计量与真实分配单调一致：正文真实增长多少容量，上界至少增长同样多。
#[test]
fn retained_grows_with_real_allocation() {
    let small = messages_source(vec![BaseMessage::human("x".repeat(64 * 1024))]);
    let big = messages_source(vec![BaseMessage::human("x".repeat(192 * 1024))]);
    let small = small.retained_bytes().expect("small retained");
    let big = big.retained_bytes().expect("big retained");
    assert!(
        big >= small + 128 * 1024,
        "真实分配多 128 KiB 时计量必须同步: small={small} big={big}"
    );
}

/// 逐元素 `to_value` 的中间副本按**最大元素**计一次：总容量相同、单元素更大时计量更大；
/// 副本不随元素数累加（否则差额会被元素数抹平）。
#[test]
fn retained_charges_one_largest_element_copy() {
    let total = 1024 * 1024;
    let one_big = messages_source(vec![BaseMessage::human("b".repeat(total))]);
    let mut many = Vec::new();
    for _ in 0..1024 {
        many.push(BaseMessage::human("s".repeat(total / 1024)));
    }
    let many_small = messages_source(many);
    let big = one_big.retained_bytes().expect("retained big");
    let small = many_small.retained_bytes().expect("retained small");
    assert!(
        big >= small + total / 2,
        "单条大 message 必须额外计入其 Value 副本: big={big} small={small}"
    );
}

/// 未知上界的动态内容（`SafeSubagentFailure` 字段私有、容量不可观察）显式拒收，不记 0。
#[test]
fn unbounded_hidden_allocation_is_rejected() {
    let message: BaseMessage = serde_json::from_value(serde_json::json!({
        "role": "tool",
        "id": "01924f2c-0000-7000-8000-000000000000",
        "tool_call_id": "call-1",
        "content": "子代理失败",
        "subagent_failure": {
            "child_thread_id": "child-1",
            "category": "llm_http",
            "message": "boom",
            "causes": ["timeout"],
            "diagnostic": null,
        },
    }))
    .expect("tool message with subagent failure");
    let source = messages_source(vec![message]);
    let error = source.retained_bytes().expect_err("未知上界必须拒收");
    assert!(
        matches!(&error, LangfuseError::Config(text) if text.contains("cannot be bounded")),
        "拒收必须是显式配置错误: {error:?}"
    );
    // 拒收只影响计费：内容仍可序列化，且不截断。
    let mut json = Vec::new();
    source.write_json(&mut json).expect("write json");
    assert!(
        String::from_utf8_lossy(&json).contains("subagent_failure"),
        "写出不受计量拒收影响"
    );
}
