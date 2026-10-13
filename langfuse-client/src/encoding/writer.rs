//! 受预算约束的导出正文 writer（契约 §3.4、§4）。
//!
//! 内存前缀在阈值内驻留内存（先预约 retained 额度，容量即预约额度）；跨阈值先按 wire 上限
//! 预留在暂存配额、create 临时对象，把前缀写完才释放内存额度。wire 上限每次写入前检查，
//! 写入不允许越 reservation，也没有隐式扩容。
//!
//! `finish` 才封装 [`ExportPayload`]：内存路径交出同一份 permit，文件路径用实测 wire 长度、
//! 摘要与期限 publish 描述信息。Drop 不做 IO、不 spawn、不分配：失败产物由原清理槽按端口
//! 协议回收，调用方必须重新分类 sink 的失败原因（[`BudgetedWriter::take_failure`]）。

use std::io::Write;
use std::sync::Arc;

use bytes::Bytes;
use sha2::{Digest, Sha256};

use crate::config::{Compression, TelemetryConfig};
use crate::error::LangfuseError;
use crate::memory::{MemoryBudget, MemoryPermit};
use crate::payload::{BudgetedBytes, ExportPayload, PayloadBody};
use crate::spool::cleanup::CleanupRegistration;
use crate::spool::{
    ObjectKind, ObjectMeta, RouteFingerprint, SpoolError, SpoolState, SpoolStore, TemporaryObject,
};

/// 暂存描述信息版本；与 native 适配器的 `DESCRIPTOR_VERSION` 同代（契约 §5.2）。
/// 该常量定义在 native 独占模块（WASM 目标不编译），这里是跨平台编码路径唯一需要它的位置。
const DESCRIPTOR_VERSION: u16 = 1;

/// 描述信息与原子更新空间的预留（契约 §4、native 的 `MAX_DESCRIPTOR_BYTES`）。
const DESCRIPTOR_ALLOWANCE_BYTES: u64 = 4 * 1024;

/// 流式嵌入冻结快照时的读写块大小（契约 §4：读写各 64 KiB 工作区账单）。
pub(crate) const STREAM_CHUNK_BYTES: usize = 64 * 1024;

/// 导出编码的部署上下文：route 与 TTL 期限，由接受事件的 owner 冻结后传入。
pub(crate) struct ExportContext {
    pub(crate) route: RouteFingerprint,
    pub(crate) accepted_at_ms: i64,
    pub(crate) expires_at_ms: i64,
}

/// sink 的失败分类；调用方收到 io 错误后按 sink 自身状态重新分类（不回显正文）。
pub(crate) enum SinkFailure {
    /// 达到字节上限（解压前后各自独立生效）。
    Limit { limit_bytes: usize },
    /// 暂存端口失败。
    Spool(SpoolError),
}

impl SinkFailure {
    /// 转成对外的分类错误；`Limit` 只携带上限值，调用方据此区分两个限额。
    pub(crate) fn into_langfuse(self) -> LangfuseError {
        match self {
            SinkFailure::Limit { limit_bytes } => LangfuseError::PayloadTooLarge { limit_bytes },
            SinkFailure::Spool(error) => LangfuseError::Spool(error),
        }
    }

    /// `io::Error` 的静态描述；不携带正文、路径与凭据。
    pub(crate) fn describe(&self) -> String {
        match self {
            SinkFailure::Limit { limit_bytes } => {
                format!("OTLP payload exceeds the {limit_bytes}-byte limit")
            }
            SinkFailure::Spool(error) => error.to_string(),
        }
    }
}

/// 受预算约束的 OTLP 正文 writer；`Write` 的每个字节都计入 wire 长度与摘要。
pub(crate) struct BudgetedWriter {
    budget: Arc<MemoryBudget>,
    /// 内存前缀；转盘后清空（内容已落盘）。
    memory: Vec<u8>,
    /// 内存前缀额度；转盘或 finish 后归还。
    permit: Option<MemoryPermit>,
    store: Option<Arc<dyn SpoolStore>>,
    /// 尚未绑定对象的清理登记；迁移时整体交给 `create`。
    cleanup: Option<CleanupRegistration>,
    object: Option<Box<dyn TemporaryObject>>,
    context: ExportContext,
    /// 内存前缀容量：spool 时受 payload 阈值约束，纯内存时即 wire 上限。
    prefix_limit: usize,
    wire_limit: usize,
    written: u64,
    hasher: Sha256,
    failure: Option<SinkFailure>,
}

impl BudgetedWriter {
    /// 按配置预约内存前缀并绑定部署预算身份；不写任何字节、不做磁盘 IO。
    ///
    /// `max_wire_bytes` 是压缩后正文上限，必须非零；`spool` 为 `None` 时正文全部驻留内存，
    /// 前缀容量以 wire 上限为上界（契约 §4）。暂存开销在真正跨阈值时才预留。
    pub(crate) fn new(
        config: &TelemetryConfig,
        budget: Arc<MemoryBudget>,
        spool: Option<Arc<dyn SpoolStore>>,
        context: ExportContext,
        max_wire_bytes: usize,
        cleanup: CleanupRegistration,
    ) -> Result<Self, LangfuseError> {
        if max_wire_bytes == 0 {
            return Err(LangfuseError::Config(
                "OTLP wire byte limit must be nonzero".into(),
            ));
        }
        if context.expires_at_ms <= context.accepted_at_ms {
            return Err(LangfuseError::Config(
                "export context deadline must follow its acceptance".into(),
            ));
        }
        let prefix_limit = match &spool {
            Some(_) => config.payload_memory_threshold_bytes.min(max_wire_bytes),
            None => max_wire_bytes,
        };
        if prefix_limit == 0 {
            return Err(LangfuseError::Config(
                "export memory prefix budget must be nonzero".into(),
            ));
        }
        let permit = budget.try_reserve(prefix_limit)?;
        Ok(Self {
            budget,
            memory: Vec::with_capacity(prefix_limit),
            permit: Some(permit),
            store: spool,
            cleanup: Some(cleanup),
            object: None,
            context,
            prefix_limit,
            wire_limit: max_wire_bytes,
            written: 0,
            hasher: Sha256::new(),
            failure: None,
        })
    }

    /// 取走 sink 的失败原因；调用方在收到 io 错误后用它给出精确分类。
    pub(crate) fn take_failure(&mut self) -> Option<SinkFailure> {
        self.failure.take()
    }

    /// 封装已完成写入的正文；`raw_len` 是解压后字节数（含信封与转义），`encoding` 是实际编码。
    ///
    /// 内存路径交出前缀 permit；文件路径 publish 描述信息（缺失的摘要/长度由端口核验）。
    pub(crate) fn finish(
        mut self,
        raw_len: u64,
        encoding: Compression,
        event_count: u64,
    ) -> Result<ExportPayload, LangfuseError> {
        if let Some(failure) = self.failure.take() {
            return Err(failure.into_langfuse());
        }
        let sha256: [u8; 32] = self.hasher.clone().finalize().into();
        let wire_len = self.written;
        let budget = Arc::clone(&self.budget);
        let body = match self.object.take() {
            Some(object) => {
                let meta = ObjectMeta {
                    version: DESCRIPTOR_VERSION,
                    id: object.id(),
                    kind: ObjectKind::Export,
                    route: self.context.route,
                    event_count,
                    encoding,
                    raw_len,
                    stored_len: wire_len,
                    sha256,
                    accepted_at_ms: self.context.accepted_at_ms,
                    expires_at_ms: self.context.expires_at_ms,
                    attempts: 0,
                    next_attempt_at_ms: None,
                    state: SpoolState::Ready,
                };
                PayloadBody::File(object.publish(meta)?)
            }
            None => {
                let permit = self.permit.take().ok_or(LangfuseError::BudgetMismatch)?;
                let bytes = Bytes::from(std::mem::take(&mut self.memory));
                PayloadBody::Memory(BudgetedBytes::new(bytes, permit)?)
            }
        };
        Ok(ExportPayload {
            body,
            encoding,
            raw_len,
            wire_len,
            sha256,
            event_count,
            budget,
        })
    }

    /// 记录失败并返回可上抛的 io 错误；同一 writer 后续写入继续失败。
    fn fail(&mut self, failure: SinkFailure) -> std::io::Error {
        let error = std::io::Error::other(failure.describe());
        self.failure = Some(failure);
        error
    }

    /// 一次写入：前缀内直接追加，跨阈值先转盘。
    fn write_inner(&mut self, buf: &[u8]) -> Result<(), SinkFailure> {
        if self.object.is_none() && self.memory.len() + buf.len() <= self.prefix_limit {
            self.memory.extend_from_slice(buf);
            return Ok(());
        }
        if self.object.is_none() {
            self.migrate()?;
        }
        let Some(object) = self.object.as_mut() else {
            // 迁移成功必然留下对象；缺失属内部不变式破坏，按端口失败上报而不是 panic。
            return Err(SinkFailure::Spool(SpoolError::Unavailable));
        };
        object.write_all(buf).map_err(|error| {
            SinkFailure::Spool(SpoolError::Io {
                operation: "write export payload",
                source: error,
            })
        })
    }

    /// 跨阈值转盘：先预留配额并 create，前缀写完才释放内存额度（契约 §4）。
    fn migrate(&mut self) -> Result<(), SinkFailure> {
        let Some(store) = self.store.clone() else {
            // 纯内存路径的前缀容量即 wire 上限，不会走到跨阈值分支。
            return Err(SinkFailure::Limit {
                limit_bytes: self.wire_limit,
            });
        };
        let Some(cleanup) = self.cleanup.take() else {
            return Err(SinkFailure::Spool(SpoolError::Unavailable));
        };
        let quota = self
            .wire_limit
            .checked_add(DESCRIPTOR_ALLOWANCE_BYTES as usize)
            .map(|bytes| bytes as u64)
            .ok_or(SinkFailure::Limit {
                limit_bytes: self.wire_limit,
            })?;
        let quota = store.reserve(quota).map_err(SinkFailure::Spool)?;
        let mut object = store
            .create(quota, ObjectKind::Export, cleanup)
            .map_err(SinkFailure::Spool)?;
        object.write_all(&self.memory).map_err(|error| {
            SinkFailure::Spool(SpoolError::Io {
                operation: "write export payload prefix",
                source: error,
            })
        })?;
        self.memory = Vec::new();
        self.permit = None;
        self.object = Some(object);
        Ok(())
    }
}

impl Write for BudgetedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Some(failure) = &self.failure {
            return Err(std::io::Error::other(failure.describe()));
        }
        let required = self.written.saturating_add(buf.len() as u64);
        if required > self.wire_limit as u64 {
            return Err(self.fail(SinkFailure::Limit {
                limit_bytes: self.wire_limit,
            }));
        }
        if let Err(failure) = self.write_inner(buf) {
            return Err(self.fail(failure));
        }
        self.hasher.update(buf);
        self.written = required;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let Some(object) = self.object.as_mut() else {
            return Ok(());
        };
        match object.flush() {
            Ok(()) => Ok(()),
            Err(error) => Err(self.fail(SinkFailure::Spool(SpoolError::Io {
                operation: "flush export payload",
                source: error,
            }))),
        }
    }
}
