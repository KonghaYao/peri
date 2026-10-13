//! 部署级内存预算与工作区租约（契约 §3.4、§4）。
//!
//! 预算身份以 `Arc` 指针相等性判定：金额相同不算同一预算，不允许重新挂账洗掉来源身份。
//! 预算分两个不互借的分区：**retained**（source/event/payload，经 [`MemoryBudget::try_reserve`]）
//! 与**进度分区** P（准备/HTTP 工作区与固定账单，只经工作区租约取得）。
//!
//! `used` 包含已隔离的 P：retained 预约只扣 retained 分区，空闲进度额度不能被 source 抢占。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::client::ExportConfig;
use crate::config::BatcherConfig;
use crate::error::LangfuseError;

/// 清理槽上限（契约 §3.2）：`3×queue_capacity + prepare_max_in_flight + max_in_flight + 64`；
/// 固定容量下「失败产物未清理」占配额而不是无限新建槽，溢出即配置错误。
pub(crate) fn slot_capacity(config: &BatcherConfig) -> Result<usize, LangfuseError> {
    config
        .queue_capacity
        .checked_mul(3)
        .and_then(|slots| slots.checked_add(config.telemetry.prepare_max_in_flight))
        .and_then(|slots| slots.checked_add(config.max_in_flight))
        .and_then(|slots| slots.checked_add(64))
        .ok_or_else(|| LangfuseError::Config("telemetry cleanup slot capacity overflows".into()))
}

/// gzip encoder 固定账单（契约 §4：1 MiB/encoder）。
pub(crate) const GZIP_BILLING_BYTES: usize = 1024 * 1024;
/// 每请求固定预留的框架账单（reqwest/hyper/TLS 用户态缓冲，契约 §4）。
const HTTP_FRAME_BILLING_BYTES: usize = 1024 * 1024;
/// 恢复分页的页 buffer 与索引预留（契约 §4：每页 64 条、页 buffer 256 KiB）。
const RECOVERY_PAGE_BILLING_BYTES: usize = 256 * 1024;
/// 调度元数据固定账单（命令槽表、计数容器等）。
const SCHEDULER_BILLING_BYTES: usize = 64 * 1024;
/// 单槽固定账单：槽记录、注册凭证与表项 `Arc` 的保守上界（见 `spool::cleanup`）。
pub(crate) const SLOT_FIXED_BILLING_BYTES: usize = 256;

/// 部署级内存预算；字段私有，外部无法构造「同金额」的伪造预算。
pub struct MemoryBudget {
    limit: usize,
    used: AtomicUsize,
    prepare: WorkspacePool,
    http: WorkspacePool,
}

/// 一次已记账的 retained 额度；Drop 归还，不可 Clone。
pub struct MemoryPermit {
    budget: Arc<MemoryBudget>,
    bytes: usize,
}

/// 固定池租约（准备阶段或 HTTP 阶段工作区）；non-Clone，Drop 回池。
///
/// 等待并发槽可以调度，但不得等待 retained 额度，避免「持全部 source 等另一批释放工作区」。
pub(crate) struct WorkspaceLease {
    budget: Arc<MemoryBudget>,
    pool: LeasePool,
    /// 租约金额只为断言保留：生产路径按池 Drop 归还，不读该值，故仅在测试构建中存在。
    #[cfg(test)]
    bytes: usize,
}

/// 租约所属的固定池。
#[derive(Clone, Copy)]
enum LeasePool {
    Prepare,
    Http,
}

/// 固定容量租约池：只在部署构造时定额，租约 Drop 无条件回池。
struct WorkspacePool {
    total: usize,
    per_lease: usize,
    leased: AtomicUsize,
}

impl MemoryBudget {
    /// 只构造独立 retained 预算；部署预算必须经 [`MemoryBudget::for_batcher`] 预留进度分区。
    ///
    /// 同步、`Send + Sync`；失败返回 [`LangfuseError::Config`]。
    pub fn new(limit: usize) -> Result<Arc<Self>, LangfuseError> {
        if limit == 0 {
            return Err(LangfuseError::Config(
                "memory budget must be nonzero".into(),
            ));
        }
        Ok(Arc::new(Self {
            limit,
            used: AtomicUsize::new(0),
            prepare: WorkspacePool::empty(),
            http: WorkspacePool::empty(),
        }))
    }

    /// 预约 retained 额度；不足立即返回 `MemoryBudgetExceeded`，永不等待。
    pub fn try_reserve(self: &Arc<Self>, bytes: usize) -> Result<MemoryPermit, LangfuseError> {
        let mut used = self.used.load(Ordering::Relaxed);
        loop {
            let requested = used
                .checked_add(bytes)
                .ok_or(LangfuseError::MemoryBudgetExceeded {
                    requested: bytes,
                    available: 0,
                })?;
            if requested > self.limit {
                return Err(LangfuseError::MemoryBudgetExceeded {
                    requested: bytes,
                    available: self.limit - used,
                });
            }
            match self.used.compare_exchange_weak(
                used,
                requested,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    return Ok(MemoryPermit {
                        budget: Arc::clone(self),
                        bytes,
                    })
                }
                Err(current) => used = current,
            }
        }
    }

    /// 当前已记账的 retained 字节数（含已隔离的进度分区）。
    pub fn used(&self) -> usize {
        self.used.load(Ordering::Acquire)
    }

    /// 部署构造入口：一次性 checked 预留进度分区 P，要求 `limit > P`。
    ///
    /// `P = Nprepare×(2×chunk + 1 MiB gzip 账单) + Nhttp×(1 MiB 框架账单 + 3×chunk +
    /// max_response_bytes) + 256 KiB 页 + 固定槽表/调度元数据账单`。全部乘加 checked，
    /// 任何溢出或 `limit <= P` 都是配置错误，禁止向上取整扩大配置。
    pub(crate) fn for_batcher(
        config: &BatcherConfig,
        export: &ExportConfig,
    ) -> Result<Arc<Self>, LangfuseError> {
        let overflow =
            || LangfuseError::Config("telemetry memory budget arithmetic overflows".into());
        let chunk = config.telemetry.stream_chunk_bytes;
        let prepare_lease = chunk
            .checked_mul(2)
            .and_then(|bytes| bytes.checked_add(GZIP_BILLING_BYTES))
            .ok_or_else(overflow)?;
        let http_lease = chunk
            .checked_mul(3)
            .and_then(|bytes| bytes.checked_add(HTTP_FRAME_BILLING_BYTES))
            .and_then(|bytes| bytes.checked_add(export.max_response_bytes))
            .ok_or_else(overflow)?;
        let prepare_total = prepare_lease
            .checked_mul(config.telemetry.prepare_max_in_flight)
            .ok_or_else(overflow)?;
        let http_total = http_lease
            .checked_mul(config.max_in_flight)
            .ok_or_else(overflow)?;
        let slot_table = SLOT_FIXED_BILLING_BYTES
            .checked_mul(slot_capacity(config)?)
            .ok_or_else(overflow)?;
        let progress = prepare_total
            .checked_add(http_total)
            .and_then(|bytes| bytes.checked_add(slot_table))
            .and_then(|bytes| bytes.checked_add(RECOVERY_PAGE_BILLING_BYTES))
            .and_then(|bytes| bytes.checked_add(SCHEDULER_BILLING_BYTES))
            .ok_or_else(overflow)?;
        let limit = config.telemetry.memory_budget_bytes;
        if limit <= progress {
            return Err(LangfuseError::Config(format!(
                "telemetry memory budget {limit} cannot isolate the {progress}-byte progress partition"
            )));
        }
        Ok(Arc::new(Self {
            limit,
            used: AtomicUsize::new(progress),
            prepare: WorkspacePool {
                total: config.telemetry.prepare_max_in_flight,
                per_lease: prepare_lease,
                leased: AtomicUsize::new(0),
            },
            http: WorkspacePool {
                total: config.max_in_flight,
                per_lease: http_lease,
                leased: AtomicUsize::new(0),
            },
        }))
    }

    /// 取得准备阶段工作区租约（编码工作区、读写块与 gzip 固定账单）。
    pub(crate) fn try_prepare_workspace(self: &Arc<Self>) -> Result<WorkspaceLease, LangfuseError> {
        self.prepare.acquire(self, LeasePool::Prepare)
    }

    /// 取得 HTTP 阶段工作区租约（框架账单、桥接 channel 与响应缓冲）。
    pub(crate) fn try_http_workspace(self: &Arc<Self>) -> Result<WorkspaceLease, LangfuseError> {
        self.http.acquire(self, LeasePool::Http)
    }

    /// 归还一份租约；只有 [`WorkspaceLease::drop`] 调用。
    fn release_workspace(&self, pool: LeasePool) {
        let pool = match pool {
            LeasePool::Prepare => &self.prepare,
            LeasePool::Http => &self.http,
        };
        pool.leased.fetch_sub(1, Ordering::AcqRel);
    }
}

impl WorkspacePool {
    const fn empty() -> Self {
        Self {
            total: 0,
            per_lease: 0,
            leased: AtomicUsize::new(0),
        }
    }

    /// 非阻塞取租约：池空立即返回 `MemoryBudgetExceeded`，不等待、不挪用 retained。
    fn acquire(
        &self,
        budget: &Arc<MemoryBudget>,
        pool: LeasePool,
    ) -> Result<WorkspaceLease, LangfuseError> {
        let mut leased = self.leased.load(Ordering::Relaxed);
        loop {
            if leased >= self.total {
                return Err(LangfuseError::MemoryBudgetExceeded {
                    requested: self.per_lease,
                    available: 0,
                });
            }
            match self.leased.compare_exchange_weak(
                leased,
                leased + 1,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    return Ok(WorkspaceLease {
                        budget: Arc::clone(budget),
                        pool,
                        #[cfg(test)]
                        bytes: self.per_lease,
                    })
                }
                Err(current) => leased = current,
            }
        }
    }
}

impl MemoryPermit {
    /// 本次预约的字节数。
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// 本次预约所属的预算身份；金额相同不算同一预算，用 `Arc::ptr_eq` 比较。
    pub(crate) fn budget(&self) -> &Arc<MemoryBudget> {
        &self.budget
    }
}

impl Drop for MemoryPermit {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

impl WorkspaceLease {
    /// 本租约覆盖的固定字节数（已在部署 P 中隔离，不再计入 retained）；仅测试断言读取。
    #[cfg(test)]
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }
}

impl Drop for WorkspaceLease {
    fn drop(&mut self) {
        self.budget.release_workspace(self.pool);
    }
}

#[cfg(test)]
#[path = "memory_test.rs"]
mod tests;
