//! 配额账本、跨进程 domain 锁与启动重建（契约 §5.2、§6.2）。
//!
//! 配额 = 写入预留 + Input/Export 已发布对象 + 终态待删除对象 + 描述信息与原子更新临时文件。
//! 账本 `quota/state.json` 只保存三类事实：存活预留条目、已发布对象占用字节、对象数；每个预留条目
//! 都有一个**存活锁文件** `quota/locks/<rid>`，锁被持有即 owner 仍存活。
//!
//! 一致性规则：
//! 1. 账本变更全部在 domain 锁临界区内完成，跨进程可见；不是进程内 Atomic（契约 §5.2）。
//! 2. 预留失败时先以磁盘为准重建再判定，避免账本滞后造成误拒（自愈路径）。
//! 3. 进程崩溃后：存活锁随进程释放 → 条目在下一次 `reserve`/`usage`/`reconcile` 被回收；
//!    `open_at` 的启动重建以磁盘为准重算占用（先重建 quota，再接受新磁盘写入）。
//! 4. 磁盘满/写失败归类为 [`SpoolError::Io`]（保留底层 `io::Error` 诊断），配额不足归类为
//!    [`SpoolError::QuotaExceeded`]；两者都不伪装成其他错误。

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::sync::Arc;

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::spool::error::SpoolError;
use crate::spool::types::{QuotaReservation, SpoolUsage};
use crate::spool::SpoolResult;

use super::native_scan;
use super::{
    create_private_file, hex, id_hex, io_error, parse_id, random_object_id, try_lock_file,
    write_atomic, LockAttempt, NativeInner, OBJECT_BUCKETS,
};

/// 账本快照；缺失字段按默认值处理，未知字段视为不可识别格式。
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct QuotaState {
    /// 预留条目：存活锁文件名（随机十六进制）→ 预留字节数。
    reserved: BTreeMap<String, u64>,
    /// 已发布对象占用（含描述信息与原子更新临时文件）。
    stored_bytes: u64,
    objects: u64,
}

/// 跨进程 domain 锁；Drop 释放。
pub(crate) struct DomainLock {
    file: File,
}

impl Drop for DomainLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn acquire_domain_lock(inner: &NativeInner) -> SpoolResult<DomainLock> {
    let file = create_private_file(&inner.domain_lock_path())
        .map_err(|error| io_error("open spool domain lock", error))?;
    file.lock_exclusive()
        .map_err(|error| io_error("lock spool domain", error))?;
    Ok(DomainLock { file })
}

fn load_state(inner: &NativeInner) -> SpoolResult<QuotaState> {
    let bytes = match fs::read(inner.quota_state_path()) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(QuotaState::default())
        }
        Err(error) => return Err(io_error("read spool quota ledger", error)),
    };
    serde_json::from_slice(&bytes).map_err(|_| SpoolError::InvalidMetadata)
}

fn save_state(inner: &NativeInner, state: &QuotaState) -> SpoolResult<()> {
    let bytes = serde_json::to_vec(state).map_err(|_| SpoolError::InvalidMetadata)?;
    write_atomic(&inner.quota_state_path(), &bytes)
        .map_err(|error| io_error("write spool quota ledger", error))
}

/// 读账本；不可读时以磁盘为准重建（调用方必须已持有 domain 锁）。
fn load_or_rebuild(inner: &NativeInner) -> SpoolResult<QuotaState> {
    match load_state(inner) {
        Ok(state) => Ok(state),
        Err(error) => {
            warn!(error = %error, "telemetry spool quota ledger unreadable; rebuilding from disk");
            rebuild_locked(inner)
        }
    }
}

/// 回收存活锁已释放的预留条目；返回是否发生变化。
///
/// OS owner lock 消失即原 owner 已不存在（崩溃或正常释放），其未写额度可回收；无法判定
/// （锁文件不可访问）时保守保留，宁可少放行也不越界写入。
fn prune_stale(inner: &NativeInner, state: &mut QuotaState) -> bool {
    let before = state.reserved.len();
    state.reserved.retain(|reservation, _| {
        let path = inner.quota_lock_path(reservation);
        match try_lock_file(&path) {
            Ok(LockAttempt::Free(lock)) => {
                drop(lock);
                let _ = fs::remove_file(&path);
                false
            }
            // 他人持锁或锁目录已被并发删除：无法判定存活，保守保留。
            Ok(LockAttempt::Held) | Ok(LockAttempt::Gone) => true,
            Err(error) => {
                warn!(error = %error, "telemetry spool reservation liveness unknown; kept");
                true
            }
        }
    });
    state.reserved.len() != before
}

fn reserved_bytes(state: &QuotaState) -> u64 {
    state.reserved.values().copied().sum()
}

fn available_bytes(inner: &NativeInner, state: &QuotaState) -> u64 {
    inner
        .quota_bytes()
        .saturating_sub(state.stored_bytes.saturating_add(reserved_bytes(state)))
}

/// 以磁盘为尊重算占用并保存（调用方已持 domain 锁）。
fn rebuild_locked(inner: &NativeInner) -> SpoolResult<QuotaState> {
    let mut state = load_state(inner).unwrap_or_default();
    prune_stale(inner, &mut state);
    let (stored_bytes, objects) = native_scan::scan_totals(inner)?;
    state.stored_bytes = stored_bytes;
    state.objects = objects;
    save_state(inner, &state)?;
    Ok(state)
}

/// 启动重建：先重建 quota，再接受新磁盘写入（契约 §5.2）。
pub(crate) fn rebuild(inner: &NativeInner) -> SpoolResult<()> {
    let _lock = acquire_domain_lock(inner)?;
    rebuild_locked(inner).map(|_| ())
}

/// 预留额度；不足时先以磁盘为准重建再做最终判定。
pub(crate) fn reserve(
    inner: &Arc<NativeInner>,
    bytes: u64,
) -> SpoolResult<Box<dyn QuotaReservation>> {
    let _lock = acquire_domain_lock(inner)?;
    let mut state = load_or_rebuild(inner)?;
    prune_stale(inner, &mut state);
    if bytes > available_bytes(inner, &state) {
        state = rebuild_locked(inner)?;
        let available = available_bytes(inner, &state);
        if bytes > available {
            return Err(SpoolError::QuotaExceeded {
                requested: bytes,
                available,
            });
        }
    }
    let reservation = hex(random_object_id().as_bytes());
    let lock = match try_lock_file(&inner.quota_lock_path(&reservation))
        .map_err(|error| io_error("create spool reservation lock", error))?
    {
        LockAttempt::Free(lock) => lock,
        LockAttempt::Held => return Err(SpoolError::Busy),
        // `quota/locks` 是装配期创建的必要目录：缺失是环境漂移，不是并发竞争。
        LockAttempt::Gone => {
            return Err(io_error(
                "create spool reservation lock",
                io::Error::from(io::ErrorKind::NotFound),
            ))
        }
    };
    state.reserved.insert(reservation.clone(), bytes);
    save_state(inner, &state)?;
    Ok(Box::new(NativeReservation {
        inner: Arc::clone(inner),
        reservation,
        bytes,
        lock,
    }))
}

/// 记入已发布对象占用（预留转实际存储）。
pub(crate) fn record_stored(inner: &NativeInner, bytes: u64) -> SpoolResult<()> {
    let _lock = acquire_domain_lock(inner)?;
    let mut state = load_or_rebuild(inner)?;
    prune_stale(inner, &mut state);
    state.stored_bytes = state.stored_bytes.saturating_add(bytes);
    state.objects = state.objects.saturating_add(1);
    save_state(inner, &state)
}

/// 扣回确认删除的对象占用。
pub(crate) fn record_deleted(inner: &NativeInner, bytes: u64) -> SpoolResult<()> {
    let _lock = acquire_domain_lock(inner)?;
    let mut state = load_or_rebuild(inner)?;
    prune_stale(inner, &mut state);
    state.stored_bytes = state.stored_bytes.saturating_sub(bytes);
    state.objects = state.objects.saturating_sub(1);
    save_state(inner, &state)
}

/// 回收失效预留条目；保留原账本值时无写入。
pub(crate) fn reconcile(inner: &NativeInner) -> SpoolResult<()> {
    let _lock = acquire_domain_lock(inner)?;
    let mut state = match load_state(inner) {
        Ok(state) => state,
        Err(error) => {
            warn!(error = %error, "telemetry spool quota reconcile skipped");
            return Ok(());
        }
    };
    if prune_stale(inner, &mut state) {
        save_state(inner, &state)?;
    }
    Ok(())
}

/// 配额使用量快照；非事务快照，仅供诊断与准入参考。
pub(crate) fn usage(inner: &NativeInner) -> SpoolResult<SpoolUsage> {
    let _lock = acquire_domain_lock(inner)?;
    let mut state = load_or_rebuild(inner)?;
    if prune_stale(inner, &mut state) {
        save_state(inner, &state)?;
    }
    Ok(SpoolUsage {
        reserved_bytes: reserved_bytes(&state),
        stored_bytes: state.stored_bytes,
        objects: state.objects,
    })
}

/// 已预留额度；存活锁随句柄释放，账本条目由后续失效回收清除。
struct NativeReservation {
    inner: Arc<NativeInner>,
    reservation: String,
    bytes: u64,
    lock: File,
}

impl QuotaReservation for NativeReservation {
    fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl Drop for NativeReservation {
    fn drop(&mut self) {
        // 只回收未写预留：已写字节必须在确认删除后由 publish/abort 结算，不由 Drop 声称已清理。
        let _ = self.lock.unlock();
        let _ = fs::remove_file(self.inner.quota_lock_path(&self.reservation));
    }
}

/// 对象目录当前占用的磁盘字节（正文 + 描述 + 原子更新临时文件）。
pub(crate) fn accounted_bytes(dir: &Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_file() {
            total = total.saturating_add(metadata.len());
        }
    }
    Ok(total)
}

/// 启动清扫：崩溃 writer 的临时残留 + 未发布对象目录（契约 §5.2「清理无锁 writing」）。
///
/// 在 domain 锁之外运行，不做账本扣减；紧随其后的 [`rebuild`] 以磁盘为准统一结算。
pub(crate) fn sweep_startup_residue(inner: &NativeInner) -> SpoolResult<()> {
    sweep_writers(inner)?;
    sweep_unpublished_objects(inner)
}

/// 只回收存活锁已释放的 writer 残留；活跃 writer 不受影响。
///
/// 顺序约束：writer 先建 `<id>.lock` 再建 `<id>.part`，因此「有 part 无 lock」只可能是残留。
fn sweep_writers(inner: &NativeInner) -> SpoolResult<()> {
    let tmp = inner.tmp_dir();
    let entries = match fs::read_dir(&tmp) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_error("scan spool temporary directory", error)),
    };
    let mut parts: Vec<String> = Vec::new();
    let mut locks: Vec<String> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| io_error("scan spool temporary directory", error))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(stem) = name.strip_suffix(".lock") {
            locks.push(stem.to_string());
        } else if let Some(stem) = name.strip_suffix(".part") {
            parts.push(stem.to_string());
        }
    }
    for stem in &locks {
        let path = tmp.join(format!("{stem}.lock"));
        match try_lock_file(&path)
            .map_err(|error| io_error("lock spool temporary object", error))?
        {
            // 存活锁仍被持有：活跃 writer，绝不清理。
            LockAttempt::Held => continue,
            // 锁目录已被并发删除：`tmp/` 只可能由本部署重建，残留留给下一轮清扫。
            LockAttempt::Gone => continue,
            LockAttempt::Free(_lock) => {}
        }
        remove_temporary(&tmp, stem)?;
    }
    for stem in parts.iter().filter(|stem| !locks.contains(stem)) {
        remove_temporary(&tmp, stem)?;
    }
    Ok(())
}

fn remove_temporary(tmp: &Path, stem: &str) -> SpoolResult<()> {
    for suffix in ["part", "lock"] {
        let path = tmp.join(format!("{stem}.{suffix}"));
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error("remove spool temporary object", error)),
        }
    }
    warn!(object = %stem, "telemetry spool reclaimed abandoned writer");
    Ok(())
}

/// 清扫 publish 中途崩溃留下的 `objects/<b2>/<id>`（无 `meta` 即未发布）。
fn sweep_unpublished_objects(inner: &NativeInner) -> SpoolResult<()> {
    // 桶号以 u16 计数：`OBJECT_BUCKETS as u8` 会把 256 截断成 0，导致整个清扫被跳过。
    for bucket in 0..OBJECT_BUCKETS {
        let bucket_dir = inner.bucket_dir(bucket as u8);
        for name in native_scan::bucket_names(&bucket_dir)? {
            let path = bucket_dir.join(&name);
            if path.join("meta").exists() || !path.is_dir() {
                continue;
            }
            let Some(id) = parse_id(&name) else {
                continue;
            };
            discard_without_quota(inner, id, &path)?;
        }
    }
    Ok(())
}

/// 启动期持锁删除条目；账本由重建统一结算（此时再取 domain 锁会死锁）。
fn discard_without_quota(
    inner: &NativeInner,
    id: crate::spool::ObjectId,
    dir: &Path,
) -> SpoolResult<()> {
    let _lock = match try_lock_file(&inner.object_lock_path(id))
        .map_err(|error| io_error("lock spool object", error))?
    {
        LockAttempt::Free(lock) => lock,
        // 其他 owner 持锁：交由其处理；条目已被并发删除：没有残留可清。
        LockAttempt::Held | LockAttempt::Gone => return Ok(()),
    };
    match fs::remove_dir_all(dir) {
        Ok(()) => warn!(object = %id_hex(id), "telemetry spool discarded startup residue"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error("remove spool residue", error)),
    }
    Ok(())
}
