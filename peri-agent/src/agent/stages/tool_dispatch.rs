//! 工具分发（v2）— before_tools_batch → 并发执行 → after_tool → 统一写入
//!
//! 关键设计：
//! - **state 来源**：v2 用 `StageContext.session.transcript`（通过 middleware_runner 桥接
//!   StageContext 调用 middleware chain）
//! - **事件总线**：v2 用 `ctx.runtime.event_bus.emit_render(RenderEvent::*)`
//! - **写入语义**：v2 用 `stage_ai_message` / `stage_tool_result` / `commit_staged`
//!
//! 不变量（与 v1 一致）：
//! - **延迟写入**：before_tool / after_tool 期间 transcript 不含本轮 AI 消息
//! - **deferred_error**：多工具并发循环不在中途返回，先收集所有错误
//! - **error_suggest 注入**：在 run_after_tool 之后、写 transcript 之前；只修改 output 文本
//! - **ToolEnd emit 时机**：在 error_suggest 注入之前 emit

use std::collections::HashMap;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

mod effective_dispatcher;
mod execution;

use super::middleware_runner::run_after_tools_batch;
use super::StageContext;
use crate::agent::events_v2::RenderEvent;
use crate::agent::react::{Reasoning, ToolCall, ToolResult};
use crate::error::{AgentError, AgentResult};
use crate::messages::{BaseMessage, ToolCallRequest};
use crate::session::tool_catalog::SessionToolCatalogSnapshot;
use crate::tools::{BaseTool, CanonicalToolInvocation};
use execution::collect_tool_results;

/// 连续失败检测阈值
const CONSECUTIVE_FAILURE_THRESHOLD: u32 = 5;

/// 分发结果
pub struct DispatchOutcome {
    /// 所有工具调用结果（顺序与 reasoning.tool_calls 一致）
    pub results: Vec<(ToolCall, ToolResult)>,
}

/// ExecuteExtraTool 在 target 解析失败时尚未投影 canonical tool；与 policy/invoke 一致，
/// 不产生 render 副作用（避免 UI 仅展示误导性的 wrapper 行）。
fn should_emit_settled_tool_render(call: &ToolCall) -> bool {
    call.name != "ExecuteExtraTool"
}

/// 为 middleware 前已结算的工具调用（解析失败、畸形 id 等）补全 render 事件。
///
/// 流式 Reason 可能已提前 emit `ToolStarted`；此处仍成对发送 Started/Ended，
/// TUI 侧对同 id 的 Started 做 upsert，Ended 负责结束 loading。
fn emit_settled_tool_render(ctx: &StageContext, call: &ToolCall, result: &ToolResult) {
    let turn_id = ctx.turn_id();
    let agent_id = ctx.session.agent_id;
    ctx.runtime.event_bus.emit_render(RenderEvent::ToolStarted {
        turn_id,
        agent_id,
        tool_call_id: call.id.clone(),
        name: call.name.clone(),
        input: call.input.clone(),
    });
    ctx.runtime.event_bus.emit_render(RenderEvent::ToolEnded {
        turn_id,
        agent_id,
        tool_call_id: call.id.clone(),
        name: call.name.clone(),
        output: result.output.clone(),
        is_error: result.is_error,
    });
}

/// 分发工具调用：审批 → 并发执行 → 收集结果 → 统一写入 transcript
pub async fn dispatch_tools(
    ctx: &StageContext,
    reasoning: &Reasoning,
    catalog: &Arc<SessionToolCatalogSnapshot>,
    cancel: &CancellationToken,
) -> AgentResult<DispatchOutcome> {
    let turn_id = ctx.turn_id();
    let agent_id = ctx.session.agent_id;

    let tc_reqs: Vec<ToolCallRequest> = reasoning
        .tool_calls
        .iter()
        .map(|tc| ToolCallRequest::new(tc.id.clone(), tc.name.clone(), tc.input.clone()))
        .collect();
    let ai_msg = reasoning
        .source_message
        .clone()
        .unwrap_or_else(|| BaseMessage::ai_with_tool_calls(reasoning.thought.clone(), tc_reqs));
    let ai_msg_id = ai_msg.id();

    // emit AI 工具前文本（非流式；流式由 LLM 适配器通过 StreamingContext emit）
    if !reasoning.streamed && !reasoning.thought.trim().is_empty() {
        ctx.runtime.event_bus.emit_render(RenderEvent::TextChunk {
            turn_id,
            agent_id,
            // 与 ai_msg（随后写入 transcript）的 ID 对齐（ACP 标准 messageId 语义）
            message_id: ai_msg_id,
            chunk: reasoning.thought.clone(),
        });
    }

    let all_tools = catalog.tool_map();
    let invalid_ids: std::collections::HashSet<&str> = reasoning
        .tool_calls
        .iter()
        .map(|call| call.id.as_str())
        .filter(|id| id.is_empty())
        .collect();
    let duplicate_ids: std::collections::HashSet<&str> = reasoning
        .tool_calls
        .iter()
        .filter_map(|call| {
            let count = reasoning
                .tool_calls
                .iter()
                .filter(|other| other.id == call.id)
                .count();
            (count > 1).then_some(call.id.as_str())
        })
        .collect();
    let malformed_ids: std::collections::HashSet<&str> =
        invalid_ids.union(&duplicate_ids).copied().collect();

    let mut invocations = Vec::<CanonicalToolInvocation>::new();
    let mut resolution_errors = Vec::<(ToolCall, ToolResult)>::new();
    for call in &reasoning.tool_calls {
        if malformed_ids.contains(call.id.as_str()) {
            resolution_errors.push((
                call.clone(),
                ToolResult::error(
                    &call.id,
                    &call.name,
                    "malformed tool call id: ids must be non-empty and unique within a batch",
                ),
            ));
            continue;
        }
        match ctx
            .runtime
            .tool_invocation_resolver
            .resolve(call, &all_tools)
        {
            Ok(invocation) => invocations.push(invocation),
            Err(error) => resolution_errors.push((
                call.clone(),
                ToolResult::error(&call.id, &call.name, error.to_string()),
            )),
        }
    }
    for (call, result) in &resolution_errors {
        if should_emit_settled_tool_render(call) {
            emit_settled_tool_render(ctx, call, result);
        }
    }
    // Invocation events/tool cards project the effective canonical target. The LLM's
    // wrapper call remains only in the source assistant message for protocol pairing.
    let event_calls: HashMap<String, ToolCall> = invocations
        .iter()
        .map(|invocation| {
            (
                invocation.policy_call.id.clone(),
                invocation.policy_call.clone(),
            )
        })
        .collect();
    let policy_calls: Vec<ToolCall> = invocations
        .iter()
        .map(|invocation| invocation.policy_call.clone())
        .collect();
    let target_tools: HashMap<String, Arc<dyn BaseTool>> = invocations
        .iter()
        .map(|invocation| {
            (
                invocation.policy_call.id.clone(),
                Arc::clone(&invocation.target),
            )
        })
        .collect();

    // 阶段 A：收集所有工具调用结果（不写 transcript）
    let mut collect_outcome = collect_tool_results(
        ctx,
        policy_calls,
        &event_calls,
        &target_tools,
        catalog,
        cancel,
        ai_msg_id,
        &ai_msg,
    )
    .await?;

    // 阶段 B：原子写入 transcript（staging 模式）
    {
        let mut tx = ctx.session.transcript.write();
        tx.stage_ai_message(ai_msg);
        for (_, result) in &collect_outcome.results {
            let tool_msg = if result.is_error {
                BaseMessage::tool_error(&result.tool_call_id, result.output.as_str())
            } else {
                BaseMessage::tool_result(&result.tool_call_id, result.output.as_str())
            };
            tx.stage_tool_result(tool_msg);
        }
        for (_, result) in &resolution_errors {
            tx.stage_tool_result(BaseMessage::tool_error(
                &result.tool_call_id,
                result.output.as_str(),
            ));
        }
        tx.commit_staged();
    }

    // 只计入已提交的最外层结果，包括错误结果；PTC 内部调用没有独立 transcript
    // 提交，不在这里重复计量。先记账再运行后置 hook，确保 hook 失败也不丢增长。
    {
        let mut tracker = ctx.compact.token_tracker.write();
        for (_, result) in collect_outcome.results.iter().chain(&resolution_errors) {
            tracker.add_estimated_tool_tokens(&result.output);
        }
    }

    // 阶段 C：仅已进入 policy 的调用触发 after_tools_batch。
    // Resolution 错误在 middleware 前结算，不能产生任何 hook 副作用。
    run_after_tools_batch(ctx, &collect_outcome.results).await?;
    collect_outcome.results.extend(resolution_errors);

    // 连续失败追踪 + ToolFailureWarning 注入
    handle_consecutive_failures(ctx, &collect_outcome.results);

    if collect_outcome.was_cancelled {
        tracing::warn!("dispatch_tools: returning Interrupted (was_cancelled)");
        return Err(AgentError::Interrupted);
    }
    if let Some(msg) = collect_outcome.deferred_error {
        tracing::warn!("dispatch_tools: returning MiddlewareError: {}", msg);
        return Err(AgentError::MiddlewareError {
            middleware: "chain".to_string(),
            reason: msg,
        });
    }

    Ok(DispatchOutcome {
        results: collect_outcome.results,
    })
}

/// 处理连续失败追踪 + ToolFailureWarning 注入
///
/// v2 简化为总计数（AtomicU32）。失败累计达阈值时推送 Info 消息到 v2 queue，
/// 下轮 Receive 阶段消费（带 `<system-reminder>` 包裹）。
fn handle_consecutive_failures(ctx: &StageContext, results: &[(ToolCall, ToolResult)]) {
    for (_, result) in results {
        if result.is_error {
            let current = ctx
                .compact
                .consecutive_failures
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                + 1;
            if current == CONSECUTIVE_FAILURE_THRESHOLD {
                tracing::warn!(
                    tool = %result.tool_name,
                    count = current,
                    "连续 {} 次工具失败，注入纠正消息",
                    current
                );
                let warning = format!(
                    "Warning: Tool '{}' has failed {} consecutive times. Consider a different approach.",
                    result.tool_name, current
                );
                let reminder = crate::session::producer_reminders::trusted_reminder(
                    peri_acp_types::system_reminder::ReminderCategory::Guidance,
                    "tool_runtime",
                    "consecutive_failures",
                    peri_acp_types::system_reminder::ReminderSeverity::Warning,
                    peri_acp_types::system_reminder::ReminderDelivery::Required,
                    warning,
                    Some(format!("Tool '{}' repeatedly failed", result.tool_name)),
                    serde_json::json!({
                        "tool_name": result.tool_name,
                        "failure_count": current,
                    }),
                );
                ctx.session
                    .queue
                    .push(crate::session::queue::QueuedMessage::system_reminder(
                        crate::session::queue::MessageKind::Info,
                        crate::session::queue::MessageSource::ToolFailureWarning,
                        reminder,
                    ));
            }
        } else {
            // 任一成功 → 重置计数
            ctx.compact
                .consecutive_failures
                .store(0, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "tool_dispatch_test.rs"]
mod tests;
