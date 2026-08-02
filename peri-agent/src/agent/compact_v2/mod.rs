//! Compact v2 — 标记代替删除的上下文压缩
//!
//! 触发流程：
//! - budget < 0.75：跳过
//! - budget ≥ 0.75：Micro Compact 或 Smart Compact（根据 Smart 开关选择）
//!   - Micro/Smart 始终先执行并应用（标记 truncated）
//!   - 若 budget ≥ 0.95 且 reclaim_target > 0：Micro/Smart 后叠加 Full Compact
//!   - 若 budget < 0.95：仅 Micro/Smart，不触发 Full
//!   - 决策指标：estimated_tokens_saved >= reclaim_target（非 micro_min_affected）
//! - force=true：直接 Full（跳过 Micro/Smart）
//!
//! 与 v1 的区别：v2 基于 `MessageTranscript` 标记 API，不修改消息本体，
//! 旧消息标 `excluded` 后 `visible_messages()` 自动过滤。
//! Full Compact 通过 `peri_model::Model` 标准链路请求摘要。
//! 所有注入消息使用 `BaseMessage::human()` —— 禁止 System，防止 hoist 污染 FrozenContext。

use tracing::{debug, info, warn};

use crate::agent::events::CompactStrategy;
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
    plan_from_persisted_directives, render_llm_view, MessageProjectionDirective, MicroCompactPlan,
    ProjectionAction, ProjectionActionEntry, ProjectionTarget, ProviderCapabilities,
    ProviderProtocol, CORRUPTED_PROJECTION, DIRECTIVE_VERSION_MISMATCH, NO_PERSISTED_DIRECTIVES,
    PROJECTION_POLICY_VERSION,
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
    /// 本轮 Compact 的实际语义结果。
    pub outcome: CompactOutcome,
    /// 去重 message_id 数量（Micro 投影变更计数）
    pub changed_messages: usize,
    /// CompactToolInput 中的所有字段总数
    pub changed_fields: usize,
    /// 通过 stale/retention 筛选但无内容的候选数
    pub no_op_candidates: usize,
}

/// Compact 执行的语义结果。
///
/// `strategy` 暂时仍是对外兼容的有效策略视图；调用方应通过本枚举判断
/// Full Compact 是否真正完成。后续可将此查询结果提升为持久化的完整 outcome。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactOutcome {
    /// 本轮未应用 Compact。
    Skipped,
    /// 已应用 Micro Compact。
    MicroApplied,
    /// 已应用 Smart Compact。
    SmartApplied,
    /// 已成功应用 Full Compact。
    FullApplied,
    /// Full Compact 未成功应用。
    FullFailed,
    /// 仅估算了 Compact 收益，未修改 transcript。
    Shadowed,
    /// 已应用 Micro Compact，但后续 Full Compact 失败。
    MicroAppliedThenFullFailed,
    /// 已应用 Smart Compact，但后续 Full Compact 失败。
    SmartAppliedThenFullFailed,
    /// Compact 已提交（transcript 已修改），但在事件发送前被取消（G6）。
    InterruptedAfterCommit,
}

impl CompactOutcome {
    /// 是否已在 transcript 中应用 Compact 变更。
    pub fn has_applied_change(self) -> bool {
        matches!(
            self,
            Self::MicroApplied
                | Self::SmartApplied
                | Self::FullApplied
                | Self::MicroAppliedThenFullFailed
                | Self::SmartAppliedThenFullFailed
                | Self::InterruptedAfterCommit
        )
    }

    /// 是否已成功应用 Full Compact。
    pub fn is_full_applied(self) -> bool {
        matches!(self, Self::FullApplied)
    }
}

impl CompactResult {
    /// 返回本轮 Compact 的语义结果。
    pub fn outcome(&self) -> CompactOutcome {
        self.outcome
    }
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
    llm: Option<&dyn peri_model::Model>,
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
            strategy: CompactStrategy::Skip,
            affected_count: 0,
            estimated_tokens_saved: 0,
            before_visible_len,
            after_visible_len: before_visible_len,
            summary: None,
            full_escalation_reason: None,
            outcome: CompactOutcome::Skipped,
            changed_messages: 0,
            changed_fields: 0,
            no_op_candidates: 0,
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
            outcome: CompactOutcome::Skipped,
            changed_messages: 0,
            changed_fields: 0,
            no_op_candidates: 0,
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
                        outcome: CompactOutcome::Skipped,
                        changed_messages: 0,
                        changed_fields: 0,
                        no_op_candidates: 0,
                    };
                }
            }

            // Dry-run：先用 plan_micro 估算效果（无副作用）
            let plan = plan_micro(transcript, config, true);

            // 空 plan → 无可 compact 消息
            // 但如果 budget 已超过 Full 阈值，直接尝试 Full Compact
            // 否则纯对话场景下 context 会持续膨胀到远超 100% 而永远不 compact
            //
            // 安全性：Full Compact 依赖 compact_llm 生成摘要；若 llm 为 None，
            // Full 必然失败（CompactNoLlm），不应在此路径触发——否则每轮都
            // consecutive_failures++ 最终达到 max_consecutive_failures 上限，
            // 导致 compact 被永久静默禁用。
            if plan.actions.is_empty() {
                if budget_pct >= config.auto_compact_threshold {
                    if llm.is_none() {
                        warn!(
                            "Micro Compact: plan 为空且 budget 高位({:.1}%)，但 compact_llm 未配置，无法执行 Full Compact。请配置 compact_llm 或启用 Micro Compact 可用工具。",
                            budget_pct * 100.0
                        );
                        return CompactResult {
                            strategy: CompactStrategy::Skip,
                            affected_count: 0,
                            estimated_tokens_saved: 0,
                            before_visible_len,
                            after_visible_len: before_visible_len,
                            summary: None,
                            full_escalation_reason: Some(
                                FullEscalationReason::ForceThresholdExceeded,
                            ),
                            outcome: CompactOutcome::Skipped,
                            changed_messages: 0,
                            changed_fields: 0,
                            no_op_candidates: 0,
                        };
                    }
                    debug!(
                        "Micro Compact: plan 为空但 budget 高位({:.1}%)，直接尝试 Full",
                        budget_pct * 100.0
                    );
                    return run_full_or_degrade(
                        transcript,
                        llm,
                        config,
                        before_visible_len,
                        consecutive_failures,
                        cwd,
                        FullEscalationReason::ForceThresholdExceeded,
                    )
                    .await;
                }
                debug!("Micro Compact: plan 为空，无消息可 compact，跳过");
                // "无事可做"不是失败，清除历史失败计数避免误触发
                // max_consecutive_failures 守卫
                *consecutive_failures = 0;
                return CompactResult {
                    strategy: CompactStrategy::Skip,
                    affected_count: 0,
                    estimated_tokens_saved: 0,
                    before_visible_len,
                    after_visible_len: before_visible_len,
                    summary: None,
                    full_escalation_reason: None,
                    outcome: CompactOutcome::Skipped,
                    changed_messages: 0,
                    changed_fields: 0,
                    no_op_candidates: 0,
                };
            }

            // Shadow mode：只估算不应用
            if config.shadow_mode_enabled {
                info!(
                    estimated_saved = plan.estimated_tokens_saved,
                    actions_count = plan.actions.len(),
                    shadow = true,
                    "Shadow mode: 估算 compact 收益（未应用）"
                );
                return CompactResult {
                    strategy: CompactStrategy::Skip,
                    affected_count: 0,
                    estimated_tokens_saved: plan.estimated_tokens_saved,
                    before_visible_len,
                    after_visible_len: before_visible_len,
                    summary: None,
                    full_escalation_reason: None,
                    outcome: CompactOutcome::Shadowed,
                    changed_messages: 0,
                    changed_fields: 0,
                    no_op_candidates: plan.no_op_candidates,
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
                    outcome: CompactOutcome::MicroApplied,
                    changed_messages: plan.changed_messages,
                    changed_fields: plan.changed_fields,
                    no_op_candidates: plan.no_op_candidates,
                }
            } else if budget_pct >= config.auto_compact_threshold && reclaim_target > 0 {
                // Micro 回收不足 + budget 高位 → 先应用 Micro，再叠加 Full
                // 设计决策：Micro 每轮都提供实际收益（truncated 标记持久化到 transcript），
                // 因此先执行 micro_compact 再叠加 Full——即使 Full 失败，Micro 的截断仍然生效。
                // Smart 路径（`mod.rs:304`）有相同模式：先 Smart 再 Full，Full 失败不影响 Smart 结果。
                let micro_affected = micro::micro_compact(transcript, config);
                // 注意：estimated_tokens_saved 使用 dry-run 估计
                // 实际 micro 应用后的节省可能略有不同，但 plan 是最佳可用近似
                debug!(
                    saved = plan.estimated_tokens_saved,
                    target = reclaim_target,
                    budget_pct,
                    micro_affected,
                    "Micro 回收不足 + budget 高位 → 先应用 Micro，再叠加 Full"
                );
                let mut full_result = run_full_or_degrade(
                    transcript,
                    llm,
                    config,
                    before_visible_len,
                    consecutive_failures,
                    cwd,
                    FullEscalationReason::InsufficientReclaim,
                )
                .await;
                // 合并 Micro 的 affected_count（即使 Full 失败，Micro 的截断已持久化）
                full_result.affected_count += micro_affected;
                // estimated_tokens_saved 也需要累加 Micro 的贡献
                full_result.estimated_tokens_saved += plan.estimated_tokens_saved;
                // 从 Micro plan 填充统计字段
                full_result.changed_messages = plan.changed_messages;
                full_result.changed_fields = plan.changed_fields;
                full_result.no_op_candidates = plan.no_op_candidates;
                if micro_affected > 0 && !full_result.outcome().is_full_applied() {
                    // Full 未完成时，对外兼容策略必须反映实际已生效的 Micro。
                    full_result.strategy = CompactStrategy::Micro;
                    full_result.outcome = CompactOutcome::MicroAppliedThenFullFailed;
                }
                full_result
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
                    outcome: CompactOutcome::MicroApplied,
                    changed_messages: plan.changed_messages,
                    changed_fields: plan.changed_fields,
                    no_op_candidates: plan.no_op_candidates,
                }
            }
        }
        CompactAction::Smart => {
            if config.shadow_mode_enabled {
                let plan = plan_micro(transcript, config, true);
                info!(
                    estimated_saved = plan.estimated_tokens_saved,
                    actions_count = plan.actions.len(),
                    shadow = true,
                    "Shadow mode: 估算 Smart Compact 收益（未应用）"
                );
                return CompactResult {
                    strategy: CompactStrategy::Skip,
                    affected_count: 0,
                    estimated_tokens_saved: plan.estimated_tokens_saved,
                    before_visible_len,
                    after_visible_len: before_visible_len,
                    summary: None,
                    full_escalation_reason: None,
                    outcome: CompactOutcome::Shadowed,
                    changed_messages: 0,
                    changed_fields: 0,
                    no_op_candidates: plan.no_op_candidates,
                };
            }

            // Smart Compact 已废弃，委托给 Micro Compact
            tracing::warn!("Smart Compact is deprecated, falling back to Micro Compact");
            let plan = plan_micro(transcript, config, true);
            let affected = micro::micro_compact(transcript, config);
            let estimated_tokens_saved = plan.estimated_tokens_saved;

            // 空结果 → 无可 compact 消息
            // 但如果 budget 已超过 Full 阈值，直接尝试 Full Compact
            //
            // 安全性：Full Compact 依赖 compact_llm；若 llm 为 None，不应在此路径
            // 触发，避免 consecutive_failures 无意义增长到上限。
            if affected == 0 {
                if budget_pct >= config.auto_compact_threshold {
                    if llm.is_none() {
                        warn!(
                            "Smart Compact: 无消息可 compact 且 budget 高位({:.1}%)，但 compact_llm 未配置，无法执行 Full Compact。",
                            budget_pct * 100.0
                        );
                        return CompactResult {
                            strategy: CompactStrategy::Skip,
                            affected_count: 0,
                            estimated_tokens_saved: 0,
                            before_visible_len,
                            after_visible_len: before_visible_len,
                            summary: None,
                            full_escalation_reason: Some(
                                FullEscalationReason::ForceThresholdExceeded,
                            ),
                            outcome: CompactOutcome::Skipped,
                            changed_messages: 0,
                            changed_fields: 0,
                            no_op_candidates: 0,
                        };
                    }
                    debug!(
                        "Smart Compact: 无消息可 compact 但 budget 高位({:.1}%)，直接尝试 Full",
                        budget_pct * 100.0
                    );
                    return run_full_or_degrade(
                        transcript,
                        llm,
                        config,
                        before_visible_len,
                        consecutive_failures,
                        cwd,
                        FullEscalationReason::ForceThresholdExceeded,
                    )
                    .await;
                }
                debug!("Smart Compact: 无消息可 compact，跳过");
                *consecutive_failures = 0;
                return CompactResult {
                    strategy: CompactStrategy::Skip,
                    affected_count: 0,
                    estimated_tokens_saved: 0,
                    before_visible_len,
                    after_visible_len: before_visible_len,
                    summary: None,
                    full_escalation_reason: None,
                    outcome: CompactOutcome::Skipped,
                    changed_messages: 0,
                    changed_fields: 0,
                    no_op_candidates: 0,
                };
            }

            // 用 estimated_tokens_saved 替代 affected 做有效性判定（P0-2）
            if estimated_tokens_saved >= reclaim_target {
                if budget_pct >= config.auto_compact_threshold {
                    debug!(affected, budget_pct, "Smart 有效 + budget 高位 → 叠加 Full");
                    let mut full_result = run_full_or_degrade(
                        transcript,
                        llm,
                        config,
                        before_visible_len,
                        consecutive_failures,
                        cwd,
                        FullEscalationReason::ForceThresholdExceeded,
                    )
                    .await;
                    // Smart 已在 Full 前实际应用；无论 Full 成功与否，指标都必须
                    // 表示本轮 Smart + Full 的总变更（与 Micro 路径保持一致）。
                    full_result.affected_count += affected;
                    full_result.estimated_tokens_saved += estimated_tokens_saved;
                    full_result.changed_messages = plan.changed_messages;
                    full_result.changed_fields = plan.changed_fields;
                    full_result.no_op_candidates = plan.no_op_candidates;
                    if !full_result.outcome().is_full_applied() {
                        full_result.strategy = CompactStrategy::Smart;
                        full_result.outcome = CompactOutcome::SmartAppliedThenFullFailed;
                    }
                    full_result
                } else {
                    // Smart 有效但未达 Full 阈值 → 清除历史失败计数
                    // 与 Micro 路径（line 355/417）行为对齐
                    *consecutive_failures = 0;
                    CompactResult {
                        strategy: CompactStrategy::Smart,
                        affected_count: affected,
                        estimated_tokens_saved,
                        before_visible_len,
                        after_visible_len: transcript.visible_messages().len(),
                        summary: None,
                        full_escalation_reason: None,
                        outcome: CompactOutcome::SmartApplied,
                        changed_messages: plan.changed_messages,
                        changed_fields: plan.changed_fields,
                        no_op_candidates: plan.no_op_candidates,
                    }
                }
            } else {
                // Smart 无效 → 升级为 Full
                debug!(affected, budget_pct, "Smart 无效 → 升级为 Full");
                let mut full_result = run_full_or_degrade(
                    transcript,
                    llm,
                    config,
                    before_visible_len,
                    consecutive_failures,
                    cwd,
                    FullEscalationReason::InsufficientReclaim,
                )
                .await;
                // Smart 已在 Full 前实际应用；无论 Full 成功与否，指标都必须
                // 表示本轮 Smart + Full 的总变更（与 Micro 路径保持一致）。
                full_result.affected_count += affected;
                full_result.estimated_tokens_saved += estimated_tokens_saved;
                full_result.changed_messages = plan.changed_messages;
                full_result.changed_fields = plan.changed_fields;
                full_result.no_op_candidates = plan.no_op_candidates;
                if !full_result.outcome().is_full_applied() {
                    full_result.strategy = CompactStrategy::Smart;
                    full_result.outcome = CompactOutcome::SmartAppliedThenFullFailed;
                }
                full_result
            }
        }
    }
}

/// 运行 Full Compact（含失败降级逻辑）
async fn run_full_or_degrade(
    transcript: &mut MessageTranscript,
    llm: Option<&dyn peri_model::Model>,
    config: &CompactConfig,
    before_visible_len: usize,
    consecutive_failures: &mut u32,
    cwd: &str,
    escalation_reason: FullEscalationReason,
) -> CompactResult {
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
                outcome: CompactOutcome::FullFailed,
                changed_messages: 0,
                changed_fields: 0,
                no_op_candidates: 0,
            }
        }
    }
}

#[cfg(test)]
#[path = "projection_test.rs"]
mod projection_tests;

#[cfg(test)]
mod _test;

#[cfg(test)]
#[path = "trigger_test.rs"]
mod trigger_test;
