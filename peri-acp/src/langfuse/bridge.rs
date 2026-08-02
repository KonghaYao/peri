//! 统一 Langfuse 事件路由层。
//!
//! 定义 [`UnifiedLangfuseEvent`] 枚举（v1 ExecutorEvent + v2 RenderEvent/ObserveEvent
//! 的并集）与 [`LangfuseBridge`] 结构体，提供单一 `process_event` 入口。
//!
//! 所有 Langfuse 追踪事件只需在一处映射到 `LangfuseTracer` 方法，
//! 消除 v1 `forward_langfuse_event` 和 v2 `forward_langfuse_{render,state,observe}`
//! 双轨处理器。

use std::sync::Arc;

use parking_lot::Mutex;
use peri_agent::agent::events::{
    CompactStrategy, CompactTrigger, ExecutorEvent, MiddlewareHook, Stage, StageStatus,
};
use peri_agent::agent::events_v2::{ObserveEvent, RenderEvent};
use peri_agent::messages::BaseMessage;
use peri_agent::tools::ToolDefinition;
use peri_model::TokenUsage;
use tracing;

use crate::langfuse::tracer::stages::StageHandle;
use crate::langfuse::tracer::LangfuseTracer;

// ── UnifiedLangfuseEvent ──────────────────────────────────────────────────────

/// 统一 Langfuse 追踪事件（v1 ExecutorEvent + v2 RenderEvent/ObserveEvent 的并集）。
///
/// 所有变体均为 Langfuse tracer 有明确映射的事件。无映射的事件（如 TurnStarted、
/// TurnEnded 等）不在此枚举中，其转换方法返回 `None`。
#[derive(Debug, Clone)]
pub enum UnifiedLangfuseEvent {
    /// LLM 调用开始
    LlmCallStart {
        step: usize,
        messages: Vec<BaseMessage>,
        tools: Vec<ToolDefinition>,
    },
    /// LLM 请求体
    LlmRequestPayload {
        step: usize,
        body: Arc<serde_json::Value>,
    },
    /// LLM 调用结束
    LlmCallEnd {
        step: usize,
        model: String,
        output: String,
        usage: Option<TokenUsage>,
        /// Provider 请求 ID（用于关联 provider 侧日志/遥测；None 表示 Provider 未返回）
        request_id: Option<String>,
    },
    /// LLM 重试中
    LlmRetrying {
        attempt: usize,
        max_attempts: usize,
        delay_ms: u64,
        error: String,
    },
    /// 文本块（流式最终回答）
    TextChunk { chunk: String },
    /// 工具调用开始
    ToolStart {
        tool_call_id: String,
        name: String,
        input: serde_json::Value,
    },
    /// 工具调用结束
    ToolEnd {
        tool_call_id: String,
        output: String,
        is_error: bool,
    },
    /// Compact 阶段开始（含真实策略和触发方式）
    CompactStarted {
        strategy: CompactStrategy,
        trigger: CompactTrigger,
    },
    /// Compact 阶段结束（成功或失败）
    CompactEnded {
        summary: String,
        files_count: usize,
        skills_count: usize,
        micro_cleared: usize,
        is_error: bool,
        error_message: String,
        estimated_tokens_saved: u64,
        estimated_tokens_before: u64,
        estimated_tokens_after: u64,
        cache_hit_rate_before: f64,
        full_escalation_reason: Option<String>,
        /// Compact 执行的语义结果（CompactOutcome 的 Display 表示）
        outcome: Option<String>,
    },
    /// 上下文窗口预算警告
    BudgetWarning {
        percentage: f64,
        used_tokens: u64,
        total_tokens: u64,
        threshold_label: String,
    },
    /// ReAct Stage 开始（v2 only）
    StageStarted { stage: Stage, turn_id: String },
    /// ReAct Stage 结束（v2 only）
    StageEnded { status: StageStatus },
    /// 消息队列排空（v2 only）
    MessageQueueDrained {
        prompt: usize,
        defer: usize,
        info: usize,
    },
    /// AI 推理内容块（v2 only）
    AiReasoningChunk { text: String },
    /// 会话开始（v1 only）
    SessionStarted { frozen_summary: serde_json::Value },
    /// 中间件开始（v1 only）
    MiddlewareStarted {
        mw_name: String,
        hook: MiddlewareHook,
    },
    /// 中间件结束（v1 only）
    MiddlewareEnded {
        mw_name: String,
        hook: MiddlewareHook,
        status: StageStatus,
        error: Option<String>,
    },
    /// Workflow 开始（v1 only）
    WorkflowStarted {
        workflow_id: String,
        plan_summary: String,
    },
    /// Workflow 结束（v1 only）
    WorkflowEnded {
        workflow_id: String,
        agents_spawned: usize,
        tool_calls: usize,
    },
    /// 子 Agent 启动（通过 v2→v1 mapper 到达，暂不追踪）
    SubagentStart {
        agent_id: String,
        task_id: Option<String>,
    },
    /// 子 Agent 停止（通过 v2→v1 mapper 到达，暂不追踪）
    SubagentStop {
        agent_id: String,
        task_id: Option<String>,
    },
}

impl UnifiedLangfuseEvent {
    /// 将 ExecutorEvent 转换为 UnifiedLangfuseEvent（v1 路径）。
    /// 无 Langfuse 映射的变体返回 `None`。
    pub fn from_executor_event(ev: ExecutorEvent) -> Option<Self> {
        match ev {
            ExecutorEvent::LlmCallStart {
                step,
                messages,
                tools,
            } => {
                let msgs: Vec<BaseMessage> = (*messages).clone();
                Some(UnifiedLangfuseEvent::LlmCallStart {
                    step,
                    messages: msgs,
                    tools,
                })
            }
            ExecutorEvent::LlmRequestPayload { step, body } => {
                Some(UnifiedLangfuseEvent::LlmRequestPayload { step, body })
            }
            ExecutorEvent::LlmCallEnd {
                step,
                model,
                output,
                usage,
                request_id,
                ..
            } => Some(UnifiedLangfuseEvent::LlmCallEnd {
                step,
                model,
                output,
                usage,
                request_id,
            }),
            ExecutorEvent::LlmRetrying {
                attempt,
                max_attempts,
                delay_ms,
                error,
            } => Some(UnifiedLangfuseEvent::LlmRetrying {
                attempt,
                max_attempts,
                delay_ms,
                error,
            }),
            ExecutorEvent::TextChunk { chunk, .. } => {
                Some(UnifiedLangfuseEvent::TextChunk { chunk })
            }
            ExecutorEvent::ToolStart {
                tool_call_id,
                name,
                input,
                ..
            } => Some(UnifiedLangfuseEvent::ToolStart {
                tool_call_id,
                name,
                input,
            }),
            ExecutorEvent::ToolEnd {
                tool_call_id,
                output,
                is_error,
                ..
            } => Some(UnifiedLangfuseEvent::ToolEnd {
                tool_call_id,
                output,
                is_error,
            }),
            ExecutorEvent::CompactStarted {
                strategy, trigger, ..
            } => Some(UnifiedLangfuseEvent::CompactStarted { strategy, trigger }),
            ExecutorEvent::CompactCompleted {
                summary,
                files,
                skills,
                micro_cleared,
                estimated_tokens_saved,
                estimated_tokens_before,
                estimated_tokens_after,
                cache_hit_rate_before,
                full_escalation_reason,
                outcome,
                ..
            } => Some(UnifiedLangfuseEvent::CompactEnded {
                summary,
                files_count: files.len(),
                skills_count: skills.len(),
                micro_cleared,
                is_error: false,
                error_message: String::new(),
                estimated_tokens_saved,
                estimated_tokens_before,
                estimated_tokens_after,
                cache_hit_rate_before,
                full_escalation_reason: full_escalation_reason.map(|r| format!("{:?}", r)),
                outcome: Some(format!("{:?}", outcome)),
            }),
            ExecutorEvent::CompactError { message } => Some(UnifiedLangfuseEvent::CompactEnded {
                summary: String::new(),
                files_count: 0,
                skills_count: 0,
                micro_cleared: 0,
                is_error: true,
                error_message: message,
                estimated_tokens_saved: 0,
                estimated_tokens_before: 0,
                estimated_tokens_after: 0,
                cache_hit_rate_before: 0.0,
                full_escalation_reason: None,
                outcome: None,
            }),
            ExecutorEvent::SessionStarted { frozen_summary, .. } => {
                Some(UnifiedLangfuseEvent::SessionStarted { frozen_summary })
            }
            ExecutorEvent::MiddlewareStarted { mw_name, hook, .. } => {
                Some(UnifiedLangfuseEvent::MiddlewareStarted { mw_name, hook })
            }
            ExecutorEvent::MiddlewareEnded {
                mw_name,
                hook,
                status,
                error,
                ..
            } => Some(UnifiedLangfuseEvent::MiddlewareEnded {
                mw_name,
                hook,
                status,
                error,
            }),
            ExecutorEvent::BudgetThresholdHit {
                threshold,
                current_pct,
                tokens_in,
                tokens_out,
                ..
            } => Some(UnifiedLangfuseEvent::BudgetWarning {
                percentage: current_pct,
                used_tokens: tokens_in,
                total_tokens: tokens_out,
                threshold_label: format!("{:?}", threshold),
            }),
            ExecutorEvent::WorkflowStarted {
                workflow_id,
                plan_summary,
                ..
            } => Some(UnifiedLangfuseEvent::WorkflowStarted {
                workflow_id,
                plan_summary,
            }),
            ExecutorEvent::WorkflowEnded {
                workflow_id,
                agents_spawned,
                tool_calls,
                ..
            } => Some(UnifiedLangfuseEvent::WorkflowEnded {
                workflow_id,
                agents_spawned,
                tool_calls,
            }),
            // 无 Langfuse 映射的事件
            ExecutorEvent::TurnStarted { .. }
            | ExecutorEvent::TurnEnded { .. }
            | ExecutorEvent::StateSnapshotMeta { .. }
            | ExecutorEvent::SubagentStarted { .. }
            | ExecutorEvent::SubagentStopped { .. }
            | ExecutorEvent::BackgroundTaskCompleted(_)
            | ExecutorEvent::MessageAdded(_)
            | ExecutorEvent::StateSnapshot(_)
            | ExecutorEvent::TurnCommitted { .. }
            | ExecutorEvent::AiReasoning { .. }
            | ExecutorEvent::ContextWarning { .. }
            | ExecutorEvent::RewindCompleted { .. }
            | ExecutorEvent::TodoUpdate(_)
            | ExecutorEvent::LspDiagnostics { .. }
            | ExecutorEvent::BgToolStep { .. }
            | ExecutorEvent::WorkflowProgress(_)
            | ExecutorEvent::AgentExecutionFailed { .. }
            | ExecutorEvent::RewindError { .. } => None,
        }
    }

    /// 将 RenderEvent 转换为 UnifiedLangfuseEvent（v2 render 路径）。
    /// 无 Langfuse 映射的变体返回 `None`。
    pub fn from_render_event(ev: RenderEvent) -> Option<Self> {
        match ev {
            RenderEvent::TextChunk { chunk, .. } => Some(UnifiedLangfuseEvent::TextChunk { chunk }),
            RenderEvent::BudgetWarning {
                percentage,
                used_tokens,
                total_tokens,
                ..
            } => Some(UnifiedLangfuseEvent::BudgetWarning {
                percentage,
                used_tokens,
                total_tokens,
                threshold_label: "context_window".to_string(),
            }),
            RenderEvent::ToolStarted {
                tool_call_id,
                name,
                input,
                ..
            } => Some(UnifiedLangfuseEvent::ToolStart {
                tool_call_id,
                name,
                input,
            }),
            RenderEvent::ToolEnded {
                tool_call_id,
                output,
                is_error,
                ..
            } => Some(UnifiedLangfuseEvent::ToolEnd {
                tool_call_id,
                output,
                is_error,
            }),
            // 其余 RenderEvent 变体无 Langfuse 映射
            _ => None,
        }
    }

    /// 将 ObserveEvent 转换为 UnifiedLangfuseEvent（v2 observe 路径）。
    /// 无 Langfuse 映射的变体返回 `None`。
    pub fn from_observe_event(ev: ObserveEvent) -> Option<Self> {
        match ev {
            ObserveEvent::LlmCallStart {
                step,
                messages,
                tools,
                ..
            } => {
                let msgs: Vec<BaseMessage> = (*messages).clone();
                Some(UnifiedLangfuseEvent::LlmCallStart {
                    step,
                    messages: msgs,
                    tools,
                })
            }
            ObserveEvent::LlmCallEnd {
                step,
                model,
                output,
                input_tokens,
                output_tokens,
                cache_creation_input_tokens,
                cache_read_input_tokens,
                request_id,
                ..
            } => {
                let usage = TokenUsage {
                    input_tokens: input_tokens as u32,
                    output_tokens: output_tokens as u32,
                    cache_creation_input_tokens: if cache_creation_input_tokens > 0 {
                        Some(cache_creation_input_tokens as u32)
                    } else {
                        None
                    },
                    cache_read_input_tokens: if cache_read_input_tokens > 0 {
                        Some(cache_read_input_tokens as u32)
                    } else {
                        None
                    },
                };
                Some(UnifiedLangfuseEvent::LlmCallEnd {
                    step,
                    model,
                    output,
                    usage: Some(usage),
                    request_id,
                })
            }
            ObserveEvent::LlmRequestPayload { step, body, .. } => {
                Some(UnifiedLangfuseEvent::LlmRequestPayload { step, body })
            }
            ObserveEvent::CompactStarted { strategy, .. } => {
                Some(UnifiedLangfuseEvent::CompactStarted {
                    strategy,
                    trigger: CompactTrigger::Auto, // v2 自动触发
                })
            }
            ObserveEvent::MessagesCompacted {
                summary,
                files,
                skills,
                estimated_tokens_saved,
                estimated_tokens_before,
                estimated_tokens_after,
                cache_hit_rate_before,
                full_escalation_reason,
                outcome,
                ..
            } => Some(UnifiedLangfuseEvent::CompactEnded {
                summary,
                files_count: files.len(),
                skills_count: skills.len(),
                micro_cleared: 0, // v2 无此字段
                is_error: false,
                error_message: String::new(),
                estimated_tokens_saved,
                estimated_tokens_before,
                estimated_tokens_after,
                cache_hit_rate_before,
                full_escalation_reason: full_escalation_reason.map(|r| format!("{:?}", r)),
                outcome: Some(format!("{:?}", outcome)),
            }),
            ObserveEvent::StageStarted { stage, turn_id, .. } => {
                Some(UnifiedLangfuseEvent::StageStarted {
                    stage,
                    turn_id: turn_id.to_string(),
                })
            }
            ObserveEvent::StageEnded { status, .. } => {
                Some(UnifiedLangfuseEvent::StageEnded { status })
            }
            ObserveEvent::MessageQueueDrained {
                prompt,
                defer,
                info,
                ..
            } => Some(UnifiedLangfuseEvent::MessageQueueDrained {
                prompt,
                defer,
                info,
            }),
            ObserveEvent::AiReasoningChunk { text, .. } => {
                Some(UnifiedLangfuseEvent::AiReasoningChunk { text })
            }
            // 无 Langfuse 映射的事件
            ObserveEvent::TurnError { .. }
            | ObserveEvent::SubagentStart { .. }
            | ObserveEvent::SubagentStop { .. } => None,
        }
    }
}

// ── LangfuseBridge ────────────────────────────────────────────────────────────

/// 统一 Langfuse 事件桥接器。
///
/// 持有 `LangfuseTracer` 的共享引用，提供 `process_event` 单一入口。
/// `active_stage` 由桥接器内部管理（`parking_lot::Mutex<Option<StageHandle>>`），
/// 调用方无需关心 Stage 生命周期。
#[derive(Clone)]
pub struct LangfuseBridge {
    tracer: Arc<Mutex<LangfuseTracer>>,
    provider_display_name: String,
    /// 活跃的 Stage Span 句柄（StageStarted→StageEnded 间持有）。
    /// 仅在 spawn_eventbus_forwarder 或 SubAgent forwarder 的 render/observe 分支中使用。
    active_stage: Arc<Mutex<Option<StageHandle>>>,
}

impl LangfuseBridge {
    /// 构造新桥接器。
    pub fn new(tracer: Arc<Mutex<LangfuseTracer>>, provider_display_name: String) -> Self {
        Self {
            tracer,
            provider_display_name,
            active_stage: Arc::new(Mutex::new(None)),
        }
    }

    /// 处理统一 Langfuse 事件，转发到 `LangfuseTracer`。
    ///
    /// `active_stage` 用于 StageStarted/StageEnded 间的 `StageHandle` 传递。
    /// 仅 `spawn_eventbus_forwarder` 传入真实可变引用；其他调用方传入 `&mut None`。
    pub(crate) fn process_event(
        &self,
        event: &UnifiedLangfuseEvent,
        active_stage: &mut Option<StageHandle>,
    ) {
        let mut t = self.tracer.lock();
        match event {
            UnifiedLangfuseEvent::LlmCallStart {
                step,
                messages,
                tools,
            } => {
                t.on_llm_start(*step, messages, tools);
            }
            UnifiedLangfuseEvent::LlmRequestPayload { step, body } => {
                t.on_llm_request_payload(*step, Arc::clone(body));
            }
            UnifiedLangfuseEvent::LlmCallEnd {
                step,
                model,
                output,
                usage,
                request_id,
            } => {
                t.on_llm_end(
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
                t.on_llm_retrying(*attempt, *max_attempts, *delay_ms, error);
            }
            UnifiedLangfuseEvent::TextChunk { chunk } => {
                t.on_text_chunk(chunk);
            }
            UnifiedLangfuseEvent::ToolStart {
                tool_call_id,
                name,
                input,
            } => {
                t.on_tool_start(tool_call_id, name, input);
            }
            UnifiedLangfuseEvent::ToolEnd {
                tool_call_id,
                output,
                is_error,
            } => {
                t.on_tool_end(tool_call_id, output, *is_error);
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
                t.on_compact_end(
                    summary,
                    *files_count,
                    *skills_count,
                    *micro_cleared,
                    *is_error,
                    error_message,
                );
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
            UnifiedLangfuseEvent::StageStarted { stage, turn_id } => {
                // 需要先释放 MutexGuard 再获取 trace_id/agent_observation_id。
                // SubAgent 活跃时，stage span 的父节点使用 SubAgent 的 observation_id；
                // 否则回退到主 Agent 的 observation_id。
                drop(t);
                let (trace_id, agent_observation_id) = {
                    let t2 = self.tracer.lock();
                    let obs_id = t2.subagent.current_agent_id(&t2.agent_observation_id);
                    (t2.trace_id.clone(), obs_id)
                };
                let mut t2 = self.tracer.lock();
                // SubAgent 栈非空时标记栈顶已启动。对齐 on_stage_start() 中 mark_top_started 行为，
                // 确保 fork subagent 的 on_tool_end("Agent") 能正确走 fork 清理路径（flush + emit ObservationCreate）。
                // 桥路径绕过了 tracer.on_stage_start()，所以必须在此处补调 mark_top_started。
                t2.subagent.mark_top_started();
                let handle = t2.stages.on_stage_start(
                    *stage,
                    &trace_id,
                    &turn_id.to_string(),
                    &agent_observation_id,
                );
                *active_stage = Some(handle);
            }
            UnifiedLangfuseEvent::StageEnded { status } => {
                if let Some(handle) = active_stage.take() {
                    t.on_stage_end(&handle, *status);
                }
            }
            UnifiedLangfuseEvent::MessageQueueDrained {
                prompt,
                defer,
                info,
            } => {
                t.on_mq_drained(*prompt, *defer, *info);
            }
            UnifiedLangfuseEvent::AiReasoningChunk { text } => {
                t.on_ai_reasoning_chunk(text);
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
                // 先释放 MutexGuard，查询活跃 middleware span
                drop(t);
                let span_id = {
                    let t2 = self.tracer.lock();
                    t2.middleware.find_active(mw_name, *hook)
                };
                if let Some(span_id) = span_id {
                    let handle = crate::langfuse::tracer::middleware::MiddlewareSpanHandle {
                        span_id,
                        name: mw_name.clone(),
                        hook: *hook,
                    };
                    self.tracer
                        .lock()
                        .on_middleware_end(&handle, *status, error.clone());
                } else {
                    tracing::warn!(
                        target: "langfuse::forward",
                        %mw_name,
                        ?hook,
                        "MiddlewareEnded without active middleware span, skipping"
                    );
                }
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
            // SubagentStart/SubagentStop 暂无 Langfuse 映射，静默跳过
            UnifiedLangfuseEvent::SubagentStart { .. }
            | UnifiedLangfuseEvent::SubagentStop { .. } => {}
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
