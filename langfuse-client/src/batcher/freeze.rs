//! 冻结快照的产出与来源解析（契约 §3.3/§3.4）：worker 只做调度，字节产出集中在这里。
//!
//! 冻结在**准备阶段工作区租约**下执行，输出规模由阈值与 allowance 共同界定：冻结 JSON 小于
//! `input_spool_threshold_bytes` 时留在内存（allowance 转成 `BudgetedBytes` 的 permit），
//! 达到阈值即落盘（`>=`，契约 §3.3），前端 allowance 在前缀写盘后归还。落盘失败只允许
//! **一次非等待内存回退**：先归还前缀额度，再按单条事件逻辑上界整笔预约，失败即显式终结
//! 该准备（不无界回退、不复制已落盘前缀回内存）。未开启暂存时超 allowance 按超限显式拒收。

use std::io::{self, Write};
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::config::Compression;
use crate::error::LangfuseError;
use crate::memory::{MemoryBudget, MemoryPermit};
use crate::payload::{BudgetedBytes, InputSnapshot};
use crate::snapshot::{InputSource, SnapshotSource, SnapshotState, TelemetryEvent};
use crate::spool::{
    CleanupRegistration, ObjectKind, ObjectMeta, RouteFingerprint, SpoolError, SpoolObject,
    SpoolState, SpoolStore, TemporaryObject,
};

/// 描述信息与原子更新空间的一次性预留（契约 §3.2/§4 的 4 KiB 单对象上限；native 适配器
/// 的同名常量只在 native cfg 存在，编码侧不依赖它）。
const DESCRIPTOR_ALLOWANCE_BYTES: u64 = 4 * 1024;

/// 描述信息格式版本；与 native 适配器 `DESCRIPTOR_VERSION` 同代（契约 §5.2）。
const DESCRIPTOR_VERSION: u16 = 1;

/// 事件来源的解析结果。
pub(super) enum Resolved {
    Ready,
    Waiting,
    Failed(LangfuseError),
}

/// 冻结落盘的部署上下文：worker 在准备任务里按部署配置构造，不含路径与凭据。
pub(super) struct InputSpool {
    /// 暂存端口；与部署预算同一份身份。
    pub(super) store: Arc<dyn SpoolStore>,
    /// 部署预算：唯一一次内存回退从这里预约。
    pub(super) budget: Arc<MemoryBudget>,
    /// 落盘阈值：冻结 JSON 达到（`>=`）该字节数即写盘。
    pub(super) threshold: usize,
    /// 单条事件逻辑上界：落盘配额与内存回退的上界（不放大配置）。
    pub(super) max_bytes: usize,
    pub(super) route: RouteFingerprint,
    pub(super) accepted_at_ms: i64,
    pub(super) expires_at_ms: i64,
}

/// 解析事件里的冻结来源：未收敛 → `Waiting`，失败 → 原错误，收敛 → 取走快照并把 cell 转 `Taken`。
///
/// 取走是所有权移交（不是复制）：同一 cell 只能被一个事件消费，重复消费按配置错误暴露。
pub(super) fn resolve_event(event: &mut TelemetryEvent) -> Resolved {
    for slot in [&mut event.input, &mut event.output] {
        let Some(InputSource::Preparing(ticket)) = slot else {
            continue;
        };
        let taken = {
            let mut state = ticket.cell.state.lock().unwrap_or_else(|p| p.into_inner());
            match &*state {
                SnapshotState::Preparing => return Resolved::Waiting,
                _ => std::mem::replace(&mut *state, SnapshotState::Taken),
            }
        };
        match taken {
            SnapshotState::Ready(snapshot) => *slot = Some(InputSource::Ready(snapshot)),
            SnapshotState::Failed(error) => return Resolved::Failed(error),
            SnapshotState::Preparing => return Resolved::Waiting,
            SnapshotState::Taken => {
                return Resolved::Failed(LangfuseError::Config(
                    "snapshot preparation result was already taken".into(),
                ))
            }
        }
    }
    Resolved::Ready
}

/// 冻结快照：按阈值决定留在内存还是落盘，失败路径按契约降级语义显式终结。
///
/// 未开启落盘（`spool = None`）时 allowance 是唯一上界，超出即
/// [`LangfuseError::PayloadTooLarge`]（不截断、不向下取整）；开启时 allowance 是阈值前缀额度。
pub(super) fn freeze_snapshot(
    source: &Arc<dyn SnapshotSource>,
    allowance: MemoryPermit,
    spool: Option<(InputSpool, CleanupRegistration)>,
) -> Result<InputSnapshot, LangfuseError> {
    let prefix_limit = match &spool {
        Some((context, _)) => context.threshold.min(allowance.bytes()),
        None => allowance.bytes(),
    };
    let mut prefix = Vec::new();
    if !write_prefix(source, &mut prefix, prefix_limit)? {
        return Ok(InputSnapshot::Inline(BudgetedBytes::new(
            bytes::Bytes::from(prefix),
            allowance,
        )?));
    }
    let Some((context, cleanup)) = spool else {
        return Err(LangfuseError::PayloadTooLarge {
            limit_bytes: prefix_limit,
        });
    };
    match spool_frozen(source, &context, cleanup) {
        Ok(object) => Ok(InputSnapshot::Spooled(object)),
        Err(SpoolFailure::Fatal(error)) => Err(error),
        Err(SpoolFailure::Disk(error)) => {
            // 一次性降级：归还前缀额度后按逻辑上界整笔预约；预约失败即拒收，不重试、不无界回退。
            drop(allowance);
            let permit = match context.budget.try_reserve(context.max_bytes) {
                Ok(permit) => permit,
                Err(_) => return Err(error),
            };
            resume_in_memory(source, prefix, context.max_bytes, permit)
        }
    }
}

/// 把来源写进内存前缀；返回是否触发落盘阈值（`true` 表示内容达到阈值）。
fn write_prefix(
    source: &Arc<dyn SnapshotSource>,
    buffer: &mut Vec<u8>,
    limit: usize,
) -> Result<bool, LangfuseError> {
    let mut sink = PrefixSink {
        buffer,
        limit,
        overflowed: false,
    };
    let outcome = source.write_json(&mut sink);
    // 超阈值优先：来源在写出足够内容后失败也按落盘路径重跑一次，失败原因原样暴露。
    if sink.overflowed {
        return Ok(true);
    }
    outcome.map(|()| false)
}

/// 内存回退：跳过已在前缀里的字节，把剩余内容按逻辑上界写进同一缓冲。
fn resume_in_memory(
    source: &Arc<dyn SnapshotSource>,
    mut buffer: Vec<u8>,
    limit: usize,
    permit: MemoryPermit,
) -> Result<InputSnapshot, LangfuseError> {
    let skip = buffer.len();
    let mut sink = ResumeSink {
        buffer: &mut buffer,
        skip,
        limit,
        overflowed: false,
    };
    let outcome = source.write_json(&mut sink);
    if sink.overflowed {
        return Err(LangfuseError::PayloadTooLarge { limit_bytes: limit });
    }
    outcome?;
    Ok(InputSnapshot::Inline(BudgetedBytes::new(
        bytes::Bytes::from(buffer),
        permit,
    )?))
}

/// 阈值内只累加的内存 sink；达到阈值即要求调用方转盘（不保留超出阈值的字节）。
struct PrefixSink<'a> {
    buffer: &'a mut Vec<u8>,
    limit: usize,
    overflowed: bool,
}

impl Write for PrefixSink<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() >= self.limit.saturating_sub(self.buffer.len()) {
            self.overflowed = true;
            return Err(io::Error::other(
                "frozen snapshot reached the spool threshold",
            ));
        }
        self.buffer.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// 内存回退用的续写 sink：丢弃已在前缀中的前 `skip` 字节，其余按上界累加。
struct ResumeSink<'a> {
    buffer: &'a mut Vec<u8>,
    skip: usize,
    limit: usize,
    overflowed: bool,
}

impl Write for ResumeSink<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut chunk = bytes;
        if self.skip > 0 {
            let skipped = self.skip.min(chunk.len());
            self.skip -= skipped;
            chunk = &chunk[skipped..];
        }
        if chunk.len() > self.limit.saturating_sub(self.buffer.len()) {
            self.overflowed = true;
            return Err(io::Error::other(
                "frozen snapshot exceeds the event logical limit",
            ));
        }
        self.buffer.extend_from_slice(chunk);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// 落盘失败的性质：`Disk` 允许一次非等待内存回退，`Fatal` 直接终结该准备。
enum SpoolFailure {
    Disk(LangfuseError),
    Fatal(LangfuseError),
}

/// 冻结正文落盘：预留逻辑上界 + descriptor 额度、写临时对象、核对长度与摘要后原子发布。
fn spool_frozen(
    source: &Arc<dyn SnapshotSource>,
    context: &InputSpool,
    cleanup: CleanupRegistration,
) -> Result<SpoolObject, SpoolFailure> {
    let limit = context.max_bytes as u64;
    let reserved = limit
        .checked_add(DESCRIPTOR_ALLOWANCE_BYTES)
        .ok_or_else(|| payload_too_large(context.max_bytes))?;
    let quota = context
        .store
        .reserve(reserved)
        .map_err(|error| SpoolFailure::Disk(error.into()))?;
    let mut object = context
        .store
        .create(quota, ObjectKind::Input, cleanup)
        .map_err(|error| SpoolFailure::Disk(error.into()))?;
    let (written, sha256) = {
        let mut sink = ObjectSink {
            object: object.as_mut(),
            hasher: Sha256::new(),
            written: 0,
            limit,
            overflowed: false,
            failure: None,
        };
        match source.write_json(&mut sink) {
            Ok(()) if !sink.overflowed => {}
            Ok(()) => return Err(payload_too_large(context.max_bytes)),
            Err(_) if sink.overflowed => return Err(payload_too_large(context.max_bytes)),
            Err(error) => {
                return Err(match sink.failure {
                    // 写盘本身失败才允许回退；来源失败原样暴露，不掩盖分类。
                    Some(error) => SpoolFailure::Disk(error.into()),
                    None => SpoolFailure::Fatal(error),
                });
            }
        }
        (sink.written, sink.hasher.clone().finalize().into())
    };
    let meta = ObjectMeta {
        version: DESCRIPTOR_VERSION,
        id: object.id(),
        kind: ObjectKind::Input,
        route: context.route,
        event_count: 0,
        encoding: Compression::None,
        raw_len: written,
        stored_len: written,
        sha256,
        accepted_at_ms: context.accepted_at_ms,
        expires_at_ms: context.expires_at_ms,
        attempts: 0,
        next_attempt_at_ms: None,
        state: SpoolState::Ready,
    };
    object
        .publish(meta)
        .map_err(|error| SpoolFailure::Disk(error.into()))
}

fn payload_too_large(limit_bytes: usize) -> SpoolFailure {
    SpoolFailure::Fatal(LangfuseError::PayloadTooLarge { limit_bytes })
}

/// 落盘 sink：写对象的同时计数与摘要，并区分「超逻辑上界」与「磁盘写失败」。
struct ObjectSink<'a> {
    object: &'a mut dyn TemporaryObject,
    hasher: Sha256,
    written: u64,
    limit: u64,
    overflowed: bool,
    failure: Option<SpoolError>,
}

impl Write for ObjectSink<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.failure.is_some() {
            return Err(io::Error::other(
                "frozen snapshot spool write already failed",
            ));
        }
        if (bytes.len() as u64) > self.limit.saturating_sub(self.written) {
            self.overflowed = true;
            return Err(io::Error::other(
                "frozen snapshot exceeds the event logical limit",
            ));
        }
        let written = match self.object.write(bytes) {
            Ok(written) => written,
            Err(error) => {
                self.failure = Some(SpoolError::Io {
                    operation: "write frozen snapshot",
                    source: error,
                });
                return Err(io::Error::other("frozen snapshot spool write failed"));
            }
        };
        self.hasher.update(&bytes[..written]);
        self.written = self.written.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.object.flush()
    }
}

#[cfg(test)]
#[path = "freeze_test.rs"]
mod tests;
