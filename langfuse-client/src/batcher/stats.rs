//! Batcher 统计计数器（契约 §3.6、§7 P4a）。
//!
//! 累计量只增不减（`accepted/rejected/.../raw_bytes/wire_bytes/...`），进度量（队列字节、活跃快照、
//! 准备中内存、spool 占用、最老积压）由唯一 worker 在合并点同步发布；字段分别读取，**不是**并发
//! 事务快照。统计只保存数字与分类，不保存 payload、凭据或服务端正文。

use std::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;

use tracing::warn;

use crate::error::LangfuseError;
use crate::spool::{SpoolError, SpoolStore, SpoolUsage};

/// 暂存降级原因（契约 §7 P4a）；只表达分类，不含路径、凭据与正文。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpoolFallbackReason {
    /// 配置显式关闭暂存。
    Disabled,
    /// 目标/部署没有暂存能力（WASM、无 native 适配器或缺少凭据）。
    Unsupported,
    /// 端口存在但当前不可用（目录/锁/文件系统不支持）。
    Unavailable,
    /// 配额不足。
    Quota,
    /// 底层 IO 失败。
    Io,
}

impl SpoolFallbackReason {
    fn code(self) -> u8 {
        match self {
            Self::Disabled => 1,
            Self::Unsupported => 2,
            Self::Unavailable => 3,
            Self::Quota => 4,
            Self::Io => 5,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Disabled),
            2 => Some(Self::Unsupported),
            3 => Some(Self::Unavailable),
            4 => Some(Self::Quota),
            5 => Some(Self::Io),
            _ => None,
        }
    }

    /// 端口错误的分类投影；诊断只带分类，不带宿主路径。
    pub(super) fn classify(error: &SpoolError) -> Self {
        match error {
            SpoolError::QuotaExceeded { .. } => Self::Quota,
            SpoolError::Unavailable => Self::Unavailable,
            SpoolError::Busy | SpoolError::NotFound | SpoolError::Corrupt { .. } => {
                Self::Unavailable
            }
            SpoolError::InvalidMetadata | SpoolError::Io { .. } => Self::Io,
        }
    }

    /// 日志用的分类标签；只表达分类，不回显端口错误里的路径或正文描述。
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Unsupported => "unsupported",
            Self::Unavailable => "unavailable",
            Self::Quota => "quota",
            Self::Io => "io",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BatcherStats {
    pub accepted_events: usize,
    pub rejected_events: usize,
    pub evicted_events: usize,
    pub submitted_batches: usize,
    pub completed_batches: usize,
    pub failed_batches: usize,
    pub partially_rejected_batches: usize,
    pub rejected_spans: u64,
    pub in_flight_batches: usize,
    /// 本进程产生或接手的 payload 原文（解压后 JSON）累计字节。
    pub raw_bytes_total: u64,
    /// 同上口径的线包（内容编码后）累计字节。
    pub wire_bytes_total: u64,
    /// 准备中来源已预约的快照输出 allowance 字节（收敛即归零）。
    pub active_snapshot_bytes: u64,
    /// 在准备中的固定工作区账单字节（并发槽租约）。
    pub preparing_memory_bytes: u64,
    /// 事件队列的原文记账字节（`budget::queued_bytes` 口径）。
    pub memory_queue_bytes: u64,
    /// 端口报告的已发布对象占用字节（含 descriptor）。
    pub spool_bytes: u64,
    /// 端口报告的可见对象数。
    pub spool_objects: u64,
    /// 最老未交付事件距今年龄；无未交付事件时为 0。
    pub oldest_pending_age_ms: u64,
    /// 已登记的重试调度次数（累计）。
    pub retry_attempts: u64,
    /// 启动恢复接手的事件数（累计）。
    pub recovered_events: u64,
    /// 超期终结的事件数（累计）。
    pub expired_events: u64,
    /// 隔离并清理的损坏/未知版本对象数（累计）。
    pub corrupt_objects: u64,
    /// 配额预留被拒次数（累计）。
    pub quota_rejections: u64,
    /// 暂存降级次数（累计）；`fallback_reason` 为最近一次分类。
    pub resource_fallbacks: u64,
    /// 最近一次暂存降级原因；未降级为 `None`。
    pub fallback_reason: Option<SpoolFallbackReason>,
}

#[derive(Default)]
pub(super) struct Counters {
    pub accepted_events: AtomicUsize,
    pub rejected_events: AtomicUsize,
    pub evicted_events: AtomicUsize,
    pub submitted_batches: AtomicUsize,
    pub completed_batches: AtomicUsize,
    pub failed_batches: AtomicUsize,
    pub partially_rejected_batches: AtomicUsize,
    pub rejected_spans: AtomicU64,
    pub in_flight_batches: AtomicUsize,
    pub raw_bytes_total: AtomicU64,
    pub wire_bytes_total: AtomicU64,
    pub active_snapshot_bytes: AtomicU64,
    pub preparing_memory_bytes: AtomicU64,
    pub memory_queue_bytes: AtomicU64,
    pub spool_bytes: AtomicU64,
    pub spool_objects: AtomicU64,
    pub oldest_pending_age_ms: AtomicU64,
    pub retry_attempts: AtomicU64,
    pub recovered_events: AtomicU64,
    pub expired_events: AtomicU64,
    pub corrupt_objects: AtomicU64,
    pub quota_rejections: AtomicU64,
    pub resource_fallbacks: AtomicU64,
    pub fallback_reason: AtomicU8,
}

impl Counters {
    pub(super) fn snapshot(&self) -> BatcherStats {
        BatcherStats {
            accepted_events: self.accepted_events.load(Ordering::Relaxed),
            rejected_events: self.rejected_events.load(Ordering::Relaxed),
            evicted_events: self.evicted_events.load(Ordering::Relaxed),
            submitted_batches: self.submitted_batches.load(Ordering::Relaxed),
            completed_batches: self.completed_batches.load(Ordering::Relaxed),
            failed_batches: self.failed_batches.load(Ordering::Relaxed),
            partially_rejected_batches: self.partially_rejected_batches.load(Ordering::Relaxed),
            rejected_spans: self.rejected_spans.load(Ordering::Relaxed),
            in_flight_batches: self.in_flight_batches.load(Ordering::Relaxed),
            raw_bytes_total: self.raw_bytes_total.load(Ordering::Relaxed),
            wire_bytes_total: self.wire_bytes_total.load(Ordering::Relaxed),
            active_snapshot_bytes: self.active_snapshot_bytes.load(Ordering::Relaxed),
            preparing_memory_bytes: self.preparing_memory_bytes.load(Ordering::Relaxed),
            memory_queue_bytes: self.memory_queue_bytes.load(Ordering::Relaxed),
            spool_bytes: self.spool_bytes.load(Ordering::Relaxed),
            spool_objects: self.spool_objects.load(Ordering::Relaxed),
            oldest_pending_age_ms: self.oldest_pending_age_ms.load(Ordering::Relaxed),
            retry_attempts: self.retry_attempts.load(Ordering::Relaxed),
            recovered_events: self.recovered_events.load(Ordering::Relaxed),
            expired_events: self.expired_events.load(Ordering::Relaxed),
            corrupt_objects: self.corrupt_objects.load(Ordering::Relaxed),
            quota_rejections: self.quota_rejections.load(Ordering::Relaxed),
            resource_fallbacks: self.resource_fallbacks.load(Ordering::Relaxed),
            fallback_reason: SpoolFallbackReason::from_code(
                self.fallback_reason.load(Ordering::Relaxed),
            ),
        }
    }

    /// payload 的原文/线包字节；同一 payload 只登记一次（不随重试重复累计）。
    pub(super) fn account_payload(&self, raw_len: u64, wire_len: u64) {
        self.raw_bytes_total.fetch_add(raw_len, Ordering::Relaxed);
        self.wire_bytes_total.fetch_add(wire_len, Ordering::Relaxed);
    }

    pub(super) fn add_active_snapshot_bytes(&self, bytes: u64) {
        self.active_snapshot_bytes
            .fetch_add(bytes, Ordering::Relaxed);
    }

    pub(super) fn release_active_snapshot_bytes(&self, bytes: u64) {
        let _ =
            self.active_snapshot_bytes
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                    Some(value.saturating_sub(bytes))
                });
    }

    pub(super) fn add_preparing_bytes(&self, bytes: u64) {
        self.preparing_memory_bytes
            .fetch_add(bytes, Ordering::Relaxed);
    }

    pub(super) fn release_preparing_bytes(&self, bytes: u64) {
        let _ =
            self.preparing_memory_bytes
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                    Some(value.saturating_sub(bytes))
                });
    }

    pub(super) fn set_memory_queue_bytes(&self, bytes: usize) {
        self.memory_queue_bytes
            .store(bytes as u64, Ordering::Relaxed);
    }

    /// 端口用量快照；只在真实读取成功后发布，不用进程内估算冒充配额占用。
    pub(super) fn set_spool_usage(&self, usage: SpoolUsage) {
        self.spool_bytes
            .store(usage.stored_bytes, Ordering::Relaxed);
        self.spool_objects.store(usage.objects, Ordering::Relaxed);
    }

    pub(super) fn set_oldest_pending_age_ms(&self, age_ms: u64) {
        self.oldest_pending_age_ms.store(age_ms, Ordering::Relaxed);
    }

    pub(super) fn add_retry_attempt(&self) {
        self.retry_attempts.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn add_recovered_events(&self, events: u64) {
        self.recovered_events.fetch_add(events, Ordering::Relaxed);
    }

    pub(super) fn add_expired_events(&self, events: u64) {
        self.expired_events.fetch_add(events, Ordering::Relaxed);
    }

    pub(super) fn add_corrupt_object(&self) {
        self.corrupt_objects.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn add_quota_rejection(&self) {
        self.quota_rejections.fetch_add(1, Ordering::Relaxed);
    }

    /// 登记一次暂存降级并冻结最近原因（先到先得，不因后续更轻的降级被覆盖成成功）。
    pub(super) fn record_fallback(&self, reason: SpoolFallbackReason) {
        self.resource_fallbacks.fetch_add(1, Ordering::Relaxed);
        if self.fallback_reason.load(Ordering::Relaxed) == 0 {
            self.fallback_reason.store(reason.code(), Ordering::Relaxed);
        }
    }

    /// 部署构造期的降级投影：只设置原因，不计入 `resource_fallbacks`（没有发生运行期降级）。
    pub(super) fn set_fallback_reason(&self, reason: SpoolFallbackReason) {
        if self.fallback_reason.load(Ordering::Relaxed) == 0 {
            self.fallback_reason.store(reason.code(), Ordering::Relaxed);
        }
    }

    /// 发布 worker 侧进度量：队列字节与最老积压年龄（含在途与重试条目）。
    pub(super) fn publish_progress(&self, queue_bytes: usize, oldest_accepted_ms: Option<i64>) {
        let now_ms = super::wall_ms();
        let age = oldest_accepted_ms
            .map(|accepted| (now_ms - accepted).max(0) as u64)
            .unwrap_or(0);
        self.set_oldest_pending_age_ms(age);
        self.set_memory_queue_bytes(queue_bytes);
    }
}

/// 输出丢弃汇总日志并清零计数；只在汇总点调用，不逐条刷日志。
pub(super) fn report_dropped(dropped: &AtomicUsize) {
    let n = dropped.swap(0, Ordering::Relaxed);
    if n > 0 {
        warn!(target: "langfuse::batcher", dropped = n, "Batcher 已丢弃 {n} 条事件（容量、字节预算或关闭）");
    }
}

/// 暂存失败的分类诊断与降级计数；日志只带分类，不带 payload、路径、凭据或服务端正文。
pub(super) fn note_spool_failure(error: &LangfuseError, counters: &Counters) {
    if let LangfuseError::Spool(reason) = error {
        let classified = SpoolFallbackReason::classify(reason);
        if classified == SpoolFallbackReason::Quota {
            counters.add_quota_rejection();
        }
        counters.record_fallback(classified);
        // 降级按 WARN 报一次；终态失败/损坏由调用点按 ERROR 报，正文不进日志。
        warn!(
            reason = classified.label(),
            "telemetry spool degraded for this unit"
        );
    }
}

/// 部分拒收的批次计数：服务端给出拒收 span 数即记一次，禁止重发整批；日志不带正文。
pub(super) fn note_partial_rejection(error: &LangfuseError, counters: &Counters) {
    if let LangfuseError::PartialSuccess { rejected_spans } = error {
        counters
            .partially_rejected_batches
            .fetch_add(1, Ordering::Relaxed);
        let _ = counters
            .rejected_spans
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                Some(count.saturating_add(*rejected_spans))
            });
        warn!(rejected_spans, "telemetry batch partially rejected");
    }
}

/// 刷新端口用量快照（blocking 读取，一次一个 job）；失败保留上一份快照并记 WARN。
pub(super) async fn refresh_spool_usage(store: Option<&Arc<dyn SpoolStore>>, counters: &Counters) {
    let Some(store) = store else {
        return;
    };
    let store = Arc::clone(store);
    match tokio::task::spawn_blocking(move || store.usage()).await {
        Ok(Ok(usage)) => counters.set_spool_usage(usage),
        Ok(Err(error)) => warn!(error = %error, "telemetry spool usage probe failed"),
        Err(_join) => warn!("telemetry spool usage probe did not complete"),
    }
}
