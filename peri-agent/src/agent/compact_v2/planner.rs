//! Planner — Compact 计划和策略类型
//!
//! planner 只能读取 MessageTranscript 和 CompactConfig，绝对不能调用
//! set_truncated、set_excluded、send_persist 或 provider。

use std::collections::{HashMap, HashSet};

use crate::messages::{BaseMessage, MessageId};
use crate::session::memory_probe::transcript_entry_bytes;
use crate::session::transcript::{MessageTranscript, TranscriptEntry};
use crate::tools::ContextRetention;

use super::config::CompactConfig;
use super::projection::{MicroCompactPlan, PROJECTION_POLICY_VERSION};

/// 上下文压力 — 用于决定是否需要 compact 及回收目标
#[derive(Debug, Clone)]
pub struct ContextPressure {
    pub estimated_tokens: u64,
    pub context_window: u32,
    pub output_reserve: u32,
    pub predicted_tool_growth: u32,
    pub safety_buffer: u32,
    pub cache_hit_rate: f64,
}

impl ContextPressure {
    /// 目标 token 用量上限
    pub fn target_tokens(&self) -> u64 {
        let reserve = self.output_reserve as u64
            + self.predicted_tool_growth as u64
            + self.safety_buffer as u64;
        (self.context_window as u64).saturating_sub(reserve)
    }

    /// 需要回收的 token 数量（饱和减法，不溢出）。
    ///
    /// 为防止 reclaim_target=0 阻断 Full 升级，加 2% 窗口最小值。
    pub fn target_reclaim_tokens(&self) -> u64 {
        let raw = self.estimated_tokens.saturating_sub(self.target_tokens());
        let min_floor = (self.context_window as u64 * 2) / 100;
        raw.max(min_floor)
    }
}

/// 历史正文的驻留字节压力 — 与 token 估算独立的只读事实。
///
/// 计量口径是 transcript 自有区与继承祖先区的 `reserved` 饱和和；`excluded_bytes`
/// 是两者的子集，不重复相加。`reserved` 来自 `String::capacity` / `Vec::capacity`，
/// 是容量下界，不是 RSS。
///
/// 自有区按 **Full 的实际归档谓词**（[`is_full_archivable_entry`]）再分为：
/// - `reclaimable_own_bytes`：Full 会归档并释放的正文；
/// - `irreducible_own_bytes`：Full 保留的 System 与受保护后台结果（尚未收缩的
///   已 excluded 条目也暂计此处，随后由 shrink 释放）。
///
/// `irreducible_inherited_bytes` 是祖先区，Full 整体跳过。三者必须分开观察，
/// 否则「总量超限」无法说明 Full 是否还有回收空间。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResidentHistoryPressure {
    /// 配置上限；0 表示关闭驻留门控。
    pub limit_bytes: u64,
    pub reclaimable_own_bytes: u64,
    pub irreducible_own_bytes: u64,
    pub irreducible_inherited_bytes: u64,
}

/// Full 是否归档该 own 条目。
///
/// 这是 Full 归档谓词的单一权威：只归档未被排除、非 System、且未被后台结果保护的
/// own 条目。驻留压力计量与 Full 必须共用它，否则会把 Full 永远保留的正文误算成
/// 可回收。
pub fn is_full_archivable_entry(
    transcript: &MessageTranscript,
    entry: &TranscriptEntry,
    protected: &HashSet<MessageId>,
) -> bool {
    !transcript.flags(entry.id()).excluded
        && !matches!(entry.as_message(), Some(BaseMessage::System { .. }))
        && !protected.contains(&entry.id())
}

impl ResidentHistoryPressure {
    /// 关闭门控（既有调用方默认行为）。
    pub const DISABLED: Self = Self {
        limit_bytes: 0,
        reclaimable_own_bytes: 0,
        irreducible_own_bytes: 0,
        irreducible_inherited_bytes: 0,
    };

    /// 从 transcript 直接采集快照；未提供保护集合时按空集合处理。
    pub fn from_transcript(limit_bytes: u64, transcript: &MessageTranscript) -> Self {
        Self::from_transcript_with_protected(limit_bytes, transcript, &HashSet::new())
    }

    /// 从 transcript 采集快照，并按 Full 的保留谓词拆分自有区。
    ///
    /// 保护集合必须与 Full 使用的后台结果集合一致，否则受保护条目会被误计为
    /// 可回收。只读遍历，不复制正文。
    pub fn from_transcript_with_protected(
        limit_bytes: u64,
        transcript: &MessageTranscript,
        protected: &HashSet<MessageId>,
    ) -> Self {
        let mut pressure = Self {
            limit_bytes,
            ..Self::default()
        };
        for (index, entry) in transcript.entries().iter().enumerate() {
            let reserved = transcript_entry_bytes(entry).reserved as u64;
            if index < transcript.ancestor_len() {
                pressure.irreducible_inherited_bytes = pressure
                    .irreducible_inherited_bytes
                    .saturating_add(reserved);
            } else if is_full_archivable_entry(transcript, entry, protected) {
                pressure.reclaimable_own_bytes =
                    pressure.reclaimable_own_bytes.saturating_add(reserved);
            } else {
                pressure.irreducible_own_bytes =
                    pressure.irreducible_own_bytes.saturating_add(reserved);
            }
        }
        pressure
    }

    /// 自有区总量（可回收 + 不可回收）。
    pub fn own_reserved_bytes(&self) -> u64 {
        self.reclaimable_own_bytes
            .saturating_add(self.irreducible_own_bytes)
    }

    /// 驻留总量（饱和相加，不溢出）。
    pub fn total_reserved_bytes(&self) -> u64 {
        self.own_reserved_bytes()
            .saturating_add(self.irreducible_inherited_bytes)
    }

    /// Full 可回收的自有区字节（仅包含 Full 实际会归档的条目）。
    pub fn reclaimable_own_reserved_bytes(&self) -> u64 {
        self.reclaimable_own_bytes
    }

    /// Full 无法回收的自有区字节：System 与 protected。
    ///
    /// 已 excluded 的非 System 条目不会再被 Full 归档，通常已由
    /// `shrink_after_full_commit` 释放；若仍存在也暂计此处，随后由收缩释放。
    pub fn irreducible_own_reserved_bytes(&self) -> u64 {
        self.irreducible_own_bytes
    }

    /// Full 无法回收的继承残余字节（祖先区由父会话/冻结上下文持有）。
    pub fn irreducible_inherited_reserved_bytes(&self) -> u64 {
        self.irreducible_inherited_bytes
    }

    /// 是否启用驻留门控。
    pub fn is_enabled(&self) -> bool {
        self.limit_bytes > 0
    }

    /// 可回收自有区自身是否严格超过完整预算。
    ///
    /// 残余主导时只有该条件成立才值得再次 Full：总量可能因继承区或不可回收 own
    /// 超限，但只有可回收部分能通过 Full 释放。未启用门控时恒为 false。
    pub fn reclaimable_exceeds_limit(&self) -> bool {
        self.is_enabled() && self.reclaimable_own_bytes > self.limit_bytes
    }

    /// 是否严格超过上限；恰好等于上限不触发，未启用不触发。
    pub fn is_exceeded(&self) -> bool {
        self.is_enabled() && self.total_reserved_bytes() > self.limit_bytes
    }

    /// 以 `total - limit` 记录超出量；未超限时为 0。
    pub fn excess_bytes(&self) -> u64 {
        self.total_reserved_bytes().saturating_sub(self.limit_bytes)
    }
}

/// 需要升级到 Full Compact 的原因
/// 升级到 Full Compact 的原因（事实源 peri-acp-types::compact）
pub use peri_acp_types::compact::FullEscalationReason;

/// Compact 策略配置
#[derive(Debug, Clone)]
pub struct CompactPolicy {
    /// 目标回收 token 下限
    pub target_reclaim_tokens: u64,
    /// 强制升级 Full 的阈值百分比（0.0-1.0）
    pub force_full_threshold: f64,
    /// Shadow mode：只估算不应用
    pub shadow_mode: bool,
    /// Cache-aware：高缓存命中时延迟清理
    pub cache_aware: bool,
}

impl Default for CompactPolicy {
    fn default() -> Self {
        Self {
            target_reclaim_tokens: 0,
            force_full_threshold: 0.95,
            shadow_mode: false,
            cache_aware: false,
        }
    }
}

/// Compact 应用结果报告
#[derive(Debug, Clone)]
pub struct ApplyReport {
    pub candidate_count: usize,
    pub changed_messages: usize,
    pub changed_fields: usize,
    pub no_op_candidates: usize,
    pub estimated_tokens_saved: u64,
    pub persistence_batch_size: usize,
}

#[path = "planner/catalogs.rs"]
mod catalogs;
#[path = "planner/costs.rs"]
mod costs;
#[path = "planner/index.rs"]
pub(crate) mod index;
#[path = "planner/planning.rs"]
mod planning;
#[path = "planner/sources.rs"]
mod sources;

pub use planning::plan_micro_with_facts;

/// Inputs are snapshots of confirmed session-runtime facts, never inferred from message age.
#[derive(Debug, Default, Clone)]
pub struct MicroPlanningFacts {
    pub exposed_message_ids: HashSet<MessageId>,
    pub protected_message_ids: HashSet<MessageId>,
    pub closed_call_ids: HashSet<String>,
    pub tool_retention: HashMap<String, ContextRetention>,
    pub archive_scope: Option<ArchivePlanningScope>,
    pub fixed_view_tokens: u64,
}

#[derive(Debug, Clone)]
pub struct ArchivePlanningScope {
    pub thread_id: String,
    pub committed_tail: MessageId,
}

#[derive(Debug, Default, Clone)]
pub struct PlanningDiagnostics {
    pub trigger_pressure_tokens: u64,
    pub desired_view_tokens: u64,
    pub soft_recent_budget_tokens: u64,
    pub receipt_budget_tokens: u64,
    pub protected_tokens_by_reason: std::collections::BTreeMap<String, u64>,
    pub remaining_tokens_by_kind: std::collections::BTreeMap<String, u64>,
    pub unhandled_by_reason: std::collections::BTreeMap<String, u64>,
    pub candidate_count: usize,
    pub preview_count: usize,
    pub receipt_count: usize,
    pub archived_exchange_count: usize,
    pub zero_gain_count: usize,
    pub scanned_messages: usize,
    pub preview_catalog_source_scans: usize,
    pub target_shortfall_tokens: u64,
}

impl PlanningDiagnostics {
    pub(crate) fn protect(&mut self, reason: &str, tokens: u64) {
        *self
            .protected_tokens_by_reason
            .entry(reason.into())
            .or_default() += tokens;
    }
    pub(crate) fn unhandled(&mut self, reason: &str, tokens: u64) {
        *self.unhandled_by_reason.entry(reason.into()).or_default() += tokens;
    }
}

#[cfg(test)]
thread_local! {
    static PLAN_MICRO_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub fn reset_plan_micro_calls() {
    PLAN_MICRO_CALLS.with(|calls| calls.set(0));
}

#[cfg(test)]
pub fn plan_micro_call_count() -> usize {
    PLAN_MICRO_CALLS.with(std::cell::Cell::get)
}

/// A caller without current-run consumption evidence cannot newly project history.
/// Use `plan_micro_with_facts` in the Compact stage after the existing pressure gate.
pub fn plan_micro(
    transcript: &MessageTranscript,
    config: &CompactConfig,
    _skip_existing_truncated: bool,
) -> MicroCompactPlan {
    let pressure = ContextPressure {
        estimated_tokens: 0,
        context_window: 0,
        output_reserve: 0,
        predicted_tool_growth: 0,
        safety_buffer: 0,
        cache_hit_rate: 0.0,
    };
    match plan_micro_with_facts(
        transcript,
        config,
        &pressure,
        &MicroPlanningFacts::default(),
        &super::projection::ProviderCapabilities::default(),
    ) {
        Ok(plan) => plan,
        Err(error) => {
            // Legacy convenience entry points cannot return errors. They produce no mutation;
            // production uses the fallible entry point and blocks the unsafe request.
            tracing::error!(%error, "micro planning rejected history without runtime facts");
            MicroCompactPlan {
                policy_version: PROJECTION_POLICY_VERSION,
                ..Default::default()
            }
        }
    }
}

#[cfg(test)]
#[path = "planner_test.rs"]
mod tests;
