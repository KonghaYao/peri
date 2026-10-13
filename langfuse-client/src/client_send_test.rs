//! 发送周期正文所有权与 attempt checkpoint 的契约测试（契约 §3.5、§6、§8）。
//!
//! 覆盖：内存正文无需读泵、同一 attempt 未 drain 前的重复 `arm` 被拒、**取消/超期的 `drain`
//! 不把读泵句柄与工作区租约交给别人**（可重入等待且失败前不放行下一次 attempt）、桥接通道
//! 满时读泵仍按取消位收敛、`settle` 在期限上按「未确认 join」上报、文件 checkpoint 先落水位
//! 再放行、发送结果只把确认送达映射为已交付。

use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use bytes::Bytes;

use super::{retire, settle, AttemptCheckpoint, BodyTaskOwner, FileCheckpoint, SendOutcome};
use crate::client::{ExportConfig, LangfuseClient};
use crate::config::{BatcherConfig, Compression, TelemetryConfig};
use crate::encoding::STREAM_CHUNK_BYTES;
use crate::error::LangfuseError;
use crate::memory::MemoryBudget;
use crate::payload::{BudgetedBytes, ExportPayload, PayloadBody};
use crate::spool::cleanup::CleanupRegistry;
use crate::spool::{
    ObjectClaim, ObjectId, ObjectKind, ObjectMeta, PageCursor, QuotaReservation, RouteFingerprint,
    SpoolError, SpoolObject, SpoolPage, SpoolReader, SpoolResult, SpoolState, SpoolStore,
    SpoolUsage,
};

/// route 摘要只在运行时构造：`from_bytes` 不是常量函数。
fn route() -> RouteFingerprint {
    RouteFingerprint::from_bytes([0x5c; 32])
}

// ─── 端口与读句柄的可观测状态 ───────────────────────────────────────

/// 端口与读句柄共享的计数与读门；读门关着时每个 `read` 先在 blocking 线程上等放行。
#[derive(Default)]
struct Stock {
    opens: AtomicUsize,
    reads: AtomicUsize,
    closes: AtomicUsize,
    gate: Option<Arc<Gate>>,
}

/// 读门：`release` 前所有 `read` 都停在 blocking 线程里，取消位无法打断（这正是读泵收敛
/// 只能由通道关闭或读返回触发的原因）。
struct Gate {
    released: Mutex<bool>,
    waiting: AtomicUsize,
    signal: Condvar,
}

impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            released: Mutex::new(false),
            waiting: AtomicUsize::new(0),
            signal: Condvar::new(),
        })
    }

    fn wait(&self) {
        self.waiting.fetch_add(1, Ordering::SeqCst);
        let mut released = self.released.lock().expect("gate");
        while !*released {
            released = self.signal.wait(released).expect("gate wait");
        }
    }

    fn release(&self) {
        *self.released.lock().expect("gate") = true;
        self.signal.notify_all();
    }

    fn waiting(&self) -> usize {
        self.waiting.load(Ordering::SeqCst)
    }
}

/// 用例失败时也放行读门，避免阻塞线程把测试进程挂在运行时收尾上。
struct GateRelease(Arc<Gate>);

impl Drop for GateRelease {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// 进程内暂存端口双：只实现读泵与 checkpoint 用到的端口方法，字节与元数据留在内存。
struct TestStore {
    next: AtomicU64,
    objects: Mutex<HashMap<ObjectId, Bytes>>,
    metas: Arc<Mutex<HashMap<ObjectId, ObjectMeta>>>,
    stock: Arc<Stock>,
}

impl crate::spool::sealed::Sealed for TestStore {}

impl TestStore {
    fn new() -> Arc<Self> {
        Self::with_gate(None)
    }

    /// 读门关着的端口：读泵会停在第一次 `read` 里，直到用例放行。
    fn gated() -> Arc<Self> {
        Self::with_gate(Some(Gate::new()))
    }

    fn with_gate(gate: Option<Arc<Gate>>) -> Arc<Self> {
        Arc::new(Self {
            next: AtomicU64::new(1),
            objects: Mutex::new(HashMap::new()),
            metas: Arc::new(Mutex::new(HashMap::new())),
            stock: Arc::new(Stock {
                gate,
                ..Default::default()
            }),
        })
    }

    fn put(&self, bytes: &[u8]) -> ObjectId {
        let counter = self.next.fetch_add(1, Ordering::Relaxed);
        let mut raw = [0u8; 16];
        raw[..8].copy_from_slice(&counter.to_be_bytes());
        let id = ObjectId::from_bytes(raw);
        let len = bytes.len() as u64;
        self.objects
            .lock()
            .expect("objects")
            .insert(id, Bytes::copy_from_slice(bytes));
        self.metas.lock().expect("metas").insert(id, meta(id, len));
        id
    }

    fn bytes(&self, id: ObjectId) -> SpoolResult<Bytes> {
        self.objects
            .lock()
            .expect("objects")
            .get(&id)
            .cloned()
            .ok_or(SpoolError::NotFound)
    }

    /// 持久元数据快照（checkpoint 断言用）。
    fn meta(&self, id: ObjectId) -> ObjectMeta {
        self.metas
            .lock()
            .expect("metas")
            .get(&id)
            .cloned()
            .expect("known object")
    }

    fn gate(&self) -> Arc<Gate> {
        Arc::clone(self.stock.gate.as_ref().expect("gated store"))
    }

    fn opens(&self) -> usize {
        self.stock.opens.load(Ordering::SeqCst)
    }

    fn reads(&self) -> usize {
        self.stock.reads.load(Ordering::SeqCst)
    }

    fn closes(&self) -> usize {
        self.stock.closes.load(Ordering::SeqCst)
    }
}

fn meta(id: ObjectId, len: u64) -> ObjectMeta {
    ObjectMeta {
        version: 1,
        id,
        kind: ObjectKind::Export,
        route: route(),
        event_count: 1,
        encoding: Compression::None,
        raw_len: len,
        stored_len: len,
        sha256: [0x3d; 32],
        accepted_at_ms: 0,
        expires_at_ms: 600_000,
        attempts: 0,
        next_attempt_at_ms: None,
        state: SpoolState::Ready,
    }
}

/// 显式 unsize：返回值位置的期望类型会把 `Arc::clone` 的泛型推到 `dyn SpoolStore`。
fn dyn_store(store: &Arc<TestStore>) -> Arc<dyn SpoolStore> {
    let handle: Arc<TestStore> = Arc::clone(store);
    handle
}

impl SpoolStore for TestStore {
    fn reserve(&self, bytes: u64) -> SpoolResult<Box<dyn QuotaReservation>> {
        struct Reserved(u64);
        impl QuotaReservation for Reserved {
            fn bytes(&self) -> u64 {
                self.0
            }
        }
        Ok(Box::new(Reserved(bytes)))
    }

    fn create(
        &self,
        _quota: Box<dyn QuotaReservation>,
        _kind: ObjectKind,
        _cleanup: crate::spool::CleanupRegistration,
    ) -> SpoolResult<Box<dyn crate::spool::TemporaryObject>> {
        Err(SpoolError::Unavailable)
    }

    fn inspect(&self, id: ObjectId) -> SpoolResult<ObjectMeta> {
        self.bytes(id).map(|_| self.meta(id))
    }

    fn open(&self, id: ObjectId) -> SpoolResult<SpoolReader> {
        let bytes = self.bytes(id)?;
        self.stock.opens.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(TestReader {
            bytes,
            position: 0,
            stock: Arc::clone(&self.stock),
        }))
    }

    fn list(&self, _after: Option<PageCursor>, _limit: usize) -> SpoolResult<SpoolPage> {
        Err(SpoolError::Unavailable)
    }

    fn claim(&self, id: ObjectId) -> SpoolResult<Option<Box<dyn ObjectClaim>>> {
        Ok(Some(Box::new(TestClaim {
            meta: self.meta(id),
            bytes: self.bytes(id)?,
            metas: Arc::clone(&self.metas),
        })))
    }

    fn delete(&self, id: ObjectId) -> SpoolResult<()> {
        self.metas.lock().expect("metas").remove(&id);
        self.objects
            .lock()
            .expect("objects")
            .remove(&id)
            .map(|_| ())
            .ok_or(SpoolError::NotFound)
    }

    fn usage(&self) -> SpoolResult<SpoolUsage> {
        let objects = self.objects.lock().expect("objects");
        let stored_bytes = objects.values().map(|bytes| bytes.len() as u64).sum();
        Ok(SpoolUsage {
            reserved_bytes: stored_bytes,
            stored_bytes,
            objects: objects.len() as u64,
        })
    }
}

/// 读泵拿到的同步句柄：按块返回正文，Drop 即证明读泵已返回。
struct TestReader {
    bytes: Bytes,
    position: usize,
    stock: Arc<Stock>,
}

impl Read for TestReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if let Some(gate) = &self.stock.gate {
            gate.wait();
        }
        self.stock.reads.fetch_add(1, Ordering::SeqCst);
        let len = (self.bytes.len() - self.position).min(buf.len());
        buf[..len].copy_from_slice(&self.bytes[self.position..self.position + len]);
        self.position += len;
        Ok(len)
    }
}

impl Drop for TestReader {
    fn drop(&mut self) {
        self.stock.closes.fetch_add(1, Ordering::SeqCst);
    }
}

/// 独占交付句柄：checkpoint 只更新内存里的持久元数据快照。
struct TestClaim {
    meta: ObjectMeta,
    bytes: Bytes,
    metas: Arc<Mutex<HashMap<ObjectId, ObjectMeta>>>,
}

impl ObjectClaim for TestClaim {
    fn meta(&self) -> &ObjectMeta {
        &self.meta
    }

    fn open(&self) -> SpoolResult<SpoolReader> {
        Ok(Box::new(Cursor::new(self.bytes.clone())))
    }

    fn checkpoint(&mut self, meta: ObjectMeta) -> SpoolResult<()> {
        self.metas
            .lock()
            .expect("metas")
            .insert(meta.id, meta.clone());
        self.meta = meta;
        Ok(())
    }

    fn delete(&mut self) -> SpoolResult<()> {
        Ok(())
    }
}

// ─── 正文构造 ───────────────────────────────────────────────────────

/// 只含 HTTP 阶段的部署预算；`max_in_flight=1` 让工作区租约的占用可观测。
fn deployment() -> Arc<MemoryBudget> {
    let config = BatcherConfig {
        max_in_flight: 1,
        telemetry: TelemetryConfig {
            memory_budget_bytes: 8 * 1024 * 1024,
            ..Default::default()
        },
        ..Default::default()
    };
    MemoryBudget::for_batcher(&config, &ExportConfig::default()).expect("deployment budget")
}

fn object(store: &Arc<TestStore>, budget: &Arc<MemoryBudget>, bytes: &[u8]) -> SpoolObject {
    let registry = CleanupRegistry::new(dyn_store(store), Arc::clone(budget), 4);
    let registration = registry.try_register().expect("cleanup slot");
    let id = store.put(bytes);
    registration
        .slot
        .bind(id, bytes.len() as u64)
        .expect("bind");
    let claim = store.claim(id).expect("claim call").expect("available");
    SpoolObject::from_claim(dyn_store(store), claim, registration).expect("from_claim")
}

fn file_payload(
    store: &Arc<TestStore>,
    budget: &Arc<MemoryBudget>,
    bytes: &[u8],
    encoding: Compression,
) -> ExportPayload {
    ExportPayload {
        body: PayloadBody::File(object(store, budget, bytes)),
        encoding,
        raw_len: bytes.len() as u64,
        wire_len: bytes.len() as u64,
        sha256: [0x11; 32],
        event_count: 1,
        budget: Arc::clone(budget),
    }
}

fn memory_payload(budget: &Arc<MemoryBudget>, bytes: &[u8]) -> ExportPayload {
    let permit = budget.try_reserve(bytes.len()).expect("permit");
    ExportPayload {
        body: PayloadBody::Memory(
            BudgetedBytes::new(Bytes::copy_from_slice(bytes), permit).expect("budgeted bytes"),
        ),
        encoding: Compression::None,
        raw_len: bytes.len() as u64,
        wire_len: bytes.len() as u64,
        sha256: [0x22; 32],
        event_count: 1,
        budget: Arc::clone(budget),
    }
}

// ─── 用例 ───────────────────────────────────────────────────────────

/// 等待读泵停在受控读里；失败时先放行，避免阻塞线程把进程挂在运行时收尾上。
async fn pump_reaches_gated_read(gate: &Gate) {
    for _ in 0..2_000 {
        if gate.waiting() > 0 {
            return;
        }
        peri_time::sleep(Duration::from_millis(1)).await;
    }
    gate.release();
    panic!("读泵没有进入受控读取");
}

/// 等待读泵读满 `expected` 块（此后它会阻塞在容量 2 的桥接通道上）。
async fn pump_reaches_reads(store: &Arc<TestStore>, expected: usize) {
    for _ in 0..2_000 {
        if store.reads() >= expected {
            return;
        }
        peri_time::sleep(Duration::from_millis(1)).await;
    }
    panic!("读泵只读了 {} 块，期望 {expected}", store.reads());
}

#[tokio::test]
async fn memory_body_arms_repeatedly_without_a_pump() {
    let budget = deployment();
    let payload = memory_payload(&budget, br#"{"resourceSpans":[]}"#);
    let mut owner = BodyTaskOwner::new();

    owner.arm(&payload).expect("内存正文可直接 arm");
    owner.drain().await.expect("无读泵时 drain 立即收敛");
    owner.arm(&payload).expect("drain 后可再次 arm");
    owner.drain().await.expect("再次 drain 仍收敛");
}

#[tokio::test]
async fn file_attempt_must_be_drained_before_the_next_arm() {
    let store = TestStore::new();
    let budget = deployment();
    let payload = file_payload(&store, &budget, &[0x5a; 1024], Compression::None);
    let mut owner = BodyTaskOwner::new();

    owner.arm(&payload).expect("文件正文可 arm");
    let rejected = owner
        .arm(&payload)
        .expect_err("未 drain 的读泵不能被静默丢弃");
    assert!(
        matches!(rejected, LangfuseError::Config(_)),
        "重复 arm 必须按配置错误上报，实际 {rejected:?}"
    );

    owner.drain().await.expect("drain 确认读泵收敛");
    // 读泵是 blocking job：只有 drain 确认 join 之后才保证它已开过正文。
    assert_eq!(store.opens(), 1, "每次 arm 只开一次正文");
    assert_eq!(store.closes(), 1, "drain 返回后读句柄已释放");

    owner
        .arm(&payload)
        .expect("drain 后重新 arm 从 byte 0 重开正文");
    owner.drain().await.expect("第二个 attempt 收敛");
    assert_eq!(store.opens(), 2, "第二个 attempt 从 byte 0 重开正文");
    assert_eq!(store.closes(), 2);
}

#[tokio::test]
async fn cancelled_drain_keeps_the_pump_and_the_workspace_lease() {
    let store = TestStore::gated();
    let budget = deployment();
    let payload = file_payload(&store, &budget, &[0x11; 4096], Compression::None);
    let mut owner = BodyTaskOwner::new();
    let _release = GateRelease(store.gate());

    owner.arm(&payload).expect("文件正文可 arm");
    pump_reaches_gated_read(&store.gate()).await;

    // 超期 drain 在 await join 处被丢弃：句柄与租约都必须留在 owner 里。
    let elapsed = peri_time::timeout_at(peri_time::monotonic_now(), owner.drain()).await;
    assert!(elapsed.is_err(), "阻塞读未返回时 drain 不得报告已收敛");
    assert!(
        matches!(owner.arm(&payload), Err(LangfuseError::Config(_))),
        "被取消的 drain 不得放行下一次 attempt"
    );
    assert!(
        matches!(
            budget.try_http_workspace(),
            Err(LangfuseError::MemoryBudgetExceeded { .. })
        ),
        "被取消的 drain 不得提前归还工作区租约"
    );

    store.gate().release();
    owner.drain().await.expect("重入等待同一读泵并确认收敛");
    assert_eq!(store.opens(), 1, "取消不重开正文");
    assert_eq!(store.closes(), 1, "读句柄已释放");
    assert!(budget.try_http_workspace().is_ok(), "租约已归还固定池");
}

#[tokio::test]
async fn settle_reports_an_unconfirmed_pump_at_the_deadline() {
    let store = TestStore::gated();
    let budget = deployment();
    let payload = file_payload(&store, &budget, &[0x22; 2048], Compression::None);
    let mut owner = BodyTaskOwner::new();
    let _release = GateRelease(store.gate());

    owner.arm(&payload).expect("文件正文可 arm");
    pump_reaches_gated_read(&store.gate()).await;

    let settled = settle(&mut owner, peri_time::monotonic_now()).await;
    match settled {
        Err(LangfuseError::IngestionApi(message)) => {
            assert!(
                message.contains("did not drain within the export cycle"),
                "期限收敛失败必须按未确认上报，实际 {message}"
            );
        }
        other => panic!("期限到点必须按未确认 join 上报，实际 {other:?}"),
    }

    store.gate().release();
    owner.drain().await.expect("用例收尾的 drain 收敛");
}

/// 等待常驻回收任务确认读泵 join 并归还工作区租约；到期 panic（不靠固定 sleep 断言进度）。
async fn wait_for_reaper_release(store: &Arc<TestStore>, budget: &Arc<MemoryBudget>) {
    for _ in 0..10_000 {
        if store.closes() > 0 && budget.try_http_workspace().is_ok() {
            return;
        }
        peri_time::sleep(Duration::from_millis(1)).await;
    }
    panic!("常驻回收任务没有在期限内归还读泵与工作区租约");
}

/// [D6] Drop 不能 await，也不能丢句柄：未确认 join 的读泵连同工作区租约整体移交常驻回收，
/// 阻塞读返回前租约不得提前归还，返回后由回收任务结清。
#[tokio::test]
async fn dropped_owner_keeps_the_blocked_pump_until_the_reaper_confirms_it() {
    let store = TestStore::gated();
    let budget = deployment();
    let payload = file_payload(&store, &budget, &[0x66; 4096], Compression::None);
    let mut owner = BodyTaskOwner::new();
    let _release = GateRelease(store.gate());

    owner.arm(&payload).expect("文件正文可 arm");
    pump_reaches_gated_read(&store.gate()).await;
    assert!(
        budget.try_http_workspace().is_err(),
        "读泵持有本次 attempt 的工作区租约"
    );

    drop(owner);
    assert_eq!(store.closes(), 0, "阻塞读未返回时读句柄不可能已释放");
    assert!(
        budget.try_http_workspace().is_err(),
        "未确认 join 不得提前归还租约"
    );

    store.gate().release();
    wait_for_reaper_release(&store, &budget).await;
    assert_eq!(store.closes(), 1, "回收任务确认 join 后读句柄释放");
}

/// [D6] 周期收口在期限上确认不了 join 时走同一条移交路径：不等待阻塞读返回、不提前归还租约，
/// 读泵返回后由常驻回收任务结清。
#[tokio::test]
async fn retired_owner_hands_the_blocked_pump_to_the_reaper() {
    let store = TestStore::gated();
    let budget = deployment();
    let payload = file_payload(&store, &budget, &[0x77; 1024], Compression::None);
    let mut owner = BodyTaskOwner::new();
    let _release = GateRelease(store.gate());

    owner.arm(&payload).expect("文件正文可 arm");
    pump_reaches_gated_read(&store.gate()).await;

    retire(&mut owner, peri_time::monotonic_now()).await;
    assert_eq!(store.closes(), 0, "收口不得等待阻塞读返回");
    assert!(
        budget.try_http_workspace().is_err(),
        "移交不提前归还工作区租约"
    );

    store.gate().release();
    wait_for_reaper_release(&store, &budget).await;
    assert_eq!(store.closes(), 1, "回收任务确认 join 后读句柄释放");
}

#[tokio::test]
async fn blocked_pump_converges_when_the_bridge_channel_stays_full() {
    let store = TestStore::new();
    let budget = deployment();
    let payload = file_payload(
        &store,
        &budget,
        &vec![0x7c; STREAM_CHUNK_BYTES * 3],
        Compression::None,
    );
    let mut owner = BodyTaskOwner::new();

    // 正文体保持存活（通道不关闭），只有取消位能让读泵收敛。
    let body = owner.arm(&payload).expect("文件正文可 arm");
    pump_reaches_reads(&store, 3).await;

    let drained = peri_time::timeout(Duration::from_secs(5), owner.drain()).await;
    assert!(
        matches!(drained, Ok(Ok(()))),
        "无消费者的读泵必须按取消位收敛，实际 {drained:?}"
    );
    assert_eq!(store.reads(), 3, "取消后不再读下一块");
    assert_eq!(store.closes(), 1, "读句柄随读泵返回释放");
    drop(body);
}

#[tokio::test]
async fn file_attempts_rebuild_the_request_from_byte_zero() {
    let client = LangfuseClient::new("pk", "sk", "http://127.0.0.1:9", 0);
    let store = TestStore::new();
    let budget = deployment();
    let plain = file_payload(&store, &budget, &[0x33; 2048], Compression::None);
    let gzipped = file_payload(&store, &budget, &[0x44; 512], Compression::Gzip);
    let mut owner = BodyTaskOwner::new();

    let request = client
        .build_request(&plain, &mut owner)
        .await
        .expect("首个 attempt");
    assert_eq!(
        request.headers()[reqwest::header::CONTENT_LENGTH]
            .to_str()
            .expect("ascii 长度"),
        "2048"
    );
    assert!(request
        .headers()
        .get(reqwest::header::CONTENT_ENCODING)
        .is_none());
    owner.drain().await.expect("首个 attempt 收敛");
    assert_eq!(store.opens(), 1, "首个 attempt 只开一次正文");

    let request = client
        .build_request(&gzipped, &mut owner)
        .await
        .expect("第二个 attempt 重建请求");
    assert_eq!(
        request.headers()[reqwest::header::CONTENT_LENGTH]
            .to_str()
            .expect("ascii 长度"),
        "512"
    );
    assert_eq!(
        request.headers()[reqwest::header::CONTENT_ENCODING]
            .to_str()
            .expect("ascii 编码"),
        "gzip"
    );
    owner.drain().await.expect("第二个 attempt 收敛");
    assert_eq!(store.opens(), 2, "每个 attempt 从 byte 0 重开正文");
}

#[tokio::test]
async fn file_checkpoint_persists_attempts_before_terminal_state() {
    let store = TestStore::new();
    let budget = deployment();
    let object = object(&store, &budget, b"frozen-export-body");
    let id = object.meta().id;
    let mut checkpoint = FileCheckpoint::new(object.take_claim().expect("交付句柄"));

    assert_eq!(checkpoint.before_attempt().await.expect("首个水位"), 1);
    let persisted = store.meta(id);
    assert_eq!(persisted.state, SpoolState::Sending);
    assert_eq!(persisted.attempts, 1);
    assert_eq!(persisted.next_attempt_at_ms, None);

    assert_eq!(checkpoint.before_attempt().await.expect("第二个水位"), 2);
    checkpoint
        .checkpoint_terminal(SpoolState::TerminalFailed)
        .await
        .expect("终态 checkpoint");
    let persisted = store.meta(id);
    assert_eq!(persisted.state, SpoolState::TerminalFailed);
    assert_eq!(persisted.attempts, 2, "终态沿用累计持久水位");
    assert_eq!(persisted.next_attempt_at_ms, None);

    let claim = checkpoint.into_claim().await.expect("句柄原样收回");
    assert_eq!(claim.meta().state, SpoolState::TerminalFailed);
    assert_eq!(claim.meta().attempts, 2);
}

#[tokio::test]
async fn memory_checkpoint_counts_attempts_without_disk() {
    let mut checkpoint = super::MemoryCheckpoint::new();
    assert_eq!(checkpoint.before_attempt().await.expect("首个水位"), 1);
    assert_eq!(checkpoint.before_attempt().await.expect("第二个水位"), 2);
}

#[test]
fn only_delivered_maps_to_the_persisted_delivered_state() {
    let delivered = SendOutcome::Delivered;
    assert_eq!(delivered.terminal_state(), SpoolState::Delivered);
    assert!(delivered.into_error().is_none());

    let outcomes = [
        SendOutcome::Retryable {
            error: LangfuseError::IngestionApi("retry exhausted".into()),
            retry_after: Duration::from_secs(1),
            attempts: 3,
        },
        SendOutcome::Terminal {
            error: LangfuseError::IngestionApi("permanent".into()),
            attempts: 1,
        },
        SendOutcome::CheckpointFailed {
            error: LangfuseError::Spool(SpoolError::Unavailable),
            attempts: 2,
        },
    ];
    for outcome in outcomes {
        assert_eq!(
            outcome.terminal_state(),
            SpoolState::TerminalFailed,
            "非送达结果不得被当成已交付"
        );
        assert!(outcome.into_error().is_some());
    }
}
