//! 统一 Langfuse 事件路由层。
//!
//! 定义 [`UnifiedLangfuseEvent`] 枚举（v1 ExecutorEvent + v2 RenderEvent/ObserveEvent
//! 的并集）与 [`LangfuseBridge`] 结构体，提供单一 `process_event` 入口。
//!
//! 所有 Langfuse 追踪事件只需在一处映射到 `LangfuseTracer` 方法，
//! 消除 v1 `forward_langfuse_event` 和 v2 `forward_langfuse_{render,state,observe}`
//! 双轨处理器。

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use peri_agent::agent::events_v2::{ObserveEvent, RenderEvent};
use tracing;

use crate::langfuse::tracer::stages::{StageHandle, MAIN_AGENT_KEY};
use crate::langfuse::tracer::LangfuseTracer;

mod lifecycle;
mod unified_event;
mod v1_conversion;
mod v2_conversion;
pub use unified_event::UnifiedLangfuseEvent;

// ── LangfuseBridge ────────────────────────────────────────────────────────────

/// 统一 Langfuse 事件桥接器。
///
/// 持有 `LangfuseTracer` 的共享引用，提供 `process_event` 单一入口。
/// `active_stage` 由桥接器内部管理（`parking_lot::Mutex<HashMap<String, StageHandle>>`，
/// key = 事件 agent_id），调用方无需关心 Stage 生命周期。
#[derive(Clone)]
pub struct LangfuseBridge {
    tracer: Arc<Mutex<LangfuseTracer>>,
    provider_display_name: String,
    /// 各 agent 活跃的 Stage Span 句柄（StageStarted→StageEnded 间持有）。
    /// 按 agent_id 隔离：并行 subagent 的 stage 事件交错到达时互不覆盖，
    /// StageEnded 精确配对到发起 agent 的 handle。
    /// 仅在 spawn_eventbus_forwarder 或 SubAgent forwarder 的 render/observe 分支中使用。
    active_stage: Arc<Mutex<HashMap<String, StageHandle>>>,
    /// 各 agent 最近一次 LlmCallStart 的 step（key = agent_id）。
    /// v1 `ExecutorEvent::LlmRetrying` 不携带 agent_id/step，而 v1 路径的 LLM
    /// 事件固定归属 MAIN_AGENT_KEY（见 from_executor_event），故 retry 查询
    /// 主 agent 自己的 step 记录；v2 ObserveEvent 路径无 LlmRetrying 变体，
    /// subagent 的 start 记录在其自身 key 下，不会覆盖主 agent 的 step。
    llm_start_steps: Arc<Mutex<HashMap<String, usize>>>,
    /// 旁路事件计数与活跃集合；不决定观测归属或生命周期。
    subagent_telemetry: Arc<Mutex<lifecycle::SubagentTelemetry>>,
}

impl LangfuseBridge {
    /// 构造新桥接器。
    ///
    /// `main_agent_id`:主 v2 session 的事件侧 AgentId(Some 时注入 tracer registry,
    /// 用于区分"主 agent 事件"与"未知 subagent 事件")。bridge2(SubAgent forwarder)
    /// 与 workflow 路径不需要主 agent 身份,传 None(registry 按"非注册成员即主"
    /// fallback,兼容旧测试,见 tracer registry 注释)。
    pub fn new(
        tracer: Arc<Mutex<LangfuseTracer>>,
        provider_display_name: String,
        main_agent_id: Option<String>,
    ) -> Self {
        if let Some(ref id) = main_agent_id {
            tracer.lock().set_main_agent_id(id.clone());
        }
        Self {
            tracer,
            provider_display_name,
            active_stage: Arc::new(Mutex::new(HashMap::new())),
            llm_start_steps: Arc::new(Mutex::new(HashMap::new())),
            subagent_telemetry: Arc::new(Mutex::new(lifecycle::SubagentTelemetry::default())),
        }
    }

    /// 当前 bridge 收到 Start 但尚未收到 Stop 的数量（仅诊断）
    #[cfg(test)]
    pub(crate) fn active_subagent_count(&self) -> usize {
        self.subagent_telemetry.lock().active_count()
    }

    /// 当前 bridge 收到的生命周期事件计数（仅诊断）
    #[cfg(test)]
    pub(crate) fn subagent_event_counts(&self) -> (u64, u64) {
        self.subagent_telemetry.lock().event_counts()
    }

    /// 处理统一 Langfuse 事件，转发到 `LangfuseTracer`。
    ///
    /// `active_stage` 用于 StageStarted/StageEnded 间的 `StageHandle` 传递。
    /// trait 入口使用 bridge 内部的表；直接调用方也可维护自己的表。
    pub fn process_event(
        &self,
        event: &UnifiedLangfuseEvent,
        active_stage: &mut HashMap<String, StageHandle>,
    ) {
        let mut t = self.tracer.lock();
        match event {
            UnifiedLangfuseEvent::LlmCallStart {
                agent_id,
                step,
                messages,
                tools,
            } => {
                self.llm_start_steps.lock().insert(agent_id.clone(), *step);
                t.on_llm_start(agent_id, *step, messages, tools);
            }
            UnifiedLangfuseEvent::LlmRequestPayload {
                agent_id,
                step,
                body,
                ..
            } => {
                t.on_llm_request_payload(agent_id, *step, Arc::clone(body));
            }
            UnifiedLangfuseEvent::LlmCallEnd {
                agent_id,
                step,
                model,
                output,
                usage,
                request_id,
            } => {
                t.on_llm_end(
                    agent_id,
                    *step,
                    model,
                    &self.provider_display_name,
                    output,
                    usage.as_ref(),
                    request_id.as_deref(),
                );
            }
            UnifiedLangfuseEvent::LlmRetrying {
                attempt,
                max_attempts,
                delay_ms,
                error,
            } => {
                // v1 retry 事件无 agent_id/step：LLM 事件在 v1 路径固定归
                // MAIN_AGENT_KEY，step 取该 agent 最近一次 LlmCallStart 的记录。
                let step = self
                    .llm_start_steps
                    .lock()
                    .get(MAIN_AGENT_KEY)
                    .copied()
                    .unwrap_or(0);
                t.on_llm_retrying(
                    MAIN_AGENT_KEY,
                    step,
                    *attempt,
                    *max_attempts,
                    *delay_ms,
                    error,
                );
            }
            UnifiedLangfuseEvent::TextChunk { chunk } => {
                t.on_text_chunk(chunk);
            }
            UnifiedLangfuseEvent::ToolStart {
                agent_id,
                tool_call_id,
                name,
                input,
            } => {
                t.on_tool_start(agent_id, tool_call_id, name, input);
            }
            UnifiedLangfuseEvent::ToolEnd {
                agent_id,
                tool_call_id,
                output,
                is_error,
            } => {
                t.on_tool_end(agent_id, tool_call_id, output, *is_error);
            }
            UnifiedLangfuseEvent::CompactStarted { strategy, trigger } => {
                t.on_compact_start(*strategy, *trigger);
            }
            UnifiedLangfuseEvent::CompactEnded {
                summary,
                files_count,
                skills_count,
                micro_cleared,
                is_error,
                error_message,
                estimated_tokens_saved,
                estimated_tokens_before,
                estimated_tokens_after,
                cache_hit_rate_before,
                full_escalation_reason,
                outcome,
            } => {
                // 协议化 CompactCompleted 现携带真实 files/skills/affected/token-saved
                // 计数；estimated_tokens_before/after 等未透传字段仍以 0 表示未知。
                tracing::info!(
                    estimated_tokens_saved,
                    estimated_tokens_before,
                    estimated_tokens_after,
                    cache_hit_rate_before,
                    full_escalation_reason = ?full_escalation_reason,
                    outcome = ?outcome,
                    files_count,
                    skills_count,
                    "CompactCompleted"
                );
                t.on_compact_end(crate::langfuse::tracer::compact::CompactEndInfo {
                    summary: summary.clone(),
                    files_count: *files_count,
                    skills_count: *skills_count,
                    micro_cleared: *micro_cleared,
                    is_error: *is_error,
                    error_message: error_message.clone(),
                    estimated_tokens_saved: *estimated_tokens_saved,
                    estimated_tokens_before: *estimated_tokens_before,
                    estimated_tokens_after: *estimated_tokens_after,
                    cache_hit_rate_before: *cache_hit_rate_before,
                    full_escalation_reason: full_escalation_reason.clone(),
                    outcome: outcome.clone(),
                });
            }
            UnifiedLangfuseEvent::BudgetWarning {
                percentage,
                used_tokens,
                total_tokens,
                threshold_label,
            } => {
                t.on_budget_threshold_hit(
                    threshold_label,
                    *percentage,
                    *used_tokens,
                    *total_tokens,
                );
            }
            UnifiedLangfuseEvent::StageStarted {
                agent_id,
                stage,
                turn_id,
            } => {
                drop(t);
                self.start_stage(agent_id, *stage, turn_id, active_stage);
            }
            UnifiedLangfuseEvent::StageEnded { agent_id, status } => {
                lifecycle::finish_stage(&mut t, agent_id, *status, active_stage);
            }
            UnifiedLangfuseEvent::MessageQueueDrained {
                agent_id,
                prompt,
                defer,
                info,
            } => {
                t.on_mq_drained(agent_id, *prompt, *defer, *info);
            }
            UnifiedLangfuseEvent::AiReasoningChunk { text } => {
                t.on_ai_reasoning_chunk(text);
            }
            UnifiedLangfuseEvent::TurnError { reason } => {
                t.on_turn_error(*reason);
            }
            UnifiedLangfuseEvent::SessionStarted { frozen_summary } => {
                t.on_session_start(frozen_summary);
            }
            UnifiedLangfuseEvent::MiddlewareStarted { mw_name, hook } => {
                t.on_middleware_start(mw_name, *hook);
            }
            UnifiedLangfuseEvent::MiddlewareEnded {
                mw_name,
                hook,
                status,
                error,
            } => {
                drop(t);
                self.finish_middleware(mw_name, *hook, *status, error);
            }
            UnifiedLangfuseEvent::WorkflowStarted {
                workflow_id,
                plan_summary,
            } => {
                t.on_workflow_start(workflow_id, plan_summary);
            }
            UnifiedLangfuseEvent::WorkflowEnded {
                workflow_id,
                agents_spawned,
                tool_calls,
            } => {
                t.on_workflow_end(workflow_id, *agents_spawned, *tool_calls);
            }
            // bridge 只统计事件；观测归属与生命周期由 tracer registry 裁决。
            UnifiedLangfuseEvent::SubagentStart {
                parent_agent_id,
                child_agent_id,
                agent_name,
                is_background,
            } => {
                self.subagent_telemetry.lock().on_start(
                    parent_agent_id,
                    child_agent_id,
                    agent_name,
                    *is_background,
                );
                // tracer registry:AGENT obs 创建(join 成功后) + gate 重放
                t.on_subagent_start(parent_agent_id, child_agent_id, agent_name, *is_background);
            }
            UnifiedLangfuseEvent::SubagentStop {
                parent_agent_id,
                child_agent_id,
                agent_name,
                result,
                is_error,
            } => {
                self.subagent_telemetry.lock().on_stop(
                    parent_agent_id,
                    child_agent_id,
                    agent_name,
                    result,
                    *is_error,
                );
                // tracer registry:AGENT obs 关闭(两信号齐备时)
                t.on_subagent_stop(parent_agent_id, child_agent_id, result, *is_error);
            }
        }
    }
}

// ── LangfuseBridge impl LangfuseBridgeLike ───────────────────────────────────

impl peri_agent::agent::LangfuseBridgeLike for LangfuseBridge {
    fn process_render_event(&self, ev: &RenderEvent) {
        if let Some(u) = UnifiedLangfuseEvent::from_render_event(ev.clone()) {
            let mut guard = self.active_stage.lock();
            self.process_event(&u, &mut guard);
        }
    }

    fn process_observe_event(&self, ev: &ObserveEvent) {
        if let Some(u) = UnifiedLangfuseEvent::from_observe_event(ev.clone()) {
            let mut guard = self.active_stage.lock();
            self.process_event(&u, &mut guard);
        }
    }
}

// ── C4 最小接入测试 ───────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "bridge/lifecycle_test.rs"]
mod tests;

#[cfg(test)]
#[path = "bridge_test.rs"]
mod bridge_test;
