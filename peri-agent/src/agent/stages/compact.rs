//! Compact 阶段 — 上下文压缩
//!
//! 根据 ContextBudget 计算使用率，调用 `compact_v2::run_compact`：
//! - budget < 0.75：跳过
//! - budget ≥ 0.75：先规划一次，再由统一收益准入决定 Micro / Deferred / Full
//!   - 未达 Full 条件：净收益 G ≥ 完整窗口的 10% 才提交 Micro，否则延后
//!   - 达到 Full 条件：G ≥ max(10%, R) 才提交 Micro，否则直接 Full
//!   - 延后（Deferred）不写入、不做归档验证、不增加失败计数，但必须闭合已发出的事件
//! - force=true：直接 Full（跳过 Micro）
//!
//! 空摘要在本阶段内有界重试；高压下 Full 失败必须终止，不能静默跳过。

use super::{CompactInput, CompactOutput};
use crate::agent::compact_v2::config::CompactConfig;
use crate::agent::compact_v2::planner::{
    ContextPressure, FullEscalationReason, ResidentHistoryPressure,
};

/// 运行 Compact 阶段
pub async fn run_compact(input: CompactInput) -> crate::error::AgentResult<CompactOutput> {
    let ctx = &input.context;

    // PreCompact 插件 hook 回调（fire-and-forget）
    if let Some(ref hook) = ctx.compact.compact_pre_hook {
        hook();
    }

    // before_compact hook：中间件可在此监听/干预 compact 生命周期
    if let Err(e) = super::middleware_runner::run_before_compact(ctx).await {
        tracing::warn!(error = %e, "before_compact hook 失败，继续 compact");
    }

    let (output, affected_count) = compact_core(CompactInput {
        context: ctx.clone(),
        has_tool_calls: input.has_tool_calls,
    })
    .await;

    // after_compact hook：无论 compact 是否实际执行，均通知中间件
    if let Err(e) = super::middleware_runner::run_after_compact(ctx).await {
        tracing::warn!(error = %e, "after_compact hook 失败");
    }

    // PostCompact 插件 hook 回调（所有返回路径统一触发）
    if let Some(ref hook) = ctx.compact.compact_post_hook {
        let compacted = affected_count > 0 || output.as_ref().map(|o| o.compacted).unwrap_or(false);
        hook(compacted, affected_count);
    }

    output
}

#[path = "context_pressure.rs"]
pub(super) mod context_pressure;

/// Reason 的末次预算检查复用同一核心，不重复运行可产生新输入的 hooks。
pub(crate) async fn run_compact_core(
    input: CompactInput,
) -> crate::error::AgentResult<CompactOutput> {
    compact_core(input).await.0
}

async fn compact_core(input: CompactInput) -> (crate::error::AgentResult<CompactOutput>, usize) {
    let ctx = &input.context;
    let step = ctx.session.turn.current_step();
    tracing::trace!(step, has_tool_calls = input.has_tool_calls, "Compact 阶段");

    // PostCompact hook 需要 affected_count；在所有 break 路径前声明
    let mut affected_count: usize = 0;

    // 用 labeled block + break 收敛所有返回路径，确保 after_compact 在函数末尾统一触发。
    let output = 'compact_core: {
        // compact_config 是自动 Compact 的开关、阈值与驻留上限来源；缺失时无法判断
        // 任何策略，明确跳过并记录，不伪造默认配置。
        let Some(config) = ctx.compact.compact_config.as_ref() else {
            tracing::trace!(step, "Compact 跳过：缺少 compact_config");
            break 'compact_core Ok(CompactOutput { compacted: false });
        };
        // context_budget 缺失时 token 压力视为 0，但驻留字节门控与 token 独立，
        // 仍需在下方采集；驻留超限时继续执行 resident Full。
        let budget = ctx.compact.context_budget.as_ref();

        // The host resolves config and environment overrides before running the loop.
        if !config.auto_compact_enabled {
            tracing::trace!(step, "Compact 已禁用（config.auto_compact_enabled=false）");
            break 'compact_core Ok(CompactOutput { compacted: false });
        }

        // 驻留字节预算与 token 压力独立：在 token refresh 之前读取，超限时无需先物化
        // 模型视图。门控区分「Full 可回收的 own」与「Full 无法回收的残余」：上次 Full
        // 后仍超限时，只有可回收 own 相对上次 Full 后的快照增长超过完整预算才会再次
        // Full，避免摘要自身、继承区或 System/protected own 主导时每轮新工作都重复
        // 支付摘要成本。Full 后仍超限只记录残余（compact_v2 内），不在本阶段重试或
        // 终止运行。
        //
        // 锁顺序：先拷贝 recovery 状态并释放锁，再读 transcript；判定为纯函数，
        // 不会在 transcript guard 存活时锁 `budget_recovery`。Reason 复用同一入口。
        let protected = ctx.session.async_result_mailbox.protected_snapshot();
        let protected_ids: std::collections::HashSet<_> = protected
            .items
            .iter()
            .map(|item| item.delivery_id)
            .collect();
        let resident_gate = resident_gate(ctx, config.max_resident_history_bytes, &protected_ids);
        let resident_exceeded = resident_gate.is_exceeded();

        if budget.is_some() && !resident_exceeded {
            if let Err(error) = context_pressure::refresh(ctx) {
                break 'compact_core Err(error);
            }
        }

        // 构建 ContextPressure（从 token_tracker 和 context_budget 中提取字段）
        // P1-3: 直接读 StageContext.token_tracker，无需经过 AgentContext 适配层
        let pressure = match budget {
            Some(budget) => {
                let tracker = ctx.compact.token_tracker.read();
                ContextPressure {
                    estimated_tokens: tracker.estimated_context_tokens().unwrap_or(0),
                    context_window: budget.context_window,
                    output_reserve: budget.output_reserve,
                    // 已提交工具增长已计入 estimated_context_tokens；不能再从目标
                    // 预算扣除同一增长。未来工具预测需要独立的输入事实源。
                    predicted_tool_growth: 0,
                    safety_buffer: 5000,
                    cache_hit_rate: tracker.cache_hit_rate(),
                }
            }
            // 无预算契约时没有 token 压力事实；驻留字节门控仍独立生效。
            None => ContextPressure {
                estimated_tokens: 0,
                context_window: 0,
                output_reserve: 0,
                predicted_tool_growth: 0,
                safety_buffer: 5000,
                cache_hit_rate: 0.0,
            },
        };

        // 从 pressure 计算 budget 百分比（供 determine_compact_action 使用）
        let pct = if pressure.context_window > 0 {
            pressure.estimated_tokens as f64 / pressure.context_window as f64
        } else {
            0.0
        };

        // 在 emit CompactStarted 前估算策略
        // determine_compact_action 判定 Skip/Micro/Smart；Full 由 run_compact 内部动态决策。
        // Skip 时不 emit CompactStarted——避免成对事件不对称（Start emit 了但 End 因
        // affected_count==0 不触发，导致 Langfuse compact span 始终缺失）。
        let compact_action = crate::agent::compact_v2::determine_compact_action(pct, config);
        if !resident_exceeded
            && matches!(
                compact_action,
                crate::agent::compact_v2::CompactAction::Skip
            )
        {
            break 'compact_core Ok(CompactOutput { compacted: false });
        }

        let pressure_sample = {
            let tracker = ctx.compact.token_tracker.read();
            tracker.pressure_sample_key()
        };
        if !resident_exceeded
            && pressure_sample.is_some_and(|key| {
                ctx.compact
                    .token_tracker
                    .read()
                    .is_pressure_sample_consumed(key)
            })
        {
            tracing::trace!(step, "Compact 压力样本已消费，等待新 usage 或工具增长");
            break 'compact_core Ok(CompactOutput { compacted: false });
        }
        let compact_strategy = if resident_exceeded {
            crate::agent::events::CompactStrategy::Full
        } else {
            match compact_action {
                crate::agent::compact_v2::CompactAction::Smart => {
                    crate::agent::events::CompactStrategy::Smart
                }
                crate::agent::compact_v2::CompactAction::Micro => {
                    crate::agent::events::CompactStrategy::Micro
                }
                crate::agent::compact_v2::CompactAction::Skip => {
                    unreachable!("Skip 已在上方 break，不会进入此分支")
                }
            }
        };

        tracing::trace!(step, budget_pct = %pct, action = ?compact_action, "Compact 预算检查");

        // 调用 compact_v2：取出 transcript 所有权，运行后放回（避免跨 await 持锁）
        let compact_llm_ref: Option<&dyn peri_model::Model> = ctx
            .compact
            .compact_llm
            .as_ref()
            .map(|arc| arc.as_ref() as &dyn peri_model::Model);

        let mut transcript_owned = {
            let mut guard = ctx.session.transcript.write();
            std::mem::take(&mut *guard)
        };

        let mut consecutive = ctx
            .compact
            .compact_consecutive_failures
            .load(std::sync::atomic::Ordering::Relaxed);

        // emit CompactStarted 观测事件（Start→End 成对原则，修复 Langfuse compact_span 断裂）
        ctx.runtime
            .event_bus
            .emit_observe(crate::agent::events_v2::ObserveEvent::CompactStarted {
                turn_id: ctx.turn_id(),
                agent_id: ctx.session.agent_id,
                step,
                strategy: compact_strategy,
            });

        let config_clone: CompactConfig = config.clone();

        // Compare actual flags: a second downgrade can change content without increasing
        // the number of truncated messages.
        let pre_compact_flags: std::collections::HashMap<_, _> = transcript_owned
            .entries()
            .iter()
            .map(|entry| (entry.id(), transcript_owned.flags(entry.id())))
            .collect();

        // G6: 记录 compact 前的可见消息数，供 cancel arm 发射 MessagesCompacted 事件使用
        let before_visible_len = transcript_owned.visible_messages().len();
        // 「同一工作单元」判定必须在 Full 改动 transcript 之前采集：Full 追加的摘要不是
        // 新工作，收缩释放条目后位置也不可比（水印用末尾摘要的稳定身份）。
        let new_work_before_full = ctx
            .compact
            .budget_recovery
            .lock()
            .new_work_since_full(&transcript_owned);

        if let Some(key) = pressure_sample {
            ctx.compact
                .token_tracker
                .write()
                .consume_pressure_sample(key);
        }

        // Full 会在 compact_v2 内另建一份整份历史视图作为摘要请求。只有确实要构造摘要
        // 视图时才释放本槽（否则整个摘要请求期间多驻留一份历史正文）；Micro/Smart 未
        // 落地、shadow、空计划未达 Full 阈值等路径没有第二份正文，缓存继续供 Reason 复用。
        let before_full_summary = || ctx.model_view.release();

        // 未结清的后台结果不参与归档：Full 只摘要可归档历史，未覆盖通知继续由
        // mailbox 负责并在随后请求中重新投影。
        let capabilities = ctx.runtime.llm.provider_capabilities();

        // 包在 select! biased 中：cancel 优先，避免 Full Compact 的长 LLM 调用阻塞中断。
        // 注：run_compact 内部不感知 turn cancel_token，必须在此层显式 select。
        let mut result = tokio::select! {
            biased;
            _ = ctx.session.turn.cancel_token.cancelled() => {
                // G6: 检查 compact 是否在 cancel 前已提交变更（通过 flag 计数对比）。
                // transcript_owned 已通过 &mut 被 run_compact 修改，变更已持久化在内存中。
                let committed_changes = transcript_owned.entries().iter().filter(|entry| {
                    pre_compact_flags.get(&entry.id()).is_none_or(|before| before != &transcript_owned.flags(entry.id()))
                }).count();

                // 把 transcript 放回 RwLock（与正常路径一致，避免遗失消息）
                *ctx.session.transcript.write() = transcript_owned;
                ctx.compact
                    .compact_consecutive_failures
                    .store(consecutive, std::sync::atomic::Ordering::Relaxed);

                if ctx.session.transcript.read().compaction_commit_state().is_uncertain() {
                    break 'compact_core Err(uncertain_compaction_error(ctx));
                }

                if committed_changes > 0 {
                    // G6: 发射配对事件，防止 Langfuse span 孤立
                    let visible: Vec<crate::messages::BaseMessage> = ctx.session.transcript.read()
                        .visible_messages().into_iter().cloned().collect();
                    ctx.runtime.event_bus.emit_observe(
                        crate::agent::events_v2::ObserveEvent::MessagesCompacted {
                            turn_id: ctx.turn_id(),
                            agent_id: ctx.session.agent_id,
                            before_count: before_visible_len,
                            after_count: visible.len(),
                            summary: String::new(),
                            messages: visible,
                            files: vec![],
                            skills: vec![],
                            re_inject_count: 0,
                            strategy: compact_strategy,
                            affected_count: committed_changes,
                            estimated_tokens_saved: 0,
                            estimated_tokens_before: pressure.estimated_tokens,
                            estimated_tokens_after: pressure.estimated_tokens,
                            changed_messages: committed_changes,
                            changed_fields: committed_changes,
                            no_op_candidates: 0,
                            full_escalation_reason: None,
                            cache_hit_rate_before: pressure.cache_hit_rate,
                            outcome: crate::agent::compact_v2::CompactOutcome::InterruptedAfterCommit,
                        },
                    );
                    affected_count = committed_changes;
                    break 'compact_core Ok(CompactOutput { compacted: true });
                }
                // S1.4：cancel 且未提交变更 → 补发 CompactEnded 配对事件
                // （与 CompactStarted 成对闭合 span）。不 emit MessagesCompacted——
                // 那会误导遥测以为压缩发生了。outcome 用 Interrupted 表明
                // "取消且无变更"（与 InterruptedAfterCommit 互补）。
                ctx.runtime.event_bus.emit_observe(
                    crate::agent::events_v2::ObserveEvent::CompactEnded {
                        turn_id: ctx.turn_id(),
                        agent_id: ctx.session.agent_id,
                        step,
                        strategy: compact_strategy,
                        outcome: crate::agent::compact_v2::CompactOutcome::Interrupted,
                        deferral: None,
                    },
                );
                break 'compact_core Err(crate::error::AgentError::Interrupted);
            }
            r = async {
                // 驻留超限路径不构建 Micro facts：该探测可能访问远程 Store，失败不应
                // 阻断与 token 压力无关的驻留 Full；此路径也不会消费 facts。
                let facts = if resident_exceeded {
                    crate::agent::compact_v2::planner::MicroPlanningFacts::default()
                } else {
                    // Recovery probing may use a remote Store. It shares the commit path's
                    // cancellation boundary and failure events, while ownership remains here.
                    match super::micro_facts::snapshot(ctx, &transcript_owned, &protected).await {
                        Ok(facts) => facts,
                        Err(error) => {
                            return crate::agent::compact_v2::micro_failure(
                                before_visible_len,
                                error,
                            )
                        }
                    }
                };
                crate::agent::compact_v2::run_compact_with_facts_and_resident(
                &mut transcript_owned,
                compact_llm_ref,
                &config_clone,
                &pressure,
                false, // force=false（自动触发）
                &mut consecutive,
                ctx.cwd(),
                Some(&before_full_summary),
                &protected,
                &facts,
                &capabilities,
                &resident_gate,
                ).await
            } => r,
        };

        if transcript_owned.compaction_commit_state().is_uncertain() {
            *ctx.session.transcript.write() = transcript_owned;
            break 'compact_core Err(uncertain_compaction_error(ctx));
        }

        // G6: 检查是否在 compact 执行期间被取消（r arm 胜出但 cancel 已被触发）
        if ctx.session.turn.cancel_token.is_cancelled() {
            let did_apply = result.outcome().has_applied_change();
            // 把 transcript 放回 RwLock（transcript 已在 run_compact 中修改）
            *ctx.session.transcript.write() = transcript_owned;
            ctx.compact
                .compact_consecutive_failures
                .store(consecutive, std::sync::atomic::Ordering::Relaxed);

            if did_apply {
                // 已提交的 Full 即使随后被取消，也必须记录驻留快照；否则下一次门控
                // 仍按旧快照判断，与「每次实际应用 Full 都记录」的语义不一致。
                if result.outcome().is_full_applied() {
                    record_applied_full(
                        ctx,
                        new_work_before_full,
                        config.max_resident_history_bytes,
                        &protected_ids,
                    );
                }
                // G6: 发射配对事件，防止 Langfuse span 孤立
                let visible: Vec<crate::messages::BaseMessage> = ctx
                    .session
                    .transcript
                    .read()
                    .visible_messages()
                    .into_iter()
                    .cloned()
                    .collect();
                ctx.runtime.event_bus.emit_observe(
                    crate::agent::events_v2::ObserveEvent::MessagesCompacted {
                        turn_id: ctx.turn_id(),
                        agent_id: ctx.session.agent_id,
                        before_count: result.before_visible_len,
                        after_count: result.after_visible_len,
                        summary: result.summary.clone().unwrap_or_default(),
                        messages: visible,
                        files: vec![],
                        skills: vec![],
                        re_inject_count: 0,
                        strategy: result.strategy,
                        affected_count: result.affected_count,
                        estimated_tokens_saved: result.estimated_tokens_saved,
                        estimated_tokens_before: pressure.estimated_tokens,
                        estimated_tokens_after: pressure
                            .estimated_tokens
                            .saturating_sub(result.estimated_tokens_saved),
                        changed_messages: result.changed_messages,
                        changed_fields: result.changed_fields,
                        no_op_candidates: result.no_op_candidates,
                        full_escalation_reason: result.full_escalation_reason,
                        cache_hit_rate_before: pressure.cache_hit_rate,
                        outcome: crate::agent::compact_v2::CompactOutcome::InterruptedAfterCommit,
                    },
                );
                affected_count = result.affected_count;
                break 'compact_core Ok(CompactOutput { compacted: true });
            } else {
                // 未提交变更 → 正常回滚。
                // S1.4：补发 CompactEnded 配对事件（理由同 cancel arm：
                // CompactStarted 已 emit，不能留悬挂 span；不 emit
                // MessagesCompacted 以免误导遥测）。
                ctx.runtime.event_bus.emit_observe(
                    crate::agent::events_v2::ObserveEvent::CompactEnded {
                        turn_id: ctx.turn_id(),
                        agent_id: ctx.session.agent_id,
                        step,
                        strategy: result.strategy,
                        outcome: crate::agent::compact_v2::CompactOutcome::Interrupted,
                        deferral: None,
                    },
                );
                break 'compact_core Err(crate::error::AgentError::Interrupted);
            }
        }

        // 写回 compact_consecutive_failures
        ctx.compact
            .compact_consecutive_failures
            .store(consecutive, std::sync::atomic::Ordering::Relaxed);

        // 把 transcript 放回 RwLock
        *ctx.session.transcript.write() = transcript_owned;

        let micro_failed = result.outcome == crate::agent::compact_v2::CompactOutcome::MicroFailed;
        let failure = result.failure.take();
        // 驻留触发的 Full 与 token 压力无关：低 token 下也必须传播其失败。
        let resident_triggered = resident_exceeded
            || result.full_escalation_reason
                == Some(FullEscalationReason::ResidentHistoryBudgetExceeded);
        let compacted = {
            let r = result; // compact_v2::run_compact 直接返回 CompactResult
            affected_count = r.affected_count;
            let did_compact = r.outcome().has_applied_change();

            if did_compact {
                tracing::info!(
                    step,
                    strategy = ?r.strategy,
                    affected = r.affected_count,
                    before = r.before_visible_len,
                    after_visible = r.after_visible_len,
                    "Compact 完成"
                );
            }
            // 取出 visible_messages 快照供 TUI 重建（必须在 transcript 还在 owned 时读）
            // 注：run_compact 内部已把 transcript_owned 还给 ctx.session.transcript.write()，
            // 此处从 ctx 重新读
            let (messages_snapshot, files, skills) = if did_compact {
                let guard = ctx.session.transcript.read();
                let visible: Vec<crate::messages::BaseMessage> =
                    guard.visible_messages().into_iter().cloned().collect();
                // Full 只追加摘要；已有消息中的元信息仍由类型层提取。
                let combined_files = crate::agent::compact_v2::extract_file_info(&visible);
                let combined_skills = crate::agent::compact_v2::extract_skill_names(&visible);
                (visible, combined_files, combined_skills)
            } else {
                (Vec::new(), Vec::new(), Vec::new())
            };

            // Full Compact 后必须 reset token_tracker——否则下轮 budget 计算会基于
            // compact 前的累积 token 数，导致每轮都触发 compact
            // 注：与 v1 CompactMiddleware 行为对齐（v1 已删除）
            if did_compact && r.outcome().is_full_applied() {
                record_applied_full(
                    ctx,
                    new_work_before_full,
                    config.max_resident_history_bytes,
                    &protected_ids,
                );
                // P1-3: 直接操作 StageContext.token_tracker
                ctx.compact.token_tracker.write().reset();
                // 注：token_tracker reset 为只读 token 操作，无需 drain recall
            }

            // 每次 Started 都须闭合；失败/无变更不能伪装成 MessagesCompacted。
            if did_compact {
                ctx.runtime.event_bus.emit_observe(
                    crate::agent::events_v2::ObserveEvent::MessagesCompacted {
                        turn_id: ctx.turn_id(),
                        agent_id: ctx.session.agent_id,
                        before_count: r.before_visible_len,
                        after_count: r.after_visible_len,
                        summary: r.summary.clone().unwrap_or_default(),
                        messages: messages_snapshot,
                        files,
                        skills,
                        re_inject_count: 0,
                        strategy: r.strategy,
                        affected_count: r.affected_count,
                        estimated_tokens_saved: r.estimated_tokens_saved,
                        estimated_tokens_before: pressure.estimated_tokens,
                        estimated_tokens_after: pressure
                            .estimated_tokens
                            .saturating_sub(r.estimated_tokens_saved),
                        changed_messages: r.changed_messages,
                        changed_fields: r.changed_fields,
                        no_op_candidates: r.no_op_candidates,
                        full_escalation_reason: r.full_escalation_reason,
                        cache_hit_rate_before: pressure.cache_hit_rate,
                        outcome: r.outcome,
                    },
                );
            } else {
                ctx.runtime.event_bus.emit_observe(
                    crate::agent::events_v2::ObserveEvent::CompactEnded {
                        turn_id: ctx.turn_id(),
                        agent_id: ctx.session.agent_id,
                        step,
                        strategy: r.strategy,
                        outcome: r.outcome,
                        deferral: r.deferral,
                    },
                );
            }
            did_compact
        };

        // 已确认提交和取消已在上方处理；高压 Full 的任何失败都必须阻止 Reason。
        // Provider 失败直接传播，其自身重试预算不能在 Compact 层重新开始。
        if let Some(error) = failure {
            if matches!(error, crate::error::AgentError::Interrupted) {
                break 'compact_core Err(error);
            }
            if micro_failed || resident_triggered || pct >= config.auto_compact_threshold {
                break 'compact_core Err(error);
            }
        }
        break 'compact_core Ok(CompactOutput { compacted });
    };

    (output, affected_count)
}

/// 驻留门控的单一入口：Compact 与 Reason 共用。
///
/// 锁序约定：先在 `budget_recovery` 锁内拷贝状态并立即释放，再读 transcript；
/// 判定为纯函数。实现中不得在 transcript guard 存活时调用 `budget_recovery.lock()`，
/// 也不得在持有 `budget_recovery` guard 时调用 `transcript.read()/write()`。
pub(super) fn resident_gate(
    ctx: &super::StageContext,
    resident_limit_bytes: u64,
    protected_ids: &std::collections::HashSet<crate::messages::MessageId>,
) -> ResidentHistoryPressure {
    let state = ctx.compact.budget_recovery.lock().resident_gate_state();
    let transcript = ctx.session.transcript.read();
    let current = ResidentHistoryPressure::from_transcript_with_protected(
        resident_limit_bytes,
        &transcript,
        protected_ids,
    );
    state.decide(current, &transcript)
}

/// 记录已应用 Full 的驻留快照。
///
/// 先在 transcript guard 内采集压力与结构事实并释放 guard，再锁 `budget_recovery`
/// 写入；不在任一 guard 存活时获取另一把锁。
fn record_applied_full(
    ctx: &super::StageContext,
    new_work_before_full: bool,
    resident_limit_bytes: u64,
    protected_ids: &std::collections::HashSet<crate::messages::MessageId>,
) {
    let snapshot = {
        let transcript = ctx.session.transcript.read();
        let pressure = ResidentHistoryPressure::from_transcript_with_protected(
            resident_limit_bytes,
            &transcript,
            protected_ids,
        );
        super::compact_progress::ResidentFullSnapshot::capture(&transcript, pressure)
    };
    ctx.compact
        .budget_recovery
        .lock()
        .record_full_applied(new_work_before_full, snapshot);
}

// An unacknowledged durable commit must stop the loop before Reason can consume the
// old in-memory history. Phase 8 reports this state for explicit history reload.
fn uncertain_compaction_error(ctx: &super::StageContext) -> crate::error::AgentError {
    ctx.runtime
        .event_bus
        .emit_observe(crate::agent::events_v2::ObserveEvent::CompactEnded {
            turn_id: ctx.turn_id(),
            agent_id: ctx.session.agent_id,
            step: ctx.session.turn.current_step(),
            strategy: crate::agent::events::CompactStrategy::Full,
            outcome: crate::agent::compact_v2::CompactOutcome::FullFailed,
            deferral: None,
        });
    anyhow::anyhow!("compact persistence outcome is unknown; reload the session").into()
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "compact_test.rs"]
mod tests;

#[cfg(test)]
#[path = "compact_retry_test.rs"]
mod compact_retry_tests;
