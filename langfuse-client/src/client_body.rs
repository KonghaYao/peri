//! 正文来源到 reqwest `Body` 的桥接（契约 §3.5）。
//!
//! 两条正文来源各有自己的桥接方式，都不重新编码：内存正文克隆 payload 里那份受 permit
//! 约束的 `Bytes`（底层缓冲共享，permit 由 payload 借用保活），文件正文把读泵容量 2 的
//! 桥接 channel 适配成分块流。文件泵只在 native 目标存在（契约 §7 P1d：WASM 仅有界内存，
//! 无文件泵），因此流式桥接也只在 native 编译，其余目标显式失败而不是伪装成内存正文。

use bytes::Bytes;
use reqwest::Body;

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "emscripten")))]
use tokio::sync::mpsc;

use crate::error::LangfuseError;

/// 内存正文桥接：`Bytes` 克隆共享同一份 permit 覆盖的底层缓冲，不重新编码。
pub(crate) fn memory_body(bytes: Bytes) -> Body {
    Body::from(bytes)
}

/// 文件正文桥接：把读泵的桥接 channel（容量 2）适配成 reqwest 分块流。
///
/// 通道里的每一块都由读泵在 blocking job 中按固定大小填充；`drain` 只 await 读泵的 join，
/// 不在 async runtime 上做磁盘 IO。
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "emscripten")))]
pub(crate) fn file_body(
    mut chunks: mpsc::Receiver<Result<Bytes, std::io::Error>>,
) -> Result<Body, LangfuseError> {
    Ok(Body::wrap_stream(futures::stream::poll_fn(move |cx| {
        chunks.poll_recv(cx)
    })))
}

/// 非 native 目标没有文件泵：文件正文不能静默退化成整份内存正文。
#[cfg(any(target_arch = "wasm32", target_os = "emscripten"))]
pub(crate) fn file_body(
    _chunks: tokio::sync::mpsc::Receiver<Result<Bytes, std::io::Error>>,
) -> Result<Body, LangfuseError> {
    Err(LangfuseError::Config(
        "file-backed telemetry bodies require the native spool".into(),
    ))
}

/// direct ingest 的无暂存端口。
///
/// `BudgetedWriter` 的构造要求一个清理槽，而 direct ingest 没有持久 owner：`spool=None` 的
/// 纯内存路径不会 reserve/create/publish，清理槽只被构造与 Drop。任一方法被调用都说明正文
/// 被误送去暂存，必须显式失败——不落盘、不伪造对象。
struct NoSpoolStore;

/// 只提供 writer 构造所需的清理槽；不绑定任何对象，Drop 时按「从未绑定」归还槽位。
pub(crate) fn memory_registration(
    budget: &std::sync::Arc<crate::memory::MemoryBudget>,
) -> Result<crate::spool::CleanupRegistration, LangfuseError> {
    let registry = crate::spool::cleanup::CleanupRegistry::new(
        std::sync::Arc::new(NoSpoolStore),
        std::sync::Arc::clone(budget),
        1,
    );
    Ok(registry.try_register()?)
}

impl crate::spool::sealed::Sealed for NoSpoolStore {}

impl crate::spool::SpoolStore for NoSpoolStore {
    fn reserve(
        &self,
        _bytes: u64,
    ) -> crate::spool::SpoolResult<Box<dyn crate::spool::QuotaReservation>> {
        Err(unavailable())
    }

    fn create(
        &self,
        _quota: Box<dyn crate::spool::QuotaReservation>,
        _kind: crate::spool::ObjectKind,
        _cleanup: crate::spool::CleanupRegistration,
    ) -> crate::spool::SpoolResult<Box<dyn crate::spool::TemporaryObject>> {
        Err(unavailable())
    }

    fn inspect(
        &self,
        _id: crate::spool::ObjectId,
    ) -> crate::spool::SpoolResult<crate::spool::ObjectMeta> {
        Err(unavailable())
    }

    fn open(
        &self,
        _id: crate::spool::ObjectId,
    ) -> crate::spool::SpoolResult<crate::spool::SpoolReader> {
        Err(unavailable())
    }

    fn list(
        &self,
        _after: Option<crate::spool::PageCursor>,
        _limit: usize,
    ) -> crate::spool::SpoolResult<crate::spool::SpoolPage> {
        Err(unavailable())
    }

    fn claim(
        &self,
        _id: crate::spool::ObjectId,
    ) -> crate::spool::SpoolResult<Option<Box<dyn crate::spool::ObjectClaim>>> {
        Err(unavailable())
    }

    fn delete(&self, _id: crate::spool::ObjectId) -> crate::spool::SpoolResult<()> {
        Err(unavailable())
    }

    fn usage(&self) -> crate::spool::SpoolResult<crate::spool::SpoolUsage> {
        Err(unavailable())
    }
}

fn unavailable() -> crate::spool::SpoolError {
    crate::spool::SpoolError::Unavailable
}
