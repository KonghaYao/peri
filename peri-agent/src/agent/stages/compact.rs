//! Compact 阶段 — 上下文压缩
//!
//! 根据 ContextBudget 计算使用率，调用 `compact_v2::run_compact`：
//! - budget < 0.75：跳过
//! - budget ≥ 0.75：Smart Compact（规则驱动保留关键消息）或 Micro Compact（按 round 截断）
//!   - Micro/Smart 始终先执行应用（标记 truncated）
//!   - budget ≥ 0.95 时叠加 Full Compact（不跳过 Micro/Smart）
//!   - 决策指标：estimated_tokens_saved >= reclaim_target
//! - force=true：直接 Full（跳过 Micro/Smart）
//!
//! Full Compact 失败时 `compact_consecutive_failures` 累加，达上限后降级跳过。

use super::{CompactInput, CompactOutput};
use crate::agent::compact_v2::config::CompactConfig;
use crate::agent::compact_v2::planner::ContextPressure;

/// 运行 Compact 阶段
pub async fn run_compact(input: CompactInput) -> crate::error::AgentResult<CompactOutput> {
    let ctx = &input.context;
    let step = ctx.session.turn.current_step();

    // PreCompact 插件 hook 回调（fire-and-forget）
    if let Some(ref hook) = ctx.compact.compact_pre_hook {
        hook();
    }

    // before_compact hook：中间件可在此监听/干预 compact 生命周期
    if let Err(e) = super::middleware_runner::run_before_compact(ctx).await {
        tracing::warn!(error = %e, "before_compact hook 失败，继续 compact");
    }

    tracing::trace!(step, has_tool_calls = input.has_tool_calls, "Compact 阶段");

    // PostCompact hook 需要 affected_count；在所有 break 路径前声明
    let mut affected_count: usize = 0;

    // 用 labeled block + break 收敛所有返回路径，确保 after_compact 在函数末尾统一触发。
    let output = 'compact_core: {
        // 必备条件：context_budget + compact_config
        let (budget, config) = match (&ctx.compact.context_budget, &ctx.compact.compact_config) {
            (Some(b), Some(c)) => (b, c),
            _ => {
                // 未配置预算或 compact_config → 跳过
                break 'compact_core Ok(CompactOutput { compacted: false });
            }
        };

        // 禁用检查（v2 stage 入口显式判定，替代已删除的 CompactMiddleware::is_disabled）
        // v1 曾通过 before_model 钩子判定；v2 必须在 stage 入口显式检查，
        // 否则 DISABLE_COMPACT/DISABLE_AUTO_COMPACT 会被忽略。
        let is_disabled = std::env::var("DISABLE_COMPACT").is_ok()
            || std::env::var("DISABLE_AUTO_COMPACT").is_ok()
            || !config.auto_compact_enabled;
        if is_disabled {
            tracing::trace!(
                step,
                "Compact 已禁用（env 或 config.auto_compact_enabled=false）"
            );
            break 'compact_core Ok(CompactOutput { compacted: false });
        }

        // 构建 ContextPressure（从 token_tracker 和 context_budget 中提取字段）
        // P1-3: 直接读 StageContext.token_tracker，无需经过 AgentContext 适配层
        let pressure = {
            let tracker = ctx.compact.token_tracker.read();
            ContextPressure {
                estimated_tokens: tracker.estimated_context_tokens().unwrap_or(0),
                context_window: budget.context_window,
                output_reserve: budget.output_reserve,
                predicted_tool_growth: tracker.estimated_tool_tokens_since_last_llm as u32,
                safety_buffer: 5000,
                cache_hit_rate: tracker.cache_hit_rate(),
            }
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
        if matches!(
            compact_action,
            crate::agent::compact_v2::CompactAction::Skip
        ) {
            break 'compact_core Ok(CompactOutput { compacted: false });
        }
        let compact_strategy = match compact_action {
            crate::agent::compact_v2::CompactAction::Smart => {
                crate::agent::events::CompactStrategy::Smart
            }
            crate::agent::compact_v2::CompactAction::Micro => {
                crate::agent::events::CompactStrategy::Micro
            }
            crate::agent::compact_v2::CompactAction::Skip => {
                unreachable!("Skip 已在上方 break，不会进入此分支")
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

        // G6: 记录 compact 前的 flag 计数，用于 cancel arm 检测是否已提交变更
        let pre_compact_flagged = transcript_owned
            .entries()
            .iter()
            .filter(|e| {
                let f = transcript_owned.flags(e.message.id());
                f.truncated || f.excluded || f.projection.is_some()
            })
            .count();

        // G6: 记录 compact 前的可见消息数，供 cancel arm 发射 MessagesCompacted 事件使用
        let before_visible_len = transcript_owned.visible_messages().len();

        // 包在 select! biased 中：cancel 优先，避免 Full Compact 的长 LLM 调用阻塞中断。
        // 注：run_compact 内部不感知 turn cancel_token，必须在此层显式 select。
        let result = tokio::select! {
            biased;
            _ = ctx.session.turn.cancel_token.cancelled() => {
                // G6: 检查 compact 是否在 cancel 前已提交变更（通过 flag 计数对比）。
                // transcript_owned 已通过 &mut 被 run_compact 修改，变更已持久化在内存中。
                let post_compact_flagged = transcript_owned
                    .entries()
                    .iter()
                    .filter(|e| {
                        let f = transcript_owned.flags(e.message.id());
                        f.truncated || f.excluded || f.projection.is_some()
                    })
                    .count();

                // 把 transcript 放回 RwLock（与正常路径一致，避免遗失消息）
                *ctx.session.transcript.write() = transcript_owned;
                ctx.compact
                    .compact_consecutive_failures
                    .store(consecutive, std::sync::atomic::Ordering::Relaxed);

                if post_compact_flagged > pre_compact_flagged {
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
                            affected_count: post_compact_flagged - pre_compact_flagged,
                            estimated_tokens_saved: 0,
                            estimated_tokens_before: pressure.estimated_tokens,
                            estimated_tokens_after: pressure.estimated_tokens,
                            changed_messages: post_compact_flagged - pre_compact_flagged,
                            changed_fields: post_compact_flagged - pre_compact_flagged,
                            no_op_candidates: 0,
                            full_escalation_reason: None,
                            cache_hit_rate_before: pressure.cache_hit_rate,
                            outcome: crate::agent::compact_v2::CompactOutcome::InterruptedAfterCommit,
                        },
                    );
                    affected_count = post_compact_flagged - pre_compact_flagged;
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
                    },
                );
                break 'compact_core Err(crate::error::AgentError::Interrupted);
            }
            r = crate::agent::compact_v2::run_compact(
                &mut transcript_owned,
                compact_llm_ref,
                &config_clone,
                &pressure,
                false, // force=false（自动触发）
                &mut consecutive,
                ctx.cwd(),
            ) => r,
        };

        // G6: 检查是否在 compact 执行期间被取消（r arm 胜出但 cancel 已被触发）
        if ctx.session.turn.cancel_token.is_cancelled() {
            let did_apply = result.outcome().has_applied_change();
            // 把 transcript 放回 RwLock（transcript 已在 run_compact 中修改）
            *ctx.session.transcript.write() = transcript_owned;
            ctx.compact
                .compact_consecutive_failures
                .store(consecutive, std::sync::atomic::Ordering::Relaxed);

            if did_apply {
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
                // 从最后几条消息提取 re_inject 元信息（CompactFileInfo / Skills 名称）
                // 注：run_compact 内 re_inject_v2 已把 [最近读取的文件: ...] / [激活的 Skill 指令: ...]
                // 追加到 transcript 末尾，可直接用 extract_file_info / extract_skill_names 解析
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
                // P1-3: 直接操作 StageContext.token_tracker
                ctx.compact.token_tracker.write().reset();
                // 注：token_tracker reset 为只读 token 操作，无需 drain recall
            }

            // `MessagesCompacted` 仅表示确有 Compact mutation；未应用 outcome
            // 仅保留 `CompactStarted` 作为 begin 观测，完整 outcome transport 留待后续切片。
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
            }
            did_compact
        };

        break 'compact_core Ok(CompactOutput { compacted });
    };

    // after_compact hook：无论 compact 是否实际执行，均通知中间件
    if let Err(e) = super::middleware_runner::run_after_compact(ctx).await {
        tracing::warn!(error = %e, "after_compact hook 失败");
    }

    // PostCompact 插件 hook 回调（所有返回路径统一触发）
    if let Some(ref hook) = ctx.compact.compact_post_hook {
        let compacted = output.as_ref().map(|o| o.compacted).unwrap_or(false);
        hook(compacted, affected_count);
    }

    output
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "compact_test.rs"]
mod tests;
