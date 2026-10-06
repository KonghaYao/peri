//! Reason 阶段 — LLM 推理
//!
//! 流程：catalog → before_model → 最终压力检查 → snapshot → LlmCallStart →
//!       LLM.generate_reasoning（与 cancel 竞争）→ LlmCallEnd → after_model

use super::middleware_runner::{
    run_after_model, run_before_model, run_before_reason_catalog, run_on_error,
};
use super::{ReasonInput, ReasonOutput};
use crate::agent::events_v2::{ObserveEvent, TurnErrorReason};
use crate::agent::react::{Reasoning, StreamingContext};
use crate::error::{AgentError, AgentResult};

pub async fn run_reason(input: ReasonInput) -> AgentResult<ReasonOutput> {
    super::execution_control::validate(&input.context).await?;
    let ctx = &input.context;
    let recovery_catalog = ctx.runtime.tool_catalog.snapshot();
    if let Some(reasoning) = super::work_reason::recover_reasoning(ctx, &recovery_catalog).await? {
        return Ok(ReasonOutput {
            reasoning,
            catalog: recovery_catalog,
            messages_snapshot: ctx.visible_messages().into(),
        });
    }
    let step = ctx.session.turn.current_step();
    let turn_id = ctx.turn_id();
    let agent_id = ctx.session.agent_id;

    tracing::trace!(step, has_tool_calls = input.has_tool_calls, "Reason 阶段");

    // 固定准备阶段之前的视图；Full reset/Micro 缩减本身不应触发第二次评估。
    super::compact::context_pressure::refresh(ctx)?;

    // Apply a new session capability generation only at the Reason boundary.
    let refreshed = match ctx.runtime.tool_catalog.refresh() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            tracing::warn!(error = %error, "tool catalog refresh failed; retaining previous snapshot");
            ctx.runtime.tool_catalog.snapshot()
        }
    };
    *ctx.runtime.tools.write() = refreshed.tool_map();
    run_before_reason_catalog(ctx).await?;

    // before_model middleware（goal_middleware / compact_middleware 等在此注入）
    run_before_model(ctx).await?;
    let catalog = {
        let working = ctx.runtime.tools.read();
        ctx.runtime
            .tool_catalog
            .pin_working_tools(&working)
            .map_err(|error| AgentError::Other(anyhow::Error::new(error)))?
    };

    // before_model 和动态工具发布在常规 Compact 之后。只有准备阶段确实新增
    // 压力且达到 Full 阈值时补检一次；不重复运行模型/compact hooks。
    let prepared_input_grew = super::compact::context_pressure::refresh(ctx)?;
    let needs_compact = prepared_input_grew
        && ctx
            .compact
            .compact_config
            .as_ref()
            .zip(ctx.compact.context_budget.as_ref())
            .is_some_and(|(config, budget)| {
                budget.context_window > 0
                    && ctx
                        .compact
                        .token_tracker
                        .read()
                        .estimated_context_tokens()
                        .is_some_and(|tokens| {
                            tokens as f64 / f64::from(budget.context_window)
                                >= config.auto_compact_threshold
                        })
            });
    if needs_compact {
        super::compact::run_compact_core(super::CompactInput {
            context: ctx.clone(),
            has_tool_calls: input.has_tool_calls,
        })
        .await?;
    }

    // 取出 messages 快照（避免跨 await 持有 RwLockReadGuard）。
    // 直接构建为 Arc<Vec>：LlmCallStart 与 LLM 调用共享同一份，避免二次深拷贝。
    let messages_snapshot: std::sync::Arc<Vec<crate::messages::BaseMessage>> =
        std::sync::Arc::new({
            let guard = ctx.session.transcript.read();
            crate::agent::compact_v2::projection::render_persisted_llm_view(
                &guard,
                &ctx.runtime.llm.provider_capabilities(),
            )?
        });

    let tools_owned: Vec<std::sync::Arc<dyn crate::tools::BaseTool>> = catalog
        .tools
        .values()
        .map(|entry| std::sync::Arc::clone(&entry.tool))
        .collect();
    let tool_refs: Vec<&dyn crate::tools::BaseTool> = tools_owned
        .iter()
        .filter(|t| t.is_direct() && t.visible_to_model())
        .map(|t| t.as_ref())
        .collect();
    // 绑定最终发送视图，不能使用 Compact 前或 before_model 前的估算来结清增长。
    let request_estimate = ctx
        .runtime
        .llm
        .estimate_request_tokens(&messages_snapshot, &tool_refs);
    ctx.compact
        .token_tracker
        .write()
        .begin_request(request_estimate);
    // 工具数量与名称追踪（调试用；默认 filter 下不写盘）
    tracing::debug!(
        step,
        tool_count = tool_refs.len(),
        tool_names = ?tool_refs.iter().map(|t| t.name()).collect::<Vec<_>>(),
        msg_count = messages_snapshot.len(),
        "Reason 阶段：准备调用 LLM"
    );

    // emit LlmCallStart（携带 messages + tools 快照，对齐 v1 Langfuse Generation input）
    // messages 为 Arc 浅拷贝，与下方 LLM 调用共享同一份快照
    let start_tools: Vec<crate::tools::ToolDefinition> =
        tool_refs.iter().map(|t| t.definition()).collect();
    ctx.runtime
        .event_bus
        .emit_observe(ObserveEvent::LlmCallStart {
            turn_id,
            agent_id,
            step,
            messages: messages_snapshot.clone(),
            tools: start_tools,
        });

    // 构造 StreamingContext：LLM 适配器（AgentModelBridge）在流式解析过程中
    // 直接 emit v2 RenderEvent/ObserveEvent（v1 ExecutorEvent 流式中间态已退役，
    // v1 兼容映射仅保留在 ACP 协议序列化面）。身份（turn_id/agent_id）随上下文注入。
    let turn_id = ctx.turn_id();
    let agent_id = ctx.session.agent_id;
    let streaming = Some(StreamingContext {
        event_bus: std::sync::Arc::clone(&ctx.runtime.event_bus),
        turn_id,
        agent_id,
        cancel: tokio_util::sync::CancellationToken::clone(&ctx.session.turn.cancel_token),
    });

    // 将本次实际请求与最近成功的 Full 对齐；不得借用 tracker 的旧 usage。
    let budget_probe = {
        let transcript = ctx.session.transcript.read();
        ctx.compact
            .budget_recovery
            .lock()
            .begin_request(&transcript)
    };

    // LLM 调用（与 cancel 竞争）。
    // 使用 generate_reasoning_with_observed_body：观测体复用本次调用已构建的
    // request（消除每轮 request 双构建），LlmRequestPayload 在成功后、LlmCallEnd
    // 之前 emit——Langfuse 按 step 缓存 raw_body，时序兼容（on_llm_end 前到达即可）。
    let prepared = super::work_reason::prepare(ctx, &messages_snapshot, &tool_refs).await?;
    let (mut reasoning, observed_body): (Reasoning, Option<serde_json::Value>) = tokio::select! {
        biased;
        _ = ctx.session.turn.cancel_token.cancelled() => {
            if let Err(error) = super::work_reason::block_uncertain_model(ctx).await {
                tracing::error!(%error, "cancelled model request checkpoint unconfirmed");
            }
            return Err(AgentError::Interrupted);
        }
        result = async {
            match prepared {
                Some(prepared) => ctx.runtime.llm.generate_prepared_reasoning(prepared, streaming).await.map(|reasoning| (reasoning, None)),
                None => ctx.runtime.llm.generate_reasoning_with_observed_body(&messages_snapshot, &tool_refs, streaming).await,
            }
        } => {
            match result {
                Ok((r, body)) => (r, body),
                Err(e) => {
                    if let Err(error) = super::work_reason::block_uncertain_model(ctx).await {
                        tracing::error!(%error, "failed model request checkpoint unconfirmed");
                    }
                    tracing::error!(
                        step,
                        model = %ctx.runtime.llm.model_name(),
                        error = %e,
                        "LLM generate_reasoning 失败"
                    );
                    // LLM 报错时 emit LlmCallEnd，让消费者可见
                    ctx.runtime.event_bus.emit_observe(ObserveEvent::LlmCallEnd {
                        turn_id,
                        agent_id,
                        step,
                        model: ctx.runtime.llm.model_name(),
                        output: format!("ERROR: {}", e),
                        input_tokens: 0,
                        output_tokens: 0,
                        cache_creation_input_tokens: None,
                        cache_read_input_tokens: None,
                        request_id: None,
                    });
                    // TurnError：通知 TUI 显示错误 SystemNote（v2_bridge → AgentExecutionFailed → 红色消息）
                    // S1.3：LLM 内部自报 cancel（model_bridge.rs is_cancelled → Err(Interrupted)，
                    // 外层 biased select 的 cancel 分支未必抢先，微竞态可达）必须映射为
                    // Interrupted，不能吞成 LlmFailure（遥测分类错误）。
                    let reason = match &e {
                        AgentError::LlmHttpError { .. }
                        | AgentError::LlmError(..)
                        | AgentError::ModelError(..) => {
                            TurnErrorReason::LlmFailure
                        }
                        AgentError::Interrupted => TurnErrorReason::Interrupted,
                        _ => TurnErrorReason::LlmFailure,
                    };
                    ctx.runtime.event_bus.emit_observe(ObserveEvent::TurnError {
                        turn_id,
                        agent_id,
                        reason,
                        message: e.to_string(),
                    });
                    // 通过 middleware chain 触发 on_error
                    let _ = run_on_error(ctx, &e).await;
                    return Err(e);
                }
            }
        }
    };

    // emit LlmRequestPayload（仅发送 Model 的安全 observation body；复用本次
    // LLM 调用已构建的 request，见 generate_reasoning_with_observed_body）
    if let Some(body) = observed_body {
        ctx.runtime
            .event_bus
            .emit_observe(ObserveEvent::LlmRequestPayload {
                turn_id,
                agent_id,
                step,
                body: std::sync::Arc::new(body),
            });
    }

    // emit LlmCallEnd（带 usage 完整字段：input/output + cache_creation/cache_read + request_id）
    // [TRAP] cache_read_input_tokens 必须透传，否则 TUI 命中率始终 0%（v2 重做回归）
    let (in_tok, out_tok, cache_create, cache_read) = reasoning
        .usage
        .as_ref()
        .map(|u| {
            (
                u.input_tokens as u64,
                u.output_tokens as u64,
                u.cache_creation_input_tokens.map(u64::from),
                u.cache_read_input_tokens.map(u64::from),
            )
        })
        .unwrap_or((0, 0, None, None));
    // request_id 与 usage 来源独立（provider 可能不返回 usage 但返回 request_id），
    // 不得随 usage 的 unwrap_or 默认值一起丢弃
    let req_id = reasoning.request_id.clone();
    // output 改为结构化 JSON：包含 text、thinking、tool_calls、stop_reason
    // 与 v1 llm_step.rs:92-93 对齐：优先 final_answer，否则回退到 thought 作为 text
    let llm_output = {
        let text = reasoning
            .final_answer
            .clone()
            .unwrap_or_else(|| reasoning.thought.clone());
        // thinking 从 source_message 的 Reasoning block 提取，fallback 到 reasoning.thought
        let thinking = reasoning
            .source_message
            .as_ref()
            .and_then(|msg| {
                msg.content_blocks()
                    .iter()
                    .find_map(|b| b.as_reasoning().map(|s| s.to_string()))
            })
            .unwrap_or_else(|| reasoning.thought.clone());
        let output_value = serde_json::json!({
            "text": text,
            "thinking": thinking,
            "tool_calls": reasoning.tool_calls.iter().map(|tc| serde_json::json!({
                "id": tc.id,
                "name": tc.name,
                "input": tc.input,
            })).collect::<Vec<_>>(),
            "stop_reason": crate::agent::model_bridge::stop_reason_display(&reasoning.stop_reason),
        });
        serde_json::to_string(&output_value).unwrap_or_else(|_| {
            // fallback: 保留纯文本行为
            reasoning
                .final_answer
                .clone()
                .unwrap_or_else(|| reasoning.thought.clone())
        })
    };
    ctx.runtime
        .event_bus
        .emit_observe(ObserveEvent::LlmCallEnd {
            turn_id,
            agent_id,
            step,
            model: reasoning.model.clone(),
            output: llm_output,
            input_tokens: in_tok,
            output_tokens: out_tok,
            cache_creation_input_tokens: cache_create,
            cache_read_input_tokens: cache_read,
            request_id: req_id,
        });

    // 累积 token_tracker（P0 #2 修复：v2 路径下 token tracker 从未累积）
    if let Some(ref usage) = reasoning.usage {
        ctx.compact.token_tracker.write().accumulate(usage);
    }

    if ctx.session.turn.cancel_token.is_cancelled() {
        return Err(AgentError::Interrupted);
    }
    if let (Some(config), Some(budget)) = (&ctx.compact.compact_config, &ctx.compact.context_budget)
    {
        ctx.compact.budget_recovery.lock().observe_response(
            budget_probe,
            reasoning.usage.as_ref(),
            config,
            budget,
        )?;
    }

    // after_model middleware（hook_middleware / git_attribution 等在此）
    super::work_reason::commit_response(ctx, &mut reasoning, &catalog).await?;
    run_after_model(ctx, &reasoning).await?;

    Ok(ReasonOutput {
        reasoning,
        catalog,
        messages_snapshot,
    })
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "reason_test.rs"]
mod tests;
