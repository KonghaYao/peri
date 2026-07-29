//! Compact 事件发出辅助函数。
//!
//! 集中所有 `event_sink.push_event(...)` 调用模板，统一第三参 `context_window = 0` 与
//! `ExecutorEvent::Compact*` 变体构造。消除原来 compact.rs 中 4 处 CompactError
//! 近乎相同模板（空历史/无模型/full_compact 失败/cancelled 分支）。
//!
// [TRAP] CompactCompleted 事件被 TUI 通过 StateSnapshot + 流式事件维护状态消费
// （MessageAdded 被 TUI 丢弃）。事件字段 messages 与 CommandResult.messages 共享
// new_messages.clone() —— 必须保持引用一致性。
// （详见 CLAUDE.md TUI 事件映射章节、spec/global/domains/compact.md）

use std::sync::Arc;

use peri_agent::{
    agent::events::{CompactFileInfo, CompactStrategy, CompactTrigger, ExecutorEvent},
    messages::BaseMessage,
};

use crate::session::event_sink::EventSink;

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
/// 保持事件观测数据与最终返回值一致——TUI 下游依赖此对齐（见文件级 [TRAP]）。
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
    outcome: peri_agent::agent::compact_v2::CompactOutcome,
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
            outcome,
        },
        COMPACT_CONTEXT_WINDOW,
    )
    .await;
}
