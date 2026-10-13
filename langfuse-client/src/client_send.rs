//! 发送子任务、读泵与 attempt checkpoint（契约 §3.5）。
//!
//! 每个 attempt（含周期内全部 retry）都严格按「drain 上次读泵 → checkpoint → 从 byte 0
//! 重开正文并重建请求 → HTTP execute → 完整响应解析 → drain 读泵」执行：不依赖
//! `Request::try_clone`，同一个不可变正文每次从头重开，请求逐次重建。
//!
//! 读泵是 `spawn_blocking` 的 blocking job，join 句柄、取消位与固定池工作区许可都握在
//! [`BodyTaskOwner`] 手里：没有脱离 owner 的 spawn，owner 被丢弃时读泵由取消位与通道关闭
//! 收敛；`drain` 才确认 join，并按其结果决定是否允许上报成功。
//!
//! checkpoint 有两条实现：内存路径的 [`MemoryCheckpoint`] 只登记累计尝试次数，文件路径的
//! [`FileCheckpoint`] 独占交付句柄，在 blocking job 里持久更新 `attempts`/`state` 后才允许
//! 发出请求；后者由发送 owner 在周期开始时构造、周期结束时收回句柄。

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::client::{body, LangfuseClient};
use crate::config::Compression;
use crate::encoding::STREAM_CHUNK_BYTES;
use crate::error::LangfuseError;
use crate::memory::WorkspaceLease;
use crate::payload::{ExportPayload, PayloadBody};
use crate::spool::{ObjectClaim, ObjectId, ObjectMeta, SpoolError, SpoolState, SpoolStore};

/// 单个发送周期的结果；`attempts` 是累计持久水位（内存路径为累计登记水位），
/// 由 checkpoint 预先登记，调用方不再次相加。
pub(crate) enum SendOutcome {
    Delivered,
    Retryable {
        error: LangfuseError,
        retry_after: Duration,
        attempts: u64,
    },
    Terminal {
        error: LangfuseError,
        attempts: u64,
    },
    CheckpointFailed {
        error: LangfuseError,
        attempts: u64,
    },
}

impl SendOutcome {
    /// 持久终态：只有确认送达才是 `Delivered`，其余（含 checkpoint 失败与周期耗尽）都是
    /// `TerminalFailed`——它们都不是 HTTP 成功，也不能被清理协议当成已交付。
    pub(crate) fn terminal_state(&self) -> SpoolState {
        match self {
            SendOutcome::Delivered => SpoolState::Delivered,
            _ => SpoolState::TerminalFailed,
        }
    }

    /// 非送达结果的分类错误；Delivered 返回 `None`。
    pub(crate) fn into_error(self) -> Option<LangfuseError> {
        match self {
            SendOutcome::Delivered => None,
            SendOutcome::Retryable { error, .. }
            | SendOutcome::Terminal { error, .. }
            | SendOutcome::CheckpointFailed { error, .. } => Some(error),
        }
    }
}

/// 每次 attempt 之前的持久化通道；只提供登记，不参与重试分类。
pub(crate) trait AttemptCheckpoint: Send {
    fn before_attempt(
        &mut self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<u64, LangfuseError>> + Send + '_>>;
}

/// direct ingest 的累计登记水位：无磁盘 checkpoint，只有预先计数的尝试次数。
pub(crate) struct MemoryCheckpoint {
    attempts: u64,
}

impl MemoryCheckpoint {
    pub(crate) fn new() -> Self {
        Self { attempts: 0 }
    }
}

impl AttemptCheckpoint for MemoryCheckpoint {
    fn before_attempt(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Result<u64, LangfuseError>> + Send + '_>> {
        Box::pin(async move {
            self.attempts = self.attempts.checked_add(1).ok_or_else(|| {
                LangfuseError::IngestionApi("OTLP attempt counter overflowed".into())
            })?;
            Ok(self.attempts)
        })
    }
}

/// checkpoint 写盘的 blocking job 句柄：`Pin<Box<..>>` 让取消 await 时句柄仍归 owner。
type PendingCheckpointWrite = Pin<Box<JoinHandle<Result<(), LangfuseError>>>>;

/// 文件路径的 attempt checkpoint：独占从 [`crate::spool::SpoolObject`] 取出的交付句柄。
///
/// 每次 `before_attempt` 在 blocking job 里按当前元数据写回 `attempts`、`state=Sending` 与
/// `next_attempt_at_ms=None`，成功后才返回累计水位——同一次 checkpoint 之后才允许读正文并发
/// 请求。正文身份与期限字段整份沿用原 meta，重试不重置 TTL；端口自身也拒绝改写这些字段。
///
/// job 句柄留在 `pending` 里：await 被取消（周期超时、上层 drop）时既不丢句柄也不丢 claim，
/// 下一次 `before_attempt` 或 [`FileCheckpoint::into_claim`] 先收口它。
pub(crate) struct FileCheckpoint {
    /// 独占交付句柄；blocking job 取出、写完后原样返还，句柄不跨 await 持有。
    claim: Arc<Mutex<Option<Box<dyn ObjectClaim>>>>,
    /// 最近一次确认的元数据；只改 attempts/state/next_attempt_at_ms 三个字段。
    meta: ObjectMeta,
    /// 累计持久水位；`meta.attempts` 始终与它一致。
    attempts: u64,
    /// 已登记未确认的持久写；句柄归 owner，await 被取消也不丢。
    pending: Option<PendingCheckpointWrite>,
}

impl FileCheckpoint {
    /// 由发送 owner 用取走的独占句柄构造；此刻不写盘、不 spawn。
    pub(crate) fn new(claim: Box<dyn ObjectClaim>) -> Self {
        let meta = claim.meta().clone();
        Self {
            attempts: meta.attempts,
            claim: Arc::new(Mutex::new(Some(claim))),
            meta,
            pending: None,
        }
    }

    /// 终态 checkpoint：把 `state` 与累计水位持久化后才允许清理。
    ///
    /// 与每次 attempt 的登记共用同一条写入路径（同一 blocking job、同一句柄归还协议）；
    /// 失败不删除对象、不报告已交付，调用方按未确认显式上报。
    pub(crate) async fn checkpoint_terminal(
        &mut self,
        state: SpoolState,
    ) -> Result<(), LangfuseError> {
        let mut meta = self.meta.clone();
        meta.state = state;
        meta.next_attempt_at_ms = None;
        self.write_meta(meta).await?;
        self.meta.state = state;
        self.meta.next_attempt_at_ms = None;
        Ok(())
    }

    /// 收回独占句柄（终态 checkpoint、删除或交回对象都由调用方经它完成）。
    pub(crate) async fn into_claim(mut self) -> Result<Box<dyn ObjectClaim>, LangfuseError> {
        self.settle_pending().await?;
        self.claim
            .lock()
            .map_err(|_| poisoned_slot())?
            .take()
            .ok_or(LangfuseError::Spool(SpoolError::Unavailable))
    }

    /// 收口上一次未确认的持久写：await 被取消时句柄仍留在 `pending` 里归本 owner。
    async fn settle_pending(&mut self) -> Result<(), LangfuseError> {
        let Some(job) = self.pending.as_mut() else {
            return Ok(());
        };
        let result = job.await;
        self.pending = None;
        match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(LangfuseError::IngestionApi(
                "telemetry checkpoint job did not complete".into(),
            )),
        }
    }

    /// 在 blocking job 内写一次元数据；句柄只在这一次 job 内易主，无论成败都原样返还。
    async fn write_meta(&mut self, meta: ObjectMeta) -> Result<(), LangfuseError> {
        self.settle_pending().await?;
        let slot = Arc::clone(&self.claim);
        self.pending = Some(Box::pin(tokio::task::spawn_blocking(move || {
            let mut held = slot.lock().map_err(|_| poisoned_slot())?;
            let mut claim = held
                .take()
                .ok_or(LangfuseError::Spool(SpoolError::Unavailable))?;
            let result = claim.checkpoint(meta).map_err(LangfuseError::Spool);
            *held = Some(claim);
            result
        })));
        self.settle_pending().await
    }
}

impl AttemptCheckpoint for FileCheckpoint {
    fn before_attempt(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Result<u64, LangfuseError>> + Send + '_>> {
        Box::pin(async move {
            let attempts = self.attempts.checked_add(1).ok_or_else(|| {
                LangfuseError::IngestionApi("OTLP attempt counter overflowed".into())
            })?;
            let mut meta = self.meta.clone();
            meta.attempts = attempts;
            meta.state = SpoolState::Sending;
            meta.next_attempt_at_ms = None;
            self.write_meta(meta).await?;
            self.attempts = attempts;
            self.meta.attempts = attempts;
            self.meta.state = SpoolState::Sending;
            self.meta.next_attempt_at_ms = None;
            Ok(attempts)
        })
    }
}

fn poisoned_slot() -> LangfuseError {
    LangfuseError::IngestionApi("telemetry checkpoint slot poisoned".into())
}

/// 本次 attempt 的正文所有权：读泵 join、取消位与 HTTP 工作区许可。
///
/// 字段私有：调用方只能通过 [`BodyTaskOwner::drain`] 确认读泵已停，不能自己 join 或复用流。
pub(crate) struct BodyTaskOwner {
    pump: Option<Pump>,
    /// 文件读泵的桥接缓冲工作区（固定池租约）；内存正文不需要。
    lease: Option<WorkspaceLease>,
}

struct Pump {
    cancel: Arc<AtomicBool>,
    /// 读泵 join 句柄；只在确认 join 后才取出——`drain` 的 await 被取消（周期超时、
    /// 上层 drop）时句柄仍归本 owner，下一次 `drain` 继续等待同一个任务。
    job: Option<JoinHandle<()>>,
}

impl BodyTaskOwner {
    pub(crate) fn new() -> Self {
        Self {
            pump: None,
            lease: None,
        }
    }

    /// 为本次 attempt 从 byte 0 打开正文；内存正文克隆共享 `Bytes`，文件正文重开读泵。
    ///
    /// 上一次的读泵必须先经 [`BodyTaskOwner::drain`] 确认收敛：句柄不会在这里被静默丢弃。
    pub(crate) fn arm(&mut self, payload: &ExportPayload) -> Result<reqwest::Body, LangfuseError> {
        if self.pump.is_some() {
            return Err(LangfuseError::Config(
                "OTLP body read pump must be drained before the next attempt".into(),
            ));
        }
        match &payload.body {
            PayloadBody::Memory(bytes) => Ok(body::memory_body(bytes.bytes().clone())),
            PayloadBody::File(object) => {
                // 通道与读块从 payload 绑定的部署预算取工作区，不在 client 内另建预算。
                let lease = payload.budget().try_http_workspace()?;
                let (sender, chunks) = mpsc::channel(2);
                let cancel = Arc::new(AtomicBool::new(false));
                let job = spawn_file_pump(
                    Arc::clone(&object.store),
                    object.meta().id,
                    Arc::clone(&cancel),
                    sender,
                );
                self.lease = Some(lease);
                self.pump = Some(Pump {
                    cancel,
                    job: Some(job),
                });
                body::file_body(chunks)
            }
        }
    }

    /// 停止并 join 读泵、归还工作区；返回后本 attempt 不再有正文读取在跑。
    ///
    /// await 被取消时**不**取走句柄与工作区：读泵任务仍由本 owner 持有，可重入等待。
    /// join 明确失败（任务 panic/被 abort）时任务已终结，句柄与租约一并结清，不再重复 poll。
    pub(crate) async fn drain(&mut self) -> Result<(), LangfuseError> {
        let Some(pump) = self.pump.as_mut() else {
            return Ok(());
        };
        pump.cancel.store(true, Ordering::Release);
        let mut failed = false;
        if let Some(job) = pump.job.as_mut() {
            failed = job.await.is_err();
        }
        self.pump = None;
        self.lease = None;
        if failed {
            return Err(LangfuseError::IngestionApi(
                "OTLP body read pump did not complete".into(),
            ));
        }
        Ok(())
    }
}

/// 未确认 join 的读泵：连工作区租约一起交给常驻回收任务，等到任务真的结束才释放内存。
///
/// 取消位只能让读泵在下一个检查点停下，既不能中断已经进入的阻塞 IO，也不能归还它占用的
/// 工作区许可。因此「确认不了 join」时既不丢弃句柄、也不假装已归还租约（契约 D6）。
struct OrphanPump {
    cancel: Arc<AtomicBool>,
    job: Option<JoinHandle<()>>,
    lease: Option<WorkspaceLease>,
}

impl OrphanPump {
    /// 等读泵真正返回；阻塞 IO 卡住时这里只会延迟回收，不阻塞 runtime 线程。
    async fn join(self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(job) = self.job {
            let _ = job.await;
        }
        drop(self.lease);
    }
}

/// 进程级待回收读泵；只在拿不到 runtime（进程退出、runtime 已关）时短暂滞留。
static ORPHAN_PUMPS: Mutex<Vec<OrphanPump>> = Mutex::new(Vec::new());
/// 常驻回收任务是否在跑；同一时刻至多一个。
static REAPER_RUNNING: AtomicBool = AtomicBool::new(false);

/// 把还持有读泵与工作区租约的 owner 移交给常驻回收：绝不静默丢弃未 join 的 owner。
fn hand_to_reaper(owner: &mut BodyTaskOwner) {
    let Some(pump) = owner.pump.take() else {
        return;
    };
    pump.cancel.store(true, Ordering::Release);
    push_orphan(OrphanPump {
        cancel: pump.cancel,
        job: pump.job,
        lease: owner.lease.take(),
    });
}

fn push_orphan(orphan: OrphanPump) {
    ORPHAN_PUMPS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(orphan);
    start_reaper();
}

/// 起一个常驻回收任务（表空即退出）；拿不到 runtime 时先留存，等下一次移交再接管。
fn start_reaper() {
    if REAPER_RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        REAPER_RUNNING.store(false, Ordering::Release);
        return;
    };
    handle.spawn(async move {
        loop {
            let next = ORPHAN_PUMPS
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .pop();
            if let Some(orphan) = next {
                orphan.join().await;
                continue;
            }
            // 与 `push_orphan` 的竞态只有这一处：持锁确认空表后才落标志，push 侧才会重起任务。
            let pending = ORPHAN_PUMPS
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            REAPER_RUNNING.store(false, Ordering::Release);
            if pending.is_empty() || REAPER_RUNNING.swap(true, Ordering::AcqRel) {
                return;
            }
            drop(pending);
        }
    });
}

/// 周期退出前的读泵收口：能在期限内 join 就 join；确认不了（期限已到、阻塞 IO 未返回）
/// 交给常驻回收任务。降级到回收仍然不丢 owner，只是归还延后（契约 §3.5、D6）。
pub(crate) async fn retire(owner: &mut BodyTaskOwner, deadline: peri_time::Instant) {
    if owner.pump.is_none() {
        return;
    }
    if let Ok(Ok(())) = peri_time::timeout_at(deadline, owner.drain()).await {
        return;
    }
    hand_to_reaper(owner);
}

impl Drop for BodyTaskOwner {
    /// Drop 不能 await（D6），但也不能只置取消位就丢句柄与租约：未确认 join 的读泵整体移交
    /// 常驻回收任务，由它等到任务结束再归还工作区。
    fn drop(&mut self) {
        hand_to_reaper(self);
    }
}

/// 文件读泵：blocking job 每次最多读一块，每块投进容量 2 的桥接 channel。
///
/// 对象不可变，每次 attempt 都从 byte 0 重新 `open`；读到的字节数由 payload 的 `wire_len`
/// 与端口元数据交叉核对，读泵自身不改变正文。
fn spawn_file_pump(
    store: Arc<dyn SpoolStore>,
    id: ObjectId,
    cancel: Arc<AtomicBool>,
    sender: mpsc::Sender<Result<Bytes, std::io::Error>>,
) -> JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        let mut reader = match store.open(id) {
            Ok(reader) => reader,
            Err(error) => {
                deliver(&sender, &cancel, Err(spool_io_error(error)));
                return;
            }
        };
        let mut chunk = vec![0u8; STREAM_CHUNK_BYTES];
        loop {
            if cancel.load(Ordering::Acquire) {
                return;
            }
            match std::io::Read::read(&mut reader, &mut chunk) {
                Ok(0) => return,
                Ok(read) => {
                    let bytes = Bytes::copy_from_slice(&chunk[..read]);
                    if !deliver(&sender, &cancel, Ok(bytes)) {
                        return;
                    }
                }
                Err(error) => {
                    deliver(&sender, &cancel, Err(error));
                    return;
                }
            }
        }
    })
}

/// 有界投递一块：通道满时只在 1ms 周期上检查取消位，保证 `drain` 不被卡住的读泵拖住。
fn deliver(
    sender: &mpsc::Sender<Result<Bytes, std::io::Error>>,
    cancel: &AtomicBool,
    mut chunk: Result<Bytes, std::io::Error>,
) -> bool {
    loop {
        if cancel.load(Ordering::Acquire) {
            return false;
        }
        match sender.try_send(chunk) {
            Ok(()) => return true,
            Err(mpsc::error::TrySendError::Full(returned)) => {
                chunk = returned;
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(mpsc::error::TrySendError::Closed(_)) => return false,
        }
    }
}

/// 读泵的失败只作为正文流错误上报（reqwest 按传输错误分类），不回显主机路径。
fn spool_io_error(error: crate::spool::SpoolError) -> std::io::Error {
    std::io::Error::other(error.to_string())
}

impl LangfuseClient {
    /// 按 payload 的正文来源重建一次请求；每次调用都从 byte 0 重开正文。
    ///
    /// `Content-Length` 始终是 wire 长度，只有 gzip 才带 `Content-Encoding`；Authorization、
    /// Content-Type 与 `x-langfuse-ingestion-version` 沿用基线。
    pub(crate) async fn build_request(
        &self,
        payload: &ExportPayload,
        body_owner: &mut BodyTaskOwner,
    ) -> Result<reqwest::Request, LangfuseError> {
        let body = body_owner.arm(payload)?;
        let mut builder = self
            .http
            .post(format!("{}/api/public/otel/v1/traces", self.base_url))
            .header("Authorization", &self.auth_header)
            .header("Content-Type", "application/json")
            .header("x-langfuse-ingestion-version", "4")
            .header(reqwest::header::CONTENT_LENGTH, payload.wire_len);
        if payload.encoding == Compression::Gzip {
            builder = builder.header(reqwest::header::CONTENT_ENCODING, "gzip");
        }
        builder
            .body(body)
            .build()
            .map_err(|_| LangfuseError::IngestionApi("OTLP request construction failed".into()))
    }
}

/// 周期期限内的读泵收敛：超时按「未确认 join」上报，不冒充已排空（契约 D6）。
pub(crate) async fn settle(
    owner: &mut BodyTaskOwner,
    deadline: peri_time::Instant,
) -> Result<(), LangfuseError> {
    match peri_time::timeout_at(deadline, owner.drain()).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(error),
        Err(_elapsed) => Err(LangfuseError::IngestionApi(
            "OTLP body read pump did not drain within the export cycle".into(),
        )),
    }
}

#[cfg(test)]
#[path = "client_send_test.rs"]
mod tests;
