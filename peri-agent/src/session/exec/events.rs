//! 命令事件发出辅助函数（L5：自 peri-acp/src/host/exec/events.rs 与
//! peri-acp/src/session/command/compact/events.rs 迁入）。
//!
//! 集中所有 `event_sink.push_event(...)` 调用模板，统一第三参 `context_window`
//! 占位与 `ExecutorEvent` 变体构造。事件发射经 [`EventSink`] 端口
//! （ACP 协议序列化面实现），本模块不触碰协议实现。

use std::sync::Arc;

use peri_acp_types::event::{CompactStrategy, CompactTrigger, EventSink, ExecutorEvent};
use peri_acp_types::messages::BaseMessage;

// ── /compact 事件 ────────────────────────────────────────────────────────────

/// Compact 事件统一使用的 context_window 占位（与原实现保持一致）。
pub const COMPACT_CONTEXT_WINDOW: u32 = 0;

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

/// 发出 `CompactCompleted` 事件（Phase 5 Step 4 收敛：状态重建信号三字段）。
///
/// `messages` 字段与 `CommandResult.messages` 共享同一个 `new_messages.clone()`，
/// 保持事件观测数据与最终返回值一致——TUI 下游依赖此对齐。
pub async fn emit_compact_completed(
    sink: &Arc<dyn EventSink>,
    session_id: &str,
    summary: String,
    messages: Vec<BaseMessage>,
    trigger: CompactTrigger,
) {
    sink.push_event(
        session_id,
        &ExecutorEvent::CompactCompleted {
            summary,
            messages,
            trigger,
        },
        COMPACT_CONTEXT_WINDOW,
    )
    .await;
}
