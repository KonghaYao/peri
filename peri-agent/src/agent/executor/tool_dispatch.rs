use std::collections::HashMap;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use super::ReActAgent;
use crate::{
    agent::{
        events::AgentEvent,
        react::{ReactLLM, Reasoning, ToolCall, ToolResult},
        state::State,
    },
    error::{AgentError, AgentResult},
    messages::{message::MessageId, BaseMessage, ToolCallRequest},
    tools::BaseTool,
};

/// 工具名语义别名表：LLM 输出的名称 → 实际注册的工具名。
const TOOL_ALIASES: &[(&str, &str)] = &[("task", "Agent"), ("shell", "Bash"), ("reading", "Read")];

/// 工具参数名别名表：LLM 输出的参数名 → 实际参数名。
/// 主要解决 Read/Write/Edit（file_path）与 Glob/Grep（path）之间的 LLM 参数名混淆。
const PARAM_ALIASES: &[(&str, &str)] = &[("path", "file_path")];

/// 将 LLM 有时会误用的参数名归一化为标准名。
/// 仅在有别名键且无目标键时才替换（不覆盖已有正确值）。
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

/// 连续失败检测阈值
const CONSECUTIVE_FAILURE_THRESHOLD: usize = 5;

/// 将错误 output 归一化为稳定的失败类别（去除路径/UUID/时间戳等动态内容）。
/// 用于连续失败检测的 key 构造，确保同种错误类型能累计计数（P0-1）。
fn classify_failure_kind(output: &str) -> &'static str {
    let lower = output.to_ascii_lowercase();
    if lower.contains("not found")
        || lower.contains("toolnotfound")
        || lower.contains("no such file")
        || lower.contains("does not exist")
        || lower.contains("unknown tool")
    {
        "not_found"
    } else if lower.contains("timeout") || lower.contains("timed out") {
        "timeout"
    } else if lower.contains("permission denied")
        || lower.contains("rejected")
        || lower.contains("not approved")
        || lower.contains("was blocked")
    {
        "rejected"
    } else if lower.contains("missing")
        && (lower.contains("parameter") || lower.contains("argument") || lower.contains("required"))
    {
        "missing_input"
    } else if lower.contains("invalid") || lower.contains("malformed") {
        "invalid_input"
    } else if lower.contains("exit code") || lower.contains("stderr") {
        "exec_failed"
    } else {
        "other"
    }
}

/// 构造连续失败检测 key：保留 `tool_name:` 前缀（供重置 retain 使用），
/// 后缀为错误类别（避免动态内容导致 key 永不重合）。
fn make_failure_key(tool_name: &str, output: &str) -> String {
    format!("{}:{}", tool_name, classify_failure_kind(output))
}

/// 工具名解析：精确匹配 → 大小写无关匹配 → 语义别名。
fn resolve_tool<'a>(
    name: &str,
    all_tools: &HashMap<String, &'a dyn BaseTool>,
) -> Option<&'a dyn BaseTool> {
    // 1. 精确匹配
    if let Some(tool) = all_tools.get(name).copied() {
        return Some(tool);
    }
    // 2. 大小写无关匹配
    for (key, tool) in all_tools {
        if key.eq_ignore_ascii_case(name) {
            return Some(*tool);
        }
    }
    // 3. 语义别名
    for (alias, real_name) in TOOL_ALIASES {
        if name.eq_ignore_ascii_case(alias) {
            if let Some(tool) = all_tools.get(*real_name).copied() {
                tracing::debug!(alias = %name, resolved = %real_name, "工具名别名匹配");
                return Some(tool);
            }
        }
    }
    None
}

/// 工具审批 → 并发执行 → 结果收集（不写 state）→ 统一写入
pub(crate) async fn dispatch_tools<L: ReactLLM, S: State>(
    agent: &ReActAgent<L, S>,
    state: &mut S,
    reasoning: &Reasoning,
    all_tools: &HashMap<String, &dyn BaseTool>,
    cancel: &CancellationToken,
    consecutive_failures: &mut HashMap<String, usize>,
) -> AgentResult<Vec<(ToolCall, ToolResult)>> {
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

    // emit AI 工具前文本（非流式；流式模式下 LLM 适配器已通过 StreamingContext emit）
    if !reasoning.streamed && !reasoning.thought.trim().is_empty() {
        agent.emit(AgentEvent::TextChunk {
            message_id: ai_msg_id,
            chunk: reasoning.thought.clone(),
            source_agent_id: None,
        });
    }

    // 阶段 A：收集所有工具调用结果（不写 state）
    // 返回 Err 仅在 before_tool 错误路径（此时 state 干净，无 AI 消息）
    // 传入 ai_msg 让工具的 ToolContext.messages 能看到本轮 AI 回答（延迟写入 TRAP 下
    // state 不含本轮 AI 消息，但 GoalTool 等需要本轮上下文做验证）。
    let (results, was_cancelled, deferred_error) = collect_tool_results(
        agent,
        state,
        reasoning.tool_calls.clone(),
        all_tools,
        cancel,
        ai_msg_id,
        &ai_msg,
    )
    .await?;

    // 阶段 B：一次性写入 state（Cancel / deferred_error 路径也写入，保证 state 一致）
    agent.emit(AgentEvent::MessageAdded(ai_msg.clone()));
    state.add_message(ai_msg);

    for (_, result) in &results {
        // 连续失败追踪
        if result.is_error {
            let key = make_failure_key(&result.tool_name, &result.output);
            let count = consecutive_failures.entry(key).or_insert(0);
            *count += 1;
            if *count >= CONSECUTIVE_FAILURE_THRESHOLD {
                tracing::warn!(
                    tool = %result.tool_name,
                    count = *count,
                    "连续 {} 次相同错误，注入纠正消息",
                    count
                );
                // [TRAP] 必须用 Human + <system-reminder> 注入，禁止 BaseMessage::system。
                // System 消息会被 anthropic/openai invoke hoist 到 system prompt 顶部，
                // 违反 frozen_system_prompt 稳定性（第一优先级）。
                // （与 goal_middleware.rs / compact_middleware.rs 注入路径一致）
                state.add_message(BaseMessage::human(format!(
                    "<system-reminder>\nWarning: Tool '{}' has failed {} consecutive times with the same error. \
                     Stop retrying and analyze the root cause. Consider using a different approach \
                     or asking the user for guidance.\n</system-reminder>",
                    result.tool_name, count
                )));
            }
        } else {
            // 成功则重置该工具的所有失败计数
            consecutive_failures.retain(|k, _| !k.starts_with(&format!("{}:", result.tool_name)));
        }

        let tool_msg = if result.is_error {
            BaseMessage::tool_error(&result.tool_call_id, result.output.as_str())
        } else {
            BaseMessage::tool_result(&result.tool_call_id, result.output.as_str())
        };
        let tool_msg_clone = tool_msg.clone();
        state.add_message(tool_msg);
        agent.emit(AgentEvent::MessageAdded(tool_msg_clone));

        // P0-5：累积工具结果 token 估算，让 TokenTracker 在下次 LLM 调用前感知 tool_result 注入，
        // 避免大工具结果组合在 compact 阈值检查前就把 context 推到极限。
        state
            .token_tracker_mut()
            .add_estimated_tool_tokens(&result.output);
    }

    // 阶段 C：所有 tool_result 写入完成，触发 PostToolBatch hook
    agent.chain.run_after_tools_batch(state, &results).await?;

    // 写入完成后再返回错误
    if was_cancelled {
        tracing::warn!("dispatch_tools: returning Interrupted (was_cancelled)");
        return Err(AgentError::Interrupted);
    }
    if let Some(msg) = deferred_error {
        tracing::warn!("dispatch_tools: returning MiddlewareError: {}", msg);
        return Err(AgentError::MiddlewareError {
            middleware: "chain".to_string(),
            reason: msg,
        });
    }

    Ok(results)
}

/// 执行 before_tool 审批 + 并发工具调用，收集所有结果。
///
/// **不变量**：调用期间 state 中不包含本轮 AI 消息。所有 `run_on_error` /
/// `run_after_tool` 实现均不依赖 `state.messages()` 包含本轮新增内容
/// （已验证：全部 17 个中间件的这些钩子均使用 `_state: &mut S` 模式）。
/// 新增中间件时必须遵守此约束。
///
/// 不写入 state，由 `dispatch_tools` 统一写入。
///
/// 返回 `(results, was_cancelled, deferred_error)`。
/// - 正常路径：`(results, false, None)`
/// - Cancel 路径：`(results, true, None)`
/// - after_tool 错误：`(results, false, Some(msg))`
/// - before_tool 错误 / Cancel in before_tool：返回 `Err`（state 未修改）
async fn collect_tool_results<L: ReactLLM, S: State>(
    agent: &ReActAgent<L, S>,
    state: &mut S,
    original_calls: Vec<ToolCall>,
    all_tools: &HashMap<String, &dyn BaseTool>,
    cancel: &CancellationToken,
    ai_msg_id: MessageId,
    ai_msg: &BaseMessage,
) -> AgentResult<(Vec<(ToolCall, ToolResult)>, bool, Option<String>)> {
    let mut ready_calls: Vec<ToolCall> = Vec::with_capacity(original_calls.len());
    let mut settled_results: Vec<(ToolCall, ToolResult)> = Vec::new();

    // 阶段一：批量 before_tool
    let before_results = agent
        .chain
        .run_before_tools_batch(state, original_calls.clone())
        .await;

    for (tool_call, before_result) in original_calls.iter().zip(before_results) {
        // before_tool 阶段也检查取消
        if cancel.is_cancelled() {
            // 为已 emit ToolStart 的 ready_calls 补发 ToolEnd，
            // 避免 TUI 的 pending_tools 短暂残留
            for tc in &ready_calls {
                agent.emit(AgentEvent::ToolEnd {
                    message_id: ai_msg_id,
                    tool_call_id: tc.id.clone(),
                    name: tc.name.clone(),
                    output: "interrupted by user".to_string(),
                    is_error: true,
                    source_agent_id: None,
                });
            }
            return Err(AgentError::Interrupted);
        }
        match before_result {
            Ok(modified_call) => {
                agent.emit(AgentEvent::ToolStart {
                    message_id: ai_msg_id,
                    tool_call_id: modified_call.id.clone(),
                    name: modified_call.name.clone(),
                    input: modified_call.input.clone(),
                    source_agent_id: None,
                });
                ready_calls.push(modified_call);
            }
            Err(AgentError::ToolRejected { ref reason, .. }) => {
                let rejection_result =
                    ToolResult::error(&tool_call.id, &tool_call.name, reason.clone());
                agent.emit(AgentEvent::ToolStart {
                    message_id: ai_msg_id,
                    tool_call_id: tool_call.id.clone(),
                    name: tool_call.name.clone(),
                    input: tool_call.input.clone(),
                    source_agent_id: None,
                });
                agent.emit(AgentEvent::ToolEnd {
                    message_id: ai_msg_id,
                    tool_call_id: tool_call.id.clone(),
                    name: tool_call.name.clone(),
                    output: rejection_result.output.clone(),
                    is_error: true,
                    source_agent_id: None,
                });
                settled_results.push((tool_call.clone(), rejection_result));
            }
            Err(e) => {
                let _ = agent.chain.run_on_error(state, &e).await;
                // 为已 emit ToolStart 的 ready_calls 补发 ToolEnd
                for tc in &ready_calls {
                    agent.emit(AgentEvent::ToolEnd {
                        message_id: ai_msg_id,
                        tool_call_id: tc.id.clone(),
                        name: tc.name.clone(),
                        output: e.to_string(),
                        is_error: true,
                        source_agent_id: None,
                    });
                }
                return Err(e);
            }
        }
    }

    // 阶段二：所有工具并发执行。
    // SubAgent 通过 child_handler_factory 的独立 event handler 避免
    // 共享 Langfuse Mutex 的锁竞争，LLM 流式支持取消令牌中断。
    //
    // 在闭包前提取 messages/cwd，避免在 async move 闭包中借用 state。
    //
    // [TRAP] 延迟写入：此时 state 不含本轮 AI 消息（dispatch_tools 阶段 B 才写入）。
    // 但某些工具（如 GoalTool.complete 的 LLM 验证）需要看到本轮 agent 的回答。
    // 解决：在 snapshot 末尾附加本轮 AI 消息的只读视图——不写入 state，不影响 TRAP。
    // 用 Arc 共享，避免每个并发闭包都 clone 完整 messages 数组。
    let messages_snapshot: Arc<Vec<BaseMessage>> = {
        let mut msgs: Vec<BaseMessage> = state.messages().to_vec();
        msgs.push(ai_msg.clone());
        Arc::new(msgs)
    };
    let cwd_snapshot = state.cwd().to_owned();
    let tool_results: Vec<Result<String, AgentError>> = {
        let futures: Vec<_> = ready_calls
            .iter()
            .map(|call| {
                let tool_name = call.name.clone();
                let call_id = call.id.clone();
                let input = call.input.clone();
                let input = normalize_params(input); // 新增：参数名归一化
                let tool = resolve_tool(&call.name, all_tools);
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
                        let ctx = crate::tools::ToolContext::new(&messages, &cwd);
                        match tool {
                            Some(t) => t.invoke(input, ctx).await.map_err(|e| {
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

    // 阶段三：串行处理结果——所有 tool_result 收集到 results 中，
    // 不写 state，由 dispatch_tools 统一写入。
    // 工具执行错误不终止循环——错误 ToolResult 收集后由 LLM 下一轮修正。
    // after_tool 中间件错误收集到 deferred_error。
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
                let _ = agent.chain.run_on_error(state, e).await;
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
            let rid = state.get_context("run_id").map(|s| s.to_owned());
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
                    "step": state.current_step(),
                }),
                state.get_context("session_id"),
                rid.as_deref(),
            );
        }
        agent.emit(AgentEvent::ToolEnd {
            message_id: ai_msg_id,
            tool_call_id: modified_call.id.clone(),
            name: modified_call.name.clone(),
            output: result.output.clone(),
            is_error: result.is_error,
            source_agent_id: None,
        });

        if let Err(e) = agent
            .chain
            .run_after_tool(state, &modified_call, &result)
            .await
        {
            let _ = agent.chain.run_on_error(state, &e).await;
            deferred_error = deferred_error.or(Some(e.to_string()));
        }

        // 错误感知建议注入：保持 is_error=true，仅追加建议文本到 output
        if result.is_error {
            if let Some(registry) = &agent.error_suggest_registry {
                let ctx = crate::error_suggest::ErrorContext::new(
                    &modified_call.name,
                    &modified_call.input,
                    &result.output,
                    std::path::Path::new(state.cwd()),
                    &agent.tool_registry_snapshot,
                );
                if let Some(sug) = registry.suggest(&ctx) {
                    result.output =
                        crate::error_suggest::format::format_suggestion(&result.output, &sug);
                }
            }
        }

        exec_results.push((modified_call, result));
    }

    // 合并 settled（rejected）+ executed 结果
    settled_results.extend(exec_results);

    // Cancel / deferred_error 不在此返回 Err，由 dispatch_tools 在写入 state 后再检查
    Ok((settled_results, was_cancelled, deferred_error))
}

#[cfg(test)]
#[path = "tool_dispatch_test.rs"]
mod tests;
