//! Act 阶段 — 工具执行或回答
//!
//! 根据 Reason 结果决定：
//! - 有 tool_calls → 通过 `tool_dispatch::dispatch_tools` 并发执行 + 写入 transcript
//! - 无 tool_calls → 产出最终回答，emit TextChunk + StateSnapshot

use super::middleware_runner::run_after_agent;
use super::tool_dispatch::dispatch_tools;
use super::{ActInput, ActOutput, StageContext};
use crate::agent::events_v2::{RenderEvent, StateEvent};
use crate::error::AgentResult;

/// emit TurnCompleted（迭代边界提交信号）：从 transcript 读快照，供成功/失败路径共用，
/// 保证 TUI committed 视图与 transcript 一致（S5.3：run_after_agent / dispatch_tools
/// 失败时最终回答或工具结果可能已写入 transcript，必须提交迭代边界再传播错误）。
fn emit_turn_completed(ctx: &StageContext) {
    let finalized_messages = ctx.session.transcript.read().visible_snapshot();
    ctx.runtime
        .event_bus
        .emit_render(RenderEvent::TurnCompleted {
            turn_id: ctx.turn_id(),
            agent_id: ctx.session.agent_id,
            steps: ctx.session.turn.current_step(),
            elapsed_secs: 0.0,
            finalized_messages,
        });
}

/// 运行 Act 阶段
pub async fn run_act(input: ActInput) -> AgentResult<ActOutput> {
    let ctx = &input.context;
    let has_tool_calls = input.reasoning.needs_tool_call();

    tracing::trace!(
        step = ctx.session.turn.current_step(),
        has_tool_calls,
        "Act 阶段"
    );

    // emit StateSnapshot：每次 Act 阶段都推送，无论有无工具调用。
    // 消费方（TUI 状态栏等）据此实时刷新上下文使用率。
    let message_count = ctx.session.transcript.read().len();
    let context_budget = ctx.compact.context_budget.clone();
    let context_total_tokens = context_budget.as_ref().map(|b| b.context_window as u64);
    let (total_tokens, budget_pct) = match context_budget.as_ref() {
        Some(budget) => {
            // P1-3: 直接读 StageContext.token_tracker，无需经过 AgentContext 适配层
            let tracker = ctx.compact.token_tracker.read();
            let used = tracker.estimated_context_tokens().unwrap_or(0);
            let pct = tracker.context_usage_percent(budget.context_window);
            (used, pct)
        }
        None => (0, None),
    };
    ctx.runtime.event_bus.emit_state(StateEvent::StateSnapshot {
        turn_id: ctx.turn_id(),
        agent_id: ctx.session.agent_id,
        message_count,
        total_tokens,
        current_step: ctx.session.turn.current_step(),
        consecutive_failures: ctx
            .compact
            .consecutive_failures
            .load(std::sync::atomic::Ordering::Relaxed),
        budget_pct,
        context_total_tokens,
    });

    if has_tool_calls {
        // 工具调用路径：dispatch_tools 处理审批 + 并发执行 + 写入 transcript
        let cancel = ctx.session.turn.cancel_token.clone();
        let outcome = match dispatch_tools(ctx, &input.reasoning, &cancel).await {
            Ok(outcome) => outcome,
            Err(e) => {
                // 镜像修复（S5.3）：dispatch 失败（cancel / middleware 错误）时
                // transcript 可能已含本轮消息（阶段 B 提交后），先 emit TurnCompleted
                // 再传播错误，保持 TUI committed 视图与 transcript 一致。
                emit_turn_completed(ctx);
                return Err(e);
            }
        };

        tracing::debug!(tool_count = outcome.results.len(), "Act 阶段执行了工具调用");

        // 迭代边界提交信号：emit TurnCompleted 携带 transcript 快照，让 TUI 同步规范状态
        // （避免下一次迭代文本渲染在本次工具调用之前）
        //
        // 必须用 emit_render（而非 emit_state）——TurnCompleted 与同迭代的
        // TextChunk/ToolStarted/ToolEnded 共享 render_tx 通道，FIFO 保证顺序。
        // 若放到 state_tx 独立通道，TUI forwarder 的 biased select! 会优先消费
        // 下一迭代的 TextChunk，把本轮 TurnCompleted 拖到后面，导致 partial 混合
        // 两轮内容，渲染出"新文本在旧工具之前"的顺序错乱（详见 RenderEvent::TurnCompleted）。
        emit_turn_completed(ctx);

        Ok(ActOutput {
            has_tool_calls: true,
            final_answer: None,
        })
    } else {
        // 最终回答路径：写入 transcript + emit TextChunk + StateSnapshot
        let final_answer = input
            .reasoning
            .final_answer
            .clone()
            .unwrap_or_else(|| input.reasoning.thought.clone());

        // 写入 AI 消息（如果 source_message 存在则用它，否则用 final_answer 构造）
        let ai_msg = input.reasoning.source_message.clone().unwrap_or_else(|| {
            crate::messages::BaseMessage::ai(crate::messages::MessageContent::text(
                final_answer.clone(),
            ))
        });
        let ai_msg_id = ai_msg.id();
        ctx.session.transcript.write().append(ai_msg);

        // 非流式时 emit TextChunk（流式由 LLM 适配器直接 emit）
        if !input.reasoning.streamed && !final_answer.trim().is_empty() {
            ctx.runtime.event_bus.emit_render(RenderEvent::TextChunk {
                turn_id: ctx.turn_id(),
                agent_id: ctx.session.agent_id,
                // 与写入 transcript 的消息 ID 对齐（ACP 标准 messageId 语义）
                message_id: ai_msg_id,
                chunk: final_answer.clone(),
            });
        }

        // 构造 AgentOutput 并触发 after_agent（允许 middleware 修改输出）
        let mut output = crate::agent::react::AgentOutput::new(
            final_answer.clone(),
            ctx.session.turn.current_step(),
        );
        output.tool_calls = input
            .reasoning
            .tool_calls
            .iter()
            .map(|tc| {
                (
                    tc.clone(),
                    crate::agent::react::ToolResult::success(&tc.id, &tc.name, ""),
                )
            })
            .collect();

        let output_after = match run_after_agent(ctx, output).await {
            Ok(output) => output,
            Err(e) => {
                // S5.3：最终回答已 append 到 transcript（:97），先 emit TurnCompleted
                // （从 transcript 读快照，RwLock 无死锁风险——append 已释放锁），
                // 再传播错误，避免 TUI committed 视图与 transcript/持久化不一致。
                emit_turn_completed(ctx);
                return Err(e);
            }
        };

        // 迭代边界提交信号：emit TurnCompleted 携带 transcript 快照（含本轮最终回答）
        //
        // 必须用 emit_render（详见上方工具路径 同款注释）——保证与同迭代 Render 事件 FIFO。
        emit_turn_completed(ctx);

        Ok(ActOutput {
            has_tool_calls: false,
            final_answer: Some(output_after.text),
        })
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "act_test.rs"]
mod tests;
