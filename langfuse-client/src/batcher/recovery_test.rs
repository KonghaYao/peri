//! P4a 启动恢复与重试调度的回归（契约 §5.2、§7、§8）。
//!
//! 覆盖四类可观察行为：上一进程留在真实临时根上的可恢复对象进入**同一** worker 续发；重试等待
//! 持久保存首次接受时间/累计尝试/下次允许时间，TTL 不因重试重置，超期终结并清理；损坏、Input
//! 快照与终态残留在不发送的前提下被隔离或清理；持久积压与永久失败在关闭报告里同时可见，重复
//! flush 不清零积压。全部用例用真实 native 对象与本地 OTLP 服务，不共享 test_helpers、不新增
//! mockall、不靠固定 sleep 断言进度。

use std::{
    collections::{BTreeSet, VecDeque},
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::*;
use crate::config::{BackpressurePolicy, Compression};
use crate::spool::native::{
    create_private_dir_all, hex, id_hex, native_scan, NativeSpool, DESCRIPTOR_VERSION,
};
use crate::spool::{
    route_fingerprint, ObjectId, ObjectKind, ObjectMeta, RouteFingerprint, SpoolError, SpoolState,
    SpoolStore,
};
use crate::types::TraceBody;

const PUBLIC_KEY: &str = "pk-test";
const SECRET_KEY: &str = "sk-test";
/// 契约 §8 的 8 MiB 配额口径（留足余量，用例只关心对象而不关心配额压力）。
const QUOTA_BYTES: u64 = 8 * 1024 * 1024;

/// 长期存活的恢复对象：TTL 远大于用例时长，只有显式写短 TTL 的用例会超期。
const LONG_TTL_MS: i64 = 60_000;

fn config() -> BatcherConfig {
    BatcherConfig {
        max_events: 8,
        queue_capacity: 8,
        max_in_flight: 2,
        flush_interval: Duration::from_secs(3600),
        ..Default::default()
    }
}

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

/// 一份最小可解析的 OTLP JSON 正文：恢复路径按字节透传，服务端只读 span 名。
fn otlp_body(name: &str) -> Vec<u8> {
    format!("{{\"resourceSpans\":[{{\"scopeSpans\":[{{\"spans\":[{{\"name\":\"{name}\"}}]}}]}}]}}")
        .into_bytes()
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

/// 关闭报告的待交付计数；分类不掩盖其它计数，故这里同时接受 Pending 与 PersistedPending。
async fn pending_report(batcher: &Batcher) -> DeliveryCounts {
    match batcher.shutdown().await {
        Err(LangfuseError::PersistedPending { counts })
        | Err(LangfuseError::Pending { counts }) => counts,
        other => panic!("shutdown must report the pending backlog instead of {other:?}"),
    }
}

/// 一次 poll 确认 future 尚未就绪：取消等待的前置条件（HTTP 仍被门控）。
async fn park_pending<T>(mut pending: std::pin::Pin<&mut impl std::future::Future<Output = T>>) {
    std::future::poll_fn(|context| {
        assert!(pending.as_mut().poll(context).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
}

// ─── 真实临时根上的「上一进程」对象 ─────────────────────────────────

/// 临时根 + 与凭据同源的隔离域；对象按适配器自己的描述符编码铺盘，不持任何锁。
struct Fixture {
    root: tempfile::TempDir,
    url: String,
    route: RouteFingerprint,
    store: Arc<dyn SpoolStore>,
}

impl Fixture {
    fn open(url: &str) -> Self {
        let root = tempfile::tempdir().expect("temporary spool root");
        let route = route_fingerprint(url, PUBLIC_KEY, SECRET_KEY).expect("route fingerprint");
        let store: Arc<dyn SpoolStore> = Arc::new(
            NativeSpool::open_at(root.path().to_path_buf(), route, QUOTA_BYTES)
                .expect("native spool"),
        );
        Self {
            root,
            url: url.to_owned(),
            route,
            store,
        }
    }

    fn batcher(&self, config: BatcherConfig) -> Batcher {
        let client = LangfuseClient::new(PUBLIC_KEY, SECRET_KEY, &self.url, 0);
        Batcher::try_new(client, config, Some(Arc::clone(&self.store))).expect("spool batcher")
    }

    fn route_dir(&self) -> PathBuf {
        self.root
            .path()
            .join("peri/telemetry-spool/v1")
            .join(hex(self.route.as_bytes()))
    }

    fn object_dir(&self, id: ObjectId) -> PathBuf {
        self.route_dir()
            .join("objects")
            .join(format!("{:02x}", id.as_bytes()[0]))
            .join(id_hex(id))
    }

    fn meta_for(
        &self,
        id: ObjectId,
        kind: ObjectKind,
        body: &[u8],
        accepted_at_ms: i64,
        expires_at_ms: i64,
        events: u64,
    ) -> ObjectMeta {
        ObjectMeta {
            version: DESCRIPTOR_VERSION,
            id,
            kind,
            route: self.route,
            event_count: events,
            encoding: Compression::None,
            raw_len: body.len() as u64,
            stored_len: body.len() as u64,
            sha256: Sha256::digest(body).into(),
            accepted_at_ms,
            expires_at_ms,
            attempts: 0,
            next_attempt_at_ms: None,
            state: SpoolState::Ready,
        }
    }

    /// 直接按适配器格式铺一个已发布对象（`meta` 最后发布即不可见），返回正文。
    fn write(&self, meta: &ObjectMeta, body: &[u8]) {
        let dir = self.object_dir(meta.id);
        create_private_dir_all(&dir).expect("object directory");
        fs::write(dir.join("body"), body).expect("object body");
        fs::write(
            dir.join("meta"),
            native_scan::descriptor_bytes(meta).expect("descriptor bytes"),
        )
        .expect("object descriptor");
    }

    /// 铺一个刚被上一进程发布并退出的 Export 对象；`window_ms` 为接受时刻起的 TTL。
    fn export(&self, seed: u8, name: &str, events: u64, window_ms: i64) -> ObjectId {
        let body = otlp_body(name);
        let accepted_at_ms = wall_ms();
        let id = ObjectId::from_bytes([seed; 16]);
        let meta = self.meta_for(
            id,
            ObjectKind::Export,
            &body,
            accepted_at_ms,
            accepted_at_ms + window_ms,
            events,
        );
        self.write(&meta, &body);
        id
    }

    fn inspect(&self, id: ObjectId) -> ObjectMeta {
        self.store.inspect(id).expect("published object")
    }

    /// 仍可见对象的元数据；已删除/不可读为 `None`（等待和断言都不 panic）。
    fn meta_of(&self, id: ObjectId) -> Option<ObjectMeta> {
        self.store.inspect(id).ok()
    }

    fn body_of(&self, id: ObjectId) -> Vec<u8> {
        fs::read(self.object_dir(id).join("body")).expect("object body")
    }

    /// 盘上仍可见的对象数（直接数目录，不依赖端口账本）。
    fn published(&self) -> usize {
        let Ok(buckets) = fs::read_dir(self.route_dir().join("objects")) else {
            return 0;
        };
        buckets
            .filter_map(Result::ok)
            .filter_map(|bucket| fs::read_dir(bucket.path()).ok())
            .flatten()
            .filter_map(Result::ok)
            .filter(|object| object.path().join("meta").is_file())
            .count()
    }

    fn store(&self) -> Arc<dyn SpoolStore> {
        Arc::clone(&self.store)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // 临时根由 TempDir 负责回收；这里只确认路径仍在夹具手里（防止提前被移动）。
        debug_assert!(self.root.path().is_dir());
    }
}

// ─── 本地 OTLP 服务 ─────────────────────────────────────────────────

/// 一次请求：正文先入账、再按门控放行、最后按状态码脚本响应（`connection: close`）。
struct Handler {
    stream: tokio::net::TcpStream,
    script: Arc<Mutex<VecDeque<u16>>>,
    gate: tokio::sync::watch::Receiver<bool>,
    bodies: Arc<Mutex<Vec<Value>>>,
}

/// 状态码脚本：按到达顺序出队，最后一项重复使用；空脚本按 200。
fn next_status(script: &Mutex<VecDeque<u16>>) -> u16 {
    let mut script = script.lock().expect("status script");
    if script.len() > 1 {
        script.pop_front().unwrap_or(200)
    } else {
        script.front().copied().unwrap_or(200)
    }
}

impl Handler {
    async fn serve(mut self) {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
        let mut reader = BufReader::new(self.stream);
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
        self.bodies
            .lock()
            .expect("collected bodies")
            .push(decode_body(&body));
        // 门控：请求已入账但响应未写回，用例据此观察「已到达」与「仍在途」。
        while !*self.gate.borrow_and_update() {
            if self.gate.changed().await.is_err() {
                break;
            }
        }
        let status = next_status(&self.script);
        let response = if status == 200 {
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}".to_owned()
        } else {
            format!("HTTP/1.1 {status} Status\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
        };
        let _ = reader.get_mut().write_all(response.as_bytes()).await;
        let _ = reader.get_mut().shutdown().await;
    }
}

/// 本地 OTLP 服务：正文按到达顺序累计（gzip 先解压），状态码可切换、响应可门控。
struct Server {
    url: String,
    script: Arc<Mutex<VecDeque<u16>>>,
    gate: tokio::sync::watch::Sender<bool>,
    bodies: Arc<Mutex<Vec<Value>>>,
    worker: tokio::task::JoinHandle<()>,
}

impl Server {
    async fn start(script: &[u16]) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("local listener");
        let url = format!("http://{}", listener.local_addr().expect("local address"));
        let script = Arc::new(Mutex::new(VecDeque::from(script.to_vec())));
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let (gate, _) = tokio::sync::watch::channel(true);
        let worker = tokio::spawn({
            let script = Arc::clone(&script);
            let bodies = Arc::clone(&bodies);
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
                            bodies: Arc::clone(&bodies),
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
            bodies,
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

    fn count(&self) -> usize {
        self.bodies.lock().expect("collected bodies").len()
    }

    fn bodies(&self) -> Vec<Value> {
        self.bodies.lock().expect("collected bodies").clone()
    }

    /// 服务端按到达顺序看到的 span 名（重试会重复出现）。
    fn names(&self) -> Vec<String> {
        span_names(&self.bodies())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

/// 正文可能是 gzip（部署默认压缩）或明文：按 magic 判断，两种情况都解出 OTLP JSON。
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

/// 每个请求里的 span 名，按到达顺序展开。
fn span_names(bodies: &[Value]) -> Vec<String> {
    bodies
        .iter()
        .flat_map(|body| {
            body["resourceSpans"][0]["scopeSpans"][0]["spans"]
                .as_array()
                .expect("OTLP spans")
                .iter()
                .map(|span| span["name"].as_str().expect("span name").to_owned())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// 服务端看到的去重事件名（重试不重复计数）。
fn delivered_names(server: &Server) -> BTreeSet<String> {
    server.names().into_iter().collect()
}

/// 夹具里的对象路径是否存在（端口删除后不再可见）。
fn object_exists(fixture: &Fixture, id: ObjectId) -> bool {
    Path::new(&fixture.object_dir(id)).is_dir()
}

// ─── 启动恢复与重试调度 ─────────────────────────────────────────────

#[tokio::test]
async fn retry_wait_reports_persisted_pending() {
    let server = Server::start(&[503]).await;
    let fixture = Fixture::open(&server.url);
    let id = fixture.export(1, "retry-wait", 2, LONG_TTL_MS);
    let original = fixture.inspect(id);

    let batcher = fixture.batcher(config());
    // flush 只对自己的前缀下结论：一次可重试失败不是交付，flush 成功不等于积压清零。
    assert!(matches!(batcher.flush().await, Ok(FlushReport::Complete)));

    let counts = pending_report(&batcher).await;
    assert_eq!(
        counts.pending_events, 2,
        "只写盘不算交付：事件仍在待交付集合"
    );
    assert_eq!(
        counts.persisted_pending_events, 2,
        "已登记为可恢复的持久积压"
    );
    assert_eq!(counts.failed_events, 0, "可重试失败不是终态失败");
    assert_eq!(counts.cleanup_pending_objects, 0, "移交对象留给下一 owner");
    assert_eq!(counts.active_tasks, 0);

    let settled = fixture.inspect(id);
    assert_eq!(settled.state, SpoolState::RetryWait);
    assert_eq!(
        settled.accepted_at_ms, original.accepted_at_ms,
        "首次接受时间不因重试改变"
    );
    assert_eq!(
        settled.expires_at_ms, original.expires_at_ms,
        "TTL 不因重试重置"
    );
    let due = settled.next_attempt_at_ms.expect("下次允许时间已持久化");
    assert!(
        due > original.accepted_at_ms && due < original.expires_at_ms,
        "重试时刻落在原 TTL 之内：{due}"
    );
    assert_eq!(fixture.body_of(id), otlp_body("retry-wait"), "正文未被重写");

    let stats = batcher.stats();
    assert_eq!(stats.recovered_events, 2);
    assert_eq!(stats.failed_batches, 0);
    assert_eq!(stats.expired_events, 0);
    assert_eq!(stats.corrupt_objects, 0);
    assert_eq!(stats.in_flight_batches, 0);
    assert_eq!(
        settled.attempts, stats.retry_attempts,
        "持久尝试水位与本进程续发次数一致"
    );
    assert!(settled.attempts >= 1);
    assert_eq!(
        server.count() as u64,
        stats.retry_attempts,
        "每次续发都对应一次真实请求"
    );
    assert_eq!(fixture.published(), 1, "对象留在盘上等待续发");
}

#[tokio::test]
async fn pending_counts_survive_repeated_flush() {
    let server = Server::start(&[503]).await;
    let fixture = Fixture::open(&server.url);
    for seed in 1..=2 {
        fixture.export(seed, &format!("persisted-{seed}"), 1, LONG_TTL_MS);
    }

    let batcher = fixture.batcher(config());
    for round in 1..=2 {
        assert!(
            matches!(batcher.flush().await, Ok(FlushReport::Complete)),
            "第 {round} 次 flush 只覆盖自己的前缀"
        );
    }
    let first = pending_report(&batcher).await;
    assert_eq!(first.pending_events, 2, "重复 flush 不清零持久积压");
    assert_eq!(first.persisted_pending_events, 2);
    assert_eq!(first.failed_events, 0);
    assert_eq!(first.cleanup_pending_objects, 0);

    // 关闭后的 flush 透传同一终态：重复 flush 既不消费也不重发已移交的积压。
    assert!(matches!(
        batcher.flush().await,
        Err(LangfuseError::PersistedPending { .. })
    ));
    let second = pending_report(&batcher).await;
    assert_eq!(second, first, "重复关闭返回同一计数快照");
    assert_eq!(fixture.published(), 2, "持久积压仍在盘上等待下一 owner");
    assert_eq!(batcher.stats().recovered_events, 2);
}

#[tokio::test]
async fn shutdown_cancel_reentry_keeps_owner() {
    let server = Server::start(&[503]).await;
    server.hold();
    let fixture = Fixture::open(&server.url);
    let id = fixture.export(1, "cancel-reentry", 1, LONG_TTL_MS);
    let batcher = fixture.batcher(config());

    let mut cancelled = Box::pin(batcher.shutdown());
    park_pending(cancelled.as_mut()).await;
    wait_until(
        "recovery request reached the gated server",
        Duration::from_secs(5),
        || server.count() == 1,
    )
    .await;
    drop(cancelled);
    assert!(!batcher.worker_is_joined().await, "取消等待不能取走 owner");
    assert!(matches!(
        batcher.try_add(event("late")),
        Err(LangfuseError::ChannelClosed)
    ));

    server.release();
    let counts = pending_report(&batcher).await;
    assert!(
        batcher.worker_is_joined().await,
        "重入必须 join 同一 worker"
    );
    assert_eq!(counts.pending_events, 1);
    assert_eq!(counts.persisted_pending_events, 1);
    assert_eq!(server.count(), 1, "取消与重入都不重复提交");
    assert_eq!(fixture.inspect(id).state, SpoolState::RetryWait);
}

#[tokio::test]
async fn ttl_never_resets_on_retry() {
    let server = Server::start(&[503]).await;
    let fixture = Fixture::open(&server.url);
    // 上一进程留下的短 TTL 对象：续发只能落在原期限之内，重试不延长 TTL。
    let id = fixture.export(1, "ttl-fixed", 1, 6_000);
    let original = fixture.inspect(id);

    let batcher = fixture.batcher(config());
    wait_until("retry_wait written back", Duration::from_secs(5), || {
        fixture
            .meta_of(id)
            .is_some_and(|meta| meta.state == SpoolState::RetryWait)
    })
    .await;
    let retried = fixture.inspect(id);
    assert_eq!(
        retried.accepted_at_ms, original.accepted_at_ms,
        "首次接受时间不因重试重置"
    );
    assert_eq!(
        retried.expires_at_ms, original.expires_at_ms,
        "TTL 不因重试重置"
    );
    let due = retried.next_attempt_at_ms.expect("下次允许时间已持久化");
    assert!(
        due < retried.expires_at_ms,
        "重试仍受原 TTL 约束：{due} < {}",
        retried.expires_at_ms
    );
    assert!(retried.attempts >= 1);

    // 指数退避的下次允许时间越过原期限：按 TTL 先到终结并清理，不再续发。
    wait_until(
        "expiry termination and cleanup",
        Duration::from_secs(20),
        || batcher.stats().expired_events == 1 && fixture.published() == 0,
    )
    .await;
    let stats = batcher.stats();
    assert_eq!(stats.failed_batches, 1);
    assert!(
        stats.retry_attempts >= 2,
        "重试次数 {}",
        stats.retry_attempts
    );
    assert_eq!(server.count() as u64, stats.retry_attempts);
    assert_eq!(fixture.published(), 0, "超期对象正文与描述都被清理");
    assert!(matches!(
        fixture.store().inspect(id),
        Err(SpoolError::NotFound)
    ));
    assert!(matches!(
        batcher.shutdown().await,
        Err(LangfuseError::IngestionApi(_))
    ));
}

#[tokio::test]
async fn permanent_failure_and_pending_are_both_reported() {
    // 首个请求 400（永久失败），其后 503（可重试）：哪个对象拿到哪次响应由到达顺序决定，聚合固定。
    let server = Server::start(&[400, 503]).await;
    let fixture = Fixture::open(&server.url);
    let first = fixture.export(1, "permanent", 1, LONG_TTL_MS);
    let second = fixture.export(2, "retryable", 1, LONG_TTL_MS);

    let batcher = fixture.batcher(config());
    let counts = pending_report(&batcher).await;
    assert_eq!(
        counts.failed_events, 1,
        "永久失败必须报告，不被待交付计数掩盖"
    );
    assert_eq!(counts.pending_events, 1);
    assert_eq!(counts.persisted_pending_events, 1);
    assert_eq!(counts.cleanup_pending_objects, 0);
    assert_eq!(counts.active_tasks, 0);

    let stats = batcher.stats();
    assert_eq!(stats.recovered_events, 2);
    assert_eq!(stats.failed_batches, 1);
    assert_eq!(stats.expired_events, 0);
    assert_eq!(
        fixture.published(),
        1,
        "终态失败的对象已清理，待交付对象留给下一 owner"
    );
    let left = [first, second]
        .into_iter()
        .filter(|id| fixture.meta_of(*id).is_some())
        .collect::<Vec<_>>();
    assert_eq!(left.len(), 1, "只留待交付对象");
    assert_eq!(fixture.inspect(left[0]).state, SpoolState::RetryWait);
    assert!(server.count() >= 2, "两个对象各尝试一次");
}

#[tokio::test]
async fn expired_entry_is_cleaned_without_send() {
    let server = Server::start(&[200]).await;
    let fixture = Fixture::open(&server.url);
    // 上一进程留下的超期对象：metadata 与正文都完好，只是已过 TTL。
    let now = wall_ms();
    let body = otlp_body("expired");
    let expired = ObjectId::from_bytes([9; 16]);
    let meta = fixture.meta_for(
        expired,
        ObjectKind::Export,
        &body,
        now - 10_000,
        now - 5_000,
        2,
    );
    fixture.write(&meta, &body);
    fixture.export(1, "fresh", 1, LONG_TTL_MS);

    let batcher = fixture.batcher(config());
    wait_until(
        "expired entry removed and fresh delivered",
        Duration::from_secs(10),
        || fixture.published() == 0 && batcher.stats().in_flight_batches == 0,
    )
    .await;

    let names = delivered_names(&server);
    assert!(names.contains("fresh"), "{names:?}");
    assert!(!names.contains("expired"), "超期条目绝不发送：{names:?}");
    let stats = batcher.stats();
    assert_eq!(stats.expired_events, 2, "超期事件数按描述符记账");
    assert_eq!(stats.recovered_events, 1);
    assert_eq!(stats.corrupt_objects, 0);
    assert_eq!(
        stats.failed_batches, 0,
        "上一进程的到期对象不是本进程的发送失败"
    );
    assert!(matches!(
        batcher.shutdown().await,
        Ok(ShutdownReport::Complete)
    ));
    assert_eq!(fixture.published(), 0);
    assert!(!object_exists(&fixture, expired), "超期对象已清理");
}

#[tokio::test]
async fn corrupt_entry_is_quarantined_without_send_and_next_round_recovers() {
    let server = Server::start(&[200]).await;
    let fixture = Fixture::open(&server.url);
    let survivor = fixture.export(1, "survivor", 1, LONG_TTL_MS);
    // 损坏描述符（未知版本）：端口在枚举时就地隔离并删除，本轮扫描不发送任何对象。
    let body = otlp_body("corrupt");
    let now = wall_ms();
    let corrupt_id = ObjectId::from_bytes([7; 16]);
    let mut corrupt = fixture.meta_for(
        corrupt_id,
        ObjectKind::Export,
        &body,
        now,
        now + LONG_TTL_MS,
        1,
    );
    corrupt.version = DESCRIPTOR_VERSION + 1;
    fixture.write(&corrupt, &body);

    let batcher = fixture.batcher(config());
    assert!(matches!(
        batcher.shutdown().await,
        Ok(ShutdownReport::Complete)
    ));
    assert_eq!(server.count(), 0, "损坏条目绝不发送");
    assert_eq!(batcher.stats().recovered_events, 0);
    assert!(!object_exists(&fixture, corrupt_id), "损坏条目已隔离并删除");
    assert_eq!(
        fixture.published(),
        1,
        "合格对象原样留在盘上，交给下一 owner"
    );
    assert_eq!(fixture.inspect(survivor).state, SpoolState::Ready);

    // 下一轮：损坏条目已隔离，合格对象恢复并交付（无数据丢失）。
    let next = fixture.batcher(config());
    wait_until("survivor delivered", Duration::from_secs(10), || {
        fixture.published() == 0
    })
    .await;
    assert_eq!(server.names(), ["survivor"]);
    assert!(matches!(
        next.shutdown().await,
        Ok(ShutdownReport::Complete)
    ));
    assert_eq!(next.stats().recovered_events, 1);
}

#[tokio::test]
async fn recovered_object_and_live_batch_share_one_worker() {
    let server = Server::start(&[503]).await;
    let fixture = Fixture::open(&server.url);
    fixture.export(1, "recovered", 1, LONG_TTL_MS);

    let batcher = fixture.batcher(BatcherConfig {
        max_events: 1,
        ..config()
    });
    batcher.try_add(event("live")).expect("live admission");
    // 恢复对象与 live 批次共用唯一 worker 与在途集合：第一次尝试失败后放行成功，同一 worker 续发。
    wait_until("both paths attempted", Duration::from_secs(10), || {
        server.count() >= 2
    })
    .await;
    server.status(200);
    wait_until("both delivered", Duration::from_secs(20), || {
        fixture.published() == 0 && batcher.stats().in_flight_batches == 0
    })
    .await;
    assert!(matches!(
        batcher.shutdown().await,
        Ok(ShutdownReport::Complete)
    ));

    let names = delivered_names(&server);
    assert!(
        names.contains("live") && names.contains("recovered"),
        "{names:?}"
    );
    let stats = batcher.stats();
    assert_eq!(stats.recovered_events, 1);
    assert!(stats.retry_attempts >= 1, "恢复对象由同一 worker 续发");
    assert_eq!(stats.submitted_batches, 1, "live 批次只提交一次");
    assert_eq!(stats.failed_batches, 0);
}

#[tokio::test]
async fn recovery_deferral_keeps_objects_for_the_next_owner() {
    let server = Server::start(&[503]).await;
    let fixture = Fixture::open(&server.url);
    // 清理槽容量 = queue_capacity×3 + prepare + in_flight + 64：小容量配置下预留必然不足。
    let mut small = config();
    small.queue_capacity = 1;
    small.max_in_flight = 1;
    let slots = crate::memory::slot_capacity(&small).expect("slot capacity");
    let total = slots + 3;
    for seed in 0..total {
        fixture.export(seed as u8, &format!("deferred-{seed}"), 1, LONG_TTL_MS);
    }

    let batcher = fixture.batcher(small);
    let counts = pending_report(&batcher).await;
    let recovered = counts.pending_events as usize;
    assert!(recovered > 0, "本轮必须恢复可发送对象");
    assert!(
        recovered < total,
        "预留不足时必须推迟而不是丢弃：{recovered}/{total}"
    );
    assert!(
        recovered > recovery::RECOVERY_PAGE_ENTRIES,
        "固定页扫描跨多页：{recovered} 条"
    );
    assert_eq!(counts.persisted_pending_events as usize, recovered);
    assert_eq!(counts.failed_events, 0);
    assert_eq!(fixture.published(), total, "推迟的对象原样留在盘上");
    assert_eq!(batcher.stats().corrupt_objects, 0, "预留不足不是损坏");
    assert_eq!(
        delivered_names(&server).len(),
        recovered,
        "推迟的对象从未尝试发送"
    );

    // 下一轮用更大的清理槽容量接手全部对象：推迟不丢数据、不重复计数。
    let mut large = config();
    large.max_in_flight = 8;
    let next = fixture.batcher(large);
    server.status(200);
    wait_until(
        "next owner delivers every object",
        Duration::from_secs(30),
        || fixture.published() == 0 && next.stats().in_flight_batches == 0,
    )
    .await;
    assert!(matches!(
        next.shutdown().await,
        Ok(ShutdownReport::Complete)
    ));
    assert_eq!(next.stats().recovered_events, total as u64);
    let names = delivered_names(&server);
    assert_eq!(names.len(), total, "每个对象恰好一个事件上线");
    assert!(
        names.iter().all(|name| name.starts_with("deferred-")),
        "{names:?}"
    );
}

// ─── 完成 / 失败 / 驱逐路径的计数 ───────────────────────────────────

#[tokio::test]
async fn stats_count_recovered_delivery_without_live_submission() {
    let server = Server::start(&[200]).await;
    let fixture = Fixture::open(&server.url);
    fixture.export(1, "counted", 3, LONG_TTL_MS);

    let batcher = fixture.batcher(config());
    wait_until(
        "recovered delivery and cleanup",
        Duration::from_secs(10),
        || batcher.stats().completed_batches == 1 && fixture.published() == 0,
    )
    .await;
    let stats = batcher.stats();
    assert_eq!(stats.recovered_events, 3);
    assert_eq!(stats.retry_attempts, 1);
    assert_eq!(stats.submitted_batches, 0, "恢复续发不计 live 提交");
    assert_eq!(stats.failed_batches, 0);
    assert_eq!(stats.expired_events, 0);
    assert_eq!(stats.corrupt_objects, 0);
    assert_eq!(stats.quota_rejections, 0);
    assert_eq!(stats.resource_fallbacks, 0);
    assert_eq!(stats.in_flight_batches, 0);
    assert_eq!(fixture.published(), 0);
    assert!(matches!(
        batcher.shutdown().await,
        Ok(ShutdownReport::Complete)
    ));
}

#[tokio::test]
async fn stats_count_permanent_failure_and_keep_it_terminal() {
    let server = Server::start(&[400]).await;
    let fixture = Fixture::open(&server.url);
    let id = fixture.export(1, "rejected", 2, LONG_TTL_MS);

    let batcher = fixture.batcher(config());
    wait_until(
        "permanent failure recorded",
        Duration::from_secs(10),
        || batcher.stats().failed_batches == 1,
    )
    .await;
    let stats = batcher.stats();
    assert_eq!(stats.completed_batches, 1);
    assert_eq!(stats.recovered_events, 2);
    assert_eq!(stats.expired_events, 0);
    assert_eq!(stats.in_flight_batches, 0);

    wait_until("terminal object cleaned", Duration::from_secs(10), || {
        fixture.published() == 0
    })
    .await;
    assert!(matches!(
        fixture.store().inspect(id),
        Err(SpoolError::NotFound)
    ));
    // 有终态失败就不得报完成，即使已无待交付事件。
    assert!(matches!(
        batcher.shutdown().await,
        Err(LangfuseError::IngestionApi(_))
    ));
}

#[tokio::test]
async fn stats_count_evicted_admissions_without_delivery() {
    let server = Server::start(&[200]).await;
    server.hold();
    let fixture = Fixture::open(&server.url);
    let mut config = config();
    config.max_events = 1;
    config.queue_capacity = 2;
    config.max_in_flight = 1;
    config.backpressure = BackpressurePolicy::DropOldest;
    let batcher = fixture.batcher(config);

    batcher.try_add(event("first")).expect("first admission");
    wait_until("first batch in flight", Duration::from_secs(5), || {
        server.count() == 1
    })
    .await;
    batcher.try_add(event("second")).expect("second admission");
    batcher.try_add(event("third")).expect("third admission");
    assert!(
        batcher.try_add(event("fourth")).is_ok(),
        "队列满时替换最旧事件而不是拒绝新事件"
    );
    let stats = batcher.stats();
    assert_eq!(stats.accepted_events, 4);
    assert_eq!(stats.evicted_events, 1, "驱逐只终结最旧一条");
    assert_eq!(batcher.dropped_count(), 1, "驱逐与丢弃计数同源");

    server.release();
    assert!(matches!(
        batcher.shutdown().await,
        Ok(ShutdownReport::Complete)
    ));
    let names = delivered_names(&server);
    assert_eq!(
        names,
        BTreeSet::from(["first".to_owned(), "third".to_owned(), "fourth".to_owned()]),
        "被驱逐的事件不上线"
    );
    assert_eq!(batcher.stats().completed_batches, 3);
    assert_eq!(batcher.stats().evicted_events, 1);
}
