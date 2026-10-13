//! 单对象生命周期的 native 实现：临时写入、原子发布、独占领取与幂等删除（契约 §3.2、§5.2）。
//!
//! 发布单元 = `body` + `meta`：正文先 rename 进 `objects/<b2>/<id>/`，描述信息最后原子写入；
//! 没有 `meta` 的对象不可见（`list` 不返回、`claim` 报 [`SpoolError::NotFound`]），读者不会看到
//! 半个正文。失败不发布：临时产物在确认删除后才返还已写配额（不能凭 Drop 声称磁盘已清理）。
//!
//! 所有权规则：writer 由 `tmp/<id>.lock` 证明存活，扫描只清理锁已释放的残留；发布后由
//! `objects/<b2>/<id>/lock` 证明独占，claim/checkpoint/delete/清理都必须先持该锁。损坏对象
//! （长度或摘要不符）在持锁条件下清理，并向调用方显式报错。
//!
//! 描述信息的编解码与格式/隔离域校验在 [`super::native_scan`]，账本与预留结算在
//! [`super::native_quota`]；本模块只做物理写入与所有权。

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest, Sha256};
use tracing::warn;

use crate::spool::error::SpoolError;
use crate::spool::types::{
    ObjectClaim, ObjectId, ObjectKind, ObjectMeta, QuotaReservation, SpoolObject, TemporaryObject,
};
use crate::spool::{CleanupRegistration, SpoolReader, SpoolResult};

use super::native_quota::{self, accounted_bytes};
use super::native_scan;
use super::{
    create_private_dir_all, create_private_file, fsync_dir, id_hex, io_error, open_read_only,
    random_object_id, try_lock_file, write_atomic, LockAttempt, NativeInner, IO_CHUNK_BYTES,
};

/// 正文实际长度；缺失视为损坏（发布单元必须完整）。
pub(crate) fn stored_len(inner: &NativeInner, id: ObjectId) -> SpoolResult<u64> {
    match fs::metadata(inner.object_body_path(id)) {
        Ok(metadata) if metadata.is_file() => Ok(metadata.len()),
        Ok(_) => Err(SpoolError::Corrupt { id }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(SpoolError::Corrupt { id })
        }
        Err(error) => Err(io_error("stat spool body", error)),
    }
}

/// 摘要核对：只流式读取，不把正文整包载入内存。
pub(crate) fn verify_digest(
    inner: &NativeInner,
    id: ObjectId,
    expected: &[u8; 32],
) -> SpoolResult<()> {
    let mut file = open_read_only(&inner.object_body_path(id))
        .map_err(|error| io_error("read spool body", error))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; IO_CHUNK_BYTES];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| io_error("read spool body", error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual: [u8; 32] = hasher.finalize().into();
    if &actual == expected {
        Ok(())
    } else {
        Err(SpoolError::Corrupt { id })
    }
}

fn remove_file_if_exists(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// 持锁删除对象目录并扣回已写空间；幂等，账本更新失败只告警（重建时以磁盘为准）。
pub(crate) fn remove_object(inner: &NativeInner, id: ObjectId, dir: &Path) -> SpoolResult<()> {
    let accounted = match accounted_bytes(dir) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_error("account spool object", error)),
    };
    if let Err(error) = fs::remove_dir_all(dir) {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(io_error("remove spool object", error));
        }
    }
    if let Err(error) = native_quota::record_deleted(inner, accounted) {
        warn!(object = %id_hex(id), error = %error, "telemetry spool quota ledger not updated after delete");
    }
    Ok(())
}

/// 结构 + 长度校验（不含摘要，供 inspect/open 只读快路径使用）。
pub(crate) fn load_validated(inner: &NativeInner, id: ObjectId) -> SpoolResult<ObjectMeta> {
    let meta = native_scan::read_meta(inner, id)?;
    if stored_len(inner, id)? != meta.stored_len {
        return Err(SpoolError::Corrupt { id });
    }
    Ok(meta)
}

/// 只读元数据快照（结构 + 长度 + 期限投影）；不取对象锁，并发发布期间可能报 [`SpoolError::NotFound`]。
pub(crate) fn inspect(inner: &NativeInner, id: ObjectId) -> SpoolResult<ObjectMeta> {
    let mut meta = load_validated(inner, id)?;
    native_scan::project_expiry(&mut meta);
    Ok(meta)
}

pub(crate) fn open_body(inner: &NativeInner, id: ObjectId) -> SpoolResult<SpoolReader> {
    load_validated(inner, id)?;
    let file = open_read_only(&inner.object_body_path(id))
        .map_err(|error| io_error("open spool body", error))?;
    Ok(Box::new(BufReader::with_capacity(IO_CHUNK_BYTES, file)))
}

/// 独占领取；`Ok(None)` 表示别的 owner 持锁（契约 §3.2）。损坏对象持锁清理后显式报错。
pub(crate) fn claim(
    inner: &Arc<NativeInner>,
    id: ObjectId,
) -> SpoolResult<Option<Box<dyn ObjectClaim>>> {
    let dir = inner.object_dir(id);
    if !dir.is_dir() {
        return Err(SpoolError::NotFound);
    }
    let lock = match try_lock_file(&inner.object_lock_path(id))
        .map_err(|error| io_error("lock spool object", error))?
    {
        LockAttempt::Free(lock) => lock,
        LockAttempt::Held => return Ok(None),
        // 对象目录已被并发删除：对象不存在，不是「有人正在发送」。
        LockAttempt::Gone => return Err(SpoolError::NotFound),
    };
    let mut meta = match load_validated(inner, id)
        .and_then(|meta| verify_digest(inner, id, &meta.sha256).map(|()| meta))
    {
        Ok(meta) => meta,
        Err(error) => {
            match remove_object(inner, id, &dir) {
                Ok(()) => warn!(
                    object = %id_hex(id),
                    reason = %error,
                    "telemetry spool discarded corrupt object"
                ),
                Err(error) => warn!(
                    object = %id_hex(id),
                    error = %error,
                    "telemetry spool corrupt object not removed"
                ),
            }
            return Err(error);
        }
    };
    native_scan::project_expiry(&mut meta);
    Ok(Some(Box::new(NativeClaim {
        inner: Arc::clone(inner),
        id,
        meta,
        dir,
        _lock: lock,
    })))
}

/// 幂等删除：不存在返回 `Ok`，忙对象返回 [`SpoolError::Busy`]（契约 §3.2）。
pub(crate) fn delete_if_present(inner: &NativeInner, id: ObjectId) -> SpoolResult<()> {
    let dir = inner.object_dir(id);
    if !dir.exists() {
        return Ok(());
    }
    match try_lock_file(&inner.object_lock_path(id))
        .map_err(|error| io_error("lock spool object", error))?
    {
        LockAttempt::Free(_lock) => remove_object(inner, id, &dir),
        LockAttempt::Held => Err(SpoolError::Busy),
        // 目录已被并发删除：删除目的已达成。
        LockAttempt::Gone => Ok(()),
    }
}

/// 创建写入中的临时对象：先建存活锁，再建正文（扫描据此区分活跃 writer 与残留）。
pub(crate) fn create(
    inner: &Arc<NativeInner>,
    quota: Box<dyn QuotaReservation>,
    kind: ObjectKind,
    cleanup: CleanupRegistration,
) -> SpoolResult<Box<dyn TemporaryObject>> {
    let id = random_object_id();
    let reserved = quota.bytes();
    // 先绑定槽再写第一个字节：句柄必须与「本次写入的 id + 已登记配额」同代，publish 才能核验身份。
    cleanup.slot.bind(id, reserved)?;
    let lock = match try_lock_file(&inner.writer_lock_path(id))
        .map_err(|error| io_error("lock spool writer", error))?
    {
        LockAttempt::Free(lock) => lock,
        LockAttempt::Held => return Err(SpoolError::Busy),
        // 创建路径上 `tmp/` 是本次写入前必须存在的布局目录：缺失是环境漂移，不是并发竞争。
        LockAttempt::Gone => {
            return Err(io_error(
                "create spool temporary object",
                io::Error::from(io::ErrorKind::NotFound),
            ))
        }
    };
    let part = create_private_file(&inner.writer_part_path(id))
        .map_err(|error| io_error("create spool temporary object", error))?;
    Ok(Box::new(NativeTemporaryObject {
        inner: Arc::clone(inner),
        id,
        kind,
        quota: Some(quota),
        cleanup: Some(cleanup),
        writer: BufWriter::with_capacity(IO_CHUNK_BYTES, part),
        hasher: Sha256::new(),
        written: 0,
        reserved,
        finished: false,
        dir_residue: false,
        _lock: lock,
    }))
}

/// writing 状态的对象；`publish` 把临时写锁无缝转成交付句柄的 claim。
///
/// `quota`/`cleanup` 用 `Option` 表达「已被 publish/abort/Drop 之一结算」；`finished` 保证三者
/// 互斥，因此 `expect("cleanup slot")` 只会在内部不变式被破坏时触发。
pub(crate) struct NativeTemporaryObject {
    inner: Arc<NativeInner>,
    id: ObjectId,
    kind: ObjectKind,
    quota: Option<Box<dyn QuotaReservation>>,
    cleanup: Option<CleanupRegistration>,
    writer: BufWriter<File>,
    hasher: Sha256,
    written: u64,
    reserved: u64,
    finished: bool,
    dir_residue: bool,
    /// writer 存活锁：Drop 即释放。
    _lock: File,
}

impl NativeTemporaryObject {
    /// 删除临时正文与存活锁；`Ok` 表示磁盘上已无可回收残留。
    fn discard_temp(&mut self) -> std::io::Result<()> {
        remove_file_if_exists(&self.inner.writer_part_path(self.id))
            .and(remove_file_if_exists(&self.inner.writer_lock_path(self.id)))
    }

    /// 失败路径统一收尾：尽力删除临时产物，并记录是否仍有残留。
    fn failure(&mut self, error: SpoolError) -> SpoolError {
        if self.discard_temp().is_err() {
            self.dir_residue = true;
        }
        error
    }

    fn remove_own_object_dir(&mut self) {
        let dir = self.inner.object_dir(self.id);
        if fs::remove_dir_all(&dir).is_err() && dir.exists() {
            self.dir_residue = true;
        }
    }

    /// 放弃写入后的统一结算：释放预留语义、回收或归还清理槽；返回是否有残留。
    fn settle_unpublished(&mut self, residue: bool) -> bool {
        drop(self.quota.take());
        if let Err(error) = native_quota::reconcile(&self.inner) {
            warn!(error = %error, "telemetry spool quota ledger reconcile skipped");
        }
        if let Some(mut cleanup) = self.cleanup.take() {
            if residue {
                // 有残留：归还原槽（启动清扫兜底），不谎报已删除，也不提前释放已写配额。
                cleanup.abandon(None);
            } else {
                cleanup.disarm_after_delete();
            }
        }
        residue
    }

    fn publish_inner(&mut self, meta: ObjectMeta) -> SpoolResult<NativeClaim> {
        if meta.id != self.id || meta.kind != self.kind {
            return Err(self.failure(SpoolError::InvalidMetadata));
        }
        if let Err(error) = native_scan::validate_meta(&self.inner, &meta) {
            return Err(self.failure(error));
        }
        // 1. 完成编码收尾并 fsync，然后核对实测长度与摘要（writer 不得写超预留）。
        if let Err(error) = self.writer.flush() {
            return Err(self.failure(io_error("flush spool temporary object", error)));
        }
        if let Err(error) = self.writer.get_ref().sync_all() {
            return Err(self.failure(io_error("sync spool temporary object", error)));
        }
        let digest: [u8; 32] = self.hasher.clone().finalize().into();
        if self.written != meta.stored_len || digest != meta.sha256 {
            return Err(self.failure(SpoolError::Corrupt { id: self.id }));
        }
        // 2. 同文件系统原子发布：正文 rename 与描述信息都在对象锁内完成。
        let dir = self.inner.object_dir(self.id);
        if dir.exists() {
            return Err(self.failure(SpoolError::Corrupt { id: self.id }));
        }
        if let Err(error) = create_private_dir_all(&dir) {
            return Err(self.failure(io_error("create spool object directory", error)));
        }
        let lock = match try_lock_file(&self.inner.object_lock_path(self.id)) {
            Ok(LockAttempt::Free(lock)) => lock,
            Ok(LockAttempt::Held) | Ok(LockAttempt::Gone) => {
                self.remove_own_object_dir();
                return Err(self.failure(SpoolError::Corrupt { id: self.id }));
            }
            Err(error) => {
                self.remove_own_object_dir();
                return Err(self.failure(io_error("lock spool object", error)));
            }
        };
        let published = fs::rename(
            self.inner.writer_part_path(self.id),
            self.inner.object_body_path(self.id),
        )
        .map_err(|error| io_error("publish spool body", error))
        .and_then(|()| fsync_dir(&dir).map_err(|error| io_error("sync spool object", error)))
        .and_then(|()| {
            fsync_dir(&self.inner.bucket_dir(NativeInner::bucket_of(self.id)))
                .map_err(|error| io_error("sync spool bucket", error))
        })
        .and_then(|()| {
            let bytes = native_scan::descriptor_bytes(&meta)?;
            write_atomic(&self.inner.object_meta_path(self.id), &bytes)
                .map_err(|error| io_error("publish spool descriptor", error))
        });
        if let Err(error) = published {
            self.remove_own_object_dir();
            return Err(self.failure(error));
        }
        // 3. 描述信息已持久：正文与描述成为可恢复单元，writer 存活锁不再需要。
        let _ = remove_file_if_exists(&self.inner.writer_lock_path(self.id));
        let accounted = accounted_bytes(&dir).unwrap_or(0);
        if let Err(error) = native_quota::record_stored(&self.inner, accounted) {
            // 对象已持久，不能因账本缓存失败丢弃已落盘数据；缓存由重建/reserve 自愈。
            warn!(object = %id_hex(self.id), error = %error, "telemetry spool quota ledger not updated after publish");
        }
        // 4. 预留转实际存储：释放预留存活锁后回收账本条目，避免双计。
        drop(self.quota.take());
        if let Err(error) = native_quota::reconcile(&self.inner) {
            warn!(error = %error, "telemetry spool quota ledger reconcile skipped");
        }
        Ok(NativeClaim {
            inner: Arc::clone(&self.inner),
            id: self.id,
            meta,
            dir,
            _lock: lock,
        })
    }
}

impl Write for NativeTemporaryObject {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.written + buf.len() as u64 > self.reserved {
            // 写超预留即失败：配额不得被 writer 悄悄突破，由 caller 重新 sizing。
            return Err(std::io::Error::other(
                "telemetry spool write exceeds the reserved quota",
            ));
        }
        let written = self.writer.write(buf)?;
        self.hasher.update(&buf[..written]);
        self.written += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

impl TemporaryObject for NativeTemporaryObject {
    fn id(&self) -> ObjectId {
        self.id
    }

    fn written_len(&self) -> u64 {
        self.written
    }

    fn publish(mut self: Box<Self>, meta: ObjectMeta) -> SpoolResult<SpoolObject> {
        self.finished = true;
        let claim = match self.publish_inner(meta) {
            Ok(claim) => claim,
            Err(error) => {
                self.settle_unpublished(self.dir_residue);
                return Err(error);
            }
        };
        let cleanup = self.cleanup.take().expect("cleanup slot");
        // 交给交付句柄的 store 必须与清理槽登记的部署身份同一 `Arc`（`from_claim` 逐项核验）。
        let store = Arc::clone(cleanup.slot.store());
        SpoolObject::from_claim(store, Box::new(claim), cleanup)
    }

    fn abort(mut self: Box<Self>) -> SpoolResult<()> {
        self.finished = true;
        let discarded = self.discard_temp();
        if self.settle_unpublished(self.dir_residue || discarded.is_err()) {
            return Err(io_error(
                "discard spool temporary object",
                discarded.err().unwrap_or_else(|| {
                    std::io::Error::other("telemetry spool temporary object residue")
                }),
            ));
        }
        Ok(())
    }
}

impl Drop for NativeTemporaryObject {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let discarded = self.discard_temp();
        if self.settle_unpublished(self.dir_residue || discarded.is_err()) {
            warn!(object = %id_hex(self.id), "telemetry spool temporary object left for startup sweep");
        }
    }
}

/// 独占交付句柄：持锁期间其他进程 `claim` 返回 `None`。
pub(crate) struct NativeClaim {
    inner: Arc<NativeInner>,
    id: ObjectId,
    meta: ObjectMeta,
    dir: PathBuf,
    /// OS advisory lock；文件句柄存活即代表所有权。
    _lock: File,
}

impl ObjectClaim for NativeClaim {
    fn meta(&self) -> &ObjectMeta {
        &self.meta
    }

    fn open(&self) -> SpoolResult<SpoolReader> {
        open_body(&self.inner, self.id)
    }

    fn checkpoint(&mut self, meta: ObjectMeta) -> SpoolResult<()> {
        if meta.id != self.id || meta.route != self.meta.route || meta.kind != self.meta.kind {
            return Err(SpoolError::InvalidMetadata);
        }
        // 正文身份与期限不可被 checkpoint 改写；只有尝试/状态字段会变化。
        let immutable = meta.version == self.meta.version
            && meta.event_count == self.meta.event_count
            && meta.encoding == self.meta.encoding
            && meta.raw_len == self.meta.raw_len
            && meta.stored_len == self.meta.stored_len
            && meta.sha256 == self.meta.sha256
            && meta.accepted_at_ms == self.meta.accepted_at_ms
            && meta.expires_at_ms == self.meta.expires_at_ms;
        if !immutable {
            return Err(SpoolError::InvalidMetadata);
        }
        native_scan::validate_meta(&self.inner, &meta)?;
        let bytes = native_scan::descriptor_bytes(&meta)?;
        write_atomic(&self.inner.object_meta_path(self.id), &bytes)
            .map_err(|error| io_error("checkpoint spool descriptor", error))?;
        self.meta = meta;
        Ok(())
    }

    fn delete(&mut self) -> SpoolResult<()> {
        remove_object(&self.inner, self.id, &self.dir)
    }
}
