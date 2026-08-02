//! Event mapping from ExecutorEvent to ACP SessionUpdate and peri/agent_event routing.
//!
//! [`AcpEvent`] is the DTO that replaces raw `ExecutorEvent` serialization on the
//! `peri/agent_event` channel. It contains only the fields that TUI consumers need,
//! avoiding a direct `peri_agent::agent::events::ExecutorEvent` dependency in the TUI.

pub mod forwarder;
pub mod mapper;

pub(crate) use forwarder::spawn_eventbus_forwarder;
pub use mapper::{map_event, MappedEvent};
pub use peri_acp_types::summary::{
    CompactFileInfoDto, StopReasonDto, TodoItemDto, TodoStatusDto, TokenUsageDto,
    WorkflowProgressDto,
};
pub use peri_agent::agent::events_v2_mapper::{
    observe_event_to_executor, render_event_to_executor, state_event_to_executor, V2Event,
};

use serde::{Deserialize, Serialize};

/// ACP event DTO — replaces raw `ExecutorEvent` on the `peri/agent_event` channel.
///
/// Contains only fields needed by TUI/IDE consumers. No `BaseMessage`,
/// `MessageId`, or other internal `peri_agent` types leak through.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum AcpEvent {
    /// State snapshot (complete message history) — messages serialized as JSON strings.
    /// TUI deserializes via `serde_json::from_str::<Vec<BaseMessage>>`.
    StateSnapshot {
        /// JSON-serialized `Vec<BaseMessage>`
        messages_json: String,
    },
    /// 单次 ReAct 迭代提交信号（v2 路径专用）
    ///
    /// v2 `StateEvent::TurnCompleted` 携带 `finalized_messages` → mapper_v2 转为
    /// `ExecutorEvent::TurnCommitted` → 本 DTO。TUI 据此调用
    /// `MessagePipeline::commit_iteration(messages)` 同步规范状态。
    ///
    /// 与 `StateSnapshot` 区别：`StateSnapshot` 用于完整快照（如 compact 后），
    /// `TurnCommitted` 用于每个 ReAct 迭代边界的高频提交（含工具路径与回答路径）。
    TurnCommitted {
        /// JSON-serialized `Vec<BaseMessage>` (当前 transcript 的可见消息全量快照)
        messages_json: String,
        /// 当前 ReAct 步数
        steps: usize,
    },
    /// 轻量级状态快照元数据（v2 路径专用，不携带消息列表）
    ///
    /// v2 `StateEvent::StateSnapshot` 经 mapper_v2 → ExecutorEvent::StateSnapshotMeta →
    /// 本 DTO 投递。TUI 据此刷新上下文使用率/步数，**不应**清空消息历史。
    StateSnapshotMeta {
        /// 可见消息数
        message_count: usize,
        /// 累计 token 数（v2 暂为 0）
        total_tokens: u64,
        /// 当前 ReAct 步数
        current_step: usize,
        /// 连续失败次数
        consecutive_failures: u32,
        /// 上下文窗口使用率（0.0-1.0）
        budget_pct: Option<f64>,
        /// 上下文窗口总量
        context_total_tokens: Option<u64>,
    },
    /// SubAgent started executing
    SubagentStarted {
        agent_name: String,
        instance_id: String,
        is_background: bool,
    },
    /// SubAgent execution completed
    SubagentStopped {
        agent_name: String,
        result: String,
        is_error: bool,
        instance_id: String,
    },
    /// Context compaction started
    CompactStarted,
    /// Context compaction completed
    CompactCompleted {
        summary: String,
        files: Vec<CompactFileInfoDto>,
        skills: Vec<String>,
        micro_cleared: usize,
        /// JSON-serialized `Vec<BaseMessage>` (the new message list after compact)
        messages_json: String,
        /// 压缩策略: "micro" | "full" | "smart"
        strategy: String,
        /// Compact 执行的语义结果
        outcome: String,
    },
    /// Context compaction failed
    CompactError { message: String },
    /// Rewind completed
    RewindCompleted {
        summary: String,
        /// JSON-serialized `Vec<BaseMessage>` (messages after rewind)
        messages_json: String,
    },
    /// Rewind failed (target message not found / argument parse error)
    RewindError { message: String },
    /// Background agent task completed
    BackgroundTaskCompleted {
        task_id: String,
        agent_name: String,
        success: bool,
        output: String,
        tool_calls_count: usize,
        duration_ms: u64,
        child_thread_id: Option<String>,
    },
    /// Background agent tool call progress
    BgToolStep { child_thread_id: String },
    /// LSP diagnostics update
    LspDiagnostics {
        errors: usize,
        warnings: usize,
        files_with_errors: usize,
    },
    /// Agent execution failed
    AgentExecutionFailed { message: String },
    /// Context window usage warning
    ContextWarning {
        used_tokens: u64,
        total_tokens: u64,
        percentage: f64,
    },
    /// LLM call retrying
    LlmRetrying {
        attempt: usize,
        max_attempts: usize,
        delay_ms: u64,
        error: String,
    },
    /// Workflow progress update
    WorkflowProgress {
        run_id: String,
        workflow_name: String,
        event_type: String,
        agent_id: Option<u64>,
        phase: Option<String>,
        label: Option<String>,
        agent_status: Option<String>,
        token_count: Option<u64>,
        tool_count: Option<u64>,
        run_status: Option<String>,
        message: Option<String>,
    },
}

#[cfg(test)]
mod variant_coverage_test;
