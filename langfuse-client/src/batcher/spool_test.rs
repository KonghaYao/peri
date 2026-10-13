//! P3a 准入/准备/拆批的 spool 侧回归（契约 §8「P3」行）。
//!
//! 覆盖四类可观察行为：显式限额下的拆批不丢事件、单条超限只拒自身、慢盘准备受有界登记槽与
//! 预算约束、以及 flush 前缀屏障只覆盖其捕获边界（含准备失败的前缀归属）。全部用例用本地
//! fixture：无共享 test_helpers、不新增 mockall、不依赖 sleep 判断进度。

use std::{
    io::Read,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use super::*;
use crate::config::TelemetryConfig;
use crate::snapshot::SnapshotSource;
use crate::types::TraceBody;

/// 契约 §8 要求的显式 512 KiB 限额：单事件原文与单批聚合上限同值，便于观察拆批与拒收。
const LIMIT_512_KIB: usize = 512 * 1024;

fn event(id: &str) -> IngestionEvent {
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

/// 带正文载荷的事件：`payload_len` 决定 input 字符串长度，用于构造超限与分析膨胀。
fn event_with_payload(id: &str, payload_len: usize) -> IngestionEvent {
    IngestionEvent::TraceCreate {
        id: id.into(),
        timestamp: "2026-01-01T00:00:00Z".into(),
        body: TraceBody {
            id: Some(id.into()),
            name: Some(id.into()),
            input: Some(serde_json::Value::String("x".repeat(payload_len))),
            ..Default::default()
        },
        metadata: None,
    }
}

fn batcher_with(url: &str, config: BatcherConfig) -> Batcher {
    batcher_with_export(url, config, ExportConfig::default())
}

/// 显式导出限额的构造：拆批的编码上限是 client 的 `ExportConfig::max_request_bytes`
/// （解压后 JSON 上限），不是批原文上限。
fn batcher_with_export(url: &str, config: BatcherConfig, export: ExportConfig) -> Batcher {
    let client = LangfuseClient::new("pk-test", "sk-test", url, 0)
        .with_export_config(export)
        .unwrap();
    Batcher::try_new(client, config, None).unwrap()
}

fn test_config() -> BatcherConfig {
    BatcherConfig {
        max_events: 8,
        queue_capacity: 4,
        max_in_flight: 1,
        flush_interval: Duration::from_secs(60),
        ..Default::default()
    }
}

fn single_prepare_config() -> BatcherConfig {
    BatcherConfig {
        telemetry: TelemetryConfig {
            prepare_max_in_flight: 1,
            ..Default::default()
        },
        ..test_config()
    }
}

/// 记录型来源：进入次数可观察，按 `outcome` 决定准备成功或显式失败。
struct RecordingSource {
    payload: Vec<u8>,
    retained: usize,
    fail: bool,
    entered: Arc<AtomicUsize>,
    started: Option<tokio::sync::mpsc::UnboundedSender<u8>>,
    id: u8,
}

impl SnapshotSource for RecordingSource {
    fn retained_bytes(&self) -> Result<usize, LangfuseError> {
        Ok(self.retained)
    }

    fn write_json(&self, out: &mut dyn std::io::Write) -> Result<(), LangfuseError> {
        self.entered.fetch_add(1, Ordering::SeqCst);
        if let Some(started) = &self.started {
            let _ = started.send(self.id);
        }
        if self.fail {
            return Err(LangfuseError::Config("snapshot source failed".into()));
        }
        out.write_all(&self.payload)
            .map_err(|error| LangfuseError::Config(format!("snapshot write failed: {error}")))
    }
}

fn source(id: u8, fail: bool) -> Arc<RecordingSource> {
    Arc::new(RecordingSource {
        payload: b"{\"frozen\":true}".to_vec(),
        retained: 64,
        fail,
        entered: Arc::new(AtomicUsize::new(0)),
        started: None,
        id,
    })
}

/// 冻结一次来源并包成事件；`try_freeze` 失败即为测试失败。
fn prepared_event(batcher: &Batcher, id: &str, source: Arc<dyn SnapshotSource>) -> TelemetryEvent {
    let input = batcher.try_freeze(source).unwrap();
    TelemetryEvent::try_new(event(id), Some(input), None, batcher.memory_budget()).unwrap()
}

/// 本地 OTLP 收集器：按到达顺序累计正文（gzip 时先解压），不靠 sleep 判断已到达。
struct Collector {
    url: String,
    bodies: Arc<Mutex<Vec<serde_json::Value>>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    worker: tokio::task::JoinHandle<()>,
}

impl Collector {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let collected = Arc::clone(&bodies);
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    _ = &mut stopped => break,
                    accepted = listener.accept() => accepted,
                };
                let Ok((stream, _)) = accepted else { break };
                let collected = Arc::clone(&collected);
                tokio::spawn(async move {
                    // 正文先入账、后回响应：调用方的 shutdown 返回时收集必然完整。
                    let body = read_otlp_request(stream).await;
                    collected.lock().unwrap().push(body);
                });
            }
        });
        Self {
            url,
            bodies,
            stop: Some(stop),
            worker,
        }
    }

    async fn finish(mut self) -> Vec<serde_json::Value> {
        self.stop.take().unwrap().send(()).unwrap();
        self.worker.await.unwrap();
        std::mem::take(&mut *self.bodies.lock().unwrap())
    }
}

async fn read_otlp_request(stream: tokio::net::TcpStream) -> serde_json::Value {
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
fn decode_body(body: &[u8]) -> serde_json::Value {
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

/// 每个请求里的 span 名，按到达顺序展开。
fn span_names(bodies: &[serde_json::Value]) -> Vec<String> {
    bodies
        .iter()
        .flat_map(|body| {
            body["resourceSpans"][0]["scopeSpans"][0]["spans"]
                .as_array()
                .unwrap()
                .iter()
                .map(|span| span["name"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        })
        .collect()
}

async fn poll_pending<T>(future: std::pin::Pin<&mut impl std::future::Future<Output = T>>) {
    let mut future = future;
    std::future::poll_fn(|cx| {
        assert!(future.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
}

/// 等到持有该票据的准备任务终结、预算回到基线；超时即失败。
async fn wait_for_baseline(batcher: &Batcher, baseline: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while batcher.memory_budget().used() != baseline {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("准备任务必须终结并归还预算");
}

/// 契约 §9「编码膨胀」：原文总量未超单批上限、等价 OTLP 正文超限时按事件边界拆批。
///
/// 显式 512 KiB 限额下：单条 ≈ 200 KiB 原文，两条可成批，三条超过聚合上限 → 恰好两包，
/// 事件集合完全相等，每包正文不超过限额。
#[tokio::test]
async fn otlp_expansion_splits_without_loss() {
    let collector = Collector::start().await;
    let config = BatcherConfig {
        max_event_bytes: LIMIT_512_KIB,
        max_batch_bytes: LIMIT_512_KIB,
        max_queue_bytes: LIMIT_512_KIB * 2,
        ..test_config()
    };
    let batcher = batcher_with(&collector.url, config);

    for id in ["expand-1", "expand-2", "expand-3"] {
        batcher
            .try_add(event_with_payload(id, 200 * 1024))
            .expect("每条都在单条原文限额内");
    }
    batcher.shutdown().await.unwrap();

    let bodies = collector.finish().await;
    assert_eq!(
        bodies.len(),
        2,
        "三条事件必须拆成两包而不是产出一包超限正文"
    );
    for body in &bodies {
        assert!(
            serde_json::to_vec(body).unwrap().len() <= LIMIT_512_KIB,
            "每个批次正文必须守在显式限额内"
        );
    }
    assert_eq!(
        span_names(&bodies),
        ["expand-1", "expand-2", "expand-3"],
        "拆批不丢事件、顺序不变、无重复"
    );
    assert_eq!(batcher.stats().failed_batches, 0);
    assert_eq!(batcher.stats().completed_batches, 2);
}

/// 契约 §9「超大单条」：超限事件精确拒收自身，邻条照常送达，不截断、不伪造 span。
///
/// 单条超限在两个层面都只影响该条：原文超限在准入即拒收（`PayloadTooLarge`），编码膨胀超限
/// 由准备/拆批规则拒收（`SingleSpanTooLarge`），其后事件继续成批。
#[tokio::test]
async fn oversized_span_rejects_only_itself() {
    let collector = Collector::start().await;
    // 编码维度是单条不可拆的硬约束：导出正文上限取“大事件正文 - 1”（同时仍覆盖其原文长度），
    // 因此超限事件必须在本地精确拒收，而不是发出后由服务端失败。
    let probe = event_with_payload("probe", 8 * 1024);
    let budget = MemoryBudget::new(64 * 1024 * 1024).unwrap();
    let measured = crate::encoding::measure_otlp(
        &[TelemetryEvent::try_new(probe, None, None, &budget).unwrap()],
        usize::MAX,
    )
    .unwrap();
    let limit = (measured as usize) - 1;
    let config = BatcherConfig {
        max_event_bytes: limit,
        max_batch_bytes: limit,
        max_queue_bytes: limit,
        ..test_config()
    };
    let batcher = batcher_with_export(
        &collector.url,
        config,
        ExportConfig {
            max_request_bytes: limit,
            ..Default::default()
        },
    );

    batcher.try_add(event("small-before")).unwrap();
    batcher
        .try_add(event_with_payload("too-large", 8 * 1024))
        .unwrap();
    batcher.try_add(event("small-after")).unwrap();
    // 原文超限的事件在准入即拒收，连队列都不进入。
    let raw_over = batcher
        .try_add(event_with_payload("raw-over", limit + 1024))
        .expect_err("原文超限必须立即拒收");
    assert!(matches!(
        raw_over,
        LangfuseError::PayloadTooLarge { limit_bytes } if limit_bytes == limit
    ));
    batcher.flush().await.unwrap();
    batcher.shutdown().await.unwrap();

    let bodies = collector.finish().await;
    assert_eq!(
        span_names(&bodies),
        ["small-before", "small-after"],
        "被拒事件不进入任何批次，邻条照常送达"
    );
    let stats = batcher.stats();
    assert_eq!(stats.rejected_events, 2, "编码超限与原文超限各计一次拒收");
    assert_eq!(stats.failed_batches, 0, "本地精确拒收不是批次提交失败");
    assert_eq!(stats.in_flight_batches, 0);
}

/// 契约 §9「慢网络/慢磁盘」：准备先经有界登记槽与预算，未收敛的准备不放大登记规模。
///
/// 契约名中的「慢」由“未终结的准备仍占住槽与额度”表达：登记槽满即拒收（不排队等大对象），
/// 准备任务全程受预算约束，终结后额度全部归还。当前线程 runtime 在两次同步调用之间不运行
/// worker，因此这里的“未终结”是确定的，不是时间推测。
#[tokio::test]
async fn prepare_budget_bounds_slow_disk() {
    let batcher = batcher_with(
        "http://127.0.0.1:1",
        BatcherConfig {
            queue_capacity: 2,
            ..single_prepare_config()
        },
    );
    let baseline = batcher.memory_budget().used();

    let first = batcher.try_freeze(source(1, false)).unwrap();
    let after_first = batcher.memory_budget().used();
    assert!(
        after_first > baseline,
        "冻结先取得预算：allowance 与 retained 已记账"
    );
    let second = batcher.try_freeze(source(2, false)).unwrap();
    assert!(batcher.memory_budget().used() >= after_first);
    // 登记槽有界：第三个冻结不放行，也不留半登记。
    assert!(matches!(
        batcher.try_freeze(source(3, false)),
        Err(LangfuseError::QueueFull)
    ));
    assert_eq!(batcher.stats().rejected_events, 1);

    drop(first);
    drop(second);
    // 取消的登记仍要被派发并逐条记账：关闭报告两条准备失败，不吞掉、不静默归还槽位。
    let shutdown = batcher.shutdown().await.unwrap_err();
    assert!(
        matches!(
            shutdown,
            LangfuseError::SnapshotPreparationFailed {
                failed_snapshots: 2,
                ..
            }
        ),
        "两个被取消的冻结都必须显式上报：{shutdown:?}"
    );
    assert_eq!(
        batcher.memory_budget().used(),
        baseline,
        "准备终结后 allowance/retained 全部归还"
    );
}

/// 契约 §9「超大单条」补充：准备失败只由覆盖其 `prepare_seq` 的屏障报告，后缀失败不并入前缀。
///
/// 前缀 `prepare_seq<=P` 的准备失败必须由本次 flush 报告；后缀登记（`prepare_seq>P`）即使同样
/// 失败也不扩大边界，只留在累计账本里由关闭报告。
#[tokio::test]
async fn flush_prefix_protects_preparing_events() {
    let batcher = batcher_with(
        "http://127.0.0.1:1",
        BatcherConfig {
            telemetry: TelemetryConfig {
                prepare_max_in_flight: 2,
                ..Default::default()
            },
            ..test_config()
        },
    );

    // 前缀：失败来源；它的失败属于本次屏障。
    let prefix = prepared_event(&batcher, "preparing-prefix", source(1, true));
    batcher.try_add_prepared(prefix).unwrap();
    let mut flush = Box::pin(batcher.flush());
    poll_pending(flush.as_mut()).await;
    // 后缀：屏障之后登记，即使同样失败也不在本次边界内。
    let suffix_ticket = batcher.try_freeze(source(2, true)).unwrap();

    let error = flush.await.expect_err("前缀准备失败必须由本次屏障显式报告");
    assert!(
        matches!(
            error,
            LangfuseError::SnapshotPreparationFailed {
                failed_snapshots: 1,
                ..
            }
        ),
        "只报告前缀准备失败，后缀失败不并入：{error:?}"
    );

    drop(suffix_ticket);
    let shutdown = batcher.shutdown().await.unwrap_err();
    assert!(
        matches!(
            shutdown,
            LangfuseError::SnapshotPreparationFailed {
                failed_snapshots: 2,
                ..
            }
        ),
        "关闭报告累计准备失败（前缀 + 后缀）：{shutdown:?}"
    );
}

/// 契约 §8「准入/构造失败清理」：取消的准备不发布内容、不吞失败，并归还全部预算与登记槽。
#[tokio::test]
async fn cancelled_freeze_releases_slot_permits_and_reports_failure() {
    let batcher = batcher_with("http://127.0.0.1:1", single_prepare_config());
    let baseline = batcher.memory_budget().used();

    let entered = Arc::new(AtomicUsize::new(0));
    let cancelled = Arc::new(RecordingSource {
        payload: b"{\"frozen\":true}".to_vec(),
        retained: 64,
        fail: false,
        entered: Arc::clone(&entered),
        started: None,
        id: 1,
    });
    let ticket = batcher.try_freeze(cancelled).unwrap();
    assert!(batcher.memory_budget().used() > baseline);
    // 提交前取消：worker 尚未取走登记（当前线程 runtime 在两个同步调用之间不运行）。
    drop(ticket);

    // 预算回到基线只可能发生在被取走的准备任务终结之后：取消的准备必然已被派发并记账。
    wait_for_baseline(&batcher, baseline).await;
    assert_eq!(entered.load(Ordering::SeqCst), 0, "取消的票据从不进入写盘");

    let error = batcher.shutdown().await.unwrap_err();
    assert!(
        matches!(
            error,
            LangfuseError::SnapshotPreparationFailed {
                failed_snapshots: 1,
                ..
            }
        ),
        "取消的准备必须显式上报，不能悄悄消失：{error:?}"
    );
    assert_eq!(batcher.memory_budget().used(), baseline);
}

/// 契约 §4 计费口径：队列保存来源引用，按描述内存计费，不按被替代正文长度计费。
#[tokio::test]
async fn queued_bytes_charges_source_descriptor_not_contents() {
    let batcher = batcher_with("http://127.0.0.1:1", test_config());
    let baseline = batcher.memory_budget().used();

    let prepared = prepared_event(&batcher, "descriptor", source(1, false));
    let raw = super::budget::event_bytes(&event("descriptor"), usize::MAX).unwrap();
    assert_eq!(
        super::budget::queued_bytes(&prepared, usize::MAX).unwrap(),
        raw + super::budget::SOURCE_DESCRIPTOR_BYTES,
        "冻结正文的引用只按固定描述账单进入队列记账"
    );

    // 丢弃未提交的冻结事件即取消其准备任务：关闭必须显式报告这次取消，不静默吞掉。
    drop(prepared);
    let shutdown = batcher.shutdown().await.unwrap_err();
    assert!(
        matches!(
            shutdown,
            LangfuseError::SnapshotPreparationFailed {
                failed_snapshots: 1,
                ..
            }
        ),
        "被取消的准备必须显式上报：{shutdown:?}"
    );
    assert_eq!(batcher.memory_budget().used(), baseline);
}
