use std::sync::Arc;

use peri_agent::agent::events::{
    CompactStrategy, CompactTrigger, MiddlewareHook, Stage, StageStatus,
};
use peri_agent::agent::events_v2::TurnErrorReason;
use peri_agent::messages::BaseMessage;
use peri_agent::tools::ToolDefinition;
use peri_model::TokenUsage;

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
