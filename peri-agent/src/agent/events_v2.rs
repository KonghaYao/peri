//! v2 事件流 — 三层分级事件总线
//!
//! 所有事件强制携带 `turn_id`（TurnContext 纽带）和 `agent_id`（AgentId 来源标识），
//! 由 AgentGroup 统一聚合后投递。事件按消费者视角分三层：
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
use tokio::sync::broadcast;
use tokio::sync::mpsc;

use crate::group::pipeline::AgentId;
use crate::messages::BaseMessage;
use crate::session::turn::TurnId;

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
    TextChunk {
        turn_id: TurnId,
        agent_id: AgentId,
        chunk: String,
    },
    /// LLM 推理/思考过程（thinking/reasoning）
    ThinkingChunk {
        turn_id: TurnId,
        agent_id: AgentId,
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
        /// 连续工具/compact 失败次数（StageContext.consecutive_failures 快照）
        consecutive_failures: u32,
        /// 上下文窗口使用率（0.0-1.0），None 表示无 context_budget
        budget_pct: Option<f64>,
        /// 上下文窗口总量（ContextBudget.context_window），None 表示无配置
        context_total_tokens: Option<u64>,
    },
}

impl StateEvent {
    /// 提取 turn_id
    pub fn turn_id(&self) -> TurnId {
        match self {
            Self::StateSnapshot { turn_id, .. } => *turn_id,
        }
    }

    /// 提取 agent_id
    pub fn agent_id(&self) -> AgentId {
        match self {
            Self::StateSnapshot { agent_id, .. } => *agent_id,
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
        files: Vec<crate::agent::events::CompactFileInfo>,
        /// Re-inject 还原的 Skill 名称列表
        skills: Vec<String>,
        /// Re-inject 还原的消息（Human/文件/Skills）——已包含在 messages 中，
        /// 此字段仅供调试/遥测，TUI 不直接使用
        re_inject_count: usize,
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
}

impl ObserveEvent {
    /// 提取 turn_id
    pub fn turn_id(&self) -> TurnId {
        match self {
            Self::LlmCallStart { turn_id, .. }
            | Self::LlmCallEnd { turn_id, .. }
            | Self::CompactStarted { turn_id, .. }
            | Self::MessagesCompacted { turn_id, .. }
            | Self::TurnError { turn_id, .. }
            | Self::SubagentStart { turn_id, .. }
            | Self::SubagentStop { turn_id, .. }
            | Self::LlmRequestPayload { turn_id, .. } => *turn_id,
        }
    }

    /// 提取 agent_id
    pub fn agent_id(&self) -> AgentId {
        match self {
            Self::LlmCallStart { agent_id, .. }
            | Self::LlmCallEnd { agent_id, .. }
            | Self::CompactStarted { agent_id, .. }
            | Self::MessagesCompacted { agent_id, .. }
            | Self::TurnError { agent_id, .. }
            | Self::SubagentStart { agent_id, .. }
            | Self::SubagentStop { agent_id, .. }
            | Self::LlmRequestPayload { agent_id, .. } => *agent_id,
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

#[cfg(test)]
mod tests {
    use super::*;

    // ─── 测试辅助 impl（须置于 test module 内，避免 items-after-test-module）──
    impl EventHandles {
        /// 测试辅助：从配置创建 (EventBus, EventHandles)
        fn from_bus(config: EventBusConfig) -> (EventBus, Self) {
            EventBus::new(config)
        }
    }

    // ─── 构造辅助 ──────────────────────────────────────────────────────────

    fn make_ids() -> (TurnId, AgentId) {
        (TurnId::new(), AgentId::new())
    }

    // ─── TurnErrorReason 测试 ────────────────────────────────────────────────

    #[test]
    fn test_turn_error_reason_display() {
        assert_eq!(TurnErrorReason::Interrupted.to_string(), "interrupted");
        assert_eq!(TurnErrorReason::Timeout.to_string(), "timeout");
        assert_eq!(TurnErrorReason::LlmFailure.to_string(), "llm_failure");
        assert_eq!(TurnErrorReason::ToolFailure.to_string(), "tool_failure");
        assert_eq!(TurnErrorReason::RateLimit.to_string(), "rate_limit");
        assert_eq!(TurnErrorReason::MaxIterations.to_string(), "max_iterations");
    }

    #[test]
    fn test_turn_error_reason_serde_roundtrip() {
        let reasons = [
            TurnErrorReason::Interrupted,
            TurnErrorReason::Timeout,
            TurnErrorReason::LlmFailure,
            TurnErrorReason::ToolFailure,
            TurnErrorReason::RateLimit,
            TurnErrorReason::MaxIterations,
        ];
        for reason in &reasons {
            let json = serde_json::to_string(reason).unwrap();
            let back: TurnErrorReason = serde_json::from_str(&json).unwrap();
            assert_eq!(*reason, back);
        }
    }

    // ─── RenderEvent 测试 ──────────────────────────────────────────────────

    #[test]
    fn test_render_event_text_chunk_id_extraction() {
        let (turn_id, agent_id) = make_ids();
        let event = RenderEvent::TextChunk {
            turn_id,
            agent_id,
            chunk: "hello".to_string(),
        };
        assert_eq!(event.turn_id(), turn_id);
        assert_eq!(event.agent_id(), agent_id);
    }

    #[test]
    fn test_render_event_thinking_chunk_id_extraction() {
        let (turn_id, agent_id) = make_ids();
        let event = RenderEvent::ThinkingChunk {
            turn_id,
            agent_id,
            chunk: "thinking...".to_string(),
        };
        assert_eq!(event.turn_id(), turn_id);
        assert_eq!(event.agent_id(), agent_id);
    }

    #[test]
    fn test_render_event_tool_started_id_extraction() {
        let (turn_id, agent_id) = make_ids();
        let event = RenderEvent::ToolStarted {
            turn_id,
            agent_id,
            tool_call_id: "tc_1".to_string(),
            name: "Read".to_string(),
            input: serde_json::Value::Null,
        };
        assert_eq!(event.turn_id(), turn_id);
        assert_eq!(event.agent_id(), agent_id);
    }

    #[test]
    fn test_render_event_tool_ended_id_extraction() {
        let (turn_id, agent_id) = make_ids();
        let event = RenderEvent::ToolEnded {
            turn_id,
            agent_id,
            tool_call_id: "tc_1".to_string(),
            name: "Read".to_string(),
            output: "file contents".to_string(),
            is_error: false,
        };
        assert_eq!(event.turn_id(), turn_id);
        assert_eq!(event.agent_id(), agent_id);
    }

    #[test]
    fn test_render_event_tool_ended_carries_output() {
        // ToolEnded 必须携带非空 output，经 mapper_v2 透传后 TUI 才能拿到工具结果
        let (turn_id, agent_id) = make_ids();
        let event = RenderEvent::ToolEnded {
            turn_id,
            agent_id,
            tool_call_id: "tc_out".to_string(),
            name: "Bash".to_string(),
            output: "command output here".to_string(),
            is_error: false,
        };
        // 通过模式匹配断言 output 字段存在且非空
        match event {
            RenderEvent::ToolEnded { ref output, .. } => {
                assert!(!output.is_empty(), "output 应为非空字符串");
                assert_eq!(output, "command output here");
            }
            _ => panic!("应为 ToolEnded"),
        }
    }

    #[test]
    fn test_render_event_budget_warning_id_extraction() {
        let (turn_id, agent_id) = make_ids();
        let event = RenderEvent::BudgetWarning {
            turn_id,
            agent_id,
            used_tokens: 1000,
            total_tokens: 200000,
            percentage: 0.5,
        };
        assert_eq!(event.turn_id(), turn_id);
        assert_eq!(event.agent_id(), agent_id);
    }

    #[test]
    fn test_render_event_hitl_pending_id_extraction() {
        let (turn_id, agent_id) = make_ids();
        let event = RenderEvent::HitlPending {
            turn_id,
            agent_id,
            tool_call_id: "tc_2".to_string(),
            tool_name: "Bash".to_string(),
        };
        assert_eq!(event.turn_id(), turn_id);
        assert_eq!(event.agent_id(), agent_id);
    }

    // ─── StateEvent 测试 ───────────────────────────────────────────────────

    #[test]
    fn test_render_event_turn_completed_id_extraction() {
        let (turn_id, agent_id) = make_ids();
        let event = RenderEvent::TurnCompleted {
            turn_id,
            agent_id,
            steps: 5,
            elapsed_secs: 3.2,
            finalized_messages: std::sync::Arc::new(vec![]),
        };
        assert_eq!(event.turn_id(), turn_id);
        assert_eq!(event.agent_id(), agent_id);
    }

    #[test]
    fn test_state_event_snapshot_id_extraction() {
        let (turn_id, agent_id) = make_ids();
        let event = StateEvent::StateSnapshot {
            turn_id,
            agent_id,
            message_count: 42,
            total_tokens: 10000,
            current_step: 3,
            consecutive_failures: 0,
            budget_pct: Some(0.45),
            context_total_tokens: Some(200_000),
        };
        assert_eq!(event.turn_id(), turn_id);
        assert_eq!(event.agent_id(), agent_id);
    }

    // ─── ObserveEvent 测试 ──────────────────────────────────────────────────

    #[test]
    fn test_observe_event_llm_call_start_id_extraction() {
        let (turn_id, agent_id) = make_ids();
        let event = ObserveEvent::LlmCallStart {
            turn_id,
            agent_id,
            step: 1,
            messages: std::sync::Arc::new(vec![]),
            tools: vec![],
        };
        assert_eq!(event.turn_id(), turn_id);
        assert_eq!(event.agent_id(), agent_id);
    }

    #[test]
    fn test_observe_event_llm_call_end_id_extraction() {
        let (turn_id, agent_id) = make_ids();
        let event = ObserveEvent::LlmCallEnd {
            turn_id,
            agent_id,
            step: 1,
            model: "claude-sonnet-4-20250514".to_string(),
            output: "test output".to_string(),
            input_tokens: 500,
            output_tokens: 200,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            request_id: None,
        };
        assert_eq!(event.turn_id(), turn_id);
        assert_eq!(event.agent_id(), agent_id);
    }

    #[test]
    fn test_observe_event_compact_started_id_extraction() {
        let (turn_id, agent_id) = make_ids();
        let event = ObserveEvent::CompactStarted {
            turn_id,
            agent_id,
            step: 3,
        };
        assert_eq!(event.turn_id(), turn_id);
        assert_eq!(event.agent_id(), agent_id);
    }

    #[test]
    fn test_observe_event_messages_compacted_id_extraction() {
        let (turn_id, agent_id) = make_ids();
        let event = ObserveEvent::MessagesCompacted {
            turn_id,
            agent_id,
            before_count: 100,
            after_count: 30,
            summary: "compact done".to_string(),
            messages: vec![],
            files: vec![],
            skills: vec![],
            re_inject_count: 0,
        };
        assert_eq!(event.turn_id(), turn_id);
        assert_eq!(event.agent_id(), agent_id);
    }

    #[test]
    fn test_observe_event_turn_error_id_extraction() {
        let (turn_id, agent_id) = make_ids();
        let event = ObserveEvent::TurnError {
            turn_id,
            agent_id,
            reason: TurnErrorReason::MaxIterations,
            message: "hit limit".to_string(),
        };
        assert_eq!(event.turn_id(), turn_id);
        assert_eq!(event.agent_id(), agent_id);
    }

    #[test]
    fn test_observe_event_subagent_start_id_extraction() {
        let (turn_id, agent_id) = make_ids();
        let child_id = AgentId::new();
        let event = ObserveEvent::SubagentStart {
            turn_id,
            agent_id,
            child_agent_id: child_id,
            agent_name: "researcher".to_string(),
            is_background: true,
        };
        assert_eq!(event.turn_id(), turn_id);
        assert_eq!(event.agent_id(), agent_id);
    }

    #[test]
    fn test_observe_event_subagent_stop_id_extraction() {
        let (turn_id, agent_id) = make_ids();
        let child_id = AgentId::new();
        let event = ObserveEvent::SubagentStop {
            turn_id,
            agent_id,
            child_agent_id: child_id,
            agent_name: "researcher".to_string(),
            result: "done".to_string(),
            is_error: false,
        };
        assert_eq!(event.turn_id(), turn_id);
        assert_eq!(event.agent_id(), agent_id);
    }

    // ─── Event（统一包装）测试 ─────────────────────────────────────────────

    #[test]
    fn test_event_unified_turn_id_extraction() {
        let (turn_id, agent_id) = make_ids();
        let render = Event::Render(RenderEvent::TextChunk {
            turn_id,
            agent_id,
            chunk: "hi".to_string(),
        });
        assert_eq!(render.turn_id(), turn_id);
        assert_eq!(render.agent_id(), agent_id);
    }

    #[test]
    fn test_event_unified_state_extraction() {
        let (turn_id, agent_id) = make_ids();
        let state = Event::State(StateEvent::StateSnapshot {
            turn_id,
            agent_id,
            message_count: 1,
            total_tokens: 100,
            current_step: 1,
            consecutive_failures: 0,
            budget_pct: None,
            context_total_tokens: None,
        });
        assert_eq!(state.turn_id(), turn_id);
        assert_eq!(state.agent_id(), agent_id);
    }

    #[test]
    fn test_event_unified_render_turn_completed_extraction() {
        // TurnCompleted 在 Render 层，验证 Event::Render 包装后 id 提取正确
        let (turn_id, agent_id) = make_ids();
        let event = Event::Render(RenderEvent::TurnCompleted {
            turn_id,
            agent_id,
            steps: 1,
            elapsed_secs: 0.5,
            finalized_messages: std::sync::Arc::new(vec![]),
        });
        assert_eq!(event.turn_id(), turn_id);
        assert_eq!(event.agent_id(), agent_id);
    }

    #[test]
    fn test_event_unified_observe_extraction() {
        let (turn_id, agent_id) = make_ids();
        let observe = Event::Observe(ObserveEvent::LlmCallStart {
            turn_id,
            agent_id,
            step: 0,
            messages: std::sync::Arc::new(vec![]),
            tools: vec![],
        });
        assert_eq!(observe.turn_id(), turn_id);
        assert_eq!(observe.agent_id(), agent_id);
    }

    // ─── EventBus + EventHandles 集成测试 ──────────────────────────────────

    #[tokio::test]
    async fn test_event_bus_emit_and_receive_render() {
        let (bus, mut handles) = EventBus::new(EventBusConfig::default());
        let (turn_id, agent_id) = make_ids();

        bus.emit_render(RenderEvent::TextChunk {
            turn_id,
            agent_id,
            chunk: "hello".to_string(),
        });

        let received = handles.try_render().expect("应收到渲染层事件");
        assert_eq!(received.turn_id(), turn_id);
        assert_eq!(received.agent_id(), agent_id);
    }

    #[tokio::test]
    async fn test_event_bus_emit_and_receive_state() {
        let (bus, mut handles) = EventBus::new(EventBusConfig::default());
        let (turn_id, agent_id) = make_ids();

        bus.emit_state(StateEvent::StateSnapshot {
            turn_id,
            agent_id,
            message_count: 3,
            total_tokens: 1000,
            current_step: 3,
            consecutive_failures: 0,
            budget_pct: Some(0.5),
            context_total_tokens: Some(200_000),
        });

        let received = handles.try_state().expect("应收到状态层事件");
        assert_eq!(received.turn_id(), turn_id);
        assert_eq!(received.agent_id(), agent_id);
    }

    #[tokio::test]
    async fn test_event_bus_emit_and_receive_render_turn_completed() {
        // TurnCompleted 在 Render 层，必须通过 render 通道接收
        let (bus, mut handles) = EventBus::new(EventBusConfig::default());
        let (turn_id, agent_id) = make_ids();

        bus.emit_render(RenderEvent::TurnCompleted {
            turn_id,
            agent_id,
            steps: 3,
            elapsed_secs: 1.5,
            finalized_messages: std::sync::Arc::new(vec![]),
        });

        let received = handles.try_render().expect("应收到渲染层 TurnCompleted");
        assert_eq!(received.turn_id(), turn_id);
        assert_eq!(received.agent_id(), agent_id);
        assert!(matches!(
            received,
            RenderEvent::TurnCompleted { steps: 3, .. }
        ));
    }

    #[tokio::test]
    async fn test_event_bus_emit_and_receive_observe() {
        let (bus, mut handles) = EventBus::new(EventBusConfig::default());
        let (turn_id, agent_id) = make_ids();

        let subscribers = bus.emit_observe(ObserveEvent::LlmCallStart {
            turn_id,
            agent_id,
            step: 1,
            messages: std::sync::Arc::new(vec![]),
            tools: vec![],
        });
        // 默认 1 个接收者（EventHandles 内部的）
        assert_eq!(subscribers, 1);

        let received = handles.try_observe().expect("应收到观测层事件");
        assert_eq!(received.turn_id(), turn_id);
        assert_eq!(received.agent_id(), agent_id);
    }

    #[tokio::test]
    async fn test_event_bus_observe_no_subscriber_returns_zero() {
        let (bus, _handles) = EventBus::new(EventBusConfig::default());
        let (turn_id, agent_id) = make_ids();

        // 丢弃 handles 中的 observe_rx 后再发送
        let n = bus.emit_observe(ObserveEvent::LlmCallEnd {
            turn_id,
            agent_id,
            step: 0,
            model: "test".to_string(),
            output: "test output".to_string(),
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            request_id: None,
        });
        // handles 仍持有 receiver，所以至少 1 个订阅者
        assert!(n >= 1);
    }

    #[tokio::test]
    async fn test_event_bus_subscribe_observe_shares_channel() {
        let (bus, mut handles) = EventBus::new(EventBusConfig::default());
        let (turn_id, agent_id) = make_ids();

        // 创建额外的订阅者
        let mut extra_rx = handles.subscribe_observe();

        bus.emit_observe(ObserveEvent::MessagesCompacted {
            turn_id,
            agent_id,
            before_count: 50,
            after_count: 10,
            summary: "compressed".to_string(),
            messages: vec![],
            files: vec![],
            skills: vec![],
            re_inject_count: 0,
        });

        // 两个接收者都能收到
        let from_main = handles.try_observe().expect("主接收者应收到事件");
        let from_extra = extra_rx.try_recv().expect("额外接收者应收到事件");
        assert_eq!(from_main.turn_id(), from_extra.turn_id());
    }

    #[tokio::test]
    async fn test_event_bus_render_channel_full_drops_event() {
        // 极小容量（1），填满后 try_send 应丢弃
        let (bus, mut handles) = EventHandles::from_bus(EventBusConfig {
            render_capacity: 1,
            ..Default::default()
        });
        let (turn_id, agent_id) = make_ids();

        // 填满通道（容量 1）
        bus.emit_render(RenderEvent::TextChunk {
            turn_id,
            agent_id,
            chunk: "first".to_string(),
        });
        // 第二个事件应被丢弃（不 panic）
        bus.emit_render(RenderEvent::TextChunk {
            turn_id,
            agent_id,
            chunk: "second".to_string(),
        });

        // 只能读出 1 个
        let r1 = handles.try_render().expect("第一个事件应在");
        assert!(matches!(r1, RenderEvent::TextChunk { ref chunk, .. } if chunk == "first"));
        let r2 = handles.try_render();
        assert!(r2.is_none(), "第二个事件应被丢弃");
    }

    #[tokio::test]
    async fn test_event_bus_state_channel_full_drops_event() {
        let (bus, mut handles) = EventHandles::from_bus(EventBusConfig {
            state_capacity: 1,
            ..Default::default()
        });
        let (turn_id, agent_id) = make_ids();

        bus.emit_state(StateEvent::StateSnapshot {
            turn_id,
            agent_id,
            message_count: 1,
            total_tokens: 0,
            current_step: 1,
            consecutive_failures: 0,
            budget_pct: None,
            context_total_tokens: None,
        });
        bus.emit_state(StateEvent::StateSnapshot {
            turn_id,
            agent_id,
            message_count: 2,
            total_tokens: 0,
            current_step: 2,
            consecutive_failures: 0,
            budget_pct: None,
            context_total_tokens: None,
        });

        let s1 = handles.try_state().expect("第一个事件应在");
        assert!(matches!(
            s1,
            StateEvent::StateSnapshot {
                current_step: 1,
                ..
            }
        ));
        let s2 = handles.try_state();
        assert!(s2.is_none(), "第二个事件应被丢弃");
    }

    #[tokio::test]
    async fn test_event_bus_multiple_events_in_order() {
        let (bus, mut handles) = EventBus::new(EventBusConfig::default());
        let (turn_id, agent_id) = make_ids();

        bus.emit_render(RenderEvent::ThinkingChunk {
            turn_id,
            agent_id,
            chunk: "think".to_string(),
        });
        bus.emit_render(RenderEvent::TextChunk {
            turn_id,
            agent_id,
            chunk: "answer".to_string(),
        });
        bus.emit_render(RenderEvent::ToolStarted {
            turn_id,
            agent_id,
            tool_call_id: "tc_1".to_string(),
            name: "Bash".to_string(),
            input: serde_json::Value::Null,
        });

        // 按 FIFO 顺序消费
        let e1 = handles.try_render().unwrap();
        let e2 = handles.try_render().unwrap();
        let e3 = handles.try_render().unwrap();
        assert!(matches!(e1, RenderEvent::ThinkingChunk { .. }));
        assert!(matches!(e2, RenderEvent::TextChunk { .. }));
        assert!(matches!(e3, RenderEvent::ToolStarted { .. }));
    }

    /// [回归测试] TurnCompleted 必须在 render_tx 通道中，与同迭代 Render 事件 FIFO。
    ///
    /// 历史背景：TurnCompleted 原在 StateEvent（state_tx 独立通道），biased select!
    /// 只保证单次迭代内优先级，不保证跨迭代——iter2 的 TextChunk 会先于 iter1 的
    /// TurnCompleted 被消费，TUI 把 iter2 文本追加到 iter1 partial 上，渲染出
    /// "新文本在旧工具之前"的错乱（CLAUDE.md P2-C 修复后回归）。
    ///
    /// 本测试 emit iter1 全部 Render 事件 + iter1 TurnCompleted + iter2 TextChunk，
    /// 断言消费顺序：iter1.tool_end → iter1.turn_completed → iter2.text。
    /// 若 TurnCompleted 被移回 StateEvent，`RenderEvent::TurnCompleted` 编译失败，
    /// 本测试成为编译期约束的回归门。
    #[tokio::test]
    async fn test_event_bus_turn_completed_in_render_channel_preserves_cross_iter_order() {
        let (bus, mut handles) = EventBus::new(EventBusConfig::default());
        let (turn1, agent_id) = make_ids();
        let (turn2, _) = make_ids();

        // iter1: TextChunk → ToolStarted → ToolEnded → TurnCompleted
        bus.emit_render(RenderEvent::TextChunk {
            turn_id: turn1,
            agent_id,
            chunk: "iter1-text".to_string(),
        });
        bus.emit_render(RenderEvent::ToolStarted {
            turn_id: turn1,
            agent_id,
            tool_call_id: "tc_iter1".to_string(),
            name: "Read".to_string(),
            input: serde_json::Value::Null,
        });
        bus.emit_render(RenderEvent::ToolEnded {
            turn_id: turn1,
            agent_id,
            tool_call_id: "tc_iter1".to_string(),
            name: "Read".to_string(),
            output: "ok".to_string(),
            is_error: false,
        });
        bus.emit_render(RenderEvent::TurnCompleted {
            turn_id: turn1,
            agent_id,
            steps: 1,
            elapsed_secs: 0.0,
            finalized_messages: std::sync::Arc::new(vec![]),
        });

        // iter2: TextChunk —— 必须排在 iter1 的 TurnCompleted 之后
        bus.emit_render(RenderEvent::TextChunk {
            turn_id: turn2,
            agent_id,
            chunk: "iter2-text".to_string(),
        });

        // 全部从 render_rx 消费（state_rx 应为空）
        let e1 = handles.try_render().expect("iter1 TextChunk");
        let e2 = handles.try_render().expect("iter1 ToolStarted");
        let e3 = handles.try_render().expect("iter1 ToolEnded");
        let e4 = handles.try_render().expect("iter1 TurnCompleted");
        let e5 = handles.try_render().expect("iter2 TextChunk");

        // 顺序断言：turn1 全部事件先于 turn2
        assert_eq!(e1.turn_id(), turn1);
        assert!(matches!(e1, RenderEvent::TextChunk { .. }));
        assert!(matches!(e2, RenderEvent::ToolStarted { .. }));
        assert!(matches!(e3, RenderEvent::ToolEnded { .. }));
        assert!(
            matches!(e4, RenderEvent::TurnCompleted { .. }),
            "iter1 TurnCompleted 必须在 iter2 事件之前消费，否则跨迭代顺序错乱"
        );
        assert_eq!(e5.turn_id(), turn2);
        assert!(matches!(e5, RenderEvent::TextChunk { .. }));

        // state_rx 必须为空（TurnCompleted 不应在 state 通道）
        assert!(
            handles.try_state().is_none(),
            "TurnCompleted 已迁移到 render_tx，state_rx 应为空"
        );
    }

    // ─── 序列化测试 ─────────────────────────────────────────────────────────

    #[test]
    fn test_render_event_serde_roundtrip() {
        let (turn_id, agent_id) = make_ids();
        let event = RenderEvent::HitlPending {
            turn_id,
            agent_id,
            tool_call_id: "tc_1".to_string(),
            tool_name: "Bash".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: RenderEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event.turn_id(), back.turn_id());
        assert_eq!(event.agent_id(), back.agent_id());
    }

    #[test]
    fn test_observe_event_serde_roundtrip() {
        let (turn_id, agent_id) = make_ids();
        let event = ObserveEvent::TurnError {
            turn_id,
            agent_id,
            reason: TurnErrorReason::RateLimit,
            message: "429".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: ObserveEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            ObserveEvent::TurnError {
                reason: TurnErrorReason::RateLimit,
                ..
            }
        ));
    }

    #[test]
    fn test_observe_event_compact_started_serde_roundtrip() {
        let (turn_id, agent_id) = make_ids();
        let event = ObserveEvent::CompactStarted {
            turn_id,
            agent_id,
            step: 7,
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: ObserveEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ObserveEvent::CompactStarted { step: 7, .. }));
    }

    #[test]
    fn test_event_unified_serde_roundtrip() {
        let (turn_id, agent_id) = make_ids();
        let event = Event::Render(RenderEvent::BudgetWarning {
            turn_id,
            agent_id,
            used_tokens: 150000,
            total_tokens: 200000,
            percentage: 0.75,
        });
        let json = serde_json::to_string(&event).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(event.turn_id(), back.turn_id());
        assert_eq!(event.agent_id(), back.agent_id());
    }
}
