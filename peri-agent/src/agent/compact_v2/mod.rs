//! Compact v2 — 标记代替删除的上下文压缩
//!
//! 触发与准入（唯一决策点见 [`decision`]）：
//! - budget < micro_compact_threshold：跳过
//! - force=true：直接 Full（不受最小收益门槛限制）
//! - 自动关闭、驻留字节超限、C == 0 的独立路径保持原语义
//! - 进入 Micro 区间：先规划一次并得到最终完整渲染的净收益 G（`estimated_tokens_saved`）
//!   - 未达 Full 条件：G >= M（完整窗口的 10%，向上取整）且候选非空 → Micro，否则延后
//!   - 达到 Full 条件：G >= max(M, R) 且候选非空 → Micro，否则直接 Full
//!   - 先决定最终策略，再执行一次提交；Full 路径不得先提交本轮 Micro
//! - shadow 只报告决策，不提交
//!
//! 与 v1 的区别：v2 基于 `MessageTranscript` 标记 API，不修改消息本体，
//! 旧消息标 `excluded` 后 `visible_messages()` 自动过滤。
//! Full Compact 通过 `peri_model::Model` 标准链路请求摘要。
//! 所有注入消息使用 `BaseMessage::human()` —— 禁止 System，防止 hoist 污染 FrozenContext。

use tracing::{debug, info, warn};

use crate::agent::events::CompactStrategy;
use crate::session::transcript::MessageTranscript;

pub mod config;
pub(crate) mod decision;
pub mod exposure;
pub mod full;
pub mod micro;
pub mod planner;
pub mod projection;
pub mod smart;

// ─── 公共重导出：保持外部调用路径不变 ─────────────────────────────────────────────

pub use config::{CompactConfig, CONTINUATION_HINT};
pub use decision::min_micro_gain_tokens;
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

use decision::GainDecision;

// ─── CompactResult ───────────────────────────────────────────────────────────────

/// Compact 执行结果
#[derive(Debug)]
pub struct CompactResult {
    /// 使用的策略
    pub strategy: CompactStrategy,
    /// 操作的消息数量（标 truncated / excluded 的数量）
    pub affected_count: usize,
    /// 估算节省的 token 数量（延后时是未提交的候选净收益 G）
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
    /// 自动 Micro 的延后事实（仅 Deferred 携带）；其他结果为 None。
    pub deferral: Option<peri_acp_types::compact::CompactDeferral>,
}

/// Compact 执行语义结果（事实源 peri-acp-types::compact）
pub use peri_acp_types::compact::CompactOutcome;

impl CompactResult {
    /// 返回本轮 Compact 的语义结果。
    pub fn outcome(&self) -> CompactOutcome {
        self.outcome
    }
}

/// 未进入自动压缩区间（或自动关闭、shadow 降级）时的无变更结果。
fn skipped_result(before_visible_len: usize) -> CompactResult {
    CompactResult {
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
        deferral: None,
    }
}

// ─── 顶层入口 ───────────────────────────────────────────────────────────────────

/// Full 摘要视图构造前的通知（[`run_compact`] 的最后一个参数）。
///
/// Full 在 [`full::full_compact_inner`] 内为摘要请求另建一份整份历史模型视图。持有等价
/// 视图副本的调用方（模型视图复用槽）必须在这份视图构造**之前**放弃自己那份，否则两份
/// 整身正文会在整个摘要请求期间同时存活。决策发生在 `compact_v2` 内部、调用方拿不到
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
    /// 执行 Smart Compact（已废弃；与 Micro 共用收益准入）
    Smart,
}

/// 根据 budget 和配置决定是否进入自动压缩区间。
///
/// 返回 `Skip` 表示预算未到 `micro_compact_threshold`。
/// 返回 `Smart` 表示启用 Smart Compact 且预算已过阈值（仍走同一收益准入）。
/// 返回 `Micro` 表示未启用 Smart Compact 且预算已过阈值。
///
/// 最终提交哪种策略由 [`decision::decide_auto_compact`] 决定；本函数不提交。
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
/// 触发流程：
/// - 自动关闭：跳过自动压缩（手动 force 独立）
/// - force=true：直接 Full
/// - 驻留字节超限：独立 Full 路径
/// - 计算 budget_pct：未达 Micro 阈值直接 Skip
/// - 计划只规划一次；`estimated_tokens_saved` 是最终完整渲染后的净收益 G
/// - 由 [`decision::decide_auto_compact`] 给出 Skip / Deferred / Micro / Full
/// - 先决定策略，再执行一次提交；Full 路径不再先应用本轮 Micro
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
        return skipped_result(before_visible_len);
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

    let compact_action = determine_compact_action(budget_pct, config);
    if compact_action == CompactAction::Skip {
        return skipped_result(before_visible_len);
    }

    // Smart 已废弃：保留 strategy 兼容值，但复用同一收益准入，禁止先写后判。
    let smart = compact_action == CompactAction::Smart;
    if smart {
        warn!("Smart Compact is deprecated; using the unified Micro admission decision");
    }
    let strategy = if smart {
        CompactStrategy::Smart
    } else {
        CompactStrategy::Micro
    };

    // Cache-aware：只延迟尚未达到 Full 条件的请求；已经成立的 Full 条件不能被屏蔽。
    if config.cache_aware_enabled
        && pressure.cache_hit_rate > 0.7
        && budget_pct < config.auto_compact_threshold
    {
        let headroom_pct = 1.0 - budget_pct;
        if headroom_pct > 0.2 {
            debug!(
                cache_hit_rate = %pressure.cache_hit_rate,
                headroom_pct = %headroom_pct,
                "Cache-aware: 高缓存命中且充足 headroom，跳过 compact"
            );
            return skipped_result(before_visible_len);
        }
    }

    let planning_started = std::time::Instant::now();
    let plan = match planner::plan_micro_with_facts(transcript, config, pressure, facts, caps) {
        Ok(plan) => plan,
        Err(error) => return micro_failure(before_visible_len, error),
    };
    let planning_elapsed = planning_started.elapsed();
    let min_gain = decision::min_micro_gain_tokens(pressure.context_window, config);
    let estimated_gain = plan.estimated_tokens_saved;
    let has_candidates = plan.has_changes();
    tracing::info!(
        trigger_pressure_tokens = pressure.estimated_tokens,
        context_window = pressure.context_window,
        trigger_ratio = budget_pct,
        min_micro_gain_tokens = min_gain,
        target_reclaim_tokens = plan.target_reclaim_tokens,
        view_before_estimate = plan.estimated_before_tokens,
        view_after_estimate = plan.estimated_after_tokens,
        estimated_gain_tokens = estimated_gain,
        candidate_actions = plan.actions.len(),
        diagnostics = ?plan.diagnostics,
        policy_version = plan.policy_version,
        planning_elapsed_ms = planning_elapsed.as_millis() as u64,
        "Micro projection planned (estimates)"
    );

    let gain_decision =
        decision::decide_auto_compact(pressure, config, estimated_gain, has_candidates);

    // Shadow mode：只报告拟采取的决策，不提交任何写入。
    if config.shadow_mode_enabled {
        info!(
            estimated_saved = estimated_gain,
            min_gain_tokens = min_gain,
            decision = ?gain_decision,
            shadow = true,
            "Shadow mode: 只报告自动压缩决策（未应用）"
        );
        return CompactResult {
            strategy: CompactStrategy::Skip,
            affected_count: 0,
            estimated_tokens_saved: estimated_gain,
            before_visible_len,
            after_visible_len: before_visible_len,
            summary: None,
            full_escalation_reason: None,
            outcome: CompactOutcome::Shadowed,
            failure: None,
            changed_messages: 0,
            changed_fields: 0,
            no_op_candidates: plan.no_op_candidates,
            deferral: None,
        };
    }

    match gain_decision {
        GainDecision::Skip => skipped_result(before_visible_len),
        GainDecision::Deferred(deferral) => {
            // 延后不是失败：不写 transcript、不做归档正文验证、不改变失败计数。
            info!(
                decision = ?deferral.reason,
                estimated_gain_tokens = deferral.estimated_gain_tokens,
                min_gain_tokens = deferral.min_gain_tokens,
                target_reclaim_tokens = plan.target_reclaim_tokens,
                candidate_actions = plan.actions.len(),
                planning_elapsed_ms = planning_elapsed.as_millis() as u64,
                "Micro projection deferred: gain below admission requirement"
            );
            CompactResult {
                strategy,
                affected_count: 0,
                estimated_tokens_saved: estimated_gain,
                before_visible_len,
                after_visible_len: before_visible_len,
                summary: None,
                full_escalation_reason: None,
                outcome: CompactOutcome::Deferred,
                failure: None,
                changed_messages: 0,
                changed_fields: 0,
                no_op_candidates: plan.no_op_candidates,
                deferral: Some(deferral),
            }
        }
        GainDecision::Micro => {
            let apply_started = std::time::Instant::now();
            // 复用同一份 dry-run 计划提交，不二次规划。
            let affected = match micro::apply_micro_plan(transcript, &plan).await {
                Ok(affected) => affected,
                Err(error) => return micro_failure(before_visible_len, error),
            };
            *consecutive_failures = 0;
            let commit_elapsed = apply_started.elapsed();
            debug!(
                saved = estimated_gain,
                min_gain_tokens = min_gain,
                target = plan.target_reclaim_tokens,
                affected,
                planning_elapsed_ms = planning_elapsed.as_millis() as u64,
                commit_elapsed_ms = commit_elapsed.as_millis() as u64,
                "Micro admission satisfied; projection committed"
            );
            CompactResult {
                strategy,
                affected_count: affected,
                estimated_tokens_saved: estimated_gain,
                before_visible_len,
                after_visible_len: transcript.visible_messages().len(),
                summary: None,
                full_escalation_reason: None,
                outcome: if smart {
                    CompactOutcome::SmartApplied
                } else {
                    CompactOutcome::MicroApplied
                },
                failure: None,
                changed_messages: plan.changed_messages,
                changed_fields: plan.changed_fields,
                no_op_candidates: plan.no_op_candidates,
                deferral: None,
            }
        }
        GainDecision::Full(reason) => {
            // 达到 Full 条件：本轮 Micro 不先提交，避免摘要输入再承受一轮有损变换。
            debug!(
                saved = estimated_gain,
                min_gain_tokens = min_gain,
                target = plan.target_reclaim_tokens,
                candidate_actions = plan.actions.len(),
                reason = ?reason,
                planning_elapsed_ms = planning_elapsed.as_millis() as u64,
                "Full condition met; skipping this round's Micro commit"
            );
            run_full_or_degrade(
                transcript,
                llm,
                config,
                pressure,
                consecutive_failures,
                cwd,
                reason,
                before_full_summary,
                protected,
            )
            .await
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
        deferral: None,
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
        resident_reclaimable_own_bytes = resident.reclaimable_own_reserved_bytes(),
        resident_irreducible_own_bytes = resident.irreducible_own_reserved_bytes(),
        resident_irreducible_inherited_bytes = resident.irreducible_inherited_reserved_bytes(),
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
            deferral: None,
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
    let protected_ids: std::collections::HashSet<_> = protected
        .items
        .iter()
        .map(|item| item.delivery_id)
        .collect();
    let residual = ResidentHistoryPressure::from_transcript_with_protected(
        config.max_resident_history_bytes,
        transcript,
        &protected_ids,
    );
    if result.outcome().is_full_applied() {
        if residual.is_exceeded() {
            warn!(
                full_outcome = ?result.outcome,
                resident_limit_bytes = residual.limit_bytes,
                resident_reclaimable_own_bytes = residual.reclaimable_own_reserved_bytes(),
                resident_irreducible_own_bytes = residual.irreducible_own_reserved_bytes(),
                resident_irreducible_inherited_bytes =
                    residual.irreducible_inherited_reserved_bytes(),
                resident_residual_total_reserved_bytes = residual.total_reserved_bytes(),
                resident_residual_excess_bytes = residual.excess_bytes(),
                "Resident history still above budget after Full; recording residual"
            );
        } else {
            info!(
                full_outcome = ?result.outcome,
                resident_limit_bytes = residual.limit_bytes,
                resident_reclaimable_own_bytes = residual.reclaimable_own_reserved_bytes(),
                resident_irreducible_own_bytes = residual.irreducible_own_reserved_bytes(),
                resident_irreducible_inherited_bytes =
                    residual.irreducible_inherited_reserved_bytes(),
                resident_residual_total_reserved_bytes = residual.total_reserved_bytes(),
                "Resident history budget restored after Full"
            );
        }
    } else {
        warn!(
            full_outcome = ?result.outcome,
            resident_limit_bytes = residual.limit_bytes,
            resident_reclaimable_own_bytes = residual.reclaimable_own_reserved_bytes(),
            resident_irreducible_own_bytes = residual.irreducible_own_reserved_bytes(),
            resident_irreducible_inherited_bytes = residual.irreducible_inherited_reserved_bytes(),
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
        deferral: None,
    }
}

#[cfg(test)]
#[path = "archive_lifecycle_test.rs"]
mod archive_lifecycle_tests;

#[cfg(test)]
#[path = "resident_budget_test.rs"]
mod resident_budget_tests;

#[cfg(test)]
#[path = "gain_gate_test.rs"]
mod gain_gate_tests;

#[cfg(test)]
#[path = "projection_position_test.rs"]
mod projection_position_tests;
