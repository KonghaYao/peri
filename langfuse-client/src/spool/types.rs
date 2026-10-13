//! 暂存对象身份、元数据、所有权句柄与存储端口（契约 §3.2）。
//! 类型分三类：可复制身份（[`ObjectId`]、[`RouteFingerprint`]）、可克隆元数据（[`ObjectMeta`]）与
//! 不可克隆句柄（[`TemporaryObject`]、[`ObjectClaim`]、[`SpoolObject`]）。句柄正文不可变，不暴露
//! 宿主路径，也不能在 crate 外伪造；端口方法同步且可能阻塞，只在有并发上限的 blocking job 中执行。

use std::io::Write;
use std::sync::{Arc, Mutex};

use crate::config::Compression;

use super::{
    cleanup::CleanupRegistration, error::SpoolError, sealed, SpoolReader, SpoolResult,
    MAX_PAGE_CURSOR_BYTES,
};

/// 对象标识；私有字节只能由 crate 内构造器生成。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ObjectId([u8; 16]);

/// 部署 route 摘要；凭据轮换换算出新摘要，从而隔离旧对象。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RouteFingerprint([u8; 32]);

/// 分页游标；由端口返回，调用方只能原样回传。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageCursor(Vec<u8>);

/// 临时对象与已完成对象：`Input` 是冻结快照（不可发送），`Export` 是可重读的标准 OTLP 正文。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    Input,
    Export,
}

/// 持久状态机（契约 §5.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpoolState {
    /// 正在写入：临时目录中，尚未原子发布。
    Writing,
    /// 已完整发布：Export 才算持久接受，Input 仅为快照。
    Ready,
    /// 已 checkpoint 为发送中；同一 payload 只允许一个 claim。
    Sending,
    /// 可恢复错误周期耗尽，等待下一次允许时间。
    RetryWait,
    /// 已持久记录交付终态，等待清理正文与描述。
    Delivered,
    /// 永久失败或超期，等待按同一借用协议删除。
    TerminalFailed,
}

/// 对象元数据（descriptor v1）；`version` 与长度/摘要/期限由 publish 校验后写入。
#[derive(Debug, Clone)]
pub struct ObjectMeta {
    pub version: u16,
    pub id: ObjectId,
    pub kind: ObjectKind,
    pub route: RouteFingerprint,
    pub event_count: u64,
    pub encoding: Compression,
    pub raw_len: u64,    // 解压后 JSON 的字节数；Input 为冻结 JSON 长度
    pub stored_len: u64, // 实际落盘字节数（含内容编码）；摘要针对该字节序列
    pub sha256: [u8; 32],
    pub accepted_at_ms: i64,
    pub expires_at_ms: i64,
    pub attempts: u64,
    pub next_attempt_at_ms: Option<i64>,
    pub state: SpoolState,
}

/// 一页元数据；`next` 为下一页游标，`None` 表示本轮扫描结束。
#[derive(Debug, Clone)]
pub struct SpoolPage {
    pub entries: Vec<ObjectMeta>,
    pub next: Option<PageCursor>,
}

/// 配额使用量快照：`reserved` 是已租用但可能尚未写入的额度，`stored` 是已发布对象占用（含 descriptor）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpoolUsage {
    pub reserved_bytes: u64,
    pub stored_bytes: u64,
    pub objects: u64,
}

/// 已预留的配额额度；Drop 只回收未写部分，已写字节必须在确认删除后返还。
pub trait QuotaReservation: Send {
    /// 本 reservation 覆盖的字节数。
    fn bytes(&self) -> u64;
}

/// 正在写入的临时对象；`publish` 把临时写锁无缝转成交付句柄的 claim。
pub trait TemporaryObject: Write + Send {
    fn id(&self) -> ObjectId;

    /// 已写入字节数；`publish` 前用于核对 reservation。
    fn written_len(&self) -> u64;

    /// 核对实测长度/摘要后原子发布；失败由原清理槽接管产物。
    fn publish(self: Box<Self>, meta: ObjectMeta) -> SpoolResult<SpoolObject>;

    /// 放弃写入并清理临时产物；已写字节在确认删除后才返还配额。
    fn abort(self: Box<Self>) -> SpoolResult<()>;
}

/// 对象的独占交付句柄；`claim = None` 表示别的 owner 持锁。
pub trait ObjectClaim: Send {
    fn meta(&self) -> &ObjectMeta;

    /// 从 0 读；只能读完整发布对象。
    fn open(&self) -> SpoolResult<SpoolReader>;

    /// 持久更新状态/尝试次数；成功后才允许发出本次请求。
    fn checkpoint(&mut self, meta: ObjectMeta) -> SpoolResult<()>;

    /// 幂等删除正文与描述；忙对象返回 [`SpoolError::Busy`]。
    fn delete(&mut self) -> SpoolResult<()>;
}

/// 文件端口；封闭实现范围——只有 langfuse-client crate 内可实现（契约 §3.2）。
pub trait SpoolStore: sealed::Sealed + Send + Sync {
    /// 预留额度；持有 token 直到写入或返还。
    fn reserve(&self, bytes: u64) -> SpoolResult<Box<dyn QuotaReservation>>;

    /// 在已登记的清理槽下创建临时对象，并消费 reservation。
    fn create(
        &self,
        quota: Box<dyn QuotaReservation>,
        kind: ObjectKind,
        cleanup: CleanupRegistration,
    ) -> SpoolResult<Box<dyn TemporaryObject>>;

    fn inspect(&self, id: ObjectId) -> SpoolResult<ObjectMeta>;

    fn open(&self, id: ObjectId) -> SpoolResult<SpoolReader>;

    /// 分页扫描可见的完整对象；删除/插入期间允许重复，由 ObjectId 去重。
    fn list(&self, after: Option<PageCursor>, limit: usize) -> SpoolResult<SpoolPage>;

    /// 领取对象；`Ok(None)` 表示已被其他 owner 持有。
    fn claim(&self, id: ObjectId) -> SpoolResult<Option<Box<dyn ObjectClaim>>>;

    fn delete(&self, id: ObjectId) -> SpoolResult<()>;

    fn usage(&self) -> SpoolResult<SpoolUsage>;
}

/// 已发布对象：持 store、元数据、交付句柄与原清理槽；Drop 只登记待清理状态（由 worker 执行实际
/// delete），不在 Drop 中做 IO、分配或 spawn。
pub struct SpoolObject {
    pub(crate) store: Arc<dyn SpoolStore>,
    pub(crate) meta: ObjectMeta,
    pub(crate) claim: Mutex<Option<Box<dyn ObjectClaim>>>,
    pub(crate) cleanup: CleanupRegistration,
}

impl SpoolObject {
    /// 只读元数据；同步、无 IO。
    pub fn meta(&self) -> &ObjectMeta {
        &self.meta
    }

    /// 交付/发送 owner 取走独占句柄；`None` 表示已不在本对象手里。
    pub(crate) fn take_claim(&self) -> Option<Box<dyn ObjectClaim>> {
        self.lock_claim().take()
    }

    /// blocking job 归还取走的同一句柄；已持有句柄时返回 [`SpoolError::Busy`]。
    pub(crate) fn return_claim(&self, claim: Box<dyn ObjectClaim>) -> SpoolResult<()> {
        let mut held = self.lock_claim();
        if held.is_some() {
            return Err(SpoolError::Busy);
        }
        *held = Some(claim);
        Ok(())
    }

    /// crate 内唯一构造入口：publish 与恢复都必须经此校验 store/id 绑定，失败交原槽清理。
    pub(crate) fn from_claim(
        store: Arc<dyn SpoolStore>,
        claim: Box<dyn ObjectClaim>,
        mut cleanup: CleanupRegistration,
    ) -> SpoolResult<Self> {
        let meta = claim.meta().clone();
        if cleanup.slot.id() != Some(meta.id) || !Arc::ptr_eq(cleanup.slot.store(), &store) {
            cleanup.abandon(Some(claim));
            return Err(SpoolError::InvalidMetadata);
        }
        Ok(Self {
            store,
            meta,
            claim: Mutex::new(Some(claim)),
            cleanup,
        })
    }

    fn lock_claim(&self) -> std::sync::MutexGuard<'_, Option<Box<dyn ObjectClaim>>> {
        self.claim
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Drop for SpoolObject {
    fn drop(&mut self) {
        let claim = self
            .claim
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        self.cleanup.abandon(claim);
    }
}

impl ObjectId {
    pub(crate) fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    pub(crate) fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl RouteFingerprint {
    pub(crate) fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl PageCursor {
    pub(crate) fn try_from_bytes(bytes: Vec<u8>) -> SpoolResult<Self> {
        (bytes.len() <= MAX_PAGE_CURSOR_BYTES)
            .then_some(Self(bytes))
            .ok_or(SpoolError::InvalidMetadata)
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[cfg(test)]
#[path = "types_test.rs"]
mod tests;
