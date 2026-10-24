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
pub mod exposure;
pub mod full;
pub mod micro;
pub mod planner;
pub mod projection;
pub mod smart;

// ─── 公共重导出：保持外部调用路径不变 ─────────────────────────────────────────────

pub use config::{CompactConfig, CONTINUATION_HINT};
pub use full::{extract_file_info, extract_skill_names};
pub use micro::micro_compact;
pub use planner::{
    plan_micro, ApplyReport, CompactPolicy, ContextPressure, FullEscalationReason,
    ResidentHistoryPressure,
};
pub use projection::{
    plan_from_persisted_directives, render_llm_view, MessageProjectionDirective, MicroCompactPlan,
    PersistedDirectiveRestore, ProjectionAction, ProjectionActionEntry, ProjectionTarget,
    ProviderCapabilities, ProviderProtocol, RestoredProjectionDirectives, CORRUPTED_PROJECTION,
    DIRECTIVE_VERSION_MISMATCH, NO_PERSISTED_DIRECTIVES, PROJECTION_POLICY_VERSION,
};

// ─── CompactResult ───────────────────────────────────────────────────────────────

/// Compact 执行结果
#[derive(Debug)]
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
    /// Full 未完成的结构化原因；已应用的 Micro 仍由 outcome 与统计保留。
    pub failure: Option<crate::error::AgentError>,
    /// 去重 message_id 数量（Micro 投影变更计数）
    pub changed_messages: usize,
    /// CompactToolInput 中的所有字段总数
    pub changed_fields: usize,
    /// 通过 stale/retention 筛选但无内容的候选数
    pub no_op_candidates: usize,
}

/// Compact 执行语义结果（事实源 peri-acp-types::compact）
pub use peri_acp_types::compact::CompactOutcome;

impl CompactResult {
    /// 返回本轮 Compact 的语义结果。
    pub fn outcome(&self) -> CompactOutcome {
        self.outcome
    }
}

// ─── 顶层入口 ───────────────────────────────────────────────────────────────────

/// Full 摘要视图构造前的通知（[`run_compact`] 的最后一个参数）。
///
/// Full 在 [`full::full_compact_inner`] 内为摘要请求另建一份整份历史模型视图。持有等价
/// 视图副本的调用方（模型视图复用槽）必须在这份视图构造**之前**放弃自己那份，否则两份
/// 整份正文会在整个摘要请求期间同时存活。决策发生在 `compact_v2` 内部、调用方拿不到
/// 「即将 Full」的信号，因此由调用方传入一次性通知：本模块只在**确实要构造摘要视图**时
/// 调用它。Micro/Smart 未落地、shadow、空计划未达 Full 阈值等不渲染摘要视图的路径不
/// 通知，缓存照常复用。
pub type FullSummaryNotice<'a> = &'a (dyn Fn() + Send + Sync);

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
/// - Full 重试预算在实际需要 Full 时统一检查，耗尽返回结构化失败
/// - force=true：直接 Full
/// - 计算 budget_pct，判定 Micro/Smart/Skip
/// - 计划只规划一次：dry-run 的 plan 直接交给 apply_micro_plan 落地，不二次规划
/// - Micro：dry-run plan_micro → 检查 estimated_tokens_saved →
///   - 满足 target：apply Micro
///   - 不足且 budget >= force_full_threshold：跳过 Micro apply → 直接 Full
///   - 不足但未达 Full 阈值：apply Micro（部分收益也好）
/// - Smart：规则驱动保留关键消息，逻辑同 Micro
///
/// `before_full_summary` 只在确实要构造摘要视图时调用一次（见 [`FullSummaryNotice`]）。
#[allow(clippy::too_many_arguments)] // 参数是 Compact 决策的既有输入，包一层结构体只会挪动调用点噪声
pub async fn run_compact(
    transcript: &mut MessageTranscript,
    llm: Option<&dyn peri_model::Model>,
    config: &CompactConfig,
    pressure: &ContextPressure,
    force: bool,
    consecutive_failures: &mut u32,
    cwd: &str,
    before_full_summary: Option<FullSummaryNotice<'_>>,
) -> CompactResult {
    run_compact_with_protected(
        transcript,
        llm,
        config,
        pressure,
        force,
        consecutive_failures,
        cwd,
        before_full_summary,
        &crate::session::async_result_mailbox::ProtectedResults::default(),
    )
    .await
}

/// 带后台结果保护集合的 Compact。
///
/// 生产路径必须使用本入口：Full Compact 不得归档尚未被主模型响应有效覆盖的
/// 终态通知。测试与无会话路径经 [`run_compact`] 使用空保护集合。
#[allow(clippy::too_many_arguments)]
pub async fn run_compact_with_protected(
    transcript: &mut MessageTranscript,
    llm: Option<&dyn peri_model::Model>,
    config: &CompactConfig,
    pressure: &ContextPressure,
    force: bool,
    consecutive_failures: &mut u32,
    cwd: &str,
    before_full_summary: Option<FullSummaryNotice<'_>>,
    protected: &crate::session::async_result_mailbox::ProtectedResults,
) -> CompactResult {
    run_compact_with_facts(
        transcript,
        llm,
        config,
        pressure,
        force,
        consecutive_failures,
        cwd,
        before_full_summary,
        protected,
        &planner::MicroPlanningFacts::default(),
        &ProviderCapabilities::default(),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn run_compact_with_facts(
    transcript: &mut MessageTranscript,
    llm: Option<&dyn peri_model::Model>,
    config: &CompactConfig,
    pressure: &ContextPressure,
    force: bool,
    consecutive_failures: &mut u32,
    cwd: &str,
    before_full_summary: Option<FullSummaryNotice<'_>>,
    protected: &crate::session::async_result_mailbox::ProtectedResults,
    facts: &planner::MicroPlanningFacts,
    caps: &ProviderCapabilities,
) -> CompactResult {
    run_compact_with_facts_and_resident(
        transcript,
        llm,
        config,
        pressure,
        force,
        consecutive_failures,
        cwd,
        before_full_summary,
        protected,
        facts,
        caps,
        &ResidentHistoryPressure::DISABLED,
    )
    .await
}

/// 带独立驻留字节压力的 Compact 入口。
///
/// 调用方在 token refresh 之前采集 [`ResidentHistoryPressure`]；超出上限时本函数跳过
/// token Skip、压力样本去重与 Micro planner，直接复用 [`run_full_or_degrade`]。
/// `force` 与自动开关仍优先：手动 force 走 `ManualForce`，关闭自动压缩不因驻留超限触发。
#[allow(clippy::too_many_arguments)]
pub async fn run_compact_with_facts_and_resident(
    transcript: &mut MessageTranscript,
    llm: Option<&dyn peri_model::Model>,
    config: &CompactConfig,
    pressure: &ContextPressure,
    force: bool,
    consecutive_failures: &mut u32,
    cwd: &str,
    before_full_summary: Option<FullSummaryNotice<'_>>,
    protected: &crate::session::async_result_mailbox::ProtectedResults,
    facts: &planner::MicroPlanningFacts,
    caps: &ProviderCapabilities,
    resident: &ResidentHistoryPressure,
) -> CompactResult {
    let before_visible_len = transcript.visible_messages().len();

    // 自动关闭不影响显式手动 force。
    if !force && !config.auto_compact_enabled {
        return CompactResult {
            strategy: CompactStrategy::Skip,
            affected_count: 0,
            estimated_tokens_saved: 0,
            before_visible_len,
            after_visible_len: before_visible_len,
            summary: None,
            full_escalation_reason: None,
            outcome: CompactOutcome::Skipped,
            failure: None,
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
            pressure,
            consecutive_failures,
            cwd,
            FullEscalationReason::ManualForce,
            before_full_summary,
            protected,
        )
        .await;
    }

    // 驻留字节预算与 token 压力独立：先于 cache-aware、Micro planner 与达标返回。
    if resident.is_exceeded() {
        return run_resident_full(
            transcript,
            llm,
            config,
            pressure,
            consecutive_failures,
            cwd,
            before_full_summary,
            protected,
            resident,
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
            failure: None,
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
                        failure: None,
                        changed_messages: 0,
                        changed_fields: 0,
                        no_op_candidates: 0,
                    };
                }
            }

            // Dry-run：先用 plan_micro 估算效果（无副作用）
            let plan =
                match planner::plan_micro_with_facts(transcript, config, pressure, facts, caps) {
                    Ok(plan) => plan,
                    Err(error) => return micro_failure(before_visible_len, error),
                };
            tracing::info!(trigger_pressure_tokens = pressure.estimated_tokens, trigger_ratio = budget_pct,
                view_before_estimate = plan.estimated_before_tokens, view_after_estimate = plan.estimated_after_tokens,
                incremental_saved_estimate = plan.estimated_tokens_saved, diagnostics = ?plan.diagnostics,
                policy_version = plan.policy_version, "Micro projection planned (estimates)");

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
                    failure: None,
                    changed_messages: 0,
                    changed_fields: 0,
                    no_op_candidates: plan.no_op_candidates,
                };
            }

            // 空 plan → 无可 compact 消息
            // 但如果 budget 已超过 Full 阈值，直接尝试 Full Compact
            // 否则纯对话场景下 context 会持续膨胀到远超 100% 而永远不 compact
            //
            // 模型缺失同样属于 Full 失败，由 stage 显式终止高压请求。
            if plan.actions.is_empty() {
                if budget_pct >= config.auto_compact_threshold {
                    debug!(
                        "Micro Compact: plan 为空但 budget 高位({:.1}%)，直接尝试 Full",
                        budget_pct * 100.0
                    );
                    return run_full_or_degrade(
                        transcript,
                        llm,
                        config,
                        pressure,
                        consecutive_failures,
                        cwd,
                        FullEscalationReason::ForceThresholdExceeded,
                        before_full_summary,
                        protected,
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
                    failure: None,
                    changed_messages: 0,
                    changed_fields: 0,
                    no_op_candidates: 0,
                };
            }

            if plan.estimated_tokens_saved >= reclaim_target && plan.has_changes() {
                // Micro 满足回收目标 → 应用（复用 dry-run 计划，不二次规划）
                let affected = match micro::apply_micro_plan(transcript, &plan).await {
                    Ok(affected) => affected,
                    Err(error) => return micro_failure(before_visible_len, error),
                };
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
                    failure: None,
                    changed_messages: plan.changed_messages,
                    changed_fields: plan.changed_fields,
                    no_op_candidates: plan.no_op_candidates,
                }
            } else if budget_pct >= config.auto_compact_threshold && reclaim_target > 0 {
                // Micro 回收不足 + budget 高位 → 先应用 Micro，再叠加 Full
                // 设计决策：Micro 每轮都提供实际收益（truncated 标记持久化到 transcript），
                // 因此先执行 micro_compact 再叠加 Full——即使 Full 失败，Micro 的截断仍然生效。
                // Smart 路径（`mod.rs:304`）有相同模式：先 Smart 再 Full，Full 失败不影响 Smart 结果。
                let micro_affected = match micro::apply_micro_plan(transcript, &plan).await {
                    Ok(affected) => affected,
                    Err(error) => return micro_failure(before_visible_len, error),
                };
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
                    pressure,
                    consecutive_failures,
                    cwd,
                    FullEscalationReason::InsufficientReclaim,
                    before_full_summary,
                    protected,
                )
                .await;
                // Full 成功时，其 excluded transition 已覆盖 Micro affected messages；
                // 只有 Full 失败时才保留已生效的 Micro affected_count。
                if !full_result.outcome().is_full_applied() {
                    full_result.affected_count += micro_affected;
                }
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
                // 不足但未达 Full 阈值 → 应用 Micro（部分收益也好），复用 dry-run 计划
                let affected = match micro::apply_micro_plan(transcript, &plan).await {
                    Ok(affected) => affected,
                    Err(error) => return micro_failure(before_visible_len, error),
                };
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
                    failure: None,
                    changed_messages: plan.changed_messages,
                    changed_fields: plan.changed_fields,
                    no_op_candidates: plan.no_op_candidates,
                }
            }
        }
        CompactAction::Smart => {
            if config.shadow_mode_enabled {
                let plan =
                    match planner::plan_micro_with_facts(transcript, config, pressure, facts, caps)
                    {
                        Ok(plan) => plan,
                        Err(error) => return micro_failure(before_visible_len, error),
                    };
                tracing::info!(trigger_pressure_tokens = pressure.estimated_tokens, trigger_ratio = budget_pct,
                view_before_estimate = plan.estimated_before_tokens, view_after_estimate = plan.estimated_after_tokens,
                incremental_saved_estimate = plan.estimated_tokens_saved, diagnostics = ?plan.diagnostics,
                policy_version = plan.policy_version, "Micro projection planned (estimates)");
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
                    failure: None,
                    changed_messages: 0,
                    changed_fields: 0,
                    no_op_candidates: plan.no_op_candidates,
                };
            }

            // Smart Compact 已废弃，委托给 Micro Compact
            tracing::warn!("Smart Compact is deprecated, falling back to Micro Compact");
            let plan =
                match planner::plan_micro_with_facts(transcript, config, pressure, facts, caps) {
                    Ok(plan) => plan,
                    Err(error) => return micro_failure(before_visible_len, error),
                };
            tracing::info!(trigger_pressure_tokens = pressure.estimated_tokens, trigger_ratio = budget_pct,
                view_before_estimate = plan.estimated_before_tokens, view_after_estimate = plan.estimated_after_tokens,
                incremental_saved_estimate = plan.estimated_tokens_saved, diagnostics = ?plan.diagnostics,
                policy_version = plan.policy_version, "Micro projection planned (estimates)");
            let affected = match micro::apply_micro_plan(transcript, &plan).await {
                Ok(affected) => affected,
                Err(error) => return micro_failure(before_visible_len, error),
            };
            let estimated_tokens_saved = plan.estimated_tokens_saved;

            // 空结果 → 无可 compact 消息
            // 但如果 budget 已超过 Full 阈值，直接尝试 Full Compact
            //
            // 模型缺失由 Full 返回结构化失败，不能静默放行高压请求。
            if affected == 0 {
                if budget_pct >= config.auto_compact_threshold {
                    debug!(
                        "Smart Compact: 无消息可 compact 但 budget 高位({:.1}%)，直接尝试 Full",
                        budget_pct * 100.0
                    );
                    return run_full_or_degrade(
                        transcript,
                        llm,
                        config,
                        pressure,
                        consecutive_failures,
                        cwd,
                        FullEscalationReason::ForceThresholdExceeded,
                        before_full_summary,
                        protected,
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
                    failure: None,
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
                        pressure,
                        consecutive_failures,
                        cwd,
                        FullEscalationReason::ForceThresholdExceeded,
                        before_full_summary,
                        protected,
                    )
                    .await;
                    // Full 成功时，其 excluded transition 已覆盖 Smart affected messages；
                    // 只有 Full 失败时才保留已生效的 Smart affected_count。
                    if !full_result.outcome().is_full_applied() {
                        full_result.affected_count += affected;
                    }
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
                        failure: None,
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
                    pressure,
                    consecutive_failures,
                    cwd,
                    FullEscalationReason::InsufficientReclaim,
                    before_full_summary,
                    protected,
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

/// 运行 Full Compact；保留跨 Full 的失败预算与失败原因。
///
/// 摘要生成本身的恢复（空响应重新生成、截断续写、未闭合补全）已在同一次
/// `full_compact_inner` 内共用一个有界计数器；这里不再重试空摘要，避免内外
/// 层次数相乘。`max_consecutive_failures` 只作为跨 Full 调用的失败预算与兼容
/// 字段保留。
#[allow(clippy::too_many_arguments)] // 与 run_compact 共用同一组决策输入，单独包结构体会偏离调用语义
async fn run_full_or_degrade(
    transcript: &mut MessageTranscript,
    llm: Option<&dyn peri_model::Model>,
    config: &CompactConfig,
    pressure: &ContextPressure,
    consecutive_failures: &mut u32,
    cwd: &str,
    escalation_reason: FullEscalationReason,
    before_full_summary: Option<FullSummaryNotice<'_>>,
    protected: &crate::session::async_result_mailbox::ProtectedResults,
) -> CompactResult {
    let before_visible_len = transcript.visible_messages().len();
    let error = if *consecutive_failures >= config.max_consecutive_failures {
        crate::error::AgentError::CompactRetriesExhausted {
            attempts: *consecutive_failures,
            context_tokens: pressure.estimated_tokens,
            context_window: pressure.context_window,
        }
    } else {
        // 摘要视图即将整份构造（重试预算已耗尽时此分支不进入，也不通知）。
        if let Some(notice) = before_full_summary {
            notice();
        }
        match full::full_compact_inner_with_protected(transcript, llm, config, cwd, protected).await
        {
            Ok(mut result) => {
                *consecutive_failures = 0;
                result.full_escalation_reason = Some(escalation_reason);
                return result;
            }
            Err(error) => {
                *consecutive_failures = consecutive_failures.saturating_add(1);
                // Provider 自己负责传输重试；持久化错误与提交不确定不得在这里重试。
                warn!(%error, attempts = *consecutive_failures, "Full Compact 失败");
                error
            }
        }
    };
    CompactResult {
        strategy: CompactStrategy::Full,
        affected_count: 0,
        estimated_tokens_saved: 0,
        before_visible_len,
        after_visible_len: transcript.visible_messages().len(),
        summary: None,
        full_escalation_reason: Some(escalation_reason),
        outcome: CompactOutcome::FullFailed,
        failure: Some(error),
        changed_messages: 0,
        changed_fields: 0,
        no_op_candidates: 0,
    }
}

/// 驻留字节超限路径：强制一次 Full，记录 Full 后残余，不做硬准入。
///
/// 只有 Full 阈值之外的独立条件会进入这里；token 压力、cache-aware、Micro 计划与
/// `max_consecutive_failures` 的跨调用失败预算仍由 [`run_full_or_degrade`] 承担。
#[allow(clippy::too_many_arguments)]
async fn run_resident_full(
    transcript: &mut MessageTranscript,
    llm: Option<&dyn peri_model::Model>,
    config: &CompactConfig,
    pressure: &ContextPressure,
    consecutive_failures: &mut u32,
    cwd: &str,
    before_full_summary: Option<FullSummaryNotice<'_>>,
    protected: &crate::session::async_result_mailbox::ProtectedResults,
    resident: &ResidentHistoryPressure,
) -> CompactResult {
    warn!(
        resident_limit_bytes = resident.limit_bytes,
        resident_own_reserved_bytes = resident.own_reserved_bytes,
        resident_inherited_reserved_bytes = resident.inherited_reserved_bytes,
        resident_total_reserved_bytes = resident.total_reserved_bytes(),
        estimated_context_tokens = pressure.estimated_tokens,
        context_window = pressure.context_window,
        shadow = config.shadow_mode_enabled,
        "Resident history budget exceeded"
    );

    if config.shadow_mode_enabled {
        let visible_len = transcript.visible_messages().len();
        info!(
            resident_limit_bytes = resident.limit_bytes,
            resident_total_reserved_bytes = resident.total_reserved_bytes(),
            "Shadow mode: resident Full Compact planned but not committed"
        );
        return CompactResult {
            strategy: CompactStrategy::Skip,
            affected_count: 0,
            estimated_tokens_saved: 0,
            before_visible_len: visible_len,
            after_visible_len: visible_len,
            summary: None,
            full_escalation_reason: Some(FullEscalationReason::ResidentHistoryBudgetExceeded),
            outcome: CompactOutcome::Shadowed,
            failure: None,
            changed_messages: 0,
            changed_fields: 0,
            no_op_candidates: 0,
        };
    }

    let result = run_full_or_degrade(
        transcript,
        llm,
        config,
        pressure,
        consecutive_failures,
        cwd,
        FullEscalationReason::ResidentHistoryBudgetExceeded,
        before_full_summary,
        protected,
    )
    .await;

    // Full 成功但残余仍超限只记录事实并返回；不做硬准入，也不在本次调用内重复 Full。
    let residual =
        ResidentHistoryPressure::from_transcript(config.max_resident_history_bytes, transcript);
    if result.outcome().is_full_applied() {
        if residual.is_exceeded() {
            warn!(
                full_outcome = ?result.outcome,
                resident_limit_bytes = residual.limit_bytes,
                resident_residual_own_reserved_bytes = residual.own_reserved_bytes,
                resident_residual_inherited_reserved_bytes = residual.inherited_reserved_bytes,
                resident_residual_total_reserved_bytes = residual.total_reserved_bytes(),
                resident_residual_excess_bytes = residual.excess_bytes(),
                "Resident history still above budget after Full; recording residual"
            );
        } else {
            info!(
                full_outcome = ?result.outcome,
                resident_limit_bytes = residual.limit_bytes,
                resident_residual_own_reserved_bytes = residual.own_reserved_bytes,
                resident_residual_inherited_reserved_bytes = residual.inherited_reserved_bytes,
                resident_residual_total_reserved_bytes = residual.total_reserved_bytes(),
                "Resident history budget restored after Full"
            );
        }
    } else {
        warn!(
            full_outcome = ?result.outcome,
            resident_limit_bytes = residual.limit_bytes,
            resident_residual_total_reserved_bytes = residual.total_reserved_bytes(),
            "Resident Full Compact did not complete"
        );
    }
    result
}

#[cfg(test)]
#[path = "projection_test.rs"]
mod projection_tests;

#[cfg(test)]
#[path = "projection_render_test.rs"]
mod projection_render_tests;

#[cfg(test)]
#[path = "projection_restore_test.rs"]
mod projection_restore_tests;

#[cfg(test)]
#[path = "projection_estimate_test.rs"]
mod projection_estimate_tests;

#[cfg(test)]
#[path = "projection_tool_use_test.rs"]
mod projection_tool_use_tests;

#[cfg(test)]
mod _test;

#[cfg(test)]
#[path = "trigger_test.rs"]
mod trigger_test;

pub(crate) fn micro_failure(visible_len: usize, error: crate::error::AgentError) -> CompactResult {
    tracing::error!(%error, commit_status = "failed", "Micro projection not published");
    CompactResult {
        strategy: CompactStrategy::Micro,
        affected_count: 0,
        estimated_tokens_saved: 0,
        before_visible_len: visible_len,
        after_visible_len: visible_len,
        summary: None,
        full_escalation_reason: None,
        outcome: CompactOutcome::MicroFailed,
        failure: Some(error),
        changed_messages: 0,
        changed_fields: 0,
        no_op_candidates: 0,
    }
}

#[cfg(test)]
#[path = "archive_lifecycle_test.rs"]
mod archive_lifecycle_tests;

#[cfg(test)]
#[path = "resident_budget_test.rs"]
mod resident_budget_tests;
