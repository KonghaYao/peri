//! v2 事件流契约 — 三层分级事件总线（自 peri-agent 迁入；
//! `peri-agent::agent::events_v2` 保留 re-export）。
//!
//! 所有事件强制携带 `turn_id`（TurnContext 纽带）和 `agent_id`（AgentId 来源标识）。
//! 事件按消费者视角分三层：
//!
//! - **渲染层**（critical 同步，有界通道）：TextChunk / ThinkingChunk /
//!   ToolStarted / ToolEnded / BudgetWarning / HitlPending
//! - **状态层**（critical 同步，有界通道）：TurnCompleted / StateSnapshot
//! - **观测层**（broadcast，无界）：LlmCallStart / LlmCallEnd / MessagesCompacted /
//!   TurnError / SubagentStart / SubagentStop
//!
//! 本模块同时承载 v1 协议序列化面映射（`*_event_to_executor`，穷尽匹配，
//! `2026-07-25-event-identity-diverges-across-dual-delivery-paths.md`）——
//! v1 `ExecutorEvent` 中间态已退役（批 2「v1-retire」），本组函数是 ACP
//! 协议化唯一需要的 v1 兼容面，发射点（EventBus emit）统一在 Agent 层。

//! v2 事件流 — 三层分级事件总线
//!
//! 所有事件强制携带 `turn_id`（TurnContext 纽带）和 `agent_id`（AgentId 来源标识），
//! 由 Agent 层 EventBus 发射，经事件转发器（`peri-acp/src/event/forwarder.rs`）
//! 投递给消费者。事件按消费者视角分三层：
//!
//! - **渲染层**（critical 同步，有界通道）：TextChunk / ThinkingChunk /
//!   ToolStarted / ToolEnded / BudgetWarning / HitlPending
//! - **状态层**（critical 同步，有界通道）：TurnCompleted / StateSnapshot
//! - **观测层**（broadcast，无界）：LlmCallStart / LlmCallEnd / MessagesCompacted /
//!   TurnError / SubagentStart / SubagentStop
//!
//! critical 通道使用 `tokio::sync::mpsc` 有界通道 + `try_send`，满时超时降级丢弃，
//! 保证慢消费者不阻塞 ReAct 循环。
//! broadcast 通道使用 `tokio::sync::broadcast`，允许任意数量消费者订阅，
//! 慢消费者自动跳过（lagging）。

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};

use crate::event::{CompactTrigger, ExecutorEvent};
use crate::identity::AgentId;
use crate::messages::{BaseMessage, MessageId};
use crate::session::TurnId;

// ─── TurnErrorReason ──────────────────────────────────────────────────────────

/// Turn 中止原因
///
/// 用于 `ObserveEvent::TurnError`，标识 turn 非正常结束的根因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnErrorReason {
    /// 用户主动中断（cancel token 触发）
    Interrupted,
    /// 执行超时
    Timeout,
    /// LLM 调用失败（非重试可恢复）
    LlmFailure,
    /// 工具执行失败
    ToolFailure,
    /// LLM 速率限制（重试耗尽）
    RateLimit,
    /// 达到最大迭代次数
    MaxIterations,
}

impl fmt::Display for TurnErrorReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Interrupted => write!(f, "interrupted"),
            Self::Timeout => write!(f, "timeout"),
            Self::LlmFailure => write!(f, "llm_failure"),
            Self::ToolFailure => write!(f, "tool_failure"),
            Self::RateLimit => write!(f, "rate_limit"),
            Self::MaxIterations => write!(f, "max_iterations"),
        }
    }
}

// ─── RenderEvent（渲染层 — critical 同步） ────────────────────────────────────

/// 渲染层事件 — TUI / 门户消费，驱动实时 UI 更新
///
/// critical 通道有界，满时降级丢弃。所有变体强制携带 `turn_id` 和 `agent_id`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RenderEvent {
    /// LLM 输出文本块（流式，可能拆分为多次）
    ///
    /// `message_id`：所属 AI 消息的稳定 ID（一次 LLM 调用的 assistant 输出）。
    /// 同一消息的所有 chunk 共享该 ID；变化即新消息——TUI 据此做段边界与
    /// 推理结束推断（ACP 标准 messageId 语义）。
    TextChunk {
        turn_id: TurnId,
        agent_id: AgentId,
        message_id: crate::messages::MessageId,
        chunk: String,
    },
    /// LLM 推理/思考过程（thinking/reasoning）
    ///
    /// `message_id` 语义同 `TextChunk`：与同消息的文本块共享同一 ID。
    ThinkingChunk {
        turn_id: TurnId,
        agent_id: AgentId,
        message_id: crate::messages::MessageId,
        chunk: String,
    },
    /// 工具调用开始
    ToolStarted {
        turn_id: TurnId,
        agent_id: AgentId,
        tool_call_id: String,
        name: String,
        input: serde_json::Value,
    },
    /// 工具调用结束
    ///
    /// `output` 携带工具输出文本（成功）或错误信息（失败）。与 v1
    /// `ExecutorEvent::ToolEnd` 字段对齐，便于 mapper_v2 透传到 TUI。
    /// 注意：emit 时机在 error_suggest 注入之前，故 TUI 看到的是原始输出
    /// （不含建议文本），与 v1 行为一致。
    ToolEnded {
        turn_id: TurnId,
        agent_id: AgentId,
        tool_call_id: String,
        name: String,
        output: String,
        is_error: bool,
    },
    /// 上下文窗口预算警告
    BudgetWarning {
        turn_id: TurnId,
        agent_id: AgentId,
        used_tokens: u64,
        total_tokens: u64,
        percentage: f64,
    },
    /// HITL 审批等待中（暂停循环等待用户响应）
    HitlPending {
        turn_id: TurnId,
        agent_id: AgentId,
        tool_call_id: String,
        tool_name: String,
    },
    /// 单次 ReAct 迭代结束（每次 Act 阶段完成时 emit，包括工具路径与最终回答路径）
    ///
    /// `finalized_messages` 携带当前 transcript 的可见消息快照（Arc 浅克隆），
    /// 让消费方（TUI）能精确同步规范状态——避免依赖 Render 事件流自洽重建
    /// transcript（后者会让多迭代场景下文本被错误地渲染在工具调用之前）。
    ///
    /// **为何在 Render 层？** TurnCompleted 必须与同迭代的 TextChunk/ToolStarted/
    /// ToolEnded 保持严格的 FIFO 顺序——否则跨迭代场景下，TUI forwarder 的
    /// biased select! 会优先消费下一迭代的 TextChunk（在 render_rx），把上一
    /// 迭代的 TurnCompleted（在 state_rx）拖到后面，导致 partial 混合两轮内容，
    /// 渲染出"新文本在旧工具之前"的顺序错乱。把 TurnCompleted 放到 render_tx
    /// 同一通道，FIFO 保证顺序。
    TurnCompleted {
        turn_id: TurnId,
        agent_id: AgentId,
        steps: usize,
        elapsed_secs: f64,
        finalized_messages: std::sync::Arc<Vec<BaseMessage>>,
    },
}

impl RenderEvent {
    /// 提取 turn_id
    pub fn turn_id(&self) -> TurnId {
        match self {
            Self::TextChunk { turn_id, .. }
            | Self::ThinkingChunk { turn_id, .. }
            | Self::ToolStarted { turn_id, .. }
            | Self::ToolEnded { turn_id, .. }
            | Self::BudgetWarning { turn_id, .. }
            | Self::HitlPending { turn_id, .. }
            | Self::TurnCompleted { turn_id, .. } => *turn_id,
        }
    }

    /// 提取 agent_id
    pub fn agent_id(&self) -> AgentId {
        match self {
            Self::TextChunk { agent_id, .. }
            | Self::ThinkingChunk { agent_id, .. }
            | Self::ToolStarted { agent_id, .. }
            | Self::ToolEnded { agent_id, .. }
            | Self::BudgetWarning { agent_id, .. }
            | Self::HitlPending { agent_id, .. }
            | Self::TurnCompleted { agent_id, .. } => *agent_id,
        }
    }
}

// ─── StateEvent（状态层 — critical 同步） ──────────────────────────────────────

/// 状态层事件 — 外部状态同步消费
///
/// critical 通道有界，满时降级丢弃。所有变体强制携带 `turn_id` 和 `agent_id`。
///
/// **注意**：`TurnCompleted` 已迁移到 `RenderEvent`（详见 `RenderEvent::TurnCompleted`
/// 文档），原因是跨迭代顺序保证需要 FIFO，而 state_tx 与 render_tx 是独立通道，
/// biased select! 无法保证跨通道顺序。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StateEvent {
    /// 状态快照（轻量级元数据，用于状态同步与 UI 刷新）
    ///
    /// 与 v1 `ExecutorEvent::StateSnapshot(Vec<BaseMessage>)` 不同，v2 快照**不携带**
    /// 完整消息历史——v2 设计上避免在事件中持有 transcript 引用（锁开销 + 拷贝成本）。
    /// 消费方（TUI）如需完整消息，应通过 transcript 通道或 `StateSnapshotMeta` 的
    /// `message_count` 自行决定何时拉取。
    ///
    /// mapper_v2 将本事件映射为 `ExecutorEvent::StateSnapshotMeta`（而非 v1
    /// `StateSnapshot(Vec<BaseMessage>)`），TUI 据此区分「元数据快照」与「完整快照」，
    /// 避免空消息列表误清空 `MessagePipeline::completed`。
    StateSnapshot {
        turn_id: TurnId,
        agent_id: AgentId,
        message_count: usize,
        total_tokens: u64,
        /// 当前 ReAct 步数（ctx.turn.current_step()）
        current_step: usize,
        /// 连续工具失败次数（StageContext.consecutive_failures 快照，不含 compact 失败）
        consecutive_failures: u32,
        /// 上下文窗口使用率（0.0-1.0），None 表示无 context_budget
        budget_pct: Option<f64>,
        /// 上下文窗口总量（ContextBudget.context_window），None 表示无配置
        context_total_tokens: Option<u64>,
    },
    /// 合成用户消息——由 agent 内部注入的 human message（如 bg agent 完成回调）。
    /// 通过 EventBus → mapper_v2 → ExecutorEvent::MessageAdded → ACP 映射到
    /// session/update(user_message_chunk) → TUI 用户气泡。
    /// 与 registry event pump 发 unstable-event 方案相比，本路径没有时序竞争窗口：
    /// 消息 emit 与 agent 将 MQ 消息写入 transcript 在同一代码点（End 阶段），
    /// 保证 TUI 气泡位于已于 committed 归档的 turn 之后、当前 turn 之前。
    SyntheticUserMessage {
        turn_id: TurnId,
        agent_id: AgentId,
        text: String,
    },
    /// Turn 已挂起等待异步事件（bg agent/cron/workflow）。
    ///
    /// Agent 在 idle/await_wake 路径中 emit 此事件，TUI 收到后：
    /// - 归档 current_turn 到 committed（flush）
    /// - 设置 is_loading = false（停止 loading spinner）
    /// - Agent 保持存活（await_wake 阻塞）
    ///
    /// bg callback 到达时新 turn 的 TextChunk/ToolStarted 事件自动恢复 loading。
    TurnSuspended { turn_id: TurnId, agent_id: AgentId },
}

impl StateEvent {
    /// 提取 turn_id
    pub fn turn_id(&self) -> TurnId {
        match self {
            Self::StateSnapshot { turn_id, .. }
            | Self::SyntheticUserMessage { turn_id, .. }
            | Self::TurnSuspended { turn_id, .. } => *turn_id,
        }
    }

    /// 提取 agent_id
    pub fn agent_id(&self) -> AgentId {
        match self {
            Self::StateSnapshot { agent_id, .. }
            | Self::SyntheticUserMessage { agent_id, .. }
            | Self::TurnSuspended { agent_id, .. } => *agent_id,
        }
    }
}

// ─── ObserveEvent（观测层 — broadcast） ───────────────────────────────────────

/// 观测层事件 — 遥测 / 持久化消费
///
/// broadcast 通道，允许任意消费者订阅。慢消费者自动跳过。
/// 所有变体强制携带 `turn_id` 和 `agent_id`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ObserveEvent {
    /// LLM 调用开始
    LlmCallStart {
        turn_id: TurnId,
        agent_id: AgentId,
        step: usize,
        /// LLM 输入消息快照（Arc 浅拷贝，与 v1 ExecutorEvent::LlmCallStart.messages 对齐）
        messages: std::sync::Arc<Vec<crate::messages::BaseMessage>>,
        /// 工具定义快照（用于 Langfuse Generation trace input）
        tools: Vec<crate::tools::ToolDefinition>,
    },
    /// LLM 调用结束
    LlmCallEnd {
        turn_id: TurnId,
        agent_id: AgentId,
        step: usize,
        model: String,
        /// LLM 输出文本（成功路径：final_answer 或 thought；错误路径：format!("ERROR: {}", e)）
        /// 与 v1 ExecutorEvent::LlmCallEnd.output 对齐，用于 Langfuse Generation 追踪
        output: String,
        input_tokens: u64,
        output_tokens: u64,
        /// Prompt cache 创建/读取的 token 数（v2 之前丢失，导致 TUI cache 命中率始终 0%）
        ///
        /// 0 表示 Provider 不支持 caching 或未命中；与 `TokenUsage::cache_creation_input_tokens`
        /// 对齐（None 用 0 占位，因为 Provider 不支持时本就是 0）。
        cache_creation_input_tokens: u64,
        cache_read_input_tokens: u64,
        /// Provider 返回的请求 ID（用于关联日志/遥测；None 表示 Provider 未返回）
        request_id: Option<String>,
    },
    /// Compact 阶段开始
    CompactStarted {
        turn_id: TurnId,
        agent_id: AgentId,
        step: usize,
        /// 压缩策略（Micro / Full / Smart）
        strategy: crate::event::CompactStrategy,
    },
    /// Compact 阶段结束（无变更发生的结束路径专用，与 `CompactStarted` 成对）。
    ///
    /// S1.4：cancel 且未提交变更时不能 emit `MessagesCompacted`（那会误导遥测
    /// 以为压缩发生了），需要独立的结束观测闭合 span。仅"确有压缩变更"的路径
    /// emit `MessagesCompacted`。
    CompactEnded {
        turn_id: TurnId,
        agent_id: AgentId,
        step: usize,
        /// 压缩策略（Micro / Full / Smart）
        strategy: crate::event::CompactStrategy,
        /// 结束语义（Interrupted = 被取消且未提交变更）
        outcome: crate::compact::CompactOutcome,
    },
    /// 消息被压缩
    MessagesCompacted {
        turn_id: TurnId,
        agent_id: AgentId,
        before_count: usize,
        after_count: usize,
        summary: String,
        /// Compact 后的可见消息快照（供 TUI pipeline 重建）
        ///
        /// v2 哲学：transcript 是会话级权威存储，但 TUI 不直接订阅 transcript 变化。
        /// 因此 compact 后必须把 visible_messages 快照随事件传递，让 TUI 通过
        /// `pipeline.clear() + restore_completed(messages)` 完整重建。
        messages: Vec<crate::messages::BaseMessage>,
        /// Re-inject 还原的文件列表（CompactCompleted 事件载荷）
        files: Vec<crate::event::CompactFileInfo>,
        /// Re-inject 还原的 Skill 名称列表
        skills: Vec<String>,
        /// Re-inject 还原的消息（Human/文件/Skills）——已包含在 messages 中，
        /// 此字段仅供调试/遥测，TUI 不直接使用
        re_inject_count: usize,
        /// 压缩策略（Micro / Full / Smart）
        strategy: crate::event::CompactStrategy,
        /// 受影响的消息数量（v2 compact 操作计数）
        affected_count: usize,
        /// 估算节省的 token 数量（v2 compact projection 估算）
        estimated_tokens_saved: u64,
        /// 压缩前估算 token 数（ContextPressure.estimated_tokens）
        estimated_tokens_before: u64,
        /// 压缩后估算 token 数（estimated_tokens_before - estimated_tokens_saved）
        estimated_tokens_after: u64,
        /// 被修改的消息数量（v2 projection 变更计数）
        changed_messages: usize,
        /// 被修改的字段数量（v2 projection 字段级变更计数）
        changed_fields: usize,
        /// 无操作候选数量（projection 判定无需变更的消息数）
        no_op_candidates: usize,
        /// 升级到 Full Compact 的原因（Micro/Smart 时为 None）
        full_escalation_reason: Option<crate::compact::FullEscalationReason>,
        /// 压缩前缓存命中率（0.0-1.0）
        cache_hit_rate_before: f64,
        /// Compact 执行的语义结果（MicroApplied / FullApplied / FullFailed / ...）
        outcome: crate::compact::CompactOutcome,
    },
    /// Turn 异常中止
    TurnError {
        turn_id: TurnId,
        agent_id: AgentId,
        reason: TurnErrorReason,
        message: String,
    },
    /// 子 Agent 开始
    SubagentStart {
        turn_id: TurnId,
        agent_id: AgentId,
        child_agent_id: AgentId,
        agent_name: String,
        is_background: bool,
    },
    /// 子 Agent 结束
    SubagentStop {
        turn_id: TurnId,
        agent_id: AgentId,
        child_agent_id: AgentId,
        agent_name: String,
        result: String,
        is_error: bool,
    },
    /// LLM Provider 实际请求体（raw body），紧随 [`Self::LlmCallStart`] 之后 emit。
    ///
    /// 用于 Langfuse Generation input：携带 Provider-native 完整请求体（含正确工具
    /// 格式与 system 位置），让 Langfuse UI 上的 input 与 Provider 实际收到的请求体
    /// 完全一致。`Arc<Value>` 浅拷贝，避免跨多订阅者重复 clone 大 JSON。
    ///
    /// tracer 在 `on_llm_start` 建 generation_data 缓存后，本事件写入 `raw_body`
    /// 字段；`on_llm_end` 时优先用 raw_body，fallback 到 messages+tools 抽象序列化。
    LlmRequestPayload {
        turn_id: TurnId,
        agent_id: AgentId,
        step: usize,
        body: std::sync::Arc<serde_json::Value>,
    },
    // ── langfuse v2：Reason 推理分片 ──
    /// AI 推理分片（流式 thinking chunk 的遥测投射）
    AiReasoningChunk {
        turn_id: TurnId,
        agent_id: AgentId,
        text: String,
        source_agent_id: Option<String>,
    },
    // ── langfuse v2：阶段生命周期 ──
    /// ReAct 阶段开始
    StageStarted {
        turn_id: TurnId,
        agent_id: AgentId,
        stage: crate::event::Stage,
    },
    /// ReAct 阶段结束
    StageEnded {
        turn_id: TurnId,
        agent_id: AgentId,
        stage: crate::event::Stage,
        status: crate::event::StageStatus,
        duration_ms: u64,
    },
    // ── langfuse v2：Receive 队列排空 ──
    /// MessageQueue 排空统计
    MessageQueueDrained {
        turn_id: TurnId,
        agent_id: AgentId,
        prompt: usize,
        defer: usize,
        info: usize,
    },
}

impl ObserveEvent {
    /// 提取 turn_id
    pub fn turn_id(&self) -> TurnId {
        match self {
            Self::LlmCallStart { turn_id, .. }
            | Self::LlmCallEnd { turn_id, .. }
            | Self::CompactStarted { turn_id, .. }
            | Self::CompactEnded { turn_id, .. }
            | Self::MessagesCompacted { turn_id, .. }
            | Self::TurnError { turn_id, .. }
            | Self::SubagentStart { turn_id, .. }
            | Self::SubagentStop { turn_id, .. }
            | Self::LlmRequestPayload { turn_id, .. }
            | Self::AiReasoningChunk { turn_id, .. }
            | Self::StageStarted { turn_id, .. }
            | Self::StageEnded { turn_id, .. }
            | Self::MessageQueueDrained { turn_id, .. } => *turn_id,
        }
    }

    /// 提取 agent_id
    pub fn agent_id(&self) -> AgentId {
        match self {
            Self::LlmCallStart { agent_id, .. }
            | Self::LlmCallEnd { agent_id, .. }
            | Self::CompactStarted { agent_id, .. }
            | Self::CompactEnded { agent_id, .. }
            | Self::MessagesCompacted { agent_id, .. }
            | Self::TurnError { agent_id, .. }
            | Self::SubagentStart { agent_id, .. }
            | Self::SubagentStop { agent_id, .. }
            | Self::LlmRequestPayload { agent_id, .. }
            | Self::AiReasoningChunk { agent_id, .. }
            | Self::StageStarted { agent_id, .. }
            | Self::StageEnded { agent_id, .. }
            | Self::MessageQueueDrained { agent_id, .. } => *agent_id,
        }
    }
}

// ─── Event（统一包装） ───────────────────────────────────────────────────────

/// 统一事件包装 — 三层事件的公共枚举
///
/// 消费者可根据需要按层订阅，也可统一接收后 match 分发。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    Render(RenderEvent),
    State(StateEvent),
    Observe(ObserveEvent),
}

impl Event {
    /// 提取 turn_id（从内层事件中取出）
    pub fn turn_id(&self) -> TurnId {
        match self {
            Self::Render(e) => e.turn_id(),
            Self::State(e) => e.turn_id(),
            Self::Observe(e) => e.turn_id(),
        }
    }

    /// 提取 agent_id（从内层事件中取出）
    pub fn agent_id(&self) -> AgentId {
        match self {
            Self::Render(e) => e.agent_id(),
            Self::State(e) => e.agent_id(),
            Self::Observe(e) => e.agent_id(),
        }
    }
}

// ─── EventBus（生产端） ───────────────────────────────────────────────────────

/// 事件总线 — 生产端，持有三个通道的 Sender
///
/// - 渲染层 / 状态层：`tokio::sync::mpsc` 有界通道，`try_send` 满时降级丢弃
/// - 观测层：`tokio::sync::broadcast` 通道，慢消费者自动 lagging
///
/// 通道容量通过 `EventBus::new()` 的参数配置。
pub struct EventBus {
    render_tx: mpsc::Sender<RenderEvent>,
    state_tx: mpsc::Sender<StateEvent>,
    observe_tx: broadcast::Sender<ObserveEvent>,
    /// critical 通道 try_send 失败后的超时降级重试时长（仅用于日志，不阻塞）
    _drop_timeout: Duration,
}

/// EventBus 构建参数
#[derive(Debug, Clone)]
pub struct EventBusConfig {
    /// 渲染层通道容量（默认 256）
    pub render_capacity: usize,
    /// 状态层通道容量（默认 64）
    pub state_capacity: usize,
    /// 观测层 broadcast 通道容量（默认 128）
    pub observe_capacity: usize,
    /// critical 通道 try_send 失败后的超时降级重试时长（默认 50ms）
    pub drop_timeout: Duration,
}

impl Default for EventBusConfig {
    fn default() -> Self {
        Self {
            render_capacity: 256,
            state_capacity: 64,
            observe_capacity: 128,
            drop_timeout: Duration::from_millis(50),
        }
    }
}

impl EventBus {
    /// 创建 EventBus，返回 (EventBus, EventHandles)
    ///
    /// `EventBus` 给生产者（Agent），`EventHandles` 给消费者（TUI / 遥测）。
    pub fn new(config: EventBusConfig) -> (Self, EventHandles) {
        let (render_tx, render_rx) = mpsc::channel(config.render_capacity);
        let (state_tx, state_rx) = mpsc::channel(config.state_capacity);
        let (observe_tx, observe_rx) = broadcast::channel(config.observe_capacity);

        let bus = Self {
            render_tx,
            state_tx,
            observe_tx,
            _drop_timeout: config.drop_timeout,
        };

        let handles = EventHandles {
            render_rx,
            state_rx,
            observe_rx,
        };

        (bus, handles)
    }

    /// 发送渲染层事件（critical，满时降级丢弃）
    pub fn emit_render(&self, event: RenderEvent) {
        // 有界通道 + try_send：满时丢弃，不阻塞循环
        if self.render_tx.try_send(event).is_err() {
            tracing::warn!(event = "render_event_dropped", "渲染层通道已满，事件丢弃");
        }
    }

    /// 发送状态层事件（critical，满时降级丢弃）
    pub fn emit_state(&self, event: StateEvent) {
        if self.state_tx.try_send(event).is_err() {
            tracing::warn!(event = "state_event_dropped", "状态层通道已满，事件丢弃");
        }
    }

    /// 发送观测层事件（broadcast，慢消费者自动跳过）
    ///
    /// 返回接收者数量（0 表示无订阅者）。
    pub fn emit_observe(&self, event: ObserveEvent) -> usize {
        match self.observe_tx.send(event) {
            Ok(n) => n,
            Err(_) => {
                tracing::debug!(event = "observe_event_no_subscriber", "观测层无订阅者");
                0
            }
        }
    }
}

// ─── EventHandles（消费端） ───────────────────────────────────────────────────

/// 事件句柄 — 消费端，持有三个通道的 Receiver
///
/// 可按层独立消费，也可通过 `next_render` / `next_state` / `observe_stream` 获取事件。
pub struct EventHandles {
    pub render_rx: mpsc::Receiver<RenderEvent>,
    pub state_rx: mpsc::Receiver<StateEvent>,
    pub observe_rx: broadcast::Receiver<ObserveEvent>,
}

impl EventHandles {
    /// 非阻塞获取下一个渲染层事件
    pub fn try_render(&mut self) -> Option<RenderEvent> {
        self.render_rx.try_recv().ok()
    }

    /// 非阻塞获取下一个状态层事件
    pub fn try_state(&mut self) -> Option<StateEvent> {
        self.state_rx.try_recv().ok()
    }

    /// 非阻塞获取下一个观测层事件（lagging 时返回 None）
    pub fn try_observe(&mut self) -> Option<ObserveEvent> {
        self.observe_rx.try_recv().ok()
    }

    /// 订阅观测层（创建新的 Receiver，共享同一 broadcast 通道）
    ///
    /// 用于多个独立消费者同时订阅观测层。
    pub fn subscribe_observe(&self) -> broadcast::Receiver<ObserveEvent> {
        self.observe_rx.resubscribe()
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

// ─── v1 兼容映射（v2 → ExecutorEvent，协议序列化面） ─────────────────────
//
// v1 `ExecutorEvent` 中间态已退役（批 2「v1-retire」：peri-agent 内部发射统一
// v2 形态，ObserveEvent 身份透传），本组函数保留为 **ACP 协议序列化面需要的
// 最小映射**——v2 事件经本组函数转为 v1 协议化载体后由 ACP 序列化
// （SessionUpdate / AcpEvent，wire format 不变）：
//
// - **穷尽匹配**：每个 v2 变体显式声明映射结果或过滤理由，新增变体无法静默
//   落入 wildcard 丢弃分支（`2026-07-25-event-identity-diverges-across-dual-delivery-paths.md`）。
// - **身份透传**：`source_agent_id` 透传 v2 `agent_id`（不再置 None 伪装）；
//   `message_id` 以 turn_id 填充（v2 chunk 事件无 message 级身份，turn_id 是
//   该 chunk 可获得的唯一稳定身份；真实 message 身份由 transcript/envelope 承载）；
//   `SubagentStart/Stop.child_agent_id` 透传为 `SubagentStarted/Stopped.instance_id`（C1 契约）。
// - **不承载 Agent 层发射**：发射点在 Agent 层 EventBus（v2），本组函数仅 ACP
//   消费侧协议化时调用（`peri-acp/src/event/forwarder.rs`、subagent 发射侧同步映射）。

/// 将 v2 `RenderEvent` 转换为 0 或 1 个 `ExecutorEvent`（穷尽匹配）。
pub fn render_event_to_executor(event: RenderEvent) -> Option<ExecutorEvent> {
    match event {
        RenderEvent::TextChunk {
            agent_id,
            message_id,
            chunk,
            ..
        } => Some(ExecutorEvent::TextChunk {
            // v2 chunk 事件携带消息级身份（每次 LLM 调用一个稳定 message_id），
            // 不再用 turn_id 填充——同一 turn 多次迭代的消息各自独立（ACP 标准
            // messageId 语义：变化 = 新消息）。
            message_id,
            chunk,
            source_agent_id: Some(agent_id.to_string()),
        }),
        RenderEvent::ThinkingChunk {
            agent_id,
            message_id,
            chunk,
            ..
        } => Some(ExecutorEvent::AiReasoning {
            message_id,
            text: chunk,
            source_agent_id: Some(agent_id.to_string()),
        }),
        RenderEvent::ToolStarted {
            turn_id,
            agent_id,
            tool_call_id,
            name,
            input,
        } => Some(ExecutorEvent::ToolStart {
            message_id: MessageId::from(turn_id.as_uuid()),
            tool_call_id,
            name,
            input,
            source_agent_id: Some(agent_id.to_string()),
        }),
        RenderEvent::ToolEnded {
            turn_id,
            agent_id,
            tool_call_id,
            name,
            output,
            is_error,
        } => Some(ExecutorEvent::ToolEnd {
            message_id: MessageId::from(turn_id.as_uuid()),
            tool_call_id,
            name,
            output,
            is_error,
            source_agent_id: Some(agent_id.to_string()),
        }),
        RenderEvent::BudgetWarning {
            used_tokens,
            total_tokens,
            percentage,
            ..
        } => Some(ExecutorEvent::ContextWarning {
            used_tokens,
            total_tokens,
            percentage,
        }),
        // HitlPending：v1 中无对应变体，由 HITL 审批独立通道（RequestPermission）
        // 处理，不在事件链映射。
        RenderEvent::HitlPending { .. } => None,
        RenderEvent::TurnCompleted {
            finalized_messages,
            steps,
            ..
        } => Some(ExecutorEvent::TurnCommitted {
            // Arc 直接透传（浅拷贝），消除每迭代的全量消息深拷贝
            messages: finalized_messages,
            steps,
        }),
    }
}

/// 将 v2 `StateEvent` 转换为 `ExecutorEvent`（穷尽匹配）。
pub fn state_event_to_executor(event: StateEvent) -> Option<ExecutorEvent> {
    match event {
        StateEvent::StateSnapshot {
            message_count,
            total_tokens,
            current_step,
            consecutive_failures,
            budget_pct,
            context_total_tokens,
            ..
        } => Some(ExecutorEvent::StateSnapshotMeta {
            message_count,
            total_tokens,
            current_step,
            consecutive_failures,
            budget_pct,
            context_total_tokens,
        }),
        StateEvent::SyntheticUserMessage { text, .. } => Some(ExecutorEvent::MessageAdded(
            crate::messages::BaseMessage::human(crate::messages::MessageContent::text(text)),
        )),
        // TurnSuspended：TUI 挂起信号（归档 current_turn + 停止 loading），
        // 经 ExecutorEvent::TurnSuspended 透传 turn_id/agent_id 身份。
        StateEvent::TurnSuspended { turn_id, agent_id } => Some(ExecutorEvent::TurnSuspended {
            turn_id: turn_id.to_string(),
            agent_id: agent_id.to_string(),
        }),
    }
}

/// 将 v2 `ObserveEvent` 转换为 `ExecutorEvent`（穷尽匹配）。
pub fn observe_event_to_executor(event: ObserveEvent) -> Option<ExecutorEvent> {
    match event {
        ObserveEvent::LlmCallStart {
            step,
            messages,
            tools,
            ..
        } => Some(ExecutorEvent::LlmCallStart {
            step,
            messages,
            tools,
        }),
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
        } => Some(ExecutorEvent::LlmCallEnd {
            step,
            model,
            output,
            usage: Some(peri_model::TokenUsage {
                input_tokens: input_tokens as u32,
                output_tokens: output_tokens as u32,
                // 0 表示 Provider 不支持 caching；保留 Option 让下游区分"不支持" vs "未命中"
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
            }),
            stop_reason: None,
            request_id,
        }),
        ObserveEvent::CompactStarted {
            turn_id,
            agent_id,
            step,
            strategy,
            ..
        } => Some(ExecutorEvent::CompactStarted {
            turn_id: turn_id.to_string(),
            agent_id: agent_id.to_string(),
            step,
            strategy,
            trigger: CompactTrigger::Auto,
        }),
        // CompactEnded：无变更的结束路径（cancel 且未提交变更），v1 无对应
        // 事件变体；仅 Langfuse bridge 直消费 v2 闭合 span。
        ObserveEvent::CompactEnded { .. } => None,
        ObserveEvent::MessagesCompacted {
            before_count,
            after_count,
            summary,
            messages,
            files,
            skills,
            strategy,
            affected_count,
            estimated_tokens_saved,
            estimated_tokens_before,
            estimated_tokens_after,
            changed_messages,
            changed_fields,
            no_op_candidates,
            full_escalation_reason,
            cache_hit_rate_before,
            outcome,
            ..
        } => Some(ExecutorEvent::CompactCompleted {
            summary,
            files,
            skills,
            micro_cleared: before_count.saturating_sub(after_count),
            messages,
            token_before: estimated_tokens_before,
            token_after: estimated_tokens_after,
            strategy,
            affected_count,
            estimated_tokens_saved,
            estimated_tokens_before,
            estimated_tokens_after,
            changed_messages,
            changed_fields,
            no_op_candidates,
            full_escalation_reason,
            cache_hit_rate_before,
            trigger: CompactTrigger::Auto,
            outcome,
        }),
        // TurnError：TUI 错误展示经 executor_helpers 的 AgentExecutionFailed
        // （LoopResult::Error 分支）；Langfuse 经 bridge 直消费 v2。v1 无对应变体。
        ObserveEvent::TurnError { .. } => None,
        ObserveEvent::SubagentStart {
            agent_name,
            child_agent_id,
            is_background,
            ..
        } => Some(ExecutorEvent::SubagentStarted {
            agent_name,
            instance_id: child_agent_id.to_string(),
            is_background,
        }),
        ObserveEvent::SubagentStop {
            agent_name,
            child_agent_id,
            result,
            is_error,
            ..
        } => Some(ExecutorEvent::SubagentStopped {
            agent_name,
            result,
            is_error,
            instance_id: child_agent_id.to_string(),
        }),
        ObserveEvent::LlmRequestPayload { step, body, .. } => {
            Some(ExecutorEvent::LlmRequestPayload { step, body })
        }
        // ── tracer-only：Langfuse bridge 直消费 v2，v1 无对应变体 ──
        ObserveEvent::AiReasoningChunk { .. } => None,
        ObserveEvent::StageStarted { .. } => None,
        ObserveEvent::StageEnded { .. } => None,
        ObserveEvent::MessageQueueDrained { .. } => None,
    }
}
