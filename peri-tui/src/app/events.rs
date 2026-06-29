use peri_agent::interaction::{InteractionContext, InteractionResponse};
use tokio::sync::oneshot;

// P4b: re-export DTO 替代 peri_middlewares::mcp::OAuthCallbackResult
pub use peri_acp_types::mcp_types::OAuthCallbackResultDto as OAuthCallbackResult;

/// TUI 与后台 Agent 任务之间的通信事件（通过 mpsc channel 传递）
pub enum AgentEvent {
    /// 工具调用开始（参数已就绪）
    ToolStart {
        tool_call_id: String,
        name: String,
        display: String,
        args: String,
        input: serde_json::Value,
        source_agent_id: Option<String>,
    },
    /// 工具调用结果
    ToolEnd {
        tool_call_id: String,
        name: String,
        output: String,
        is_error: bool,
        source_agent_id: Option<String>,
    },
    AssistantChunk {
        chunk: String,
        source_agent_id: Option<String>,
    },
    /// AI 推理/思考内容（与文本内容分开）
    AiReasoning(String),
    Done,
    Error(String),
    /// 用户中断（Ctrl+C），工具已以 error 结尾，消息已持久化
    Interrupted,
    /// 统一人机交互请求（HITL 审批 / AskUser 问答）
    InteractionRequest {
        ctx: InteractionContext,
        response_tx: oneshot::Sender<InteractionResponse>,
    },
    /// Todo 列表更新
    TodoUpdate(Vec<peri_acp::event::TodoItemDto>),
    /// Agent 执行结束后的消息快照（用于多轮对话续接）
    StateSnapshot(Vec<peri_agent::messages::BaseMessage>),
    /// 单次 ReAct 迭代提交信号（v2 路径专用）
    ///
    /// 每个 Act 阶段结束时由 v2 stages 触发，携带当前 transcript 全量快照。
    /// Pipeline 据此调用 `commit_iteration(messages)`：将 `transcript` 替换为
    /// 权威快照、清空 `partial`，从而消除多迭代场景下文本/工具顺序错乱。
    TurnCommitted {
        messages: Vec<peri_agent::messages::BaseMessage>,
        steps: usize,
    },
    /// 轻量级状态快照元数据（v2 路径专用）
    ///
    /// v2 `StateEvent::StateSnapshot` 经 ACP 桥接投递。**不应**清空 MessagePipeline 状态，
    /// 仅用于刷新上下文使用率、步数等元数据。
    StateSnapshotMeta {
        message_count: usize,
        total_tokens: u64,
        current_step: usize,
        consecutive_failures: u32,
        budget_pct: Option<f64>,
        context_total_tokens: Option<u64>,
    },
    /// Compact 开始（来自 executor 或手动 /compact）
    CompactStarted,
    /// 上下文压缩完成，携带摘要、保留的文件和 skill 信息
    CompactCompleted {
        summary: String,
        files: Vec<peri_acp::event::CompactFileInfoDto>,
        skills: Vec<String>,
        micro_cleared: usize,
        /// 压缩后的新消息列表
        messages: Vec<peri_agent::messages::BaseMessage>,
    },
    /// 上下文压缩失败，携带错误信息
    CompactError(String),
    /// 对话回退完成（rewind 命令）
    RewindCompleted {
        summary: String,
        messages: Vec<peri_agent::messages::BaseMessage>,
    },
    /// SubAgent 生命周期事件（中间件发出，用于 UI 状态同步）
    ///
    /// 在 SubAgent 实际开始/停止执行时由 SubAgentMiddleware 发出。
    /// 不修改 pipeline 状态，仅用于触发 spinner 更新 + RebuildAll 刷新显示。
    SubagentLifecycle {
        agent_name: String,
        started: bool,
    },
    /// SubAgent 开始执行（由 SubagentStarted 映射而来，携带唯一实例 ID）
    SubAgentStart {
        agent_id: String,
        /// 唯一实例标识符（并发同类型 SubAgent 路由用）
        instance_id: String,
        task_preview: String,
        is_background: bool,
    },
    /// SubAgent 执行结束
    SubAgentEnd {
        result: String,
        is_error: bool,
        agent_id: Option<String>,
        /// 唯一实例标识符
        instance_id: Option<String>,
    },
    /// Token 使用量更新（从 enriched UsageUpdate _meta 解析而来）
    TokenUsageUpdate {
        usage: peri_acp::event::TokenUsageDto,
        model: String,
        /// LLM 响应停止原因
        stop_reason: Option<peri_acp::event::StopReasonDto>,
    },
    /// LLM 调用重试中（从核心层 LlmRetrying 映射而来）
    LlmRetrying {
        attempt: usize,
        max_attempts: usize,
        delay_ms: u64,
        error: String,
    },
    /// 上下文使用警告（从核心层 ContextWarning 映射而来）
    ContextWarning {
        used_tokens: u64,
        total_tokens: u64,
        percentage: f64,
    },
    /// OAuth 授权需要用户交互（打开浏览器或手动粘贴回调 URL）
    OAuthAuthorizationNeeded {
        server_name: String,
        /// 浏览器授权 URL
        authorization_url: String,
        /// 回调通道：用户粘贴的 URL 或授权结果通过此通道传回后台
        callback_tx: oneshot::Sender<OAuthCallbackResult>,
    },
    /// OAuth 授权完成
    OAuthAuthorizationCompleted {
        server_name: String,
    },
    /// OAuth 授权失败
    OAuthAuthorizationFailed {
        server_name: String,
        error: String,
    },
    /// 后台 agent 任务完成通知
    BackgroundTaskCompleted {
        task_id: String,
        agent_name: String,
        success: bool,
        output: String,
        tool_calls_count: usize,
        duration_ms: u64,
        /// 子 agent 唯一实例 ID（child_thread_id / uuid7），用于精确匹配并发同类型后台 agent
        child_thread_id: Option<String>,
    },
    /// MCP 面板异步操作完成
    McpActionCompleted {
        server_name: String,
        action: String,
        success: bool,
    },
    /// 插件操作完成（安装/卸载/更新）
    PluginActionCompleted {
        plugin_id: String,
        action: String,
        success: bool,
        message: String,
    },
    /// LSP 诊断更新（被动推送）
    LspDiagnostics {
        errors: usize,
        warnings: usize,
        files_with_errors: usize,
    },
    /// 后台 agent 工具调用进度（轻量级，仅用于 bg_agent_bar 实时计数）
    BgToolStep {
        child_thread_id: String,
    },
    /// Workflow 进度更新（WorkflowRunner 发出，用于 WorkflowPanel 渲染）
    WorkflowProgress(peri_acp::event::WorkflowProgressDto),
}
