//! Tool dispatcher.

use super::{
    effective_dispatcher, AgentError, AgentResult, Arc, BaseMessage, BaseTool, CancellationToken,
    CanonicalToolInvocation, ExecutionEntryOrigin, HashMap, InvocationContext, Reasoning,
    RenderEvent, SessionToolCatalogSnapshot, SettledInvocation, StageContext, ToolCall,
    ToolCallRequest, ToolResult,
};

/// 连续失败检测阈值
pub(super) const CONSECUTIVE_FAILURE_THRESHOLD: u32 = 5;

/// session-owned canonical 执行入口：把一次执行（会话事实 + pinned catalog）投影为
/// [`EffectiveToolDispatcher`](peri_acp_types::tools::EffectiveToolDispatcher)。
///
/// 与 Act 批次走**同一条**管线（before-tool 规则 → 批准 → 执行 → after_tool），
/// 但调用方自带 `InvocationContext`（App / Host idle 入口由
/// [`InvocationContext::from_source`] 构造）。具体实现类型保持 crate 私有。
#[must_use]
pub fn invocation_dispatcher(
    ctx: InvocationContext,
    catalog: Arc<SessionToolCatalogSnapshot>,
) -> Arc<dyn crate::tools::EffectiveToolDispatcher> {
    Arc::new(effective_dispatcher::StageEffectiveToolDispatcher::new(
        ctx, catalog,
    ))
}

/// 分发结果
#[derive(Debug)]
#[must_use]
pub struct DispatchOutcome {
    /// 所有工具调用结果（顺序与 reasoning.tool_calls 一致）
    pub results: Vec<(ToolCall, ToolResult)>,
}

/// ToolInvoke 在 target 解析失败时尚未投影 canonical tool；与 policy/invoke 一致，
/// 不产生 render 副作用（避免 UI 仅展示误导性的 wrapper 行）。仅当目录把该名字
/// 解析为宿主代理时抑制；外部 MCP 同名工具与无法解析的名字保持成对 emit。
pub(super) fn should_emit_settled_tool_render(ctx: &StageContext, call: &ToolCall) -> bool {
    if !peri_acp_types::tool_proxy::is_invoke_proxy_name(&call.name) {
        return true;
    }
    let snapshot = ctx.runtime.tool_catalog.snapshot();
    let canonical = snapshot
        .aliases
        .get(&call.name.to_ascii_lowercase())
        .map_or(call.name.as_str(), String::as_str);
    match snapshot.tools.get(canonical) {
        Some(entry) => !peri_acp_types::tool_proxy::is_host_invoke_proxy(entry.tool.as_ref()),
        None => true,
    }
}

/// 为 middleware 前已结算的工具调用（解析失败、畸形 id 等）补全 render 事件。
///
/// 流式 Reason 可能已提前 emit `ToolStarted`；此处仍成对发送 Started/Ended，
/// TUI 侧对同 id 的 Started 做 upsert，Ended 负责结束 loading。
pub(super) fn emit_settled_tool_render(
    ctx: &StageContext,
    call: &ToolCall,
    result: &ToolResult,
    origin_message_id: crate::messages::MessageId,
) {
    let turn_id = ctx.turn_id();
    let agent_id = ctx.session.agent_id;
    ctx.runtime.event_bus.emit_render(RenderEvent::ToolStarted {
        turn_id,
        agent_id,
        tool_call_id: call.id.clone(),
        name: call.name.clone(),
        input: call.input.clone(),
        origin_message_id: Some(origin_message_id),
        // 模型批次的工具调用归属于本次 LLM 调用的持久 assistant 消息。
        origin_readable: Some(true),
    });
    ctx.runtime.event_bus.emit_render(RenderEvent::ToolEnded {
        images: Vec::new(),
        turn_id,
        agent_id,
        tool_call_id: call.id.clone(),
        name: call.name.clone(),
        output: result.output.clone(),
        is_error: result.is_error,
        subagent_failure: result.subagent_failure.clone(),
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
        let resolved = ctx
            .runtime
            .tool_invocation_resolver
            .resolve_model(call, &all_tools);
        match resolved {
            Ok(invocation) => invocations.push(invocation),
            Err(error) => resolution_errors.push((
                call.clone(),
                ToolResult::error(&call.id, &call.name, error.to_string()),
            )),
        }
    }
    for (call, result) in &resolution_errors {
        // 结算路径的逐条记录点：解析失败的工具不进入 policy/invoke，执行路径的
        // `tool call failed`（tool_dispatch/execution.rs）覆盖不到它们，只能在
        // 这里补齐，否则错误只以事件（ToolEnded{is_error:true}）离开进程。
        //
        tracing::warn!(
            tool_call_id = %call.id,
            tool = %call.name,
            reason = %result.output,
            "tool call resolution failed"
        );
        if should_emit_settled_tool_render(ctx, call) {
            emit_settled_tool_render(ctx, call, result, ai_msg_id);
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
    //
    // canonical 执行上下文只投影会话事实与端口：transcript 提交、after_tools_batch、
    // 失败计数与事件投影留在本函数（Act 侧）。
    let invocation_context = InvocationContext::from_stage(
        ctx,
        ExecutionEntryOrigin::ModelBatch,
        cancel.clone(),
        Some(&ai_msg),
    );
    let mut collect_outcome = crate::agent::stages::tool_dispatch::execution::collect_tool_results(
        &invocation_context,
        policy_calls,
        &event_calls,
        &target_tools,
        catalog,
        cancel,
        Some(ai_msg_id),
        // 模型批次：归属本次 LLM 调用的真实持久 assistant 消息。
        Some(true),
        &ai_msg,
        None,
    )
    .await?;

    // 阶段 B：完整交换由 writer 原子落库，确认后才发布 canonical；大图片批次
    // 不再先深拷贝到 16 MiB 普通 Append 积压，也不把半个交换暴露给恢复路径。
    let mut exchange =
        Vec::with_capacity(1 + collect_outcome.results.len() + resolution_errors.len());
    exchange.push(ai_msg);
    for settled in &mut collect_outcome.results {
        exchange
            .push(crate::agent::stages::tool_dispatch::execution::settled_tool_message(settled));
        // canonical 已取得所需媒体；Act 后续只用文本结果，不跨落库等待保留 raw 副本。
        settled.outcome = crate::tools::ToolOutcome::Unreported;
    }
    for (_, result) in &resolution_errors {
        exchange.push(BaseMessage::tool_error(
            &result.tool_call_id,
            result.output.as_str(),
        ));
    }
    crate::session::MessageTranscript::commit_tool_exchange(&ctx.session.transcript, exchange)
        .await?;

    // 只计入已提交的工具结果，包括错误结果。
    // 提交，不在这里重复计量。先记账再运行后置 hook，确保 hook 失败也不丢增长。
    {
        let mut tracker = ctx.compact.token_tracker.write();
        let result_items = collect_outcome
            .results
            .iter()
            .map(|settled| &settled.result)
            .chain(resolution_errors.iter().map(|(_, result)| result));
        for result in result_items {
            tracker.add_estimated_tool_tokens(&result.output);
        }
    }

    // 阶段 C：仅已进入 policy 的调用触发 after_tools_batch。
    // Resolution 错误在 middleware 前结算，不能产生任何 hook 副作用。
    crate::agent::stages::middleware_runner::run_after_tools_batch(
        ctx,
        &collect_outcome.result_pairs(),
    )
    .await?;
    collect_outcome.results.extend(
        resolution_errors
            .into_iter()
            .map(|(call, result)| SettledInvocation::unreported(call, result)),
    );

    // 连续失败追踪 + ToolFailureWarning 注入
    handle_consecutive_failures(ctx, &collect_outcome.results).await;

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
        results: collect_outcome.result_pairs(),
    })
}

/// 处理连续失败追踪 + ToolFailureWarning 注入
///
/// v2 简化为总计数（AtomicU32）。失败累计达阈值时推送 Info 消息到 v2 queue，
/// 下轮 Receive 阶段消费（带 `<system-reminder>` 包裹）。
pub(super) async fn handle_consecutive_failures(ctx: &StageContext, results: &[SettledInvocation]) {
    for settled in results {
        let result = &settled.result;
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
                let message = crate::session::queue::QueuedMessage::system_reminder(
                    crate::session::queue::MessageKind::Info,
                    crate::session::queue::MessageSource::ToolFailureWarning,
                    reminder,
                );
                if let Err(error) = ctx.session.queue.enqueue(message).await {
                    tracing::warn!(%error, "tool failure warning enqueue failed");
                }
            }
        } else {
            // 任一成功 → 重置计数
            ctx.compact
                .consecutive_failures
                .store(0, std::sync::atomic::Ordering::Relaxed);
        }
    }
}
