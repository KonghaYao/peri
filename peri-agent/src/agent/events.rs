/// 后台任务完成通知（注入到主 agent 消息流中）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BackgroundTaskResult {
    pub task_id: String,
    pub agent_name: String,
    pub prompt_summary: String,
    pub success: bool,
    pub output: String,
    pub tool_calls_count: usize,
    pub duration_ms: u64,
    /// 后台任务是否因超时被终止（进程组已终止，逃逸子进程可能存活）
    #[serde(default)]
    pub timed_out: bool,
    /// SQLite child thread ID（uuid7），用于 TUI 聚焦时 load_messages
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_thread_id: Option<String>,
}

impl BackgroundTaskResult {
    /// 格式化为注入到 LLM 消息流的通知文本
    pub fn to_notification(&self) -> String {
        let short_id = &self.task_id[..8.min(self.task_id.len())];
        if self.success {
            format!(
                "[后台任务 {} 已完成] Agent: {} | 工具调用: {} | 耗时: {}ms\n结果:\n{}",
                short_id, self.agent_name, self.tool_calls_count, self.duration_ms, self.output,
            )
        } else if self.timed_out {
            format!(
                "[后台任务 {} 超时被终止]（进程组已终止，逃逸子进程可能存活） Agent: {}\n错误:\n{}",
                short_id, self.agent_name, self.output,
            )
        } else {
            format!(
                "[后台任务 {} 执行失败] Agent: {}\n错误:\n{}",
                short_id, self.agent_name, self.output,
            )
        }
    }
}

/// Compact 保留的文件信息摘要
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CompactFileInfo {
    pub path: String,
    pub lines: usize,
}

/// Todo 列表条目（用于 ExecutorEvent::TodoUpdate）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct TodoEntry {
    pub content: String,
    #[serde(
        default,
        rename = "activeForm",
        skip_serializing_if = "Option::is_none"
    )]
    pub active_form: Option<String>,
    pub status: TodoStatus,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

/// Workflow 进度更新载荷（从 WorkflowRunner 推送到 TUI）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkflowProgressPayload {
    /// Run ID (UUID v7)
    pub run_id: String,
    /// Workflow 名称
    pub workflow_name: String,
    /// 事件类型（run_started / phase_started / phase_done / agent_started / agent_progress / agent_done / run_done）
    pub event_type: String,
    /// Agent ID（仅 agent_* 事件有值）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<u64>,
    /// Phase 名称（仅 phase_* 事件有值）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// Agent 标签
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Agent 状态（started/progress/done/dead/skipped）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_status: Option<String>,
    /// Token 计数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_count: Option<u64>,
    /// 工具调用计数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_count: Option<u64>,
    /// Run 状态（completed/failed/killed）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_status: Option<String>,
    /// 日志消息
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// ReAct 循环 4 阶段
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Compact,
    Receive,
    Reason,
    Act,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StageStatus {
    Done,
    Skipped,
    Error,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    Done,
    Interrupted,
    Error,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TurnErrorKind {
    Interrupted,
    Timeout,
    LlmFailure,
    ToolFailure,
    RateLimit,
    MaxIterations,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompactStrategy {
    /// 跳过 compact（cache-aware delay 或预算充足）
    Skip,
    Micro,
    Full,
    Smart,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompactTrigger {
    Auto,
    Manual,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompactThreshold {
    Micro,
    Full,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MiddlewareHook {
    BeforeAgent,
    AfterAgent,
    BeforeTool,
    AfterTool,
    BeforeModel,
    AfterModel,
    OnError,
    OnSessionStart,
    OnSessionEnd,
    OnUserPrompt,
    BeforeCompact,
    AfterCompact,
    OnPermissionRequest,
    OnSubagentStart,
    OnSubagentStop,
    OnTurnEnd,
    OnNotification,
}

/// Agent 执行过程中的增量事件
///
/// 历史名 `AgentEvent`，因与 `peri-tui::app::events::AgentEvent` 同名造成歧义，
/// 重命名为 `ExecutorEvent`（更准确地反映其作为 executor 层事件类型的角色）。
/// serde tag/content 序列化不含 enum 名，wire format 零变化。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ExecutorEvent {
    /// AI 推理内容（reasoning/思考过程）
    AiReasoning {
        text: String,
        source_agent_id: Option<String>,
    },
    /// LLM 输出最终文字（非流式，整段答案），携带所属 AI 消息的 message_id
    TextChunk {
        message_id: crate::messages::MessageId,
        chunk: String,
        source_agent_id: Option<String>,
    },
    /// 工具调用开始（工具名 + 参数），携带所属 AI 消息的 message_id
    ToolStart {
        message_id: crate::messages::MessageId,
        tool_call_id: String,
        name: String,
        input: serde_json::Value,
        source_agent_id: Option<String>,
    },
    /// 工具调用结束（结果或错误），携带所属 AI 消息的 message_id
    ToolEnd {
        message_id: crate::messages::MessageId,
        tool_call_id: String,
        name: String,
        output: String,
        is_error: bool,
        source_agent_id: Option<String>,
    },
    /// 状态快照（含完整的消息历史），用于持久化和断点续跑
    StateSnapshot(Vec<crate::messages::BaseMessage>),
    /// 单次 ReAct 迭代提交信号（v2）
    ///
    /// 由 v2 stages 在每次 Act 阶段结束时通过 `RenderEvent::TurnCompleted` 触发，
    /// mapper_v2 将 `finalized_messages` 透传为本变体的 `messages` 字段。
    ///
    /// TUI 据此调用 `MessagePipeline::commit_iteration(messages)` 同步规范状态，
    /// 避免 Render 事件流自洽重建 transcript 时多迭代文本/工具顺序错乱。
    TurnCommitted {
        /// 当前 transcript 的可见消息全量快照。
        ///
        /// Arc 共享引用——Clone ExecutorEvent 时为浅拷贝（引用计数 +1），
        /// 避免事件管道多级转发时的全量深拷贝（与 `LlmCallStart.messages`
        /// 同一模式）。serde 序列化结果与 `Vec<BaseMessage>` 完全一致。
        messages: std::sync::Arc<Vec<crate::messages::BaseMessage>>,
        /// 当前 ReAct 步数
        steps: usize,
    },
    /// 轻量级状态快照元数据（v2 路径专用）
    ///
    /// v2 `StateEvent::StateSnapshot` 不携带消息历史（设计上避免 transcript 锁开销），
    /// mapper_v2 将其映射为本变体。TUI 据此区分「元数据快照」与「完整快照」：
    /// 收到 `StateSnapshotMeta` 时**不应**清空 `MessagePipeline::completed`，
    /// 仅用于刷新上下文使用率、步数等元数据。
    StateSnapshotMeta {
        /// 当前可见消息数（transcript.read().len()）
        message_count: usize,
        /// 累计 token 数（来自 token_tracker，v2 暂为 0）
        total_tokens: u64,
        /// 当前 ReAct 步数
        current_step: usize,
        /// 连续工具失败次数
        consecutive_failures: u32,
        /// 上下文窗口使用率（0.0-1.0），None 表示无 context_budget
        budget_pct: Option<f64>,
        /// 上下文窗口总量（ContextBudget.context_window），None 表示无配置
        context_total_tokens: Option<u64>,
    },
    /// 增量消息（BaseMessage），持久化和遥测的最小数据单元
    MessageAdded(crate::messages::BaseMessage),
    /// LLM 调用开始（携带完整 input messages 快照 + 工具定义，用于 Langfuse Generation）
    LlmCallStart {
        step: usize,
        /// Arc 共享引用——Clone ExecutorEvent 时为浅拷贝（引用计数 +1），不产生独立副本
        messages: std::sync::Arc<Vec<crate::messages::BaseMessage>>,
        tools: Vec<crate::tools::ToolDefinition>,
    },
    /// LLM Provider 实际请求体（raw body），紧随 [`Self::LlmCallStart`] 之后 emit。
    ///
    /// 用于 Langfuse Generation input：携带 Provider-native 完整请求体（含正确工具
    /// 格式与 system 位置）。tracer 在 `on_llm_start` 建 generation_data 缓存后写入
    /// `raw_body` 字段；`on_llm_end` 时优先用 raw_body，fallback 到 messages+tools
    /// 抽象序列化。`Arc<Value>` 浅拷贝，跨多层转发不重复 clone 大 JSON。
    LlmRequestPayload {
        step: usize,
        body: std::sync::Arc<serde_json::Value>,
    },
    /// LLM 调用结束（携带模型名、输出文本、token 使用量）
    LlmCallEnd {
        step: usize,
        model: String,
        output: String,
        usage: Option<peri_model::TokenUsage>,
        /// LLM 响应停止原因（None 表示 LLM 调用失败/异常）
        stop_reason: Option<peri_model::StopReason>,
        /// Provider 请求 ID（迁移后从 TokenUsage 提升为事件字段，避免随 usage 丢失）
        request_id: Option<String>,
    },
    /// 上下文窗口使用警告（阈值触发时发出）
    ContextWarning {
        used_tokens: u64,
        total_tokens: u64,
        percentage: f64,
    },
    /// LLM 调用重试中
    LlmRetrying {
        attempt: usize,
        max_attempts: usize,
        delay_ms: u64,
        error: String,
    },
    /// 后台 agent 任务完成（TUI 使用，用于空闲时通知）
    BackgroundTaskCompleted(BackgroundTaskResult),
    /// 子 agent 开始执行
    SubagentStarted {
        agent_name: String,
        /// 唯一实例标识符（用于并发同类型 SubAgent 路由）
        instance_id: String,
        /// 是否为后台模式（run_in_background）
        is_background: bool,
    },
    /// 子 agent 执行完成
    SubagentStopped {
        agent_name: String,
        result: String,
        is_error: bool,
        /// 唯一实例标识符
        instance_id: String,
    },
    /// 上下文压缩开始
    CompactStarted {
        /// 所属 Turn ID
        turn_id: String,
        /// 触发压缩的 Agent ID
        agent_id: String,
        /// 当前 ReAct 步数
        step: usize,
        /// 压缩策略
        strategy: CompactStrategy,
        /// 压缩触发方式
        trigger: CompactTrigger,
    },
    /// 上下文压缩完成
    CompactCompleted {
        /// 摘要文本（full compact 时非空，micro compact 时为空）
        summary: String,
        /// 保留的文件摘要列表
        files: Vec<CompactFileInfo>,
        /// 保留的 Skill 名称列表
        skills: Vec<String>,
        /// micro-compact 清除的工具结果数量（>0 表示 micro-compact）
        micro_cleared: usize,
        /// 压缩后的新消息列表（full compact 时非空）
        messages: Vec<crate::messages::BaseMessage>,
        /// 压缩前 token 数
        token_before: u64,
        /// 压缩后 token 数
        token_after: u64,
        /// 本次使用的压缩策略
        strategy: CompactStrategy,
        /// 受影响的消息数量（v2 compact 操作计数）
        #[serde(default)]
        affected_count: usize,
        /// 估算节省的 token 数量（v2 compact projection 估算）
        #[serde(default)]
        estimated_tokens_saved: u64,
        /// 压缩前估算 token 数（ContextPressure.estimated_tokens）
        #[serde(default)]
        estimated_tokens_before: u64,
        /// 压缩后估算 token 数（estimated_tokens_before - estimated_tokens_saved）
        #[serde(default)]
        estimated_tokens_after: u64,
        /// 被修改的消息数量（v2 projection 变更计数）
        #[serde(default)]
        changed_messages: usize,
        /// 被修改的字段数量（v2 projection 字段级变更计数）
        #[serde(default)]
        changed_fields: usize,
        /// 无操作候选数量（projection 判定无需变更的消息数）
        #[serde(default)]
        no_op_candidates: usize,
        /// 升级到 Full Compact 的原因（Micro/Smart 时为 None）
        #[serde(default)]
        full_escalation_reason: Option<crate::agent::compact_v2::planner::FullEscalationReason>,
        /// 压缩前缓存命中率（0.0-1.0）
        #[serde(default)]
        cache_hit_rate_before: f64,
        /// Compact 执行的语义结果
        outcome: crate::agent::compact_v2::CompactOutcome,
    },
    /// 对话回退完成（rewind 命令，移除目标用户消息及其之后的所有消息）
    RewindCompleted {
        /// 摘要文本（如"已回滚 N 条消息"）
        summary: String,
        /// 回退后的新消息列表（目标消息之前，不含目标本身）
        messages: Vec<crate::messages::BaseMessage>,
    },
    /// 对话回退失败（rewind 目标消息不存在 / 参数解析失败）
    ///
    /// 与 [`Self::CompactError`] 分开：rewind 失败与上下文压缩无关，
    /// 复用 CompactError 会让 TUI 渲染压缩语境、langfuse 误报压缩失败。
    RewindError {
        message: String,
    },
    /// 上下文压缩失败
    CompactError {
        message: String,
    },
    /// Agent 执行失败（LLM API 错误等致命错误，TUI 显示红色 SystemNote）
    AgentExecutionFailed {
        message: String,
    },
    /// Todo 列表更新
    TodoUpdate(Vec<TodoEntry>),
    /// LSP 诊断更新
    LspDiagnostics {
        errors: usize,
        warnings: usize,
        files_with_errors: usize,
    },

    /// 后台 agent 工具调用进度通知（轻量级，仅用于 TUI bg_agent_bar 实时计数）
    BgToolStep {
        child_thread_id: String,
    },
    /// Workflow 进度更新（WorkflowRunner 发出，TUI 消费渲染面板）
    WorkflowProgress(WorkflowProgressPayload),
    // ── langfuse v2：会话/Turn 生命周期 ──
    SessionStarted {
        session_id: String,
        frozen_summary: serde_json::Value,
    },
    TurnStarted {
        turn_id: String,
        session_id: String,
    },
    TurnEnded {
        turn_id: String,
        session_id: String,
        status: TurnStatus,
        error_kind: Option<TurnErrorKind>,
    },
    // ── langfuse v2：中间件链 ──
    MiddlewareStarted {
        turn_id: String,
        mw_name: String,
        hook: MiddlewareHook,
    },
    MiddlewareEnded {
        turn_id: String,
        mw_name: String,
        hook: MiddlewareHook,
        status: StageStatus,
        error: Option<String>,
    },
    // ── langfuse v2：Compact ──
    BudgetThresholdHit {
        turn_id: String,
        threshold: CompactThreshold,
        current_pct: f64,
        tokens_in: u64,
        tokens_out: u64,
    },
    // ── langfuse v2：Act / Workflow ──
    WorkflowStarted {
        turn_id: String,
        workflow_id: String,
        plan_summary: String,
    },
    WorkflowEnded {
        turn_id: String,
        workflow_id: String,
        agents_spawned: usize,
        tool_calls: usize,
    },
}

/// 事件回调 trait（应用层实现）
///
/// 在 Agent 执行过程中，关键节点会调用 `on_event`。
/// 实现者通过 `mpsc::Sender` 等机制将事件转发给 UI 层。
pub trait AgentEventHandler: Send + Sync {
    fn on_event(&self, event: ExecutorEvent);
}

/// 函数闭包适配器 —— 方便快速实现 `AgentEventHandler`
///
/// # 示例
/// ```rust,ignore
/// let tx = tx.clone();
/// let handler = FnEventHandler(move |event| {
///     let _ = tx.try_send(event);
/// });
/// executor.with_event_handler(Arc::new(handler))
/// ```
pub struct FnEventHandler<F>(pub F)
where
    F: Fn(ExecutorEvent) + Send + Sync;

impl<F> AgentEventHandler for FnEventHandler<F>
where
    F: Fn(ExecutorEvent) + Send + Sync,
{
    fn on_event(&self, event: ExecutorEvent) {
        (self.0)(event)
    }
}

#[cfg(test)]
#[path = "events_test.rs"]
mod tests;
