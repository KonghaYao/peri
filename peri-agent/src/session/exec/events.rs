//! 命令事件发出辅助函数（L5：自 peri-acp/src/host/exec/events.rs 与
//! peri-acp/src/session/command/compact/events.rs 迁入）。
//!
//! 集中所有 `event_sink.push_event(...)` 调用模板，统一第三参 `context_window`
//! 占位与 `ExecutorEvent` 变体构造。事件发射经 [`EventSink`] 端口
//! （ACP 协议序列化面实现），本模块不触碰协议实现。

use std::fmt::Display;
use std::sync::Arc;

use peri_acp_types::event::{
    CompactFileInfo, CompactStrategy, CompactTrigger, EventSink, ExecutorEvent,
};
use peri_acp_types::messages::{BaseMessage, MessageId};

// ── /bg 事件 ─────────────────────────────────────────────────────────────────

/// 发出 `/bg` 用法提示。
pub async fn emit_bg_usage_hint(sink: &Arc<dyn EventSink>, session_id: &str) {
    sink.push_event(
        session_id,
        &ExecutorEvent::TextChunk {
            message_id: MessageId::new(),
            chunk: "用法: /bg <任务描述>\n".into(),
            source_agent_id: None,
        },
        0,
    )
    .await;
}

/// 发出后台任务 spawn 失败的错误提示。
pub async fn emit_bg_spawn_error(sink: &Arc<dyn EventSink>, session_id: &str, error: impl Display) {
    sink.push_event(
        session_id,
        &ExecutorEvent::TextChunk {
            message_id: MessageId::new(),
            chunk: format!("✗ 后台任务启动失败: {error}\n"),
            source_agent_id: None,
        },
        0,
    )
    .await;
}

/// 发出后台任务启动确认消息（prompt 自动 CJK-safe truncation: chars().take(80)）。
pub async fn emit_bg_confirmation(sink: &Arc<dyn EventSink>, session_id: &str, prompt: &str) {
    let truncated: String = prompt.chars().take(80).collect();
    sink.push_event(
        session_id,
        &ExecutorEvent::TextChunk {
            message_id: MessageId::new(),
            chunk: format!("◆ 后台任务已启动: {truncated}\n"),
            source_agent_id: None,
        },
        0,
    )
    .await;
}

// ── /compact 事件 ────────────────────────────────────────────────────────────

/// Compact 事件统一使用的 context_window 占位（与原实现保持一致）。
pub const COMPACT_CONTEXT_WINDOW: u32 = 0;

/// CompactCompleted 事件的 micro_cleared 占位（full compact 恒为 0；micro compact
/// 才会 > 0，CompactCommand 仅支持 full compact）。
pub const FULL_COMPACT_MICRO_CLEARED: usize = 0;

/// 发出 `CompactError` 事件。
pub async fn emit_compact_error(
    sink: &Arc<dyn EventSink>,
    session_id: &str,
    message: impl Into<String>,
) {
    sink.push_event(
        session_id,
        &ExecutorEvent::CompactError {
            message: message.into(),
        },
        COMPACT_CONTEXT_WINDOW,
    )
    .await;
}

/// 发出 `CompactStarted` 事件。
pub async fn emit_compact_started(sink: &Arc<dyn EventSink>, session_id: &str) {
    sink.push_event(
        session_id,
        &ExecutorEvent::CompactStarted {
            turn_id: String::new(),
            agent_id: String::new(),
            step: 0,
            strategy: CompactStrategy::Full,
            trigger: CompactTrigger::Manual,
        },
        COMPACT_CONTEXT_WINDOW,
    )
    .await;
}

/// 发出 `CompactCompleted` 事件。
///
/// `messages` 字段与 `CommandResult.messages` 共享同一个 `new_messages.clone()`，
/// 保持事件观测数据与最终返回值一致——TUI 下游依赖此对齐。
#[allow(clippy::too_many_arguments)]
pub async fn emit_compact_completed(
    sink: &Arc<dyn EventSink>,
    session_id: &str,
    summary: String,
    files: Vec<CompactFileInfo>,
    skills: Vec<String>,
    micro_cleared: usize,
    messages: Vec<BaseMessage>,
    strategy: CompactStrategy,
    outcome: peri_acp_types::compact::CompactOutcome,
    estimated_tokens_saved: u64,
    affected_count: usize,
) {
    sink.push_event(
        session_id,
        &ExecutorEvent::CompactCompleted {
            summary,
            files,
            skills,
            micro_cleared,
            messages,
            token_before: 0,
            token_after: 0,
            strategy,
            affected_count,
            estimated_tokens_saved,
            estimated_tokens_before: 0,
            estimated_tokens_after: 0,
            changed_messages: 0,
            changed_fields: 0,
            no_op_candidates: 0,
            full_escalation_reason: None,
            cache_hit_rate_before: 0.0,
            trigger: CompactTrigger::Manual,
            outcome,
        },
        COMPACT_CONTEXT_WINDOW,
    )
    .await;
}
