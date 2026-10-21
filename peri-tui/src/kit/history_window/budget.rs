//! 窗口预算限值与运行账本。
//!
//! 初始值来自计划 §3.1，是实施初值，不是性能结论。账本严格区分：
//! - `resident_body_bytes`：已驻留段的实际正文字节；
//! - `resident_reserved_bytes`：驻留段的容量下界（capacity/容器开销估算）；
//! - `page_reserved_bytes`：在途页预留（容量下界），不占 body；
//! - `pinned_body_bytes`：驻留 body 中不可淘汰的部分（子集）。
//!
//! body 上限约束**全部驻留正文**（pinned 也是实际内存）；pinned 只表示淘汰不可
//! 触碰它。这样“全部固定”时接纳会显式失败，而不是假装预算仍有余量。

pub(crate) const INITIAL_CHECKPOINT_HISTORY: usize = 32;
pub(crate) const MIB: usize = 1024 * 1024;

pub const INITIAL_BODY_BYTES: usize = 16 * MIB;
pub const INITIAL_RESERVED_BYTES: usize = 2 * MIB;
pub const INITIAL_PAGE_TARGET_BYTES: usize = 512 * 1024;
pub const INITIAL_DETAIL_BYTES: usize = 4 * MIB;
pub const INITIAL_RELEASE_WATERMARK_PERCENT: u8 = 75;
pub const INITIAL_PREFETCH_THRESHOLD_PERCENT: u8 = 50;
pub const INITIAL_MAX_IN_FLIGHT_PAGES: usize = 1;
pub const INITIAL_MAX_PENDING_MERGE_PAGES: usize = 1;

/// 跨页未配对工具终点的条数上限（超过即按旧到新丢弃完整正文条目）。
pub(crate) const PENDING_TOOL_ENDS_LIMIT: usize = 256;

/// 跨页暂存**完整结果**的内存预算。
///
/// 8 MiB 是唯一生产上限（原先 `PENDING_TOOL_ENDS_BYTES_MAX` 与
/// `WindowLimits.pending_tool_bytes` 存在 8 MiB / 1 MiB 两套值，后者只供已删除的
/// `ResidentIndex` 配对轨道使用）。单个工具输出可达数百 KiB，8 MiB 能容纳数十个
/// 完整结果，同时低于窗口驻留正文预算（16 MiB），避免暂存反客为主；超预算条目
/// 降级为来源定位，等用户展开时补读，绝不截断正文。
pub(crate) const PENDING_TOOL_ENDS_BYTES_MAX: usize = 8 * MIB;

/// 窗口预算限值。可配置；默认值只表达计划初值。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowLimits {
    pub body_bytes: usize,
    pub reserved_bytes: usize,
    pub page_target_bytes: usize,
    pub release_watermark_percent: u8,
    pub prefetch_threshold_percent: u8,
    pub detail_bytes: usize,
    pub max_in_flight_pages: usize,
    pub max_pending_merge_pages: usize,
    pub checkpoint_history: usize,
}

impl Default for WindowLimits {
    fn default() -> Self {
        Self {
            body_bytes: INITIAL_BODY_BYTES,
            reserved_bytes: INITIAL_RESERVED_BYTES,
            page_target_bytes: INITIAL_PAGE_TARGET_BYTES,
            release_watermark_percent: INITIAL_RELEASE_WATERMARK_PERCENT,
            prefetch_threshold_percent: INITIAL_PREFETCH_THRESHOLD_PERCENT,
            detail_bytes: INITIAL_DETAIL_BYTES,
            max_in_flight_pages: INITIAL_MAX_IN_FLIGHT_PAGES,
            max_pending_merge_pages: INITIAL_MAX_PENDING_MERGE_PAGES,
            checkpoint_history: INITIAL_CHECKPOINT_HISTORY,
        }
    }
}

/// 运行账本：body 与 reserved 分开，pinned 是 body 的子集。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowBudget {
    pub limits: WindowLimits,
    pub resident_body_bytes: usize,
    /// 已归档但尚无持久身份的实时单元（工具卡、子 Agent 等）正文。
    ///
    /// 计入总驻留预算，但不可淘汰：实时协议没有给出这些单元的持久位置，
    /// 释放后无法按页取回。等协议提供持久关联后才能转入可释放段。
    pub unattributed_live_bytes: usize,
    pub resident_reserved_bytes: usize,
    pub pinned_body_bytes: usize,
    pub page_reserved_bytes: usize,
    pub pending_merge_bytes: usize,
    pub detail_bytes: usize,
    /// 已从 canonical 移除、但渲染侧尚未 ack 的正文；保守占用额度。
    pub pending_release_bytes: usize,
}

impl WindowBudget {
    pub fn new(limits: WindowLimits) -> Self {
        Self {
            limits,
            resident_body_bytes: 0,
            unattributed_live_bytes: 0,
            resident_reserved_bytes: 0,
            pinned_body_bytes: 0,
            page_reserved_bytes: 0,
            pending_merge_bytes: 0,
            detail_bytes: 0,
            pending_release_bytes: 0,
        }
    }

    pub fn evictable_body_bytes(&self) -> usize {
        self.resident_body_bytes
            .saturating_sub(self.pinned_body_bytes)
    }

    /// 总驻留正文：可淘汰段 + 尚无持久身份、不可淘汰的实时单元。
    pub fn total_resident_body_bytes(&self) -> usize {
        self.resident_body_bytes
            .saturating_add(self.unattributed_live_bytes)
    }

    pub fn watermark_bytes(&self) -> usize {
        percent_of(
            self.limits.body_bytes,
            self.limits.release_watermark_percent,
        )
    }

    pub fn prefetch_threshold_bytes(&self) -> usize {
        percent_of(
            self.limits.body_bytes,
            self.limits.prefetch_threshold_percent,
        )
    }

    /// 超限时释放到 `min(watermark, limit - incoming)`（计划 §3.4）。
    pub fn release_target_bytes(&self, incoming_body_bytes: usize) -> usize {
        self.watermark_bytes()
            .min(self.limits.body_bytes.saturating_sub(incoming_body_bytes))
    }

    pub fn has_body_capacity_for(&self, incoming_body_bytes: usize) -> bool {
        // 已移除但未 ack 的正文仍保守占额，避免渲染侧未释放就重复接纳。
        // 无持久身份的实时单元同样占用真实内存，必须计入上限。
        self.total_resident_body_bytes()
            .saturating_add(self.pending_release_bytes)
            .saturating_add(incoming_body_bytes)
            <= self.limits.body_bytes
    }

    pub fn can_prefetch(&self) -> bool {
        self.total_resident_body_bytes() < self.prefetch_threshold_bytes()
            && self.page_reserved_bytes == 0
            && self.pending_merge_bytes == 0
    }

    pub fn page_reserve_remaining(&self) -> usize {
        self.limits
            .reserved_bytes
            .saturating_sub(self.page_reserved_bytes)
    }
}

pub(crate) fn percent_of(total: usize, percent: u8) -> usize {
    let percent = percent.min(100) as usize;
    total / 100 * percent + total % 100 * percent / 100
}
