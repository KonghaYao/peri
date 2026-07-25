//! Compact v2 — 标记代替删除的上下文压缩
//!
//! 触发流程：
//! - budget < 0.75：跳过
//! - budget ≥ 0.75：Micro Compact 或 Smart Compact（根据配置选择）
//!   - Micro：按 round 分组，旧轮次的工具消息标 truncated
//!   - Smart：保留最近 N 条 Human/Ai + 最近 M 个 Tool 结果 + 所有错误消息，其余标 truncated
//!   - affected_count ≥ micro_min_affected → 有效，budget ≥ 0.95 时叠加 Full
//!   - affected_count < micro_min_affected → 无效，升级为 Full
//! - force=true：直接 Full（跳过 Micro/Smart）
//!
//! 与 v1 的区别：v2 基于 `MessageTranscript` 标记 API，不修改消息本体，
//! 旧消息标 `excluded` 后 `visible_messages()` 自动过滤。
//! Full Compact 通过 `BaseModel::invoke` 标准链路请求摘要。
//! 所有注入消息使用 `BaseMessage::human()` —— 禁止 System，防止 hoist 污染 FrozenContext。

use tracing::{debug, info, warn};

use crate::agent::events::CompactStrategy;
use crate::llm::BaseModel;
use crate::session::transcript::MessageTranscript;

pub mod config;
pub mod full;
pub mod micro;
pub mod planner;
pub mod projection;
pub mod smart;

// ─── 公共重导出：保持外部调用路径不变 ─────────────────────────────────────────────

pub use config::{CompactConfig, CONTINUATION_HINT};
pub use full::{extract_file_info, extract_skill_names, re_inject_v2, ReInjectResult};
pub use micro::micro_compact;
pub use planner::{plan_micro, ApplyReport, CompactPolicy, ContextPressure, FullEscalationReason};
pub use projection::{
    render_llm_view, MessageProjectionDirective, MicroCompactPlan, ProjectionAction,
    ProjectionActionEntry, ProjectionTarget, ProviderCapabilities, ProviderProtocol,
};

// ─── CompactResult ───────────────────────────────────────────────────────────────

/// Compact 执行结果
#[derive(Debug, Clone)]
pub struct CompactResult {
    /// 使用的策略
    pub strategy: CompactStrategy,
    /// 操作的消息数量（标 truncated / excluded 的数量）
    pub affected_count: usize,
    /// 估算节省的 token 数量
    pub estimated_tokens_saved: u64,
    /// 操作前可见消息数量
    pub before_visible_len: usize,
    /// 操作后可见消息数量
    pub after_visible_len: usize,
    /// Full Compact 生成的摘要（Micro/Smart 时为 None）
    pub summary: Option<String>,
    /// 升级到 Full 的原因（Micro/Smart 时为 None）
    pub full_escalation_reason: Option<FullEscalationReason>,
}

// ─── 顶层入口 ───────────────────────────────────────────────────────────────────

/// Compact 阶段动作
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactAction {
    /// 跳过 compact（预算充足）
    Skip,
    /// 执行 Micro Compact
    Micro,
    /// 执行 Smart Compact（规则驱动，保留关键消息）
    Smart,
}

/// 根据 budget 和配置决定 Compact 动作。
///
/// 返回 `Skip` 表示预算未到 75%，跳过 compact。
/// 返回 `Smart` 表示启用 Smart Compact 且预算 ≥ 75%。
/// 返回 `Micro` 表示未启用 Smart Compact 且预算 ≥ 75%。
///
/// Full Compact 的触发不在本函数内判定——由 run_compact 在执行后
/// 根据 affected_count 和 budget 动态决策。
pub fn determine_compact_action(budget: f64, config: &CompactConfig) -> CompactAction {
    if budget >= config.micro_compact_threshold {
        if config.smart_compact_enabled {
            CompactAction::Smart
        } else {
            CompactAction::Micro
        }
    } else {
        CompactAction::Skip
    }
}

/// 根据 ContextPressure 选择策略并执行 Compact
///
/// 触发流程（新）：
/// - 防死循环：连续失败超限则跳过
/// - force=true：直接 Full
/// - 计算 budget_pct，判定 Micro/Smart/Skip
/// - Micro：dry-run plan_micro → 检查 estimated_tokens_saved →
///   - 满足 target：apply Micro
///   - 不足且 budget >= force_full_threshold：跳过 Micro apply → 直接 Full
///   - 不足但未达 Full 阈值：apply Micro（部分收益也好）
/// - Smart：规则驱动保留关键消息，逻辑同 Micro
pub async fn run_compact(
    transcript: &mut MessageTranscript,
    llm: Option<&dyn BaseModel>,
    config: &CompactConfig,
    pressure: &ContextPressure,
    force: bool,
    consecutive_failures: &mut u32,
    cwd: &str,
) -> CompactResult {
    let before_visible_len = transcript.visible_messages().len();

    // 防死循环：连续失败超限则跳过
    if *consecutive_failures >= config.max_consecutive_failures {
        debug!(consecutive_failures, "Compact 降级：连续失败超限，跳过本轮");
        return CompactResult {
            strategy: CompactStrategy::Micro,
            affected_count: 0,
            estimated_tokens_saved: 0,
            before_visible_len,
            after_visible_len: before_visible_len,
            summary: None,
            full_escalation_reason: None,
        };
    }

    // 手动触发 → 直接 Full
    if force {
        return run_full_or_degrade(
            transcript,
            llm,
            config,
            before_visible_len,
            consecutive_failures,
            cwd,
            FullEscalationReason::ManualForce,
        )
        .await;
    }

    // 从 pressure 计算 budget 百分比
    let budget_pct = if pressure.context_window > 0 {
        pressure.estimated_tokens as f64 / pressure.context_window as f64
    } else {
        0.0
    };

    // 从 pressure 计算目标回收量（Micro 和 Smart 共享判定依据）
    let reclaim_target = pressure.target_reclaim_tokens();

    // 检查 Compact 触发条件
    match determine_compact_action(budget_pct, config) {
        CompactAction::Skip => CompactResult {
            strategy: CompactStrategy::Skip,
            affected_count: 0,
            estimated_tokens_saved: 0,
            before_visible_len,
            after_visible_len: before_visible_len,
            summary: None,
            full_escalation_reason: None,
        },
        CompactAction::Micro => {
            // Cache-aware：高缓存命中 + headroom 足够时，延迟 compact
            let cache_hit_rate = pressure.cache_hit_rate;
            if config.cache_aware_enabled && cache_hit_rate > 0.7 {
                let headroom_pct =
                    1.0 - (pressure.estimated_tokens as f64 / pressure.context_window as f64);
                if headroom_pct > 0.2 {
                    debug!(
                        cache_hit_rate = %cache_hit_rate,
                        headroom_pct = %headroom_pct,
                        "Cache-aware: 高缓存命中且充足 headroom，跳过 compact"
                    );
                    return CompactResult {
                        strategy: CompactStrategy::Skip,
                        affected_count: 0,
                        estimated_tokens_saved: 0,
                        before_visible_len,
                        after_visible_len: before_visible_len,
                        summary: None,
                        full_escalation_reason: None,
                    };
                }
            }

            // Dry-run：先用 plan_micro 估算效果（无副作用）
            let plan = plan_micro(transcript, config);

            // Shadow mode：只估算不应用
            if config.shadow_mode_enabled {
                info!(
                    estimated_saved = plan.estimated_tokens_saved,
                    actions_count = plan.actions.len(),
                    shadow = true,
                    "Shadow mode: 估算 compact 收益（未应用）"
                );
                return CompactResult {
                    strategy: CompactStrategy::Micro,
                    affected_count: plan.actions.len(),
                    estimated_tokens_saved: plan.estimated_tokens_saved,
                    before_visible_len,
                    after_visible_len: before_visible_len, // 实际未改变
                    summary: None,
                    full_escalation_reason: None,
                };
            }

            if plan.estimated_tokens_saved >= reclaim_target && plan.has_changes() {
                // Micro 满足回收目标 → 应用
                let affected = micro::micro_compact(transcript, config);
                *consecutive_failures = 0;
                debug!(
                    saved = plan.estimated_tokens_saved,
                    target = reclaim_target,
                    affected,
                    "Micro 满足回收目标，已应用"
                );
                CompactResult {
                    strategy: CompactStrategy::Micro,
                    affected_count: affected,
                    estimated_tokens_saved: plan.estimated_tokens_saved,
                    before_visible_len,
                    after_visible_len: transcript.visible_messages().len(),
                    summary: None,
                    full_escalation_reason: None,
                }
            } else if budget_pct >= config.auto_compact_threshold && reclaim_target > 0 {
                // 不足且达到 Full 阈值 → 跳过 Micro apply，直接 Full
                debug!(
                    saved = plan.estimated_tokens_saved,
                    target = reclaim_target,
                    budget_pct,
                    "Micro 回收不足 + budget 高位 → 跳过 Micro apply，直接 Full"
                );
                run_full_or_degrade(
                    transcript,
                    llm,
                    config,
                    before_visible_len,
                    consecutive_failures,
                    cwd,
                    FullEscalationReason::InsufficientReclaim,
                )
                .await
            } else {
                // 不足但未达 Full 阈值 → 应用 Micro（部分收益也好）
                let affected = micro::micro_compact(transcript, config);
                *consecutive_failures = 0;
                debug!(
                    saved = plan.estimated_tokens_saved,
                    target = reclaim_target,
                    affected,
                    "Micro 回收不足但未达 Full 阈值 → 应用 Micro 部分收益"
                );
                CompactResult {
                    strategy: CompactStrategy::Micro,
                    affected_count: affected,
                    estimated_tokens_saved: plan.estimated_tokens_saved,
                    before_visible_len,
                    after_visible_len: transcript.visible_messages().len(),
                    summary: None,
                    full_escalation_reason: None,
                }
            }
        }
        CompactAction::Smart => {
            // 执行 Smart Compact（规则驱动，不调用 LLM）
            let (affected, estimated_tokens_saved) = smart::smart_compact(transcript, config);
            *consecutive_failures = 0;

            // 用 estimated_tokens_saved 替代 affected 做有效性判定（P0-2）
            if estimated_tokens_saved >= reclaim_target {
                if budget_pct >= config.auto_compact_threshold {
                    debug!(affected, budget_pct, "Smart 有效 + budget 高位 → 叠加 Full");
                    run_full_or_degrade(
                        transcript,
                        llm,
                        config,
                        before_visible_len,
                        consecutive_failures,
                        cwd,
                        FullEscalationReason::ForceThresholdExceeded,
                    )
                    .await
                } else {
                    CompactResult {
                        strategy: CompactStrategy::Smart,
                        affected_count: affected,
                        estimated_tokens_saved,
                        before_visible_len,
                        after_visible_len: transcript.visible_messages().len(),
                        summary: None,
                        full_escalation_reason: None,
                    }
                }
            } else {
                // Smart 无效 → 升级为 Full
                debug!(affected, budget_pct, "Smart 无效 → 升级为 Full");
                run_full_or_degrade(
                    transcript,
                    llm,
                    config,
                    before_visible_len,
                    consecutive_failures,
                    cwd,
                    FullEscalationReason::InsufficientReclaim,
                )
                .await
            }
        }
    }
}

/// 运行 Full Compact（含失败降级逻辑）
async fn run_full_or_degrade(
    transcript: &mut MessageTranscript,
    llm: Option<&dyn BaseModel>,
    config: &CompactConfig,
    before_visible_len: usize,
    consecutive_failures: &mut u32,
    cwd: &str,
    escalation_reason: FullEscalationReason,
) -> CompactResult {
    // 重跑保护：清除上轮残留 excluded 标记
    if *consecutive_failures > 0 {
        let stale_ids: Vec<_> = transcript
            .entries()
            .iter()
            .filter(|e| transcript.flags(e.message.id()).excluded)
            .map(|e| e.message.id())
            .collect();
        for id in &stale_ids {
            transcript.set_excluded(*id, false);
        }
        if !stale_ids.is_empty() {
            debug!(count = stale_ids.len(), "清除上轮残留 excluded 标记");
        }
    }

    match full::full_compact_inner(transcript, llm, config, cwd).await {
        Ok(mut result) => {
            *consecutive_failures = 0;
            result.full_escalation_reason = Some(escalation_reason);
            result
        }
        Err(e) => {
            warn!(error = %e, "Full Compact 失败");
            *consecutive_failures += 1;
            CompactResult {
                strategy: CompactStrategy::Full,
                affected_count: 0,
                estimated_tokens_saved: 0,
                before_visible_len,
                after_visible_len: transcript.visible_messages().len(),
                summary: None,
                full_escalation_reason: Some(escalation_reason),
            }
        }
    }
}

#[cfg(test)]
#[path = "planner_test.rs"]
mod planner_tests;

#[cfg(test)]
#[path = "projection_test.rs"]
mod projection_tests;

#[cfg(test)]
mod _test;

#[cfg(test)]
#[path = "trigger_test.rs"]
mod trigger_test;
