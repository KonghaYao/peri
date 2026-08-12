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
use peri_agent::agent::events::{
    CompactStrategy, CompactTrigger, ExecutorEvent, MiddlewareHook, Stage, StageStatus,
};
use peri_agent::agent::events_v2::{ObserveEvent, RenderEvent, TurnErrorReason};
use peri_agent::messages::BaseMessage;
use peri_agent::tools::ToolDefinition;
use peri_model::TokenUsage;
use tracing;

use crate::langfuse::tracer::stages::{StageHandle, MAIN_AGENT_KEY};
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
        /// 事件来源 agent（主 agent 或 subagent 的 AgentId 字符串）。
        /// 并行 subagent 场景下用于隔离 generation 缓存与 stage parent 归属。
        agent_id: String,
        step: usize,
        messages: Vec<BaseMessage>,
        tools: Vec<ToolDefinition>,
    },
    /// LLM 请求体
    LlmRequestPayload {
        agent_id: String,
        step: usize,
        body: Arc<serde_json::Value>,
    },
    /// LLM 调用结束
    LlmCallEnd {
        agent_id: String,
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
        /// 事件来源 agent（主 agent / subagent 的 AgentId 字符串）。
        /// 用于将 tool-batch 父节点定位到该 agent 自己的活跃 stage span。
        agent_id: String,
        tool_call_id: String,
        name: String,
        input: serde_json::Value,
    },
    /// 工具调用结束
    ToolEnd {
        /// 事件来源 agent（与 ToolStart 对齐，暂用于日志/后续路由）。
        agent_id: String,
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
    StageStarted {
        agent_id: String,
        stage: Stage,
        turn_id: String,
    },
    /// ReAct Stage 结束（v2 only）
    StageEnded {
        agent_id: String,
        status: StageStatus,
    },
    /// 消息队列排空（v2 only）
    MessageQueueDrained {
        agent_id: String,
        prompt: usize,
        defer: usize,
        info: usize,
    },
    /// AI 推理内容块（v2 only）
    AiReasoningChunk { text: String },
    /// Turn 错误（v2 only）：仅传递稳定的分类，绝不进入原始错误正文。
    TurnError { reason: TurnErrorReason },
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
    /// 子 Agent 启动（v2 ObserveEvent::SubagentStart 直达；v1 直发事件不映射）。
    /// C4 最小接入：仅注册/日志/计数，归属逻辑由阶段② tracer registry 接管。
    SubagentStart {
        parent_agent_id: String,
        child_agent_id: String,
        agent_name: String,
        is_background: bool,
    },
    /// 子 Agent 停止（v2 ObserveEvent::SubagentStop 直达）
    SubagentStop {
        parent_agent_id: String,
        child_agent_id: String,
        agent_name: String,
        result: String,
        is_error: bool,
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
                    // v1 ExecutorEvent 无 agent_id（v2 ObserveEvent 才携带）：
                    // workflow agent 事件固定归属主 agent slot。
                    agent_id: MAIN_AGENT_KEY.to_string(),
                    step,
                    messages: msgs,
                    tools,
                })
            }
            ExecutorEvent::LlmRequestPayload { step, body } => {
                Some(UnifiedLangfuseEvent::LlmRequestPayload {
                    agent_id: MAIN_AGENT_KEY.to_string(),
                    step,
                    body,
                })
            }
            ExecutorEvent::LlmCallEnd {
                step,
                model,
                output,
                usage,
                request_id,
                ..
            } => Some(UnifiedLangfuseEvent::LlmCallEnd {
                agent_id: MAIN_AGENT_KEY.to_string(),
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
                source_agent_id,
                ..
            } => Some(UnifiedLangfuseEvent::ToolStart {
                // v1 事件若无 source_agent_id 则归属主 agent slot
                agent_id: source_agent_id.unwrap_or_else(|| MAIN_AGENT_KEY.to_string()),
                tool_call_id,
                name,
                input,
            }),
            ExecutorEvent::ToolEnd {
                tool_call_id,
                output,
                is_error,
                source_agent_id,
                ..
            } => Some(UnifiedLangfuseEvent::ToolEnd {
                agent_id: source_agent_id.unwrap_or_else(|| MAIN_AGENT_KEY.to_string()),
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
            | ExecutorEvent::RewindError { .. }
            | ExecutorEvent::TurnSuspended { .. }
            | ExecutorEvent::SystemNotification { .. }
            | ExecutorEvent::OauthNeeded { .. }
            | ExecutorEvent::OauthCompleted { .. }
            | ExecutorEvent::OauthFailed { .. }
            | ExecutorEvent::BgRegistryEvent(_) => None,
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
                agent_id,
                tool_call_id,
                name,
                input,
                ..
            } => Some(UnifiedLangfuseEvent::ToolStart {
                agent_id: agent_id.to_string(),
                tool_call_id,
                name,
                input,
            }),
            RenderEvent::ToolEnded {
                agent_id,
                tool_call_id,
                output,
                is_error,
                ..
            } => Some(UnifiedLangfuseEvent::ToolEnd {
                agent_id: agent_id.to_string(),
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
                agent_id,
                step,
                messages,
                tools,
                ..
            } => {
                let msgs: Vec<BaseMessage> = (*messages).clone();
                Some(UnifiedLangfuseEvent::LlmCallStart {
                    agent_id: agent_id.to_string(),
                    step,
                    messages: msgs,
                    tools,
                })
            }
            ObserveEvent::LlmCallEnd {
                agent_id,
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
                    agent_id: agent_id.to_string(),
                    step,
                    model,
                    output,
                    usage: Some(usage),
                    request_id,
                })
            }
            ObserveEvent::LlmRequestPayload {
                agent_id,
                step,
                body,
                ..
            } => Some(UnifiedLangfuseEvent::LlmRequestPayload {
                agent_id: agent_id.to_string(),
                step,
                body,
            }),
            ObserveEvent::CompactStarted { strategy, .. } => {
                Some(UnifiedLangfuseEvent::CompactStarted {
                    strategy,
                    trigger: CompactTrigger::Auto, // v2 自动触发
                })
            }
            // S1.4：cancel 且未提交变更的 CompactEnded → 闭合 compact span。
            // 不携带 token 估算（无变更发生）；outcome 字段区分
            // Interrupted（取消未提交）与 MessagesCompacted 路径。
            ObserveEvent::CompactEnded { outcome, .. } => {
                Some(UnifiedLangfuseEvent::CompactEnded {
                    summary: String::new(),
                    files_count: 0,
                    skills_count: 0,
                    micro_cleared: 0,
                    is_error: false,
                    error_message: String::new(),
                    estimated_tokens_saved: 0,
                    estimated_tokens_before: 0,
                    estimated_tokens_after: 0,
                    cache_hit_rate_before: 0.0,
                    full_escalation_reason: None,
                    outcome: Some(format!("{:?}", outcome)),
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
            ObserveEvent::StageStarted {
                agent_id,
                stage,
                turn_id,
                ..
            } => Some(UnifiedLangfuseEvent::StageStarted {
                agent_id: agent_id.to_string(),
                stage,
                turn_id: turn_id.to_string(),
            }),
            ObserveEvent::StageEnded {
                agent_id, status, ..
            } => Some(UnifiedLangfuseEvent::StageEnded {
                agent_id: agent_id.to_string(),
                status,
            }),
            ObserveEvent::MessageQueueDrained {
                agent_id,
                prompt,
                defer,
                info,
                ..
            } => Some(UnifiedLangfuseEvent::MessageQueueDrained {
                agent_id: agent_id.to_string(),
                prompt,
                defer,
                info,
            }),
            ObserveEvent::AiReasoningChunk { text, .. } => {
                Some(UnifiedLangfuseEvent::AiReasoningChunk { text })
            }
            ObserveEvent::TurnError { reason, .. } => {
                Some(UnifiedLangfuseEvent::TurnError { reason })
            }
            // v2 SubagentStart/Stop → Unified（C4）：子 agent 生命周期事件直达
            ObserveEvent::SubagentStart {
                agent_id,
                child_agent_id,
                agent_name,
                is_background,
                ..
            } => Some(UnifiedLangfuseEvent::SubagentStart {
                parent_agent_id: agent_id.to_string(),
                child_agent_id: child_agent_id.to_string(),
                agent_name,
                is_background,
            }),
            ObserveEvent::SubagentStop {
                agent_id,
                child_agent_id,
                agent_name,
                result,
                is_error,
                ..
            } => Some(UnifiedLangfuseEvent::SubagentStop {
                parent_agent_id: agent_id.to_string(),
                child_agent_id: child_agent_id.to_string(),
                agent_name,
                result,
                is_error,
            }),
        }
    }
}

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
    /// 活跃 subagent 注册表（C4 最小接入）：child_agent_id → 生命周期信息。
    /// Start 注册 / Stop 注销，仅验证事件到达与字段完整 + 计数；
    /// 归属逻辑在阶段②由 tracer registry 接管（此处不影响任何归属决策）。
    subagent_registry: Arc<Mutex<HashMap<String, SubagentLifecycle>>>,
    /// subagent 生命周期事件计数（C4 指标，供阶段②对照）：(start, stop)
    subagent_counters: Arc<Mutex<(u64, u64)>>,
}

/// C4 最小注册条目：SubagentStart 到达时记录的字段快照。
/// 阶段①只验证事件到达与字段完整（写入+注销）；字段读取在阶段②
/// （tracer registry 归属）接管，故暂允许 dead_code。
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct SubagentLifecycle {
    pub parent_agent_id: String,
    pub agent_name: String,
    pub is_background: bool,
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
            subagent_registry: Arc::new(Mutex::new(HashMap::new())),
            subagent_counters: Arc::new(Mutex::new((0, 0))),
        }
    }

    /// 当前活跃 subagent 注册数量（C4 指标，供测试/阶段②对照）
    #[cfg(test)]
    pub(crate) fn active_subagent_count(&self) -> usize {
        self.subagent_registry.lock().len()
    }

    /// 生命周期事件计数（C4 指标，供测试/阶段②对照）
    #[cfg(test)]
    pub(crate) fn subagent_event_counts(&self) -> (u64, u64) {
        *self.subagent_counters.lock()
    }

    /// 处理统一 Langfuse 事件，转发到 `LangfuseTracer`。
    ///
    /// `active_stage` 用于 StageStarted/StageEnded 间的 `StageHandle` 传递。
    /// 仅 `spawn_eventbus_forwarder` 传入真实可变引用；其他调用方传入 `&mut None`。
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
            UnifiedLangfuseEvent::StageStarted {
                agent_id,
                stage,
                turn_id,
            } => {
                // 先释放 MutexGuard 再获取 trace_id/agent_observation_id。
                // 归属按事件 agent_id 查 tracer registry(替代旧栈顶近似):
                // ① 命中 by_agent_id → parent = 该 child 的 AGENT obs id;
                // ② main_agent_id 匹配(或未注入时非 registry 成员)→ 主 agent obs;
                // ③ 其余 → 注册闸门缓存(等 Start join 重放)或跳过(incomplete),
                //    绝不 fallback 主 agent。
                drop(t);
                let handle = {
                    let mut t2 = self.tracer.lock();
                    t2.on_stage_start_gated(agent_id, *stage, &turn_id.to_string())
                };
                if let Some(handle) = handle {
                    active_stage.insert(agent_id.clone(), handle);
                }
            }
            UnifiedLangfuseEvent::StageEnded { agent_id, status } => {
                // 按 agent_id 精确配对：只结束该 agent 自己的 handle，
                // 其他并行 subagent 的活跃 stage 不受影响。
                if let Some(handle) = active_stage.remove(agent_id) {
                    t.on_stage_end(agent_id, &handle, *status);
                } else {
                    // 乱序场景:StageStarted 被注册闸门缓存后重放,handle 在 tracer 侧
                    if let Some(handle) = t.take_replayed_stage_handle(agent_id) {
                        t.on_stage_end(agent_id, &handle, *status);
                    } else {
                        tracing::warn!(
                            target: "langfuse::forward",
                            %agent_id,
                            "StageEnded 无匹配的活跃 stage handle（可能事件乱序或已结束），跳过"
                        );
                    }
                }
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
            // SubagentStart/Stop:bridge 层保留 C4 注册/注销 + 日志 + 计数(指标),
            // 归属/生命周期由 tracer registry 接管(AGENT obs 创建/关闭)。
            UnifiedLangfuseEvent::SubagentStart {
                parent_agent_id,
                child_agent_id,
                agent_name,
                is_background,
            } => {
                let mut reg = self.subagent_registry.lock();
                if reg.contains_key(child_agent_id) {
                    tracing::warn!(
                        target: "langfuse::subagent",
                        %child_agent_id,
                        "SubagentStart 重复（child_agent_id 已有活跃记录），覆盖注册"
                    );
                }
                reg.insert(
                    child_agent_id.clone(),
                    SubagentLifecycle {
                        parent_agent_id: parent_agent_id.clone(),
                        agent_name: agent_name.clone(),
                        is_background: *is_background,
                    },
                );
                self.subagent_counters.lock().0 += 1;
                tracing::info!(
                    target: "langfuse::subagent",
                    event = "subagent_start",
                    %parent_agent_id,
                    %child_agent_id,
                    %agent_name,
                    is_background,
                    active = reg.len(),
                    "SubagentStart 注册"
                );
                drop(reg);
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
                let mut reg = self.subagent_registry.lock();
                let was_registered = reg.remove(child_agent_id).is_some();
                self.subagent_counters.lock().1 += 1;
                tracing::info!(
                    target: "langfuse::subagent",
                    event = "subagent_stop",
                    %parent_agent_id,
                    %child_agent_id,
                    %agent_name,
                    is_error,
                    was_registered,
                    active = reg.len(),
                    result_len = result.len(),
                    "SubagentStop 注销"
                );
                if !was_registered {
                    tracing::warn!(
                        target: "langfuse::subagent",
                        %child_agent_id,
                        "SubagentStop 无对应 Start（丢失/乱序），阶段②走 incomplete 分支"
                    );
                }
                drop(reg);
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
mod tests {
    use super::*;
    use peri_agent::agent::LangfuseBridgeLike;

    fn make_bridge() -> (
        LangfuseBridge,
        std::sync::Arc<crate::langfuse::fake_session::FakeLangfuseSession>,
    ) {
        // FakeLangfuseSession::new() 已返回 Arc<Self>
        let session = crate::langfuse::fake_session::FakeLangfuseSession::new("sess_c4");
        let config = crate::langfuse::config::LangfuseConfig {
            public_key: None,
            secret_key: None,
            host: "https://cloud.langfuse.com".to_string(),
            trace_sampling: 0.0,
            error_span_always: true,
            batch_max_events: 50,
            batch_flush_interval_secs: 10,
            user_id: None,
        };
        let tracer = crate::langfuse::tracer::LangfuseTracer::new(
            session.clone(),
            "sess_c4".to_string(),
            config,
        );
        let bridge = LangfuseBridge::new(
            Arc::new(parking_lot::Mutex::new(tracer)),
            "test-provider".to_string(),
            None,
        );
        (bridge, session)
    }

    /// C4: v2 SubagentStart/Stop → Unified 映射字段完整（child/parent/name/bg/result/error）
    #[test]
    fn test_from_observe_event_subagent_start_stop_mapping() {
        use peri_acp_types::identity::AgentId;
        use peri_agent::session::turn::TurnId;

        let turn_id = TurnId::new();
        let parent = AgentId::new();
        let child = AgentId::new();

        let start = ObserveEvent::SubagentStart {
            turn_id,
            agent_id: parent,
            child_agent_id: child,
            agent_name: "code-reviewer".to_string(),
            is_background: true,
        };
        match UnifiedLangfuseEvent::from_observe_event(start) {
            Some(UnifiedLangfuseEvent::SubagentStart {
                parent_agent_id,
                child_agent_id,
                agent_name,
                is_background,
            }) => {
                assert_eq!(parent_agent_id, parent.to_string());
                assert_eq!(child_agent_id, child.to_string());
                assert_eq!(agent_name, "code-reviewer");
                assert!(is_background);
            }
            other => panic!("应为 SubagentStart，实际 {:?}", other),
        }

        let stop = ObserveEvent::SubagentStop {
            turn_id,
            agent_id: parent,
            child_agent_id: child,
            agent_name: "code-reviewer".to_string(),
            result: "done".to_string(),
            is_error: false,
        };
        match UnifiedLangfuseEvent::from_observe_event(stop) {
            Some(UnifiedLangfuseEvent::SubagentStop {
                parent_agent_id,
                child_agent_id,
                agent_name,
                result,
                is_error,
            }) => {
                assert_eq!(parent_agent_id, parent.to_string());
                assert_eq!(child_agent_id, child.to_string());
                assert_eq!(agent_name, "code-reviewer");
                assert_eq!(result, "done");
                assert!(!is_error);
            }
            other => panic!("应为 SubagentStop，实际 {:?}", other),
        }
    }

    /// C4: process_event 的 Start 注册 / Stop 注销 + 计数（归属逻辑未动）
    #[test]
    fn test_process_event_registers_and_deregisters() {
        use peri_acp_types::identity::AgentId;

        let (bridge, _session) = make_bridge();
        let mut active_stage = HashMap::new();
        let parent = AgentId::new();
        let child = AgentId::new();

        // Start → 注册 + 计数
        bridge.process_event(
            &UnifiedLangfuseEvent::SubagentStart {
                parent_agent_id: parent.to_string(),
                child_agent_id: child.to_string(),
                agent_name: "explorer".to_string(),
                is_background: false,
            },
            &mut active_stage,
        );
        assert_eq!(
            bridge.active_subagent_count(),
            1,
            "Start 后应有 1 个活跃注册"
        );
        assert_eq!(
            bridge.subagent_event_counts(),
            (1, 0),
            "Start 计数应为 (1, 0)"
        );

        // 重复 Start → 覆盖注册（不增加条目），计数仍递增
        bridge.process_event(
            &UnifiedLangfuseEvent::SubagentStart {
                parent_agent_id: parent.to_string(),
                child_agent_id: child.to_string(),
                agent_name: "explorer".to_string(),
                is_background: false,
            },
            &mut active_stage,
        );
        assert_eq!(
            bridge.active_subagent_count(),
            1,
            "重复 Start 不增加注册条目"
        );

        // Stop → 注销 + 计数
        bridge.process_event(
            &UnifiedLangfuseEvent::SubagentStop {
                parent_agent_id: parent.to_string(),
                child_agent_id: child.to_string(),
                agent_name: "explorer".to_string(),
                result: "found".to_string(),
                is_error: false,
            },
            &mut active_stage,
        );
        assert_eq!(bridge.active_subagent_count(), 0, "Stop 后注册应清空");
        assert_eq!(bridge.subagent_event_counts(), (2, 1));

        // 无对应 Start 的 Stop → 不 panic，计数仍递增（阶段② incomplete 分支）
        bridge.process_event(
            &UnifiedLangfuseEvent::SubagentStop {
                parent_agent_id: parent.to_string(),
                child_agent_id: AgentId::new().to_string(),
                agent_name: "ghost".to_string(),
                result: "lost".to_string(),
                is_error: true,
            },
            &mut active_stage,
        );
        assert_eq!(bridge.active_subagent_count(), 0);
        assert_eq!(bridge.subagent_event_counts(), (2, 2));
    }

    /// C4: 经 LangfuseBridgeLike 完整链路（forwarder 同入口）Start/Stop 可达
    #[test]
    fn test_bridge_like_process_observe_start_stop() {
        use peri_acp_types::identity::AgentId;
        use peri_agent::session::turn::TurnId;

        let (bridge, _session) = make_bridge();
        let parent = AgentId::new();
        let child = AgentId::new();
        let turn_id = TurnId::new();

        bridge.process_observe_event(&ObserveEvent::SubagentStart {
            turn_id,
            agent_id: parent,
            child_agent_id: child,
            agent_name: "plan".to_string(),
            is_background: false,
        });
        assert_eq!(bridge.active_subagent_count(), 1);

        bridge.process_observe_event(&ObserveEvent::SubagentStop {
            turn_id,
            agent_id: parent,
            child_agent_id: child,
            agent_name: "plan".to_string(),
            result: "done".to_string(),
            is_error: false,
        });
        assert_eq!(bridge.active_subagent_count(), 0);
        assert_eq!(bridge.subagent_event_counts(), (1, 1));
    }
}

#[cfg(test)]
#[path = "bridge_test.rs"]
mod bridge_test;
