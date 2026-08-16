use std::sync::Arc;

use peri_agent::agent::events::{
    CompactStrategy, CompactTrigger, ExecutorEvent, MiddlewareHook, Stage, StageStatus,
};
use peri_agent::agent::events_v2::{ObserveEvent, RenderEvent, TurnErrorReason};
use peri_agent::messages::BaseMessage;
use peri_agent::tools::ToolDefinition;
use peri_model::TokenUsage;

use crate::langfuse::tracer::stages::MAIN_AGENT_KEY;
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
            // Phase 5 Step 4：CompactCompleted 收敛为重建信号三字段
            // （summary/messages/trigger），观测映射改读 summary/messages，
            // 被删字段（files/skills/micro_cleared/strategy/outcome 等）不再
            // 参与映射；错误反馈已移交 CommandFeedback（CompactError 变体删除）。
            // [P2-4] CompactEnded span 模型要求数字字段，收敛后已无观测来源：
            // 下方 files_count/skills_count/estimated_tokens_* 等硬编码 0/0.0
            // 仅表示「未知/不可用」，勿在 Langfuse 面板读作真实观测
            // （「0 文件、0 token 节省」）；完整观测见 ObserveEvent::MessagesCompacted。
            ExecutorEvent::CompactCompleted { summary, .. } => {
                Some(UnifiedLangfuseEvent::CompactEnded {
                    summary,
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
                    outcome: None,
                })
            }
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
            | ExecutorEvent::TurnSuspended { .. }
            | ExecutorEvent::SystemNotification { .. }
            | ExecutorEvent::OauthNeeded { .. }
            | ExecutorEvent::OauthCompleted { .. }
            | ExecutorEvent::OauthFailed { .. }
            | ExecutorEvent::BgRegistryEvent(_)
            | ExecutorEvent::CommandFeedback(_) => None,
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
