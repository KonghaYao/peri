//! Event mapping from ExecutorEvent to ACP SessionUpdate and peri/agent_event routing.
//!
//! [`AcpEvent`] is the DTO that replaces raw `ExecutorEvent` serialization on the
//! `peri/agent_event` channel. It contains only the fields that TUI consumers need,
//! avoiding a direct `ExecutorEvent` dependency in the TUI.

mod forwarder;
#[cfg(test)]
mod forwarder_test;
pub mod mapper;

pub(crate) use self::forwarder::spawn_eventbus_forwarder;
pub use mapper::{map_event, MappedEvent};
pub use peri_acp_types::summary::{
    CompactFileInfoDto, StopReasonDto, TodoItemDto, TodoStatusDto, TokenUsageDto,
    WorkflowProgressDto,
};
// v1 兼容映射（v2 → ExecutorEvent）保留在 ACP 协议面
// `peri_acp_types::event_v2`（`2026-07-18-events-v2-mapper-removal.md`：
// events_v2_mapper 模块已退役；3.0 M-event-chain + 批 2「v1-retire」：
// Agent 层发射统一 v2（EventBus），v1 `ExecutorEvent` 中间态退役、仅保留为
// 协议序列化面载体——发射点经 `Controller::publish_event`
// （Controller → Runtime 补打身份 → 弹出队列 + 订阅广播），
// 协议化消费经 `Controller::subscribe` / `pop_events` 订阅——事件
// 三层化的统一出口在 Controller，见 `event/forwarder.rs` 与
// 事件泵（`peri-agent::session::exec::executor_helpers::spawn_event_pump`，
// 经 `peri_acp_types::event::{EventPublisher, EventSubscriber}` 端口接入））。
pub use peri_acp_types::event_v2::{
    observe_event_to_executor, render_event_to_executor, state_event_to_executor,
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
    /// Turn 已挂起等待异步事件（bg agent/cron/workflow）。
    ///
    /// v2 `StateEvent::TurnSuspended` → ExecutorEvent::TurnSuspended → 本 DTO。
    /// TUI 收到后归档 current_turn、停止 loading spinner。
    ///
    /// `turn_id` / `agent_id` 为 v2 事件透传的身份（v1 兼容层最小身份载体）。
    TurnSuspended { turn_id: String, agent_id: String },
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
        /// 压缩触发方式: "auto" | "manual"（旧事件缺省视为 "auto"）
        #[serde(default = "default_compact_trigger")]
        trigger: String,
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
    /// System-level notification text（MCP 上下线等连接状态变化）。
    ///
    /// TUI 经 peri/agent_event 通道解码为 `AcpEventData::SystemNotification`
    /// 显示为系统通知；level: "info" | "warn" | "error"。
    SystemNotification { text: String, level: String },
    /// MCP OAuth 授权需要用户交互（`oauth-needed`）。TUI 解码为
    /// `AcpEventData::OauthNeeded` 打开 OAuthPopup（`OAUTH_INFO` atom）。
    ///
    /// 发射点：host 装配面 `oauth_event_callback`（`AuthorizationNeeded` 事件），
    /// 非 agent 执行路径——经 host 级通道（`AcpServerConfig::oauth_event_tx`）
    /// 直达 `peri/agent_event` 通知，不依赖 session event_sink。
    OauthNeeded {
        server_name: String,
        auth_url: String,
    },
    /// MCP OAuth 授权完成（`oauth-completed`）。
    OauthCompleted { server_name: String },
    /// MCP OAuth 授权失败/取消/超时（`oauth-failed`）。
    OauthFailed { server_name: String, error: String },
    /// MCP OAuth 凭证恢复成功（`oauth-restored`）——快速路径：磁盘已有
    /// 有效凭证，无需重新授权；TUI 用于反馈「已使用已保存凭证连接」。
    OauthRestored { server_name: String },
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

/// CompactCompleted.trigger 缺省值：旧事件（无 trigger 字段）按 "auto" 处理。
fn default_compact_trigger() -> String {
    "auto".to_string()
}

#[cfg(test)]
mod variant_coverage_test;
