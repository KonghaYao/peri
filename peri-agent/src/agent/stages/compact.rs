//! Compact 阶段 — 上下文压缩
//!
//! 根据 ContextBudget 计算使用率，调用 `compact_v2::run_compact`：
//! - budget < 0.75：跳过
//! - budget ≥ 0.75：Smart Compact（规则驱动保留关键消息）或 Micro Compact（按 round 截断）
//!   - affected_count >= micro_min_affected → 有效，budget ≥ 0.95 时叠加 Full
//!   - affected_count < micro_min_affected → 无效，升级为 Full
//! - force=true：直接 Full（跳过 Micro/Smart）
//!
//! Full Compact 失败时 `consecutive_failures` 累加，达上限后降级跳过。

use super::{CompactInput, CompactOutput};
use crate::agent::compact_v2::config::CompactConfig;

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

        // 读 token_tracker（只读操作，无需 drain recall）
        // P1-3: 直接读 StageContext.token_tracker，无需经过 AgentContext 适配层
        let budget_pct = {
            let tracker = ctx.compact.token_tracker.read();
            tracker.context_usage_percent(budget.context_window)
        };

        let pct = match budget_pct {
            Some(p) => p / 100.0,
            None => break 'compact_core Ok(CompactOutput { compacted: false }),
        };

        // 在 emit CompactStarted 前估算策略
        // determine_compact_action 判定 Skip/Micro/Smart；Full 由 run_compact 内部动态决策。
        // 此处 event 的策略字段用于观测。
        let compact_action = crate::agent::compact_v2::determine_compact_action(pct, config);
        let compact_strategy = match compact_action {
            crate::agent::compact_v2::CompactAction::Smart => {
                crate::agent::events::CompactStrategy::Smart
            }
            _ => crate::agent::events::CompactStrategy::Micro,
        };

        tracing::trace!(step, budget_pct = %pct, "Compact 预算检查");

        // 调用 compact_v2：取出 transcript 所有权，运行后放回（避免跨 await 持锁）
        let compact_llm_ref: Option<&dyn crate::llm::BaseModel> = ctx
            .compact
            .compact_llm
            .as_ref()
            .map(|arc| arc.as_ref() as &dyn crate::llm::BaseModel);

        let mut transcript_owned = {
            let mut guard = ctx.session.transcript.write();
            std::mem::take(&mut *guard)
        };

        let mut consecutive = ctx
            .compact
            .consecutive_failures
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
        // 包在 select! biased 中：cancel 优先，避免 Full Compact 的长 LLM 调用阻塞中断。
        // 注：run_compact 内部不感知 turn cancel_token，必须在此层显式 select。
        let result = tokio::select! {
            biased;
            _ = ctx.session.turn.cancel_token.cancelled() => {
                // 把 transcript 放回 RwLock（与正常路径一致，避免遗失消息）
                *ctx.session.transcript.write() = transcript_owned;
                ctx.compact
                    .consecutive_failures
                    .store(consecutive, std::sync::atomic::Ordering::Relaxed);
                break 'compact_core Err(crate::error::AgentError::Interrupted);
            }
            r = crate::agent::compact_v2::run_compact(
                &mut transcript_owned,
                compact_llm_ref,
                &config_clone,
                pct,
                false, // force=false（自动触发）
                &mut consecutive,
                ctx.cwd(),
            ) => r,
        };

        // 写回 consecutive_failures
        ctx.compact
            .consecutive_failures
            .store(consecutive, std::sync::atomic::Ordering::Relaxed);

        // 把 transcript 放回 RwLock
        *ctx.session.transcript.write() = transcript_owned;

        let compacted = {
            let r = result; // compact_v2::run_compact 直接返回 CompactResult
            affected_count = r.affected_count;
            if r.affected_count > 0 {
                tracing::info!(
                    step,
                    strategy = ?r.strategy,
                    affected = r.affected_count,
                    before = r.before_len,
                    after_visible = r.after_visible_len,
                    "Compact 完成"
                );
                // 取出 visible_messages 快照供 TUI 重建（必须在 transcript 还在 owned 时读）
                // 注：run_compact 内部已把 transcript_owned 还给 ctx.session.transcript.write()，
                // 此处从 ctx 重新读
                let (messages_snapshot, files, skills) = {
                    let guard = ctx.session.transcript.read();
                    let visible: Vec<crate::messages::BaseMessage> =
                        guard.visible_messages().into_iter().cloned().collect();
                    // 从最后几条消息提取 re_inject 元信息（CompactFileInfo / Skills 名称）
                    // 注：run_compact 内 re_inject_v2 已把 [最近读取的文件: ...] / [激活的 Skill 指令: ...]
                    // 追加到 transcript 末尾，可直接用 extract_file_info / extract_skill_names 解析
                    let combined_files = crate::agent::compact_v2::extract_file_info(&visible);
                    let combined_skills = crate::agent::compact_v2::extract_skill_names(&visible);
                    (visible, combined_files, combined_skills)
                };

                // Full Compact 后必须 reset token_tracker——否则下轮 budget 计算会基于
                // compact 前的累积 token 数，导致每轮都触发 compact
                // 注：与 v1 CompactMiddleware 行为对齐（v1 已删除）
                if r.strategy == crate::agent::events::CompactStrategy::Full {
                    // P1-3: 直接操作 StageContext.token_tracker
                    ctx.compact.token_tracker.write().reset();
                    // 注：token_tracker reset 为只读 token 操作，无需 drain recall
                }

                // emit 观测事件（携带 messages 快照供 TUI 重建 pipeline）
                ctx.runtime.event_bus.emit_observe(
                    crate::agent::events_v2::ObserveEvent::MessagesCompacted {
                        turn_id: ctx.turn_id(),
                        agent_id: ctx.session.agent_id,
                        before_count: r.before_len,
                        after_count: r.after_visible_len,
                        summary: r.summary.clone().unwrap_or_default(),
                        messages: messages_snapshot,
                        files,
                        skills,
                        re_inject_count: 0,
                        strategy: r.strategy,
                    },
                );
                true
            } else {
                false
            }
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
