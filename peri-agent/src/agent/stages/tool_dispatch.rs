//! 工具分发（v2）— before_tools_batch → 并发执行 → after_tool → 统一写入
//!
//! 关键设计：
//! - **state 来源**：v2 用 `StageContext.transcript`（通过 middleware_runner 桥接
//!   AgentState 调用 middleware chain）
//! - **事件总线**：v2 用 `ctx.event_bus.emit_render(RenderEvent::*)`
//! - **写入语义**：v2 用 `ctx.transcript.write().append()`
//!
//! 不变量（与 v1 一致）：
//! - **延迟写入**：before_tool / after_tool 期间 transcript 不含本轮 AI 消息
//! - **deferred_error**：多工具并发循环不在中途返回，先收集所有错误
//! - **error_suggest 注入**：在 run_after_tool 之后、写 transcript 之前；只修改 output 文本
//! - **ToolEnd emit 时机**：在 error_suggest 注入之前 emit

use std::collections::HashMap;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use super::middleware_runner::{
    run_after_tool, run_after_tools_batch, run_before_tools_batch, run_on_error,
};
use super::StageContext;
use crate::agent::events_v2::RenderEvent;
use crate::agent::react::{Reasoning, ToolCall, ToolResult};
use crate::error::{AgentError, AgentResult};
use crate::messages::{message::MessageId, BaseMessage, ToolCallRequest};
use crate::tools::BaseTool;

/// 连续失败检测阈值
const CONSECUTIVE_FAILURE_THRESHOLD: u32 = 5;

/// 工具名语义别名表：LLM 输出的名称 → 实际注册的工具名。
const TOOL_ALIASES: &[(&str, &str)] = &[("task", "Agent"), ("shell", "Bash"), ("reading", "Read")];

/// 工具参数名别名表：LLM 输出的参数名 → 实际参数名。
const PARAM_ALIASES: &[(&str, &str)] = &[("path", "file_path")];

/// 将 LLM 有时会误用的参数名归一化为标准名。
fn normalize_params(input: serde_json::Value) -> serde_json::Value {
    let mut obj = match input {
        serde_json::Value::Object(map) => map,
        _ => return input,
    };
    for (alias, real) in PARAM_ALIASES {
        if obj.contains_key(*alias) && !obj.contains_key(*real) {
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

/// 工具名解析：精确匹配 → 大小写无关匹配 → 语义别名。
fn resolve_tool<'a>(
    name: &str,
    all_tools: &'a HashMap<String, Arc<dyn BaseTool>>,
) -> Option<&'a Arc<dyn BaseTool>> {
    if let Some(tool) = all_tools.get(name) {
        return Some(tool);
    }
    for (key, tool) in all_tools {
        if key.eq_ignore_ascii_case(name) {
            return Some(tool);
        }
    }
    for (alias, real_name) in TOOL_ALIASES {
        if name.eq_ignore_ascii_case(alias) {
            if let Some(tool) = all_tools.get(*real_name) {
                tracing::debug!(alias = %name, resolved = %real_name, "工具名别名匹配");
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
    let agent_id = ctx.agent_id;

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
        ctx.event_bus.emit_render(RenderEvent::TextChunk {
            turn_id,
            agent_id,
            chunk: reasoning.thought.clone(),
        });
    }

    let all_tools: HashMap<String, Arc<dyn BaseTool>> = {
        let tools_guard = ctx.tools.read();
        tools_guard
            .iter()
            .map(|(k, v)| (k.clone(), Arc::clone(v)))
            .collect()
    };

    // 阶段 A：收集所有工具调用结果（不写 transcript）
    let collect_outcome = collect_tool_results(
        ctx,
        reasoning.tool_calls.clone(),
        &all_tools,
        cancel,
        ai_msg_id,
        &ai_msg,
    )
    .await?;

    // 阶段 B：原子写入 transcript（staging 模式）
    {
        let mut tx = ctx.transcript.write();
        tx.stage_ai_message(ai_msg);
        for (_, result) in &collect_outcome.results {
            let tool_msg = if result.is_error {
                BaseMessage::tool_error(&result.tool_call_id, result.output.as_str())
            } else {
                BaseMessage::tool_result(&result.tool_call_id, result.output.as_str())
            };
            tx.stage_tool_result(tool_msg);
        }
        tx.commit_staged();
    }

    // 阶段 C：触发 after_tools_batch（写入完成后）
    run_after_tools_batch(ctx, &collect_outcome.results).await?;

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

/// 执行 before_tool 审批 + 并发工具调用，收集所有结果（不写 transcript）
async fn collect_tool_results(
    ctx: &StageContext,
    original_calls: Vec<ToolCall>,
    all_tools: &HashMap<String, Arc<dyn BaseTool>>,
    cancel: &CancellationToken,
    // ai_msg_id 保留为 API 契约（未来 ToolEnd 事件可携带 message_id）
    ai_msg_id: MessageId,
    ai_msg: &BaseMessage,
) -> AgentResult<CollectOutcome> {
    let _ = ai_msg_id;
    let turn_id = ctx.turn_id();
    let agent_id = ctx.agent_id;

    let mut ready_calls: Vec<ToolCall> = Vec::with_capacity(original_calls.len());
    let mut settled_results: Vec<(ToolCall, ToolResult)> = Vec::new();

    // 阶段一：批量 before_tool
    let before_results = run_before_tools_batch(ctx, &original_calls).await;

    for (tool_call, before_result) in original_calls.iter().zip(before_results) {
        if cancel.is_cancelled() {
            // 为已 emit ToolStart 的 ready_calls 补发 ToolEnd
            for tc in &ready_calls {
                ctx.event_bus.emit_render(RenderEvent::ToolEnded {
                    turn_id,
                    agent_id,
                    tool_call_id: tc.id.clone(),
                    name: tc.name.clone(),
                    output: "interrupted by user".to_string(),
                    is_error: true,
                });
            }
            return Err(AgentError::Interrupted);
        }
        match before_result {
            Ok(modified_call) => {
                ctx.event_bus.emit_render(RenderEvent::ToolStarted {
                    turn_id,
                    agent_id,
                    tool_call_id: modified_call.id.clone(),
                    name: modified_call.name.clone(),
                    input: modified_call.input.clone(),
                });
                ready_calls.push(modified_call);
            }
            Err(AgentError::ToolRejected { ref reason, .. }) => {
                let rejection_result =
                    ToolResult::error(&tool_call.id, &tool_call.name, reason.clone());
                ctx.event_bus.emit_render(RenderEvent::ToolStarted {
                    turn_id,
                    agent_id,
                    tool_call_id: tool_call.id.clone(),
                    name: tool_call.name.clone(),
                    input: tool_call.input.clone(),
                });
                ctx.event_bus.emit_render(RenderEvent::ToolEnded {
                    turn_id,
                    agent_id,
                    tool_call_id: tool_call.id.clone(),
                    name: tool_call.name.clone(),
                    output: rejection_result.output.clone(),
                    is_error: true,
                });
                settled_results.push((tool_call.clone(), rejection_result));
            }
            Err(e) => {
                let _ = run_on_error(ctx, &e).await;
                for tc in &ready_calls {
                    ctx.event_bus.emit_render(RenderEvent::ToolEnded {
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

    // 阶段二：并发执行（snapshot messages + ai_msg 只读视图）
    let messages_snapshot: Arc<Vec<BaseMessage>> = {
        let mut msgs = ctx.visible_messages();
        msgs.push(ai_msg.clone());
        Arc::new(msgs)
    };
    let cwd_snapshot = ctx.cwd().to_owned();

    let tool_results: Vec<Result<String, AgentError>> = {
        let futures: Vec<_> = ready_calls
            .iter()
            .map(|call| {
                let tool_name = call.name.clone();
                let call_id = call.id.clone();
                let input = normalize_params(call.input.clone());
                let tool = resolve_tool(&call.name, all_tools).cloned();
                let cancel = cancel.clone();
                let messages = Arc::clone(&messages_snapshot);
                let cwd = cwd_snapshot.clone();
                async move {
                    let span = tracing::info_span!(
                        "agent.tool_call",
                        tool.name = %tool_name,
                        tool.call_id = %call_id,
                    );
                    let _enter = span.enter();
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
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => {
                            Err(AgentError::ToolExecutionFailed {
                                tool: tool_name,
                                reason: "interrupted by user".to_string(),
                            })
                        }
                        result = invoke_fut => result,
                    }
                }
            })
            .collect();
        futures::future::join_all(futures).await
    };

    let was_cancelled = cancel.is_cancelled();

    // 阶段三：串行处理结果
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
            let session_id = ctx.session_context.read().get("session_id").cloned();
            let run_id = ctx.session_context.read().get("run_id").cloned();
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
                    "step": ctx.turn.current_step(),
                }),
                session_id.as_deref(),
                run_id.as_deref(),
            );
        }

        // ToolEnd emit 在 error_suggest 注入之前
        ctx.event_bus.emit_render(RenderEvent::ToolEnded {
            turn_id,
            agent_id,
            tool_call_id: modified_call.id.clone(),
            name: modified_call.name.clone(),
            output: result.output.clone(),
            is_error: result.is_error,
        });

        if let Err(e) = run_after_tool(ctx, &modified_call, &result).await {
            let _ = run_on_error(ctx, &e).await;
            deferred_error = deferred_error.or(Some(e.to_string()));
        }

        // error_suggest 注入：仅修改 output 文本
        if result.is_error {
            if let Some(registry) = &ctx.error_suggest_registry {
                let ec = crate::error_suggest::ErrorContext::new(
                    &modified_call.name,
                    &modified_call.input,
                    &result.output,
                    std::path::Path::new(ctx.cwd()),
                    &ctx.tool_registry_snapshot,
                );
                if let Some(sug) = registry.suggest(&ec) {
                    result.output =
                        crate::error_suggest::format::format_suggestion(&result.output, &sug);
                }
            }
        }

        // output_char_limit 截断：工具声明输出上限时按字符截断
        if let Some(tool) = all_tools.get(&modified_call.name) {
            if let Some(limit) = tool.output_char_limit() {
                if result.output.chars().count() > limit {
                    let truncated: String = result.output.chars().take(limit).collect();
                    result.output =
                        format!("{}\n\n[Output truncated at {} chars]", truncated, limit);
                }
            }
        }

        exec_results.push((modified_call, result));
    }

    settled_results.extend(exec_results);

    Ok(CollectOutcome {
        results: settled_results,
        was_cancelled,
        deferred_error,
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
                ctx.queue.push(crate::session::queue::QueuedMessage::info(
                    crate::session::queue::MessageSource::ToolFailureWarning,
                    BaseMessage::human(crate::messages::MessageContent::text(content)),
                ));
            }
        } else {
            // 任一成功 → 重置计数
            ctx.consecutive_failures
                .store(0, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── normalize_params ──

    #[test]
    fn test_normalize_params_path_alias_to_file_path() {
        let input = json!({"path": "/tmp/foo.rs"});
        let out = normalize_params(input);
        assert!(out.get("file_path").is_some());
        assert!(out.get("path").is_none());
    }

    #[test]
    fn test_normalize_params_keep_file_path_when_present() {
        // 当 file_path 已存在时，path 别名不覆盖
        let input = json!({"path": "/a", "file_path": "/b"});
        let out = normalize_params(input);
        assert_eq!(out.get("file_path").unwrap(), &json!("/b"));
        // path 仍然保留（未触发别名替换）
        assert!(out.get("path").is_some());
    }

    #[test]
    fn test_normalize_params_passthrough_non_object() {
        let input = json!("string");
        let out = normalize_params(input.clone());
        assert_eq!(out, input);
    }

    #[test]
    fn test_normalize_params_keep_unrelated_keys() {
        let input = json!({"query": "hello", "limit": 10});
        let out = normalize_params(input);
        assert_eq!(out.get("query").unwrap(), &json!("hello"));
        assert_eq!(out.get("limit").unwrap(), &json!(10));
    }

    // ── resolve_tool ──

    fn make_tools() -> HashMap<String, Arc<dyn BaseTool>> {
        // 用空 ToolStub 占位以验证名字解析
        #[derive(Default)]
        struct ToolStub;
        #[async_trait::async_trait]
        impl BaseTool for ToolStub {
            fn name(&self) -> &str {
                "stub"
            }
            fn description(&self) -> &str {
                ""
            }
            fn parameters(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            async fn invoke(
                &self,
                _input: serde_json::Value,
                _ctx: crate::tools::ToolContext<'_>,
            ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
                Ok(String::new())
            }
        }
        let mut map: HashMap<String, Arc<dyn BaseTool>> = HashMap::new();
        map.insert("Read".to_string(), Arc::new(ToolStub));
        map.insert("Bash".to_string(), Arc::new(ToolStub));
        map.insert("Agent".to_string(), Arc::new(ToolStub));
        map
    }

    #[test]
    fn test_resolve_tool_exact_match() {
        let tools = make_tools();
        let tool = resolve_tool("Read", &tools);
        assert!(tool.is_some());
    }

    #[test]
    fn test_resolve_tool_case_insensitive_match() {
        let tools = make_tools();
        let tool = resolve_tool("read", &tools);
        assert!(tool.is_some());
    }

    #[test]
    fn test_resolve_tool_semantic_alias_reading() {
        let tools = make_tools();
        // "reading" 别名应解析为 "Read"
        let tool = resolve_tool("reading", &tools);
        assert!(tool.is_some());
    }

    #[test]
    fn test_resolve_tool_semantic_alias_task() {
        let tools = make_tools();
        // "task" 别名应解析为 "Agent"
        let tool = resolve_tool("task", &tools);
        assert!(tool.is_some());
    }

    #[test]
    fn test_resolve_tool_unknown_returns_none() {
        let tools = make_tools();
        let tool = resolve_tool("NonExistent", &tools);
        assert!(tool.is_none());
    }

    #[test]
    fn test_resolve_tool_alias_case_insensitive() {
        let tools = make_tools();
        // 别名大小写无关：SHELL → Bash
        let tool = resolve_tool("SHELL", &tools);
        assert!(tool.is_some());
    }
}
