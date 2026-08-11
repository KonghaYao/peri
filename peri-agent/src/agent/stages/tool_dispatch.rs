//! 工具分发（v2）— before_tools_batch → 并发执行 → after_tool → 统一写入
//!
//! 关键设计：
//! - **state 来源**：v2 用 `StageContext.transcript`（通过 middleware_runner 桥接
//!   StageContext 调用 middleware chain）
//! - **事件总线**：v2 用 `ctx.runtime.event_bus.emit_render(RenderEvent::*)`
//! - **写入语义**：v2 用 `ctx.session.transcript.write().append()`
//!
//! 不变量（与 v1 一致）：
//! - **延迟写入**：before_tool / after_tool 期间 transcript 不含本轮 AI 消息
//! - **deferred_error**：多工具并发循环不在中途返回，先收集所有错误
//! - **error_suggest 注入**：在 run_after_tool 之后、写 transcript 之前；只修改 output 文本
//! - **ToolEnd emit 时机**：在 error_suggest 注入之前 emit

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use super::middleware_runner::{
    run_after_tool, run_after_tools_batch, run_before_tools_batch, run_on_error,
};
use super::StageContext;
use crate::agent::events_v2::RenderEvent;
use crate::agent::react::{Reasoning, ToolCall, ToolResult};
use crate::error::{AgentError, AgentResult};
use crate::messages::{BaseMessage, MessageId, ToolCallRequest};
use crate::tools::{BaseTool, CanonicalToolInvocation};

/// 连续失败检测阈值
const CONSECUTIVE_FAILURE_THRESHOLD: u32 = 5;

/// 工具参数名别名表：LLM 输出的参数名 → 实际参数名。
const PARAM_ALIASES: &[(&str, &str)] = &[("path", "file_path")];

/// 将 LLM 有时会误用的参数名归一化为标准名。
/// 仅当目标工具 schema 声明 real 参数时执行别名替换——Grep/Glob 的
/// `path` 是其标准参数名而非 `file_path` 的别名，不能替换。
fn normalize_params(input: serde_json::Value, target: Option<&dyn BaseTool>) -> serde_json::Value {
    let mut obj = match input {
        serde_json::Value::Object(map) => map,
        _ => return input,
    };
    let declared_params: Vec<String> = target
        .map(|t| {
            t.parameters()
                .get("properties")
                .and_then(|p| p.as_object())
                .map(|props| props.keys().cloned().collect())
                .unwrap_or_default()
        })
        .unwrap_or_default();
    for (alias, real) in PARAM_ALIASES {
        if declared_params.iter().any(|p| p == real)
            && obj.contains_key(*alias)
            && !obj.contains_key(*real)
        {
            let value = obj.remove(*alias).unwrap();
            obj.insert(real.to_string(), value);
            tracing::warn!(
                alias = %alias,
                resolved = %real,
                "参数名别名归一化：LLM 使用了非标准参数名"
            );
        }
    }
    serde_json::Value::Object(obj)
}

/// 工具名解析：精确匹配 → 大小写无关匹配 → 工具自声明别名。
#[cfg(test)]
fn resolve_tool<'a>(
    name: &str,
    all_tools: &'a HashMap<String, Arc<dyn BaseTool>>,
) -> Option<&'a Arc<dyn BaseTool>> {
    // 1. 精确匹配
    if let Some(tool) = all_tools.get(name) {
        return Some(tool);
    }
    // 2. 大小写无关匹配（注册 key）
    for (key, tool) in all_tools {
        if key.eq_ignore_ascii_case(name) {
            return Some(tool);
        }
    }
    // 3. 工具自声明别名（BaseTool::aliases()）
    for tool in all_tools.values() {
        for alias in tool.aliases() {
            if name.eq_ignore_ascii_case(alias) {
                tracing::debug!(alias = %name, resolved = %tool.name(), "工具自声明别名匹配");
                return Some(tool);
            }
        }
    }
    None
}

/// 分发结果
pub struct DispatchOutcome {
    /// 所有工具调用结果（顺序与 reasoning.tool_calls 一致）
    pub results: Vec<(ToolCall, ToolResult)>,
}

/// 分发工具调用：审批 → 并发执行 → 收集结果 → 统一写入 transcript
pub async fn dispatch_tools(
    ctx: &StageContext,
    reasoning: &Reasoning,
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

    let all_tools: BTreeMap<String, Arc<dyn BaseTool>> = {
        let tools_guard = ctx.runtime.tools.read();
        tools_guard
            .iter()
            .map(|(k, v)| (k.clone(), Arc::clone(v)))
            .collect()
    };
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
    let raw_calls: HashMap<String, ToolCall> = invocations
        .iter()
        .map(|invocation| {
            (
                invocation.policy_call.id.clone(),
                invocation.raw_call.clone(),
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
        &raw_calls,
        &target_tools,
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

/// 收集阶段产物（内部使用）
struct CollectOutcome {
    results: Vec<(ToolCall, ToolResult)>,
    was_cancelled: bool,
    deferred_error: Option<String>,
}

/// before_tool 审批阶段的产出
struct ApprovalOutcome {
    /// 通过审批、准备并发执行的调用
    ready_calls: Vec<ToolCall>,
    /// 已在审批阶段就结算完成的（例如 ToolRejected）结果
    settled_results: Vec<(ToolCall, ToolResult)>,
}

/// 执行 before_tool 审批 + 并发工具调用，收集所有结果（不写 transcript）
///
/// Orchestrator：按顺序调用三个子阶段函数。
async fn collect_tool_results(
    ctx: &StageContext,
    original_calls: Vec<ToolCall>,
    raw_calls: &HashMap<String, ToolCall>,
    all_tools: &HashMap<String, Arc<dyn BaseTool>>,
    cancel: &CancellationToken,
    // ai_msg_id 保留为 API 契约（未来 ToolEnd 事件可携带 message_id）
    ai_msg_id: MessageId,
    ai_msg: &BaseMessage,
) -> AgentResult<CollectOutcome> {
    let _ = ai_msg_id;

    // 阶段一：批量 before_tool 审批
    let approval = run_before_tool_approvals(ctx, original_calls, raw_calls, cancel).await?;

    // yield 使 EventBus forwarder task 排空 render_tx 中由阶段一 emit 的
    // ToolStarted 事件（转发到 event_tx），保证在 SubAgent 工具 invoke 内部
    // 通过 handler.on_event(SubagentStarted) 直发 event_tx 之前，ToolStart
    // 已就位。否则 forwarder 的两个 hops 延迟会让 SubagentStarted 抢先到达
    // event_tx，导致 TUI segment 顺序反转（SubAgent 段落在 ToolCard(Agent) 前），
    // SubAgent 工具调用跑到 Agent 卡片上方。
    tokio::task::yield_now().await;

    // 阶段二：并发执行（snapshot messages + ai_msg 只读视图）
    let tool_results = dispatch_concurrent(
        ctx,
        &approval.ready_calls,
        raw_calls,
        all_tools,
        cancel,
        ai_msg,
    )
    .await;

    // 阶段三：聚合 + 错误延迟
    Ok(settle_results(
        ctx,
        approval,
        tool_results,
        cancel.is_cancelled(),
        all_tools,
    )
    .await)
}

/// 阶段一：批量 before_tool 审批。
///
/// 遍历 `run_before_tools_batch` 结果，emit `ToolStarted`，分流：
/// - `Ok(call)` → 推入 ready_calls
/// - `Err(ToolRejected)` → emit ToolStart + ToolEnd，推入 settled_results
/// - `Err(e)` → run_on_error + 为已 emit ToolStart 的补发 ToolEnd，向上传播错误
///
/// 取消检查发生在 zip 迭代开头：若已取消，为 ready_calls 补发 ToolEnd 后返回 Interrupted。
async fn run_before_tool_approvals(
    ctx: &StageContext,
    original_calls: Vec<ToolCall>,
    raw_calls: &HashMap<String, ToolCall>,
    cancel: &CancellationToken,
) -> AgentResult<ApprovalOutcome> {
    let turn_id = ctx.turn_id();
    let agent_id = ctx.session.agent_id;

    let mut ready_calls: Vec<ToolCall> = Vec::with_capacity(original_calls.len());
    let mut settled_results: Vec<(ToolCall, ToolResult)> = Vec::new();

    let before_results = run_before_tools_batch(ctx, &original_calls).await;

    for (tool_call, before_result) in original_calls.iter().zip(before_results) {
        if cancel.is_cancelled() {
            // 为已 emit ToolStart 的 ready_calls 补发 ToolEnd
            for tc in &ready_calls {
                let raw_call = raw_calls.get(&tc.id).unwrap_or(tc);
                ctx.runtime.event_bus.emit_render(RenderEvent::ToolEnded {
                    turn_id,
                    agent_id,
                    tool_call_id: raw_call.id.clone(),
                    name: raw_call.name.clone(),
                    output: "interrupted by user".to_string(),
                    is_error: true,
                });
            }
            return Err(AgentError::Interrupted);
        }
        match before_result {
            Ok(modified_call) => {
                if modified_call.id != tool_call.id || modified_call.name != tool_call.name {
                    let reason = "middleware cannot modify tool call id or name".to_string();
                    let raw_call = raw_calls.get(&tool_call.id).unwrap_or(tool_call);
                    let rejection_result = ToolResult::error(&raw_call.id, &tool_call.name, reason);
                    settled_results.push((raw_call.clone(), rejection_result));
                    continue;
                }
                let raw_call = raw_calls.get(&tool_call.id).unwrap_or(tool_call);
                ctx.runtime.event_bus.emit_render(RenderEvent::ToolStarted {
                    turn_id,
                    agent_id,
                    tool_call_id: raw_call.id.clone(),
                    name: raw_call.name.clone(),
                    input: raw_call.input.clone(),
                });
                ready_calls.push(modified_call);
            }
            Err(AgentError::ToolRejected { ref reason, .. }) => {
                let raw_call = raw_calls.get(&tool_call.id).unwrap_or(tool_call);
                let rejection_result =
                    ToolResult::error(&tool_call.id, &tool_call.name, reason.clone());
                ctx.runtime.event_bus.emit_render(RenderEvent::ToolStarted {
                    turn_id,
                    agent_id,
                    tool_call_id: raw_call.id.clone(),
                    name: raw_call.name.clone(),
                    input: raw_call.input.clone(),
                });
                ctx.runtime.event_bus.emit_render(RenderEvent::ToolEnded {
                    turn_id,
                    agent_id,
                    tool_call_id: raw_call.id.clone(),
                    name: raw_call.name.clone(),
                    output: rejection_result.output.clone(),
                    is_error: true,
                });
                settled_results.push((tool_call.clone(), rejection_result));
            }
            Err(e) => {
                let _ = run_on_error(ctx, &e).await;
                for tc in &ready_calls {
                    ctx.runtime.event_bus.emit_render(RenderEvent::ToolEnded {
                        turn_id,
                        agent_id,
                        tool_call_id: tc.id.clone(),
                        name: tc.name.clone(),
                        output: e.to_string(),
                        is_error: true,
                    });
                }
                return Err(e);
            }
        }
    }

    Ok(ApprovalOutcome {
        ready_calls,
        settled_results,
    })
}

/// 阶段二：并发执行 ready_calls（snapshot messages + ai_msg 只读视图）。
///
/// 每个调用走 `biased` select：cancel.cancelled() 优先于 invoke_fut，
/// 命中时返回 `ToolExecutionFailed { reason: "interrupted by user" }`。
async fn dispatch_concurrent(
    ctx: &StageContext,
    ready_calls: &[ToolCall],
    raw_calls: &HashMap<String, ToolCall>,
    all_tools: &HashMap<String, Arc<dyn BaseTool>>,
    cancel: &CancellationToken,
    ai_msg: &BaseMessage,
) -> Vec<Result<String, AgentError>> {
    if ready_calls.is_empty() {
        return Vec::new();
    }

    let messages_snapshot: Arc<Vec<BaseMessage>> = {
        let mut msgs = ctx.visible_messages();
        msgs.push(ai_msg.clone());
        Arc::new(msgs)
    };
    let cwd_snapshot = ctx.cwd().to_owned();
    let turn_id = ctx.turn_id();
    let agent_id = ctx.session.agent_id;
    let event_bus = Arc::clone(&ctx.runtime.event_bus);

    let futures: Vec<_> = ready_calls
        .iter()
        .map(|call| {
            let tool_name = call.name.clone();
            let call_id = call.id.clone();
            let raw_call = raw_calls
                .get(&call.id)
                .cloned()
                .unwrap_or_else(|| call.clone());
            let tool = all_tools.get(&call.id).cloned();
            let input = match &tool {
                Some(t) => normalize_params(call.input.clone(), Some(t.as_ref())),
                None => call.input.clone(),
            };
            let cancel = cancel.clone();
            let messages = Arc::clone(&messages_snapshot);
            let cwd = cwd_snapshot.clone();
            let event_bus = Arc::clone(&event_bus);
            // [Fix] span 在 async 块外创建、用 .instrument() 包裹整个 future：
            // span.enter() 的 guard 跨 await 持有在 tokio multi-thread 下会随 task
            // 线程迁移错误重置 thread-local current span，导致 tracing-subscriber
            // `lookup_current` panic（'the subscriber should have data for the current span'）。
            // instrument 在每次 poll 时重新 enter，跨线程安全。
            let span = tracing::info_span!(
                "agent.tool_call",
                tool.name = %tool_name,
                tool.call_id = %call_id,
            );
            async move {
                let timeout_opt = tool.as_ref().and_then(|t| t.timeout());
                let invoke_fut = async {
                    let ctx_param = crate::tools::ToolContext::new(&messages, &cwd);
                    match tool {
                        Some(t) => t.invoke(input, ctx_param).await.map_err(|e| {
                            AgentError::ToolExecutionFailed {
                                tool: tool_name.clone(),
                                reason: e.to_string(),
                            }
                        }),
                        None => Err(AgentError::ToolNotFound(tool_name.clone())),
                    }
                };
                let result = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        Err(AgentError::ToolExecutionFailed {
                            tool: tool_name.clone(),
                            reason: "interrupted by user".to_string(),
                        })
                    }
                    result = async {
                        if let Some(d) = timeout_opt {
                            tokio::time::timeout(d, invoke_fut).await
                        } else {
                            Ok(invoke_fut.await)
                        }
                    } => {
                        match result {
                            Ok(tool_result) => tool_result,
                            Err(_elapsed) => {
                                // 安全：Err 分支仅在 timeout_opt 为 Some 时可达
                                let secs = timeout_opt.unwrap().as_secs();
                                Err(AgentError::ToolExecutionFailed {
                                    tool: tool_name.clone(),
                                    reason: format!("tool call timed out after {}s", secs),
                                })
                            }
                        }
                    }
                };
                // 工具完成即刻 emit ToolEnded，不等 join_all 返回
                // 快速工具的 Langfuse observation endTime 不再被慢工具拖高
                let (output, is_error) = match &result {
                    Ok(o) => (o.clone(), false),
                    Err(e) => (e.to_string(), true),
                };
                event_bus.emit_render(RenderEvent::ToolEnded {
                    turn_id,
                    agent_id,
                    tool_call_id: raw_call.id.clone(),
                    name: raw_call.name.clone(),
                    output,
                    is_error,
                });
                result
            }
            .instrument(span)
        })
        .collect();
    futures::future::join_all(futures).await
}

/// 阶段三：串行处理结果（ToolEnd 已在 dispatch_concurrent 中 emit）
/// + after_tool + error_suggest + 截断 + 聚合。
///
/// 不变量：deferred_error 取首个 after_tool 错误，后续错误不覆盖。
async fn settle_results(
    ctx: &StageContext,
    approval: ApprovalOutcome,
    tool_results: Vec<Result<String, AgentError>>,
    was_cancelled: bool,
    all_tools: &HashMap<String, Arc<dyn BaseTool>>,
) -> CollectOutcome {
    let all_tools_ref = all_tools;

    let ApprovalOutcome {
        ready_calls,
        mut settled_results,
    } = approval;

    let mut deferred_error: Option<String> = None;
    let mut exec_results: Vec<(ToolCall, ToolResult)> = Vec::with_capacity(ready_calls.len());

    for (modified_call, tool_result) in ready_calls.into_iter().zip(tool_results) {
        let mut result = match tool_result {
            Ok(output) => ToolResult::success(&modified_call.id, &modified_call.name, output),
            Err(AgentError::ToolNotFound(ref name)) => {
                tracing::warn!(tool.name = %name, "工具未找到，作为错误结果返回");
                ToolResult::error(
                    &modified_call.id,
                    &modified_call.name,
                    format!("Tool '{}' not found", name),
                )
            }
            Err(ref e) => {
                let _ = run_on_error(ctx, e).await;
                ToolResult::error(&modified_call.id, &modified_call.name, e.to_string())
            }
        };

        if result.is_error {
            tracing::warn!(
                tool.name = %result.tool_name,
                tool.is_error = true,
                error_len = result.output.len(),
                "tool call failed"
            );
            let session_id = ctx
                .session
                .session_context
                .read()
                .get("session_id")
                .cloned();
            let run_id = ctx.session.session_context.read().get("run_id").cloned();
            let input_summary: String = modified_call
                .input
                .as_str()
                .unwrap_or("")
                .chars()
                .take(200)
                .collect();
            crate::metrics::emit(
                "tool.error",
                serde_json::json!({
                    "name": result.tool_name,
                    "tool_call_id": modified_call.id,
                    "error": result.output,
                    "input_summary": input_summary,
                    "step": ctx.session.turn.current_step(),
                }),
                session_id.as_deref(),
                run_id.as_deref(),
            );
        }

        // ToolEnd 已在 dispatch_concurrent 中 emit（工具完成即刻发射）
        // 此处仅处理 after_tool + error_suggest 等后处理逻辑

        if let Err(e) = run_after_tool(ctx, &modified_call, &result).await {
            let _ = run_on_error(ctx, &e).await;
            deferred_error = deferred_error.or(Some(e.to_string()));
        }

        // error_suggest 注入 + output_char_limit 截断
        post_process_result(ctx, &modified_call, &mut result, all_tools_ref);

        exec_results.push((modified_call, result));
    }

    settled_results.extend(exec_results);

    CollectOutcome {
        results: settled_results,
        was_cancelled,
        deferred_error,
    }
}

/// 单条结果的后处理：error_suggest 注入（仅 error 分支）+ output_char_limit 截断。
///
/// 顺序：先注入建议文本，再按工具声明的 `output_char_limit` 截断。
fn post_process_result(
    ctx: &StageContext,
    modified_call: &ToolCall,
    result: &mut ToolResult,
    all_tools: &HashMap<String, Arc<dyn BaseTool>>,
) {
    // error_suggest 注入：仅修改 output 文本
    if result.is_error {
        if let Some(registry) = &ctx.runtime.error_suggest_registry {
            let ec = crate::error_suggest::ErrorContext::new(
                &modified_call.name,
                &modified_call.input,
                &result.output,
                std::path::Path::new(ctx.cwd()),
                &ctx.runtime.tool_registry_snapshot,
            );
            if let Some(sug) = registry.suggest(&ec) {
                result.output =
                    crate::error_suggest::format::format_suggestion(&result.output, &sug);
            }
        }
    }

    // output_char_limit 截断：已经解析完成的 target 工具声明输出上限时按字符截断
    if let Some(tool) = all_tools.get(&modified_call.id) {
        if let Some(limit) = tool.output_char_limit() {
            if result.output.chars().count() > limit {
                let truncated: String = result.output.chars().take(limit).collect();
                result.output = format!("{}\n\n[Output truncated at {} chars]", truncated, limit);
            }
        }
    }
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
                let content = format!("<system-reminder>\n{}\n</system-reminder>", warning);
                ctx.session
                    .queue
                    .push(crate::session::queue::QueuedMessage::info(
                        crate::session::queue::MessageSource::ToolFailureWarning,
                        BaseMessage::human(crate::messages::MessageContent::text(content)),
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
