//! 未交付积压的屏障与配额回归（契约 §3.6、§5.2、§6、§7 P4a）。
//!
//! 两类可观察行为：
//! 1. 续发等待中的**活前缀**不能报告完成：`flush` 只对自己的前缀下结论，前缀内事件若在
//!    `retry_wait`，屏障返回 `Pending`/`PersistedPending` 并在同一 worker 到期续发；只有
//!    `Delivered` 才让后续屏障变回 `Complete`。
//! 2. 真实 native 端口上的配额预留被拒：事件按**失败**记账（不是交付、也不是写盘成功），
//!    降级原因与拒收计数可见；被拒那次预留当次回收，交付删除后额度归还，后续对象仍能落盘并上线。
//!
//! 关闭报告若还能给出「仅批次失败」的摘要，就证明没有任何槽/配额/待清理对象被拒收泄漏——
//! `DeliveryIncomplete` 会优先于该摘要返回。不共享 `recovery_test` 的夹具：本用例只需要一个
//! 门控/可切状态的服务端与一个真实临时根。

use std::{
    collections::VecDeque,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::watch;

use super::{stats::SpoolFallbackReason, Batcher, BatcherConfig, FlushReport, ShutdownReport};
use crate::client::{ExportConfig, LangfuseClient};
use crate::config::{Compression, TelemetryConfig};
use crate::error::LangfuseError;
use crate::spool::native::NativeSpool;
use crate::spool::{route_fingerprint, SpoolStore};
use crate::types::{IngestionEvent, TraceBody};

const PUBLIC_KEY: &str = "pk-test";
const SECRET_KEY: &str = "sk-test";
/// 压缩关闭：wire 上限即正文字节口径，阈值判定不依赖压缩率。
const WIRE_BYTES: usize = 8 * 1024;
/// 单次导出对象的预留额（`wire 上限 + descriptor 预留`）：整份配额只容得下一次 reserve。
const OBJECT_QUOTA_BYTES: u64 = WIRE_BYTES as u64 + 4 * 1024;
/// 正文内存前缀压到最小：任何批次都跨阈值转盘，真实文件路径与预留必然发生。
const PREFIX_BYTES: usize = 64;

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

/// 有界等待：条件成立立即返回，到期 panic（不靠固定 sleep 断言进度）。
async fn wait_until(what: &str, deadline: Duration, mut ready: impl FnMut() -> bool) {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if ready() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition not reached within {deadline:?}: {what}");
}

/// 真实临时根上的 native 端口与 batcher：端口 route 与同一凭据/端点的导出 route 一致。
fn spool_batcher(url: &str, quota: u64) -> (tempfile::TempDir, Arc<dyn SpoolStore>, Batcher) {
    let root = tempfile::tempdir().expect("temporary spool root");
    let route = route_fingerprint(url, PUBLIC_KEY, SECRET_KEY).expect("route fingerprint");
    let store: Arc<dyn SpoolStore> = Arc::new(
        NativeSpool::open_at(root.path().to_path_buf(), route, quota).expect("native spool"),
    );
    let client = LangfuseClient::new(PUBLIC_KEY, SECRET_KEY, url, 0)
        .with_export_config(ExportConfig {
            compression: Compression::None,
            max_wire_bytes: WIRE_BYTES,
            ..Default::default()
        })
        .expect("export config");
    let config = BatcherConfig {
        max_events: 1,
        queue_capacity: 4,
        max_in_flight: 2,
        flush_interval: Duration::from_secs(3600),
        telemetry: TelemetryConfig {
            compression: Compression::None,
            max_wire_bytes: WIRE_BYTES,
            payload_memory_threshold_bytes: PREFIX_BYTES,
            spool_quota_bytes: quota,
            ..Default::default()
        },
        ..Default::default()
    };
    let batcher =
        Batcher::try_new(client, config, Some(Arc::clone(&store))).expect("spool batcher");
    (root, store, batcher)
}

// ─── 本地 OTLP 服务（状态码脚本 + 响应门控） ────────────────────────

/// 状态码脚本：按到达顺序出队，最后一项重复使用；空脚本按 200。
fn next_status(script: &Mutex<VecDeque<u16>>) -> u16 {
    let mut script = script.lock().expect("status script");
    if script.len() > 1 {
        script.pop_front().unwrap_or(200)
    } else {
        script.front().copied().unwrap_or(200)
    }
}

/// 一次请求：正文先入账、再按门控放行、最后按状态码脚本响应（`connection: close`）。
struct Handler {
    stream: tokio::net::TcpStream,
    script: Arc<Mutex<VecDeque<u16>>>,
    gate: watch::Receiver<bool>,
    names: Arc<Mutex<Vec<String>>>,
}

impl Handler {
    async fn serve(self) {
        let Self {
            stream,
            script,
            mut gate,
            names,
        } = self;
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        if reader.read_line(&mut line).await.is_err() {
            return;
        }
        assert!(
            line.starts_with("POST /api/public/otel/v1/traces "),
            "unexpected request line: {line}"
        );
        let mut length = 0usize;
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            if line == "\r\n" {
                break;
            }
            if let Some((key, value)) = line.split_once(':') {
                if key.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().expect("content length");
                }
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).await.expect("request body");
        names
            .lock()
            .expect("delivered span names")
            .extend(span_names(&decode_body(&body)));
        // 门控：请求已入账但响应未写回，用例据此把对象扣在在途状态。
        while !*gate.borrow_and_update() {
            if gate.changed().await.is_err() {
                break;
            }
        }
        let status = next_status(&script);
        let response = if status == 200 {
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}".to_owned()
        } else {
            format!("HTTP/1.1 {status} Status\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
        };
        let _ = reader.get_mut().write_all(response.as_bytes()).await;
        let _ = reader.get_mut().shutdown().await;
    }
}

/// 本地 OTLP 服务：按到达顺序累计 span 名，状态码可切换、响应可门控。
struct Server {
    url: String,
    script: Arc<Mutex<VecDeque<u16>>>,
    gate: watch::Sender<bool>,
    names: Arc<Mutex<Vec<String>>>,
    worker: tokio::task::JoinHandle<()>,
}

impl Server {
    async fn start(script: &[u16]) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("local listener");
        let url = format!("http://{}", listener.local_addr().expect("local address"));
        let script = Arc::new(Mutex::new(VecDeque::from(script.to_vec())));
        let names = Arc::new(Mutex::new(Vec::new()));
        let (gate, _) = watch::channel(true);
        let worker = tokio::spawn({
            let script = Arc::clone(&script);
            let names = Arc::clone(&names);
            let gate = gate.clone();
            async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        return;
                    };
                    tokio::spawn(
                        Handler {
                            stream,
                            script: Arc::clone(&script),
                            gate: gate.subscribe(),
                            names: Arc::clone(&names),
                        }
                        .serve(),
                    );
                }
            }
        });
        Self {
            url,
            script,
            gate,
            names,
            worker,
        }
    }

    /// 改写整段状态码脚本（此后所有响应都用它）。
    fn status(&self, code: u16) {
        let mut script = self.script.lock().expect("status script");
        script.clear();
        script.push_back(code);
    }

    fn hold(&self) {
        // `send` 在所有 receiver 已析构时不会写值；门控必须无条件生效。
        self.gate.send_replace(false);
    }

    fn release(&self) {
        self.gate.send_replace(true);
    }

    fn names(&self) -> Vec<String> {
        self.names.lock().expect("delivered span names").clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

fn decode_body(body: &[u8]) -> Value {
    let raw = if body.starts_with(&[0x1f, 0x8b]) {
        let mut inflated = Vec::new();
        flate2::read::GzDecoder::new(body)
            .read_to_end(&mut inflated)
            .expect("gzip body");
        inflated
    } else {
        body.to_vec()
    };
    serde_json::from_slice(&raw).expect("OTLP JSON body")
}

fn span_names(body: &Value) -> Vec<String> {
    body["resourceSpans"][0]["scopeSpans"][0]["spans"]
        .as_array()
        .expect("OTLP spans")
        .iter()
        .map(|span| span["name"].as_str().expect("span name").to_owned())
        .collect()
}

// ─── 未终态前缀的屏障 ───────────────────────────────────────────────

/// 前缀内事件进入 `retry_wait`：屏障给出待交付计数而不是完成；续发成功后前缀才终态。
#[tokio::test]
async fn flush_reports_a_retry_waiting_prefix_as_pending() {
    let server = Server::start(&[503]).await;
    let (_root, _store, batcher) = spool_batcher(&server.url, OBJECT_QUOTA_BYTES * 4);

    batcher.try_add(event("retry-1")).expect("admission");
    // 可重试失败不是交付：屏障必须报告待交付，且不伪造failed_events。
    match batcher.flush().await {
        Err(LangfuseError::PersistedPending { counts })
        | Err(LangfuseError::Pending { counts }) => {
            assert_eq!(counts.pending_events, 1, "事件仍在待交付集合");
            assert_eq!(
                counts.persisted_pending_events, 1,
                "已落入可恢复 Export 的持久积压"
            );
            assert_eq!(counts.failed_events, 0, "可重试失败不是终态失败");
            assert_eq!(counts.cleanup_pending_objects, 0);
        }
        other => panic!("retry_wait 前缀的屏障不能报告完成：{other:?}"),
    }
    assert_eq!(server.names(), ["retry-1"], "本次只发出一次请求");

    // 同一 worker 到期续发；第二次响应的 200 才是交付，屏障随之回到 Complete。
    server.status(200);
    wait_until(
        "retry cycle delivered the prefix",
        Duration::from_secs(20),
        || server.names().len() == 2 && batcher.stats().in_flight_batches == 0,
    )
    .await;
    assert_eq!(server.names(), ["retry-1", "retry-1"]);
    assert!(matches!(batcher.flush().await, Ok(FlushReport::Complete)));
    let stats = batcher.stats();
    assert_eq!(stats.retry_attempts, 1, "续发只调度一次");
    assert_eq!(stats.failed_batches, 0, "续发不是终态失败");
    assert_eq!(stats.spool_objects, 0, "交付后正文已清理");
    assert!(matches!(
        batcher.shutdown().await,
        Ok(ShutdownReport::Complete)
    ));
}

// ─── 配额预留被拒 ───────────────────────────────────────────────────

/// 配额刚好一个对象：第二个对象的预留被拒后当次回收，交付删除后额度可再次使用。
#[tokio::test]
async fn quota_rejection_is_counted_and_its_reservation_reclaimed() {
    let server = Server::start(&[200]).await;
    server.hold();
    let (_root, _store, batcher) = spool_batcher(&server.url, OBJECT_QUOTA_BYTES);

    // 第一个对象占满整份配额，并被门控服务端扣在在途状态：删除前额度不归还。
    batcher.try_add(event("quota-1")).expect("first admission");
    wait_until(
        "first export reached the gated server",
        Duration::from_secs(10),
        || server.names() == ["quota-1"],
    )
    .await;
    assert_eq!(
        batcher.stats().quota_rejections,
        0,
        "配额尚能容纳第一个对象"
    );

    // 第二个对象预留失败：按失败记账、可见分类，且从未上线、不冒充交付。
    batcher.try_add(event("quota-2")).expect("second admission");
    wait_until(
        "quota rejection is accounted",
        Duration::from_secs(10),
        || batcher.stats().quota_rejections == 1,
    )
    .await;
    let stats = batcher.stats();
    assert_eq!(
        stats.fallback_reason,
        Some(SpoolFallbackReason::Quota),
        "降级原因分类为配额"
    );
    assert_eq!(stats.resource_fallbacks, 1);
    assert_eq!(stats.failed_batches, 1, "被拒的批次记一次失败");
    assert_eq!(stats.in_flight_batches, 1, "只有第一个对象在途");
    assert_eq!(
        server.names(),
        ["quota-1"],
        "被拒的事件既没有落盘也没有上线"
    );

    // 放行第一个对象：交付后删除正文，额度归还——否则后续对象会永久被拒。
    server.release();
    wait_until(
        "delivered object released its quota",
        Duration::from_secs(20),
        || batcher.stats().spool_objects == 0 && batcher.stats().in_flight_batches == 0,
    )
    .await;
    batcher
        .try_add(event("quota-3"))
        .expect("third admission after reclamation");
    wait_until(
        "reclaimed quota serves the next object",
        Duration::from_secs(20),
        || server.names().contains(&"quota-3".to_owned()),
    )
    .await;
    assert_eq!(
        server.names(),
        ["quota-1", "quota-3"],
        "归还的额度让新对象照常落盘并上线"
    );

    // 关闭报告仍是「仅批次失败」的摘要：pending/active/待清理都为 0，没有槽或配额被拒收泄漏。
    let error = batcher
        .shutdown()
        .await
        .expect_err("配额拒收的批次必须在关闭报告里可见");
    assert!(
        matches!(
            error,
            LangfuseError::IngestionApi(ref summary)
                if summary == "1 batch submission(s) failed before the flush barrier"
        ),
        "关闭报告应为失败摘要而不是待交付/待清理：{error:?}"
    );
    let stats = batcher.stats();
    assert_eq!(stats.quota_rejections, 1, "同一事件只记一次拒收");
    assert_eq!(stats.failed_batches, 1);
    assert_eq!(stats.completed_batches, 3, "三个批次各结算一次");
    assert_eq!(stats.spool_objects, 0, "交付对象与失败产物都已清空");
    assert_eq!(stats.in_flight_batches, 0);
}

// ─── 删除失败不换 owner（真实 native 磁盘） ─────────────────────────

/// 盘上对象目录（含 `meta` 文件的那一级）；按目录树查找，不复制适配器的路径约定。
#[cfg(unix)]
fn object_dirs(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut has_meta = false;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|name| name == "meta") {
                has_meta = true;
            }
        }
        if has_meta {
            found.push(dir);
        }
    }
    found
}

/// 掐掉对象**父目录**的写权限：终态 checkpoint 写在对象目录内不受影响，只有删除（rmdir）会失败；
/// 返回被改动的父目录，供恢复使用（删除失败后对象目录已空，不能再按 `meta` 反查）。
#[cfg(unix)]
fn make_object_parents_readonly(root: &Path) -> Vec<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let mut changed = Vec::new();
    for dir in object_dirs(root) {
        let parent = dir.parent().expect("对象目录必有父目录").to_path_buf();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o500))
            .expect("chmod 对象父目录");
        changed.push(parent);
    }
    assert!(!changed.is_empty(), "磁盘上必须已有对象目录");
    changed
}

/// 恢复对象父目录的写权限，让同一次清理重试真正删除。
#[cfg(unix)]
fn restore_object_parents(changed: Vec<PathBuf>) {
    use std::os::unix::fs::PermissionsExt;
    for parent in changed {
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700))
            .expect("chmod 对象父目录");
    }
}

/// [契约 §5.2 / §3.2] 真实 native 磁盘上的删除失败不换 owner、不释放槽：关闭只报告待清理对象
/// 而不是完成；同一个 owner 的**下一次**关闭必须接管清理重试并真正收敛到 Complete。
#[cfg(unix)]
#[tokio::test]
async fn delete_failure_stays_pending_until_the_next_shutdown_retries_it() {
    let server = Server::start(&[200]).await;
    server.hold();
    let root = tempfile::tempdir().expect("temporary spool root");
    let route = route_fingerprint(&server.url, PUBLIC_KEY, SECRET_KEY).expect("route fingerprint");
    let store: Arc<dyn SpoolStore> = Arc::new(
        NativeSpool::open_at(root.path().to_path_buf(), route, OBJECT_QUOTA_BYTES * 2)
            .expect("native spool"),
    );
    let client = LangfuseClient::new(PUBLIC_KEY, SECRET_KEY, &server.url, 0)
        .with_export_config(ExportConfig {
            compression: Compression::None,
            max_wire_bytes: WIRE_BYTES,
            ..Default::default()
        })
        .expect("export config");
    let config = BatcherConfig {
        max_events: 1,
        queue_capacity: 4,
        max_in_flight: 2,
        flush_interval: Duration::from_secs(3600),
        telemetry: TelemetryConfig {
            compression: Compression::None,
            max_wire_bytes: WIRE_BYTES,
            payload_memory_threshold_bytes: PREFIX_BYTES,
            spool_quota_bytes: OBJECT_QUOTA_BYTES * 2,
            ..Default::default()
        },
        ..Default::default()
    };
    let batcher =
        Batcher::try_new(client, config, Some(Arc::clone(&store))).expect("spool batcher");

    batcher
        .try_add(event("delivered-then-undeletable"))
        .expect("admission");
    wait_until(
        "request reached the gated endpoint",
        Duration::from_secs(20),
        || server.names() == ["delivered-then-undeletable"],
    )
    .await;
    // 对象已发布、请求在途：此刻掐掉父目录写权限，删除必然失败，交付本身不受影响。
    let parents = make_object_parents_readonly(root.path());
    server.release();
    wait_until(
        "cleanup attempt failed and the slot is retained",
        Duration::from_secs(20),
        || store.usage().expect("usage").objects == 1,
    )
    .await;

    let first = batcher.shutdown().await.expect_err("删除失败不得报告完成");
    match first {
        LangfuseError::DeliveryIncomplete { counts } => {
            assert_eq!(counts.pending_events, 0, "事件已交付");
            assert_eq!(counts.active_tasks, 0, "worker 已正常 join");
            assert_eq!(
                counts.cleanup_pending_objects, 1,
                "删除失败的对象仍待清理：{counts:?}"
            );
            assert_eq!(counts.failed_events, 0, "删除失败不伪造事件失败");
        }
        other => panic!("删除失败必须报告待清理对象，实际 {other:?}"),
    }
    assert_eq!(
        store.usage().expect("usage").objects,
        1,
        "未确认删除前不得回收槽与配额"
    );

    // 同一 owner 的第二次关闭：删除成功后清理才收敛，终态从待清理升级为完成。
    restore_object_parents(parents);
    assert!(
        matches!(batcher.shutdown().await, Ok(ShutdownReport::Complete)),
        "删除成功后重入关闭必须收敛"
    );
    assert_eq!(
        store.usage().expect("usage").objects,
        0,
        "确认删除后对象离盘"
    );
}
