//! 自动压缩的唯一收益准入决策。
//!
//! 门槛只比较同一冻结请求的事实：`C` 是完整 `context_window`（不扣输出预留），
//! `G` 是 planner 最终完整渲染后的估算净减少量，`R` 是既有必要回收量。
//! 本模块不规划、不提交、不做 I/O；调用方必须先完成一次合法规划再询问决策。

use super::planner::ContextPressure;
use super::CompactConfig;
use peri_acp_types::compact::{CompactDeferral, CompactDeferralReason, FullEscalationReason};

/// 最小 Micro 净收益的门槛分母：完整上下文窗口的 1/10，向上取整。
///
/// 比例只在 [`min_micro_gain_tokens`] 定义一次；planner、Full 与 Smart 分支
/// 都复用它，避免复制出第二套经济门槛。
pub fn min_micro_gain_tokens(context_window: u32) -> u64 {
    u64::from(context_window).div_ceil(10)
}

/// 自动压缩的决策结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GainDecision {
    /// 未进入 Micro 触发区间，保持当前视图。
    Skip,
    /// 已进入 Micro 区间，但候选为空或净收益不足；本轮主动延后。
    Deferred(CompactDeferral),
    /// 提交一次 Micro。
    Micro,
    /// 进入 Full；本轮不得先提交 Micro。
    Full(FullEscalationReason),
}

/// 按设计决策表给出唯一结论。
///
/// `estimated_gain_tokens` 必须来自同一冻结请求的最终完整渲染；
/// `has_candidates` 表示通过内容与协议校验的候选非空。
pub(crate) fn decide_auto_compact(
    pressure: &ContextPressure,
    config: &CompactConfig,
    estimated_gain_tokens: u64,
    has_candidates: bool,
) -> GainDecision {
    let context_window = pressure.context_window;
    let min_gain = min_micro_gain_tokens(context_window);
    if context_window == 0 {
        // 零窗口没有比例型自动 Micro 的权威门槛；手动 force、驻留 Full 独立。
        return GainDecision::Deferred(CompactDeferral {
            reason: CompactDeferralReason::MissingContextBudget,
            estimated_gain_tokens,
            min_gain_tokens: 0,
        });
    }
    let budget_pct = pressure.estimated_tokens as f64 / context_window as f64;
    if budget_pct < config.micro_compact_threshold {
        return GainDecision::Skip;
    }
    let deferral = |reason| CompactDeferral {
        reason,
        estimated_gain_tokens,
        min_gain_tokens: min_gain,
    };
    if budget_pct >= config.auto_compact_threshold {
        // Full 条件优先：不足 max(M, R) 时直接 Full，不再先提交低收益 Micro。
        if has_candidates && estimated_gain_tokens >= min_gain.max(pressure.target_reclaim_tokens())
        {
            return GainDecision::Micro;
        }
        return GainDecision::Full(if has_candidates {
            FullEscalationReason::InsufficientReclaim
        } else {
            FullEscalationReason::ForceThresholdExceeded
        });
    }
    if !has_candidates {
        return GainDecision::Deferred(deferral(CompactDeferralReason::NoCandidates));
    }
    if estimated_gain_tokens < min_gain {
        return GainDecision::Deferred(deferral(CompactDeferralReason::InsufficientGain));
    }
    GainDecision::Micro
}
